// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 — the pure rules of journal ownership (`ds/ownership.ts`) and the
 * wasm derivations they lean on (`ds/journal-authority.ts`), run against the
 * REAL core where the property is a derivation: every device of the owner's
 * account derives the same root, a registrar key is what the owner derives
 * for that member, and the statement bodies go to the routes the DS serves.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  RANK_ADMIN,
  RANK_MEMBER,
  NoJournalAuthorityError,
  attestedResetBody,
  bindPushRouting,
  resetWithAttestation,
  claimAsOwner,
  claimOrReset,
  desiredRegistrars,
  evicteeAccountKeys,
  JournalClaimFailedError,
  claimAtCreation,
  mayBindPush,
  probeRoot,
  registerEpochWith,
  replaceEpochWith,
  revocationKeys,
  sendStatement,
  type StatementSender,
} from "./ownership";
import {
  fakeRegistrarAuthority,
  fakeRootAuthority,
  fakeStatementSigner,
  parseFakeStatement,
} from "./access-test-doubles";
import { wasmJournalAuthorityFactory, wasmRegistrarDerivation } from "./journal-authority";
import { RegistrarSeedStore } from "./registrar-seeds";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const GID = new Uint8Array(32).fill(0x42);

/** A sender that answers from a script of statuses and records every PUT. */
function scriptedSender(statuses: number[]): StatementSender & { puts: Array<{ path: string; body: string }> } {
  const puts: Array<{ path: string; body: string }> = [];
  return {
    puts,
    putStatement: vi.fn(async (path: string, body: string) => {
      puts.push({ path, body });
      const status = statuses.shift() ?? 204;
      if (status >= 400) throw Object.assign(new Error(`HTTP ${status}`), { status });
    }),
  };
}

const segmentOf = (path: string): string => path.split("/").pop() ?? "";

describe("statements on the wire", () => {
  it("a 409 is retried once with a freshly built statement, then surfaces", async () => {
    const s = scriptedSender([409, 204]);
    let builds = 0;
    await sendStatement(s, () => {
      builds += 1;
      return fakeStatementSigner("r").registerEpoch(GID, 3n, new Uint8Array(32), new Uint8Array(0));
    });
    expect(builds).toBe(2);
    const t = scriptedSender([409, 409]);
    await expect(
      sendStatement(t, () => fakeStatementSigner("r").registrars(GID, new Uint8Array(0), new Uint8Array(0))),
    ).rejects.toMatchObject({ status: 409 });
  });

  it("any other refusal surfaces at once", async () => {
    const s = scriptedSender([403]);
    await expect(
      sendStatement(s, () => fakeStatementSigner("r").registrars(GID, new Uint8Array(0), new Uint8Array(0))),
    ).rejects.toMatchObject({ status: 403 });
    expect(s.puts).toHaveLength(1);
  });
});

describe("obligation 5 — register with the root or a registrar, never without", () => {
  it("the root signs the register-epoch statement, revoke list included", async () => {
    const s = scriptedSender([]);
    await registerEpochWith(s, GID, fakeRootAuthority("root"), 8n, new Uint8Array(32).fill(8), [
      new Uint8Array(32).fill(1),
      new Uint8Array(32).fill(2),
    ]);
    const st = parseFakeStatement(s.puts[0]?.body ?? "");
    expect(segmentOf(s.puts[0]?.path ?? "")).toBe("access");
    expect(st.signer).toBe("root");
    expect(st.epoch).toBe("8");
    expect(st.keys).toBe("01".repeat(32) + "02".repeat(32));
  });

  it("a registrar signs it too", async () => {
    const s = scriptedSender([]);
    await registerEpochWith(s, GID, fakeRegistrarAuthority("reg"), 8n, new Uint8Array(32));
    expect(parseFakeStatement(s.puts[0]?.body ?? "").signer).toBe("reg");
  });

  it("without either, nothing is sent and the caller must not commit", async () => {
    const s = scriptedSender([]);
    await expect(registerEpochWith(s, GID, { kind: "none" }, 8n, new Uint8Array(32))).rejects.toBeInstanceOf(
      NoJournalAuthorityError,
    );
    expect(s.puts).toHaveLength(0);
  });
});

