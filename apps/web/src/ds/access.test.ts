// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0053 — the pure rules of the client contract (`ds/access.ts`), and the
 * two facts about the real wasm signer the rest of the client leans on: the
 * header name is the one the DS reads, and a mailbox signer names EXACTLY the
 * mailbox each of the two pollers fetches (a signer for a different pseudonym
 * would sign every read of the right mailbox under the wrong key, and every
 * read would be refused).
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

import {
  CLOCK_SKEW_NOTICE,
  DS_PROOF_HEADER,
  DsAccessRegistry,
  GroupNotClaimedError,
  dsClockSkewNotice,
  isClockSkewed,
  knockToJoin,
  parseHttpDate,
  registerBeforePublish,
  type EpochRegistrar,
  type KnockPorts,
} from "./access";
import { fakeRegistrarAuthority, fakeRootAuthority, fakeSigner } from "./access-test-doubles";

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

describe("the wasm signer the client drives", () => {
  it("the header the DS client sends is the one the DS reads", () => {
    expect(wasm.dsProofHeaderName()).toBe(DS_PROOF_HEADER);
  });

  it("the ACCOUNT mailbox signer names the mailbox `pollMailbox` fetches", () => {
    // `pollMailbox` derives its pseudonym from the account identity's key.
    const account = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x21));
    const polled = wasm.IdentityPseudonym.fromIdentity(account.signaturePublicKey).asBytes();
    const signer = wasm.dsMailboxSigner(account);
    expect(sameBytes(signer.mailboxPseudonym(), polled)).toBe(true);
    expect(signer.epoch).toBe(wasm.dsMailboxEpoch());
    signer.free();
    account.free();
  });

  it("the DEVICE mailbox signer names the mailbox `pollDeviceMailbox` fetches (where Welcomes land)", () => {
    // `pollDeviceMailbox` derives its pseudonym from the DEVICE identity's
    // key, and the direct add and the sweep address a Welcome to it through
    // `IdentityPseudonym.fromIdentity(keyPackageSignatureKey(kp))`.
    const account = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x22));
    const device = wasm.Identity.fromSeedBound(
      new Uint8Array(32).fill(0x23),
      account.ed25519Seed,
      1_700_000_000n,
    );
    const polled = wasm.IdentityPseudonym.fromIdentity(device.signaturePublicKey).asBytes();
    const kp = device.createKeyPackage();
    const addressed = wasm.IdentityPseudonym.fromIdentity(wasm.keyPackageSignatureKey(kp)).asBytes();
    const signer = wasm.dsMailboxSigner(device);
    expect(sameBytes(signer.mailboxPseudonym(), polled)).toBe(true);
    expect(sameBytes(signer.mailboxPseudonym(), addressed)).toBe(true);
    // And the two mailboxes are different journals with different keys.
    const accountSigner = wasm.dsMailboxSigner(account);
    expect(sameBytes(accountSigner.mailboxPseudonym(), polled)).toBe(false);
    accountSigner.free();
    signer.free();
    device.free();
    account.free();
  });

  it("a group signer proves requests at its epoch (ADR-0057: it registers nothing)", () => {
    const id = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x24));
    const group = new wasm.Group(id);
    const s0 = group.dsAccessSigner(id);
    expect(s0.verifyingKey().length).toBe(32);
    const header = s0.sign("GET", "/ds/group/aa/messages", "after=0&wait=25", new Uint8Array(0));
    expect(header.startsWith(`v1.${s0.epoch}.`)).toBe(true);
    // No staged sweep: nothing to register ahead of a publish.
    expect(group.dsStagedAccessSigner(id)).toBeUndefined();
    s0.free();
    group.free();
    id.free();
  });
});

