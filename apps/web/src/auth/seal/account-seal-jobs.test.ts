// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * When a device publishes the account's protection and touches the
 * owned-channels locks (ADR-0101 sections 7 and 10), with the real wasm
 * core, a directory that keeps what is uploaded, and the channels ports
 * recorded.
 */
import { beforeAll, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../../wasm/lakko_ffi_wasm";
import type { DeviceCipher } from "../device-cipher";
import {
  clearSealJobFacts,
  createSealJobs,
  owedSealRemovals,
  type OwnedPorts,
  type SealAccount,
  type SealJobDeps,
} from "./account-seal-jobs";
import { adoptOwnSealFacts, chatsKeptBy, ownSealFacts } from "./own-seals";
import { hex, readSeals } from "./seal-announcement";
import type { OwnBundleSeals, SealPorts } from "./seal-runtime";

const ACCOUNT_B64 = "acct";
const ACCOUNT_ID = new Uint8Array(16).fill(0x0a);
const PHRASE = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const exportKey = (b: number): Uint8Array => new Uint8Array(64).fill(b);
const seed = (b: number): Uint8Array => new Uint8Array(64).fill(b);
const half = (b: number): Uint8Array => new Uint8Array(32).fill(b);
const keyOf = (b: number): Uint8Array => wasm.Identity.fromSeed(seed(b)).signaturePublicKey;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

/** A device key that is not one: XOR, enough to round-trip a row. */
const deviceKey: DeviceCipher = {
  encrypt: async (_iv, plain) => plain.map((b) => b ^ 0x5a).buffer as ArrayBuffer,
  decrypt: async (_iv, ct) => ct.map((b) => b ^ 0x5a).buffer as ArrayBuffer,
};

function world(options: { account?: Partial<SealAccount> | null } = {}) {
  const map = new Map<string, string>();
  const storage = {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
  };
  const dir = {
    row: { signaturePubkey: keyOf(1), keyGeneration: 1, resetSeals: null, previousResetSeals: null } as OwnBundleSeals,
    offline: false,
    uploads: 0,
  };
  const ports: SealPorts = {
    fetchOwn: async () => {
      if (dir.offline) throw new Error("offline");
      return dir.row;
    },
    upload: async (_id, _identity, resetSeals) => {
      if (dir.offline) throw new Error("offline");
      dir.uploads += 1;
      dir.row = { ...dir.row, resetSeals };
    },
    storage,
    nowMs: () => 1,
  };
  const calls: string[] = [];
  const answers = {
    make: "made" as "made" | "retry",
    ensure: "current" as "current" | "made" | "absent" | "retry",
    retake: "done" as "done" | "retry" | "nothing",
    addLock: "added" as "added" | "absent" | "full" | "retry",
    removeLock: "removed" as "removed" | "absent" | "retry",
  };
  const seen: { retake: Parameters<OwnedPorts["retake"]>[0] | null; make: Parameters<OwnedPorts["make"]>[0] | null } = {
    retake: null,
    make: null,
  };
  const owned: OwnedPorts = {
    make: async (input) => {
      calls.push("make");
      seen.make = { ...input, exportKey: input.exportKey.slice(), serverHalf: input.serverHalf.slice() };
      return answers.make;
    },
    ensurePasswordLock: async () => {
      calls.push("ensure");
      return answers.ensure;
    },
    addPasskeyLock: async () => {
      calls.push("add-lock");
      return answers.addLock;
    },
    removePasskeyLock: async () => {
      calls.push("remove-lock");
      return answers.removeLock;
    },
    retake: async (input) => {
      calls.push("retake");
      seen.retake = {
        ...input,
        serverHalf: input.serverHalf?.slice() ?? null,
        currentExportKey: input.currentExportKey?.slice() ?? null,
      };
      return answers.retake;
    },
  };
  let account: SealAccount | null =
    options.account === null
      ? null
      : {
          accountIdB64: ACCOUNT_B64,
          accountId: ACCOUNT_ID,
          accountIdentitySeed: seed(1),
          lineageSeed: seed(1),
          keyGeneration: 1n,
          ...options.account,
        };
  const cached: { key: Uint8Array | null } = { key: null };
  const previous: { keys: Uint8Array[] } = { keys: [] };
  const deps: SealJobDeps = {
    wasm: async () => wasm,
    ports,
    owned,
    account: () => account,
    deviceKey: async () => deviceKey,
    devicePhrase: async () => PHRASE,
    cachedExportKey: () => cached.key,
    previousAccountKeys: async () => previous.keys,
    storage,
  };
  return {
    jobs: createSealJobs(deps),
    deps,
    dir,
    calls,
    answers,
    seen,
    storage,
    map,
    cached,
    previous,
    setAccount: (next: SealAccount | null) => {
      account = next;
    },
    published: () =>
      dir.row.resetSeals === null ? null : readSeals(wasm, dir.row.resetSeals, dir.row.signaturePubkey, ACCOUNT_ID),
  };
}

beforeEach(() => adoptOwnSealFacts(ACCOUNT_B64, null));

describe("sign-up and password events", () => {
  it("sign-up publishes the first announcement and makes the list and its locks, then forgets S", async () => {
    const w = world();
    w.jobs.notePassword(ACCOUNT_B64, exportKey(1));
    await w.jobs.noteServerHalf(ACCOUNT_B64, half(7));
    expect([...w.map.keys()]).toContain(`lakko:owned-server-half:${ACCOUNT_B64}`);
    await w.jobs.settle();
    expect(w.published()?.entries.map((e) => e.kind)).toEqual(["password"]);
    expect(w.calls).toEqual(["make"]);
    expect(w.seen.make?.serverHalf).toEqual(half(7));
    expect(w.seen.make?.exportKey).toEqual(exportKey(1));
    expect([...w.map.keys()]).not.toContain(`lakko:owned-server-half:${ACCOUNT_B64}`);
    expect(chatsKeptBy(ownSealFacts(ACCOUNT_B64))).toBe("password");
    // Settled: a second landing only reads.
    await w.jobs.settle();
    expect(w.calls).toEqual(["make"]);
    expect(w.dir.uploads).toBe(1);
  });

  it("keeps S while the vaults could not be made, across a reload", async () => {
    const w = world();
    w.answers.make = "retry";
    w.jobs.notePassword(ACCOUNT_B64, exportKey(1));
    await w.jobs.noteServerHalf(ACCOUNT_B64, half(7));
    await w.jobs.settle();
    expect([...w.map.keys()]).toContain(`lakko:owned-server-half:${ACCOUNT_B64}`);
    // A new page: memory is gone, the row is not; a password sign-in brings
    // the export key back and the vaults are made.
    const next = createSealJobs(w.deps);
    w.answers.make = "made";
    next.notePassword(ACCOUNT_B64, exportKey(1));
    await next.settle();
    expect(w.calls).toEqual(["make", "make"]);
    expect(w.seen.make?.serverHalf).toEqual(half(7));
    expect([...w.map.keys()]).not.toContain(`lakko:owned-server-half:${ACCOUNT_B64}`);
  });

  it("a guest keeps S and makes nothing until its first password", async () => {
    const w = world();
    await w.jobs.noteServerHalf(ACCOUNT_B64, half(7));
    await w.jobs.settle();
    expect(w.calls).toEqual([]);
    expect(w.dir.uploads).toBe(0);
    w.jobs.notePassword(ACCOUNT_B64, exportKey(3));
    await w.jobs.settle();
    expect(w.calls).toEqual(["make"]);
    expect(w.published()?.entries.map((e) => e.kind)).toEqual(["password"]);
  });

  it("a password sign-in on a device with the keys takes the password over and checks the lock", async () => {
    const w = world();
    w.jobs.notePassword(ACCOUNT_B64, exportKey(1));
    await w.jobs.settle();
    const first = hex(w.published()!.entries[0]!.sealPk);
    expect(w.calls).toEqual(["ensure"]);
    // The password was changed by a mailed code elsewhere; this device signs
    // in with the new one.
    w.jobs.notePassword(ACCOUNT_B64, exportKey(2));
    await w.jobs.settle();
    expect(hex(w.published()!.entries[0]!.sealPk)).not.toBe(first);
    expect(w.calls).toEqual(["ensure", "ensure"]);
  });

  it("a landing without a password event only reads: a cached export key is never announced", async () => {
    const w = world();
    w.jobs.notePassword(ACCOUNT_B64, exportKey(2));
    await w.jobs.settle();
    const current = hex(w.published()!.entries[0]!.sealPk);
    const later = createSealJobs(w.deps);
    w.cached.key = exportKey(1); // the previous password's, still cached
    await later.settle();
    expect(hex(w.published()!.entries[0]!.sealPk)).toBe(current);
    expect(w.dir.uploads).toBe(1);
    expect(ownSealFacts(ACCOUNT_B64)?.password).toBe(true);
  });

  it("does nothing with no account on screen, and keeps the event while offline", async () => {
    const none = world({ account: null });
    none.jobs.notePassword(ACCOUNT_B64, exportKey(1));
    await none.jobs.settle();
    expect(none.dir.uploads).toBe(0);

    const w = world();
    w.dir.offline = true;
    w.jobs.notePassword(ACCOUNT_B64, exportKey(1));
    await w.jobs.settle();
    expect(w.dir.uploads).toBe(0);
    w.dir.offline = false;
    await w.jobs.settle();
    expect(w.dir.uploads).toBe(1);
  });
});

describe("the phrase and passkeys", () => {
  async function signedUp() {
    const w = world();
    w.jobs.notePassword(ACCOUNT_B64, exportKey(1));
    await w.jobs.settle();
    w.calls.length = 0;
    return w;
  }

  it("saving a phrase publishes it; offline it is not saved", async () => {
    const w = await signedUp();
    w.dir.offline = true;
    expect(await w.jobs.savePhrase()).toBe("retry");
    expect(ownSealFacts(ACCOUNT_B64)?.phrase).toBe(false);
    w.dir.offline = false;
    expect(await w.jobs.savePhrase()).toBe("published");
    expect(chatsKeptBy(ownSealFacts(ACCOUNT_B64))).toBe("phrase");
    expect(await w.jobs.savePhrase()).toBe("published");
  });

  it("a passkey that gave a seal secret keeps the chats and gets its lock; one that gave none does not", async () => {
    const w = await signedUp();
    const id = new Uint8Array(20).fill(9);
    expect(await w.jobs.passkeyAdded({ credentialId: id, sealPrfOutput: null })).toBe(false);
    expect(w.calls).toEqual([]);
    expect(await w.jobs.passkeyAdded({ credentialId: id, sealPrfOutput: new Uint8Array(32).fill(4) })).toBe(true);
    expect(w.calls).toEqual(["add-lock"]);
    expect(chatsKeptBy(ownSealFacts(ACCOUNT_B64))).toBe("passkey");
    w.dir.offline = true;
    expect(
      await w.jobs.passkeyAdded({ credentialId: new Uint8Array(20).fill(8), sealPrfOutput: new Uint8Array(32).fill(5) }),
    ).toBe(false);
  });

  it("deleting a passkey removes its seal and its lock; offline the removal is owed and done at the next landing", async () => {
    const w = await signedUp();
    const id = new Uint8Array(20).fill(9);
    await w.jobs.passkeyAdded({ credentialId: id, sealPrfOutput: new Uint8Array(32).fill(4) });
    w.dir.offline = true;
    await w.jobs.passkeyRemoved(id);
    expect(owedSealRemovals(w.storage, ACCOUNT_B64)).toEqual([hex(wasm.resetSealPasskeyTag(id))]);
    expect(w.published()?.entries.map((e) => e.kind)).toEqual(["password", "passkey"]);
    w.dir.offline = false;
    await w.jobs.settle();
    expect(owedSealRemovals(w.storage, ACCOUNT_B64)).toEqual([]);
    expect(w.published()?.entries.map((e) => e.kind)).toEqual(["password"]);
    expect(chatsKeptBy(ownSealFacts(ACCOUNT_B64))).toBe("password");
    expect(w.calls).toContain("remove-lock");
  });

  it("a rotation carries the announcement to the new key with the new phrase", async () => {
    const w = await signedUp();
    await w.jobs.savePhrase();
    const before = w.published()!;
    w.dir.row = {
      signaturePubkey: keyOf(2),
      keyGeneration: 2,
      resetSeals: null,
      previousResetSeals: w.dir.row.resetSeals,
    };
    w.setAccount({
      accountIdB64: ACCOUNT_B64,
      accountId: ACCOUNT_ID,
      accountIdentitySeed: seed(2),
      lineageSeed: seed(1),
      keyGeneration: 2n,
    });
    expect(await w.jobs.afterRotation(keyOf(1))).toBe("published");
    const after = w.published()!;
    expect(after.keyGeneration).toBe(2n);
    expect(after.seq > before.seq).toBe(true);
    expect(after.entries.map((e) => e.kind)).toEqual(["password", "phrase"]);
    expect(hex(after.entries[0]!.sealPk)).toBe(hex(before.entries[0]!.sealPk));
  });
});

describe("after a rotation, at the next landing of any device with the keys", () => {
  it("carries the kept announcement to the new key, and only from a key the account had before", async () => {
    const w = world();
    w.jobs.notePassword(ACCOUNT_B64, exportKey(1));
    await w.jobs.settle();
    await w.jobs.savePhrase();
    const before = w.published()!;
    w.dir.row = { signaturePubkey: keyOf(2), keyGeneration: 2, resetSeals: null, previousResetSeals: w.dir.row.resetSeals };
    w.setAccount({
      accountIdB64: ACCOUNT_B64,
      accountId: ACCOUNT_ID,
      accountIdentitySeed: seed(2),
      lineageSeed: seed(1),
      keyGeneration: 2n,
    });
    // A new page, no password event. With no earlier key held, nothing is
    // carried (the kept bytes open under no key this device vouches for).
    const landed = createSealJobs(w.deps);
    w.previous.keys = [keyOf(9)];
    await landed.settle();
    expect(w.published()).toBeNull();
    w.previous.keys = [keyOf(9), keyOf(1)];
    await landed.settle();
    const after = w.published()!;
    expect(after.keyGeneration).toBe(2n);
    expect(after.entries.map((e) => e.kind)).toEqual(["password", "phrase"]);
    expect(hex(after.entries[0]!.sealPk)).toBe(hex(before.entries[0]!.sealPk));
    expect(ownSealFacts(ACCOUNT_B64)?.phrase).toBe(true);
  });
});

describe("after a reset made on this device", () => {
  const resetAccount: SealAccount = {
    accountIdB64: ACCOUNT_B64,
    accountId: ACCOUNT_ID,
    accountIdentitySeed: seed(2),
    lineageSeed: seed(2),
    keyGeneration: 2n,
  };

  function afterReset() {
    const w = world({ account: resetAccount });
    w.dir.row = { signaturePubkey: keyOf(2), keyGeneration: 2, resetSeals: null, previousResetSeals: null };
    return w;
  }

  it("publishes the password alone at the new generation and takes the channels back with what sealed it", async () => {
    const w = afterReset();
    await w.jobs.noteServerHalf(ACCOUNT_B64, half(7));
    w.jobs.noteReset({
      accountIdB64: ACCOUNT_B64,
      sealedBy: { kind: "password", exportKey: exportKey(1) },
      exportKey: exportKey(1),
    });
    expect(await w.jobs.announceAfterReset(resetAccount)).toBe("published");
    await w.jobs.settle();
    expect(w.published()?.keyGeneration).toBe(2n);
    expect(w.published()?.entries.map((e) => e.kind)).toEqual(["password"]);
    expect(w.calls).toEqual(["retake"]);
    expect(w.seen.retake?.serverHalf).toEqual(half(7));
    expect(w.seen.retake?.sealedBy?.kind).toBe("password");
    expect([...w.map.keys()]).toEqual([`lakko:seal-announcement:${ACCOUNT_B64}`]);
  });

  it("an unsealed reset opens nothing: the new account gets a list of its own", async () => {
    const w = afterReset();
    w.answers.retake = "nothing";
    await w.jobs.noteServerHalf(ACCOUNT_B64, half(7));
    w.jobs.noteReset({ accountIdB64: ACCOUNT_B64, sealedBy: null, exportKey: exportKey(4) });
    await w.jobs.settle();
    expect(w.calls).toEqual(["retake", "make"]);
    expect(w.seen.retake?.sealedBy).toBeNull();
    expect(w.seen.make?.exportKey).toEqual(exportKey(4));
  });

  it("a retake that got no answer stays owed with S kept; after a reload the cached export key opens the password lock", async () => {
    const w = afterReset();
    w.answers.retake = "retry";
    await w.jobs.noteServerHalf(ACCOUNT_B64, half(7));
    w.jobs.noteReset({
      accountIdB64: ACCOUNT_B64,
      sealedBy: { kind: "password", exportKey: exportKey(1) },
      exportKey: exportKey(1),
    });
    await w.jobs.settle();
    expect([...w.map.keys()]).toContain(`lakko:owned-retake-owed:${ACCOUNT_B64}`);
    expect([...w.map.keys()]).toContain(`lakko:owned-server-half:${ACCOUNT_B64}`);
    const reloaded = createSealJobs(w.deps);
    w.cached.key = exportKey(1);
    w.answers.retake = "done";
    await reloaded.settle();
    expect(w.seen.retake?.sealedBy).toMatchObject({ kind: "password" });
    expect(w.seen.retake?.serverHalf).toEqual(half(7));
    expect([...w.map.keys()]).not.toContain(`lakko:owned-retake-owed:${ACCOUNT_B64}`);
    expect([...w.map.keys()]).not.toContain(`lakko:owned-server-half:${ACCOUNT_B64}`);
  });

  it("Forget device clears every row this module keeps", async () => {
    const w = afterReset();
    w.answers.retake = "retry";
    await w.jobs.noteServerHalf(ACCOUNT_B64, half(7));
    w.jobs.noteReset({ accountIdB64: ACCOUNT_B64, sealedBy: null, exportKey: null });
    await w.jobs.settle();
    clearSealJobFacts(w.storage, ACCOUNT_B64);
    expect([...w.map.keys()].filter((k) => !k.startsWith("lakko:seal-announcement:"))).toEqual([]);
  });
});

describe("owed steps survive a reload (alignment 2026-11-27, item C)", () => {
  const OWED = `lakko:owned-owed-secrets:${ACCOUNT_B64}`;

  it("a passkey-sealed reset keeps what sealed it under the device key until the retake answered", async () => {
    const resetAccount: SealAccount = {
      accountIdB64: ACCOUNT_B64,
      accountId: ACCOUNT_ID,
      accountIdentitySeed: seed(2),
      lineageSeed: seed(2),
      keyGeneration: 2n,
    };
    const w = world({ account: resetAccount });
    w.dir.row = { signaturePubkey: keyOf(2), keyGeneration: 2, resetSeals: null, previousResetSeals: null };
    w.answers.retake = "retry";
    await w.jobs.noteServerHalf(ACCOUNT_B64, half(7));
    w.jobs.noteReset({
      accountIdB64: ACCOUNT_B64,
      sealedBy: { kind: "passkey", prfOutput: half(9), credentialTag: new Uint8Array(16).fill(3) },
      exportKey: exportKey(4),
    });
    await w.jobs.settle();
    expect([...w.map.keys()]).toContain(OWED);
    // The row is sealed: no secret in the clear.
    expect(w.map.get(OWED)).not.toContain(btoa(String.fromCharCode(...half(9))));
    const reloaded = createSealJobs(w.deps);
    w.answers.retake = "done";
    await reloaded.settle();
    expect(w.seen.retake?.sealedBy).toMatchObject({ kind: "passkey" });
    expect(w.seen.retake?.sealedBy?.kind === "passkey" ? Array.from(w.seen.retake.sealedBy.prfOutput) : []).toEqual(
      Array.from(half(9)),
    );
    expect(w.seen.retake?.currentExportKey).toEqual(exportKey(4));
    expect(w.seen.retake?.serverHalf).toEqual(half(7));
    expect([...w.map.keys()]).not.toContain(OWED);
  });

  it("a recall publishes the new password's seal and takes the channels back through the old keys lock, across a reload", async () => {
    const resetAccount: SealAccount = {
      accountIdB64: ACCOUNT_B64,
      accountId: ACCOUNT_ID,
      accountIdentitySeed: seed(2),
      lineageSeed: seed(2),
      keyGeneration: 2n,
    };
    const w = world({ account: resetAccount });
    w.dir.row = { signaturePubkey: keyOf(2), keyGeneration: 2, resetSeals: null, previousResetSeals: null };
    w.answers.retake = "retry";
    const root = new Uint8Array(32).fill(0x31);
    w.jobs.noteReset({ accountIdB64: ACCOUNT_B64, sealedBy: null, exportKey: exportKey(6), oldKeysRoot: root });
    expect(await w.jobs.announceAfterReset(resetAccount)).toBe("published");
    expect(w.published()?.entries.map((e) => e.kind)).toEqual(["password"]);
    await w.jobs.settle();
    expect(w.seen.retake?.oldKeysRoot).toEqual(root);
    w.answers.retake = "done";
    await createSealJobs(w.deps).settle();
    expect(w.seen.retake?.oldKeysRoot).toEqual(root);
    expect(w.seen.retake?.currentExportKey).toEqual(exportKey(6));
    expect([...w.map.keys()]).not.toContain(OWED);
  });

  async function signedUp() {
    const w = world();
    w.jobs.notePassword(ACCOUNT_B64, exportKey(1));
    await w.jobs.settle();
    w.calls.length = 0;
    return w;
  }

  it("a passkey's lock that failed is tried again at every landing, across a reload, until it lands", async () => {
    const w = await signedUp();
    w.answers.addLock = "retry";
    const id = new Uint8Array(20).fill(9);
    expect(await w.jobs.passkeyAdded({ credentialId: id, sealPrfOutput: half(4) })).toBe(true);
    expect([...w.map.keys()]).toContain(OWED);
    const reloaded = createSealJobs(w.deps);
    w.calls.length = 0;
    await reloaded.settle();
    expect(w.calls).toEqual(["add-lock"]);
    expect([...w.map.keys()]).toContain(OWED);
    w.answers.addLock = "added";
    w.calls.length = 0;
    await reloaded.settle();
    expect(w.calls).toEqual(["add-lock"]);
    expect([...w.map.keys()]).not.toContain(OWED);
    w.calls.length = 0;
    await reloaded.settle();
    expect(w.calls).toEqual([]);
  });

  it("a passkey added offline gets its seal and its lock at a later landing", async () => {
    const w = await signedUp();
    w.dir.offline = true;
    const id = new Uint8Array(20).fill(8);
    expect(await w.jobs.passkeyAdded({ credentialId: id, sealPrfOutput: half(5) })).toBe(false);
    w.dir.offline = false;
    await createSealJobs(w.deps).settle();
    expect(w.published()?.entries.map((e) => e.kind)).toEqual(["password", "passkey"]);
    expect(w.calls).toEqual(["add-lock"]);
    expect([...w.map.keys()]).not.toContain(OWED);
  });

  it("a deleted passkey's lock that could not be deleted is deleted later", async () => {
    const w = await signedUp();
    const id = new Uint8Array(20).fill(9);
    await w.jobs.passkeyAdded({ credentialId: id, sealPrfOutput: half(4) });
    w.answers.removeLock = "retry";
    await w.jobs.passkeyRemoved(id);
    w.answers.removeLock = "removed";
    w.calls.length = 0;
    await createSealJobs(w.deps).settle();
    expect(w.calls).toEqual(["remove-lock"]);
    expect([...w.map.keys()]).not.toContain(OWED);
  });

  it("a password lock that failed is made later, only while the announcement lists that password", async () => {
    const w = world();
    w.answers.make = "retry";
    w.jobs.notePassword(ACCOUNT_B64, exportKey(1));
    await w.jobs.noteServerHalf(ACCOUNT_B64, half(7));
    await w.jobs.settle();
    expect([...w.map.keys()]).toContain(OWED);
    // After a reload: the announcement still lists that password.
    w.answers.make = "made";
    w.calls.length = 0;
    await createSealJobs(w.deps).settle();
    expect(w.calls).toEqual(["make"]);
    expect(w.seen.make?.exportKey).toEqual(exportKey(1));
    expect([...w.map.keys()]).not.toContain(OWED);
  });

  it("an owed password lock is dropped once the announcement lists another password", async () => {
    const w = world();
    w.answers.ensure = "retry";
    w.jobs.notePassword(ACCOUNT_B64, exportKey(1));
    await w.jobs.settle();
    expect([...w.map.keys()]).toContain(OWED);
    // The password changed on another device: its announcement lists it.
    const other = world();
    other.dir.row = w.dir.row;
    other.jobs.notePassword(ACCOUNT_B64, exportKey(2));
    await other.jobs.settle();
    w.dir.row = other.dir.row;
    w.calls.length = 0;
    await createSealJobs(w.deps).settle();
    expect(w.calls).toEqual([]);
    expect([...w.map.keys()]).not.toContain(OWED);
  });
});