describe("obligations 1, 2 and 7 — claim, lazy claim, reset", () => {
  it("the owner's claim names the epoch and its key", async () => {
    const s = scriptedSender([204]);
    expect(await claimAsOwner(s, GID, fakeStatementSigner("root"), 3n, new Uint8Array(32).fill(3))).toBe(
      "claimed",
    );
    expect(segmentOf(s.puts[0]?.path ?? "")).toBe("claim");
    expect(parseFakeStatement(s.puts[0]?.body ?? "").epochKey).toBe("03".repeat(32));
  });

  it("an already-claimed journal (409): nothing more is sent — a root re-registering an old epoch would drop every later one", async () => {
    const s = scriptedSender([409, 204]);
    expect(await claimAsOwner(s, GID, fakeStatementSigner("root"), 3n, new Uint8Array(32))).toBe(
      "already-claimed",
    );
    expect(s.puts.map((p) => segmentOf(p.path))).toEqual(["claim"]);
  });

  it("a claim refused as a stranger's (401) surfaces: another root owns the journal", async () => {
    const s = scriptedSender([401]);
    await expect(claimAsOwner(s, GID, fakeStatementSigner("root"), 3n, new Uint8Array(32))).rejects.toMatchObject({
      status: 401,
    });
  });

  it("a re-founded DeviceGroup / PublisherGroup: a 409 on the claim becomes a root RESET", async () => {
    const s = scriptedSender([409, 204]);
    expect(await claimOrReset(s, GID, fakeStatementSigner("root"), 1n, new Uint8Array(32))).toBe("reset");
    expect(s.puts.map((p) => segmentOf(p.path))).toEqual(["claim", "reset"]);
    const fresh = scriptedSender([204]);
    expect(await claimOrReset(fresh, GID, fakeStatementSigner("root"), 1n, new Uint8Array(32))).toBe("claimed");
    expect(fresh.puts).toHaveLength(1);
  });
});

describe("removal: evictees and their revocation", () => {
  const acct = (n: number): Uint8Array => new Uint8Array(32).fill(n);
  const roster = [
    { leafIndex: 0, signaturePublicKey: acct(0xa0), accountKey: acct(1) },
    { leafIndex: 1, signaturePublicKey: acct(0xa1), accountKey: acct(2) },
    { leafIndex: 2, signaturePublicKey: acct(0xa2), accountKey: acct(2) },
    { leafIndex: 3, signaturePublicKey: acct(0xa3), accountKey: acct(3) },
  ];

  it("an account is evicted only when EVERY one of its leaves goes (ADR-0050)", () => {
    expect(evicteeAccountKeys(roster, new Set([1]))).toEqual([]);
    expect(evicteeAccountKeys(roster, new Set([1, 2]))).toEqual([acct(2)]);
    expect(evicteeAccountKeys(roster, new Set([3]))).toEqual([acct(3)]);
  });

  it("the owner derives the evictee's registrar key from the root seed (real core)", () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x11));
    const rootSeed = wasm.dsJournalRootSeed(owner.ed25519Seed, GID);
    const derive = wasmRegistrarDerivation(wasm);
    const [key] = revocationKeys(
      { kind: "root", signer: fakeStatementSigner("root"), rootSeed },
      [acct(3)],
      derive,
    );
    const expected = wasm.dsStatementSigner(wasm.dsJournalRegistrarSeed(rootSeed, acct(3)));
    expect(key).toEqual(expected.publicKey());
    expected.free();
    owner.free();
  });

  it("an admin cannot derive them and sends an empty list (the owner revokes on its next reconcile)", () => {
    const derive = { registrarSeed: vi.fn(), publicKeyOfSeed: vi.fn() };
    expect(revocationKeys(fakeRegistrarAuthority("adm", RANK_ADMIN), [acct(3)], derive)).toEqual([]);
    expect(derive.registrarSeed).not.toHaveBeenCalled();
  });
});

describe("obligation 3 — the owner's desired registrar set", () => {
  it("every non-owner account, admins at admin rank", () => {
    const d = desiredRegistrars({
      memberAccountKeysB64: ["own", "a", "b"],
      ownerAccountKeyB64: "own",
      adminAccountKeysB64: new Set(["b"]),
    });
    expect([...d]).toEqual([
      ["a", RANK_MEMBER],
      ["b", RANK_ADMIN],
    ]);
  });

});