describe("DsAccessRegistry", () => {
  it("resolves through the first source that holds the group, and keeps the PREVIOUS epoch's signer on a change", () => {
    const reg = new DsAccessRegistry();
    let epoch = 3n;
    reg.registerGroupSource(() => null);
    reg.registerGroupSource(() => fakeSigner(epoch));
    const gid = new Uint8Array([1, 2]);
    expect(reg.groupSigners(gid).current?.epoch).toBe(3n);
    expect(reg.groupSigners(gid).previous).toBeNull();
    epoch = 4n;
    const after = reg.groupSigners(gid);
    expect(after.current?.epoch).toBe(4n);
    expect(after.previous?.epoch).toBe(3n);
  });

  it("frees a fresh signer that repeats the cached one, and the evicted one on a second change", () => {
    const reg = new DsAccessRegistry();
    let epoch = 1n;
    const freed: bigint[] = [];
    reg.registerGroupSource(() => {
      const s = fakeSigner(epoch);
      return { ...s, free: () => freed.push(s.epoch) };
    });
    const gid = new Uint8Array([7]);
    reg.groupSigners(gid);
    reg.groupSigners(gid); // same epoch: the fresh one is dropped
    expect(freed).toEqual([1n]);
    epoch = 2n;
    reg.groupSigners(gid);
    epoch = 3n;
    reg.groupSigners(gid); // epoch 1 was `previous`; now evicted
    expect(freed).toEqual([1n, 1n]);
  });

  it("an unregistered source stops answering; a mailbox signer answers only its own pseudonym", () => {
    const reg = new DsAccessRegistry();
    const off = reg.registerGroupSource(() => fakeSigner(9n));
    off();
    expect(reg.groupSigners(new Uint8Array([5])).current).toBeNull();
    const signer = fakeSigner(0n, "m");
    reg.setMailboxSigner(signer);
    expect(reg.mailboxSigner(signer.mailboxPseudonym())).toBe(signer);
    expect(reg.mailboxSigner(new Uint8Array(32).fill(0x01))).toBeNull();
  });

  it("ADR-0057: the authority is the root when a source says this account owns the journal", () => {
    const reg = new DsAccessRegistry();
    const gid = new Uint8Array([4]);
    expect(reg.journalAuthority(gid).kind).toBe("none");
    const factory = {
      root: vi.fn(() => fakeRootAuthority()),
      registrar: vi.fn(() => fakeRegistrarAuthority()),
    };
    reg.setAuthorityFactory(factory);
    // No owner source: the registrar seed (or none).
    expect(reg.journalAuthority(gid).kind).toBe("registrar");
    const off = reg.registerOwnerSource((g) => (g[0] === 4 ? true : null));
    expect(reg.ownsJournal(gid)).toBe(true);
    expect(reg.journalAuthority(gid).kind).toBe("root");
    expect(reg.ownsJournal(new Uint8Array([5]))).toBe(false);
    off();
    expect(reg.journalAuthority(gid).kind).toBe("registrar");
  });

  it("an owner source that says `false` stops the search: not the owner", () => {
    const reg = new DsAccessRegistry();
    reg.registerOwnerSource(() => false);
    reg.registerOwnerSource(() => true);
    expect(reg.ownsJournal(new Uint8Array([1]))).toBe(false);
  });

  it("sign-out clears the factory: no authority", () => {
    const reg = new DsAccessRegistry();
    reg.setAuthorityFactory({ root: () => fakeRootAuthority(), registrar: () => fakeRegistrarAuthority() });
    reg.clear();
    expect(reg.journalAuthority(new Uint8Array([1])).kind).toBe("none");
  });
});