describe("ADR-0065 — the attested reset of the DeviceGroup journal after a key reset", () => {
  /** A root signer whose reset body is the real `{"statement_b64"}` shape. */
  function rootWithWireBody(label: string) {
    const inner = fakeStatementSigner(label, new Uint8Array(32).fill(0xbb));
    return {
      ...inner,
      reset: (j: Uint8Array, epoch: bigint, epochKey: Uint8Array) => {
        const built = inner.reset(j, epoch, epochKey);
        return {
          path: built.path,
          bodyJson: JSON.stringify({ statement_b64: btoa(built.bodyJson) }),
          free: () => undefined,
        };
      },
    };
  }

  it("the body carries the statement AND the attestation, nothing else", () => {
    const body = JSON.parse(attestedResetBody('{"statement_b64":"c3RtdA=="}', "YXR0ZXN0")) as Record<string, string>;
    expect(body).toEqual({ statement_b64: "c3RtdA==", key_reset_attest_b64: "YXR0ZXN0" });
    expect(() => attestedResetBody('{"other":1}', "YXR0ZXN0")).toThrow(/statement_b64/);
  });

  it("a 2xx is a reset: one PUT to .../reset with both fields", async () => {
    const s = scriptedSender([204]);
    expect(await resetWithAttestation(s, GID, rootWithWireBody("new"), 1n, new Uint8Array(32).fill(1), "YXR0ZXN0")).toBe("reset");
    expect(s.puts.map((p) => segmentOf(p.path))).toEqual(["reset"]);
    const body = JSON.parse(s.puts[0]?.body ?? "") as Record<string, string>;
    expect(body["key_reset_attest_b64"]).toBe("YXR0ZXN0");
    expect(parseFakeStatement(atob(body["statement_b64"] ?? "")).signer).toBe("new");
  });

  it("a 409 is 'already reset' only when the root probe naming this root answers 204", async () => {
    const s = scriptedSender([409, 204]);
    expect(await resetWithAttestation(s, GID, rootWithWireBody("new"), 1n, new Uint8Array(32), "YXR0ZXN0")).toBe("already-reset");
    expect(s.puts.map((p) => segmentOf(p.path))).toEqual(["reset", "root"]);
    const raced = scriptedSender([409, 401]);
    await expect(
      resetWithAttestation(raced, GID, rootWithWireBody("new"), 1n, new Uint8Array(32), "YXR0ZXN0"),
    ).rejects.toMatchObject({ status: 409 });
  });

  it("a stranger's 401, a bad attestation (400) and a server without the key (503) surface as they are", async () => {
    for (const status of [401, 400, 503]) {
      await expect(
        resetWithAttestation(scriptedSender([status]), GID, rootWithWireBody("new"), 1n, new Uint8Array(32), "YXR0ZXN0"),
      ).rejects.toMatchObject({ status });
    }
  });
});

describe("second security pass — Append, Replace, the probe and the claim at creation", () => {
  it("an Append 409 surfaces to the commit path; nothing is sent as a Replace", async () => {
    const s = scriptedSender([409, 409]);
    await expect(
      registerEpochWith(s, GID, fakeRootAuthority("root"), 5n, new Uint8Array(32).fill(5)),
    ).rejects.toMatchObject({ status: 409 });
    expect(s.puts.map((p) => segmentOf(p.path))).toEqual(["access", "access"]);
    expect(s.puts.every((p) => parseFakeStatement(p.body).segment === "access")).toBe(true);
  });

  it("the repair's Replace names the epoch and its key; a member cannot send one", async () => {
    const s = scriptedSender([204]);
    await replaceEpochWith(s, GID, fakeRegistrarAuthority("adm", RANK_ADMIN), 4n, new Uint8Array(32).fill(4));
    const st = parseFakeStatement(s.puts[0]?.body ?? "");
    expect(st.segment).toBe("access-replace");
    expect(st.signer).toBe("adm");
    expect(st.epoch).toBe("4");
    expect(st.keys).toBe("");
    await expect(replaceEpochWith(scriptedSender([]), GID, { kind: "none" }, 4n, new Uint8Array(32))).rejects.toBeInstanceOf(
      NoJournalAuthorityError,
    );
  });

  it("the probe names the signer's own root: 204 = root, 401 = not root, anything else throws", async () => {
    const root = fakeStatementSigner("r", new Uint8Array(32).fill(0xaa));
    const s = scriptedSender([204]);
    expect(await probeRoot(s, GID, root)).toBe("root");
    const st = parseFakeStatement(s.puts[0]?.body ?? "");
    expect(st.segment).toBe("root");
    expect(st.newRoot).toBe("aa".repeat(32));
    expect(await probeRoot(scriptedSender([401]), GID, root)).toBe("not-root");
    await expect(probeRoot(scriptedSender([503]), GID, root)).rejects.toMatchObject({ status: 503 });
  });

  it("a claim at creation must answer 204: a 409 or any failure throws", async () => {
    await claimAtCreation(scriptedSender([204]), GID, fakeStatementSigner("root"), 1n, new Uint8Array(32));
    await expect(
      claimAtCreation(scriptedSender([409]), GID, fakeStatementSigner("root"), 1n, new Uint8Array(32)),
    ).rejects.toBeInstanceOf(JournalClaimFailedError);
    await expect(
      claimAtCreation(scriptedSender([500]), GID, fakeStatementSigner("root"), 1n, new Uint8Array(32)),
    ).rejects.toBeInstanceOf(JournalClaimFailedError);
  });
});

describe("obligation 8 — push binding", () => {
  it("the root and an admin bind; a member does not try", () => {
    expect(mayBindPush(fakeRootAuthority())).toBe(true);
    expect(mayBindPush(fakeRegistrarAuthority("a", RANK_ADMIN))).toBe(true);
    expect(mayBindPush(fakeRegistrarAuthority("m", RANK_MEMBER))).toBe(false);
    expect(mayBindPush({ kind: "none" })).toBe(false);
  });

  it("binds the journal to the push_gid", async () => {
    const s = scriptedSender([204]);
    const gidPush = new Uint8Array(32).fill(0x99);
    expect(await bindPushRouting(s, GID, fakeRootAuthority("root"), gidPush)).toBe("bound");
    expect(segmentOf(s.puts[0]?.path ?? "")).toBe("push-binding");
    expect(parseFakeStatement(s.puts[0]?.body ?? "").pushGid).toBe("99".repeat(32));
  });

  it("409 and 503 are logged and the caller continues; nothing throws", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    expect(await bindPushRouting(scriptedSender([409]), GID, fakeRootAuthority(), new Uint8Array(32))).toBe(
      "conflict",
    );
    expect(await bindPushRouting(scriptedSender([503]), GID, fakeRootAuthority(), new Uint8Array(32))).toBe(
      "disabled",
    );
    expect(await bindPushRouting(scriptedSender([]), GID, fakeRegistrarAuthority(), new Uint8Array(32))).toBe(
      "not-permitted",
    );
    warn.mockRestore();
  });
});

describe("the real signers (journal-authority.ts)", () => {
  it("every device of the owner's account derives the SAME root; another account does not", () => {
    const seeds = new RegistrarSeedStore();
    const phrase = new Uint8Array(64).fill(0x5a);
    const deviceA = wasm.Identity.fromSeed(phrase);
    const deviceB = wasm.Identity.fromSeed(phrase);
    const stranger = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x5b));
    const rootOf = (id: typeof deviceA): Uint8Array => {
      const a = wasmJournalAuthorityFactory(wasm, () => id, seeds).root(GID);
      if (a.kind !== "root") throw new Error("no root");
      const pk = a.signer.publicKey();
      a.signer.free();
      return pk;
    };
    expect(rootOf(deviceA)).toEqual(rootOf(deviceB));
    expect(rootOf(deviceA)).not.toEqual(rootOf(stranger));
    deviceA.free();
    deviceB.free();
    stranger.free();
  });

  it("no account identity: no root; no stored seed: no registrar", () => {
    const f = wasmJournalAuthorityFactory(wasm, () => null, new RegistrarSeedStore());
    expect(f.root(GID).kind).toBe("none");
    expect(f.registrar(GID).kind).toBe("none");
  });

  it("the real statements name the routes the DS serves", () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x31));
    const root = wasm.dsJournalRootSigner(owner, GID);
    const hex = "42".repeat(32);
    const key = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x32)).signaturePublicKey;
    const cases: Array<[string, { path: string; bodyJson: string; free(): void }]> = [
      ["claim", root.claim(GID, 1n, key)],
      ["reset", root.reset(GID, 1n, key)],
      ["access", root.registerEpoch(GID, 2n, key, new Uint8Array(0))],
      ["registrars", root.registrars(GID, key, Uint8Array.from([RANK_MEMBER]))],
      ["registrars-set", root.registrarsSet(GID, key, Uint8Array.from([RANK_MEMBER]))],
      ["access", root.replaceEpoch(GID, 2n, key, new Uint8Array(0))],
      ["root", root.rootTransfer(GID, root.publicKey())],
      ["push-binding", root.pushBinding(GID, new Uint8Array(32).fill(7))],
    ];
    for (const [segment, req] of cases) {
      expect(req.path).toBe(`/ds/group/${hex}/${segment}`);
      expect(Object.keys(JSON.parse(req.bodyJson) as object)).toEqual(["statement_b64"]);
      req.free();
    }
    root.free();
    owner.free();
  });
});