describe("contract point 5 — the clock", () => {
  it("more than a minute either way is skew; a missing Date is not", () => {
    const now = Date.parse("2026-09-27T12:00:00Z");
    expect(isClockSkewed(now + 60_000, now)).toBe(false);
    expect(isClockSkewed(now + 60_001, now)).toBe(true);
    expect(isClockSkewed(now - 120_000, now)).toBe(true);
    expect(isClockSkewed(null, now)).toBe(false);
    expect(parseHttpDate("Sun, 27 Sep 2026 12:00:00 GMT")).toBe(now);
    expect(parseHttpDate("not a date")).toBeNull();
  });

  it("the copy reaches the user through a wrapped error, and only for a skewed 401", () => {
    const skewed = { status: 401, clockSkewed: true };
    expect(dsClockSkewNotice({ message: "x", cause: skewed })).toBe(CLOCK_SKEW_NOTICE());
    expect(dsClockSkewNotice({ variant: { cause: skewed } })).toBe(CLOCK_SKEW_NOTICE());
    expect(dsClockSkewNotice({ cause: { status: 401, clockSkewed: false } })).toBeNull();
    expect(dsClockSkewNotice({ cause: { status: 500, clockSkewed: true } })).toBeNull();
    expect(dsClockSkewNotice(new Error("offline"))).toBeNull();
    expect(CLOCK_SKEW_NOTICE()).toBe("Check this device's date and time.");
  });
});

function recordingRegistrar(fail = false): EpochRegistrar & {
  readonly calls: Array<{ epoch: bigint; revoke: number }>;
} {
  const calls: Array<{ epoch: bigint; revoke: number }> = [];
  return {
    calls,
    registerGroupEpoch: vi.fn(async (_g: Uint8Array, epoch: bigint, _k: Uint8Array, revoke?: readonly Uint8Array[]) => {
      calls.push({ epoch, revoke: revoke?.length ?? 0 });
      if (fail) throw new Error("401 proof required");
    }),
  };
}

describe("contract point 3 — register before publish (ADR-0057: signed by the authority)", () => {
  it("registers N+1's key; the key of the epoch left proves nothing", async () => {
    const r = recordingRegistrar();
    await registerBeforePublish(r, new Uint8Array([1]), fakeSigner(6n));
    expect(r.calls).toEqual([{ epoch: 6n, revoke: 0 }]);
  });

  it("carries the revocation list", async () => {
    const r = recordingRegistrar();
    await registerBeforePublish(r, new Uint8Array([1]), fakeSigner(6n), [new Uint8Array(32)]);
    expect(r.calls).toEqual([{ epoch: 6n, revoke: 1 }]);
  });

  it("a refusal propagates, so the caller aborts", async () => {
    const r = recordingRegistrar(true);
    await expect(registerBeforePublish(r, new Uint8Array([1]), fakeSigner(6n))).rejects.toThrow(
      /proof required/,
    );
  });

  it("an underivable next key aborts without a request", async () => {
    const r = recordingRegistrar();
    await expect(registerBeforePublish(r, new Uint8Array([1]), null)).rejects.toThrow(
      /could not be derived/,
    );
    expect(r.calls).toEqual([]);
  });
});

describe("contract point 6 — the knock", () => {
  function ports(epoch: number | null): KnockPorts & { readonly log: string[]; knock: KnockPorts["knock"] } {
    const log: string[] = [];
    return {
      log,
      knock: async (payloads) => {
        log.push(`knock:[${payloads.map((p) => p[0]).join(",")}]`);
        return { ids: payloads.map((_, i) => i + 1), epoch };
      },
      buildProposal: (e) => {
        log.push(`proposal@${e}`);
        return new Uint8Array([0x0b]);
      },
      buildAttribution: () => new Uint8Array([0x0a]),
    };
  }

  it("probes with an empty knock, builds the proposal for THAT epoch, then knocks with attribution + proposal", async () => {
    const p = ports(12);
    const out = await knockToJoin(p);
    expect(out.epoch).toBe(12n);
    expect(p.log).toEqual(["knock:[]", "proposal@12", "knock:[10,11]"]);
  });

  it("an unclaimed group returns no epoch: no proposal is built, nothing else is knocked", async () => {
    const p = ports(null);
    await expect(knockToJoin(p)).rejects.toBeInstanceOf(GroupNotClaimedError);
    expect(p.log).toEqual(["knock:[]"]);
  });

  it("a refused knock surfaces as is (the token retry lives in the DS client)", async () => {
    const p = ports(12);
    p.knock = async () => {
      throw Object.assign(new Error("boom"), { status: 500 });
    };
    await expect(knockToJoin(p)).rejects.toMatchObject({ status: 500 });
  });
});
