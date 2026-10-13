// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * KT-log S3c — posture tests for the transparency-log check threaded
 * into `ContactsClient.verifyBundleAccountKey`.
 *
 * Verifies the EXACT S3c posture:
 *   * `"rollback"`  → HARD FAIL (`LogRollbackDetected`).
 *   * `"mismatch"`  → recorded + surfaced, does NOT block.
 *   * `"absent"` / `"unreachable"` / `"off"` → recorded, does NOT block.
 *   * The supersession gate is NOT weakened — a `SupersessionRefused`
 *     short-circuits before the log check runs.
 *
 * `transparency/verify` is mocked (`vi.mock`) so the posture is driven
 * deterministically without standing up a transparency service. The
 * supersession gate uses the REAL wasm core so the primary trust path
 * is exercised end-to-end.
 */
import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import type {
  LogVerifyResult,
  LogVerifyStatus,
  RotationChainResult,
} from "../transparency/verify";
import type { IdentityFetchResponse } from "../discovery/wire";

// Mock the transparency verifier — the contacts client calls
// `verifyIdentityInLog` (gate 2, non-rotation) and `verifyRotationChain`
// (gate 2, G-30 #2 rotation); we control both per test.
const verifyIdentityInLogMock = vi.fn<
  (a: Uint8Array, b: Uint8Array) => Promise<LogVerifyResult>
>();
const verifyRotationChainMock = vi.fn<
  (
    a: Uint8Array,
    b: Uint8Array,
    ctx: {
      supersessionProofB64: string;
      pinnedOldPubkey: Uint8Array;
      pinnedLogVersion: number;
    },
  ) => Promise<RotationChainResult>
>();
vi.mock("../transparency/verify", () => ({
  verifyIdentityInLog: (a: Uint8Array, b: Uint8Array) =>
    verifyIdentityInLogMock(a, b),
  verifyRotationChain: (
    a: Uint8Array,
    b: Uint8Array,
    ctx: {
      supersessionProofB64: string;
      pinnedOldPubkey: Uint8Array;
      pinnedLogVersion: number;
    },
  ) => verifyRotationChainMock(a, b, ctx),
}));

/** Helper — wrap a bare status into the `{status, isReset}` shape the
 *  client now consumes. `isReset` defaults to `null` (no latest read). */
function result(
  status: LogVerifyStatus,
  isReset: boolean | null = null,
): LogVerifyResult {
  return { status, isReset };
}

/** Helper — build a `RotationChainResult` for the chain mock. */
function chainResult(
  status: LogVerifyStatus,
  isReset: boolean | null = null,
  version: number | null = null,
): RotationChainResult {
  return { status, isReset, version };
}

// Import AFTER the mock is registered.
const { ContactsClient, SupersessionRefused, LogRollbackDetected } =
  await import("./client");
const { bindContactsAccount, readPin, readLogStatus, readLogReset, readLogVersion } =
  await import("./local-store");

/** In-memory `localStorage` shim. */
class MemoryStorage {
  private map = new Map<string, string>();
  getItem(k: string): string | null {
    return this.map.has(k) ? (this.map.get(k) as string) : null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, String(v));
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
  clear(): void {
    this.map.clear();
  }
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return Array.from(this.map.keys())[i] ?? null;
  }
}

function b64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i] ?? 0);
  return btoa(s);
}

const ACCOUNT_ID = new Uint8Array([
  1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
]);

function identityFromSeed(seed: number): {
  signaturePubkeyB64: string;
  x25519PubkeyB64: string;
  ed25519Seed: Uint8Array;
} {
  const id = wasm.Identity.fromSeed(new Uint8Array(32).fill(seed));
  return {
    signaturePubkeyB64: b64(id.signaturePublicKey),
    x25519PubkeyB64: b64(id.x25519PublicKey),
    ed25519Seed: id.ed25519Seed,
  };
}

/** Window (seconds) for a minted supersession proof — well within the
 *  verifier's 30-day ceiling. */
const PROOF_WINDOW_SECS = 14n * 24n * 60n * 60n;

function gate(bundle: IdentityFetchResponse): Promise<void> {
  const client = new ContactsClient();
  return (
    client as unknown as {
      verifyBundleAccountKey: (
        accountId: Uint8Array,
        bundle: IdentityFetchResponse,
      ) => Promise<void>;
    }
  ).verifyBundleAccountKey(ACCOUNT_ID, bundle);
}

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  // Pins belong to the signed-in account.
  bindContactsAccount("dmlld2VyLWFjY291bnQ=");
  verifyIdentityInLogMock.mockReset();
  verifyRotationChainMock.mockReset();
  // Silence the best-effort console.warn calls during the suite.
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
});

afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
  vi.restoreAllMocks();
});

describe("transparency-log posture in verifyBundleAccountKey (S3c)", () => {
  it('"rollback" HARD-FAILS with LogRollbackDetected (after TOFU-pin)', async () => {
    const peer = identityFromSeed(11);
    verifyIdentityInLogMock.mockResolvedValue(result("rollback"));
    let thrown: unknown;
    try {
      await gate({
        signature_pubkey: peer.signaturePubkeyB64,
        x25519_pubkey: peer.x25519PubkeyB64, key_generation: 1
      });
    } catch (e) {
      thrown = e;
    }
    expect(thrown).toBeInstanceOf(LogRollbackDetected);
    // The supersession gate still ran first and TOFU-pinned the key
    // (rollback is the ADDITIVE layer's verdict, raised after).
    expect(readPin(b64(ACCOUNT_ID))?.pinnedSignaturePubkeyB64).toBe(
      peer.signaturePubkeyB64,
    );
  });

  it('"confirmed" records the verdict and does NOT block', async () => {
    const peer = identityFromSeed(12);
    verifyIdentityInLogMock.mockResolvedValue(result("confirmed", false));
    await expect(
      gate({
        signature_pubkey: peer.signaturePubkeyB64,
        x25519_pubkey: peer.x25519PubkeyB64, key_generation: 1
      }),
    ).resolves.toBeUndefined();
    expect(readLogStatus(b64(ACCOUNT_ID))).toBe("confirmed");
    // KT-log S4: the latest entry's isReset marker is persisted too.
    expect(readLogReset(b64(ACCOUNT_ID))).toBe(false);
  });

  it('"mismatch" is recorded + surfaced but does NOT block (S3c)', async () => {
    const peer = identityFromSeed(13);
    verifyIdentityInLogMock.mockResolvedValue(result("mismatch", false));
    await expect(
      gate({
        signature_pubkey: peer.signaturePubkeyB64,
        x25519_pubkey: peer.x25519PubkeyB64, key_generation: 1
      }),
    ).resolves.toBeUndefined();
    expect(readLogStatus(b64(ACCOUNT_ID))).toBe("mismatch");
  });

  it.each(["absent", "unreachable", "off"] as const)(
    '"%s" is recorded and does NOT block',
    async (status) => {
      const peer = identityFromSeed(14);
      verifyIdentityInLogMock.mockResolvedValue(result(status));
      await expect(
        gate({
          signature_pubkey: peer.signaturePubkeyB64,
          x25519_pubkey: peer.x25519PubkeyB64, key_generation: 1
        }),
      ).resolves.toBeUndefined();
      expect(readLogStatus(b64(ACCOUNT_ID))).toBe(status);
    },
  );

  it("an unexpected throw from the log verifier does NOT break the path", async () => {
    const peer = identityFromSeed(15);
    verifyIdentityInLogMock.mockRejectedValue(new Error("boom"));
    await expect(
      gate({
        signature_pubkey: peer.signaturePubkeyB64,
        x25519_pubkey: peer.x25519PubkeyB64, key_generation: 1
      }),
    ).resolves.toBeUndefined();
    // No status recorded (the check threw), but the pin is set and the
    // path completed — the additive check never breaks the primary.
    expect(readPin(b64(ACCOUNT_ID))?.pinnedSignaturePubkeyB64).toBe(
      peer.signaturePubkeyB64,
    );
  });

  it("does NOT weaken the supersession gate: a changed key with no proof and no attestation still refuses", async () => {
    const original = identityFromSeed(21);
    const rotated = identityFromSeed(22);
    // First contact TOFU-pins the original key (log says off).
    verifyIdentityInLogMock.mockResolvedValue(result("off"));
    await gate({
      signature_pubkey: original.signaturePubkeyB64,
      x25519_pubkey: original.x25519PubkeyB64, key_generation: 1
    });
    verifyIdentityInLogMock.mockClear();

    // A changed key with NO supersession proof and NO key-reset attestation
    // must fail-closed (ADR-0073 R.2 row 1). The verified log read only
    // chooses the refusal copy.
    let thrown: unknown;
    try {
      await gate({
        signature_pubkey: rotated.signaturePubkeyB64,
        x25519_pubkey: rotated.x25519PubkeyB64, key_generation: 1
      });
    } catch (e) {
      thrown = e;
    }
    expect(thrown).toBeInstanceOf(SupersessionRefused);
    expect((thrown as InstanceType<typeof SupersessionRefused>).reason).toBe(
      "proof-absent",
    );
    // The pin still holds the ORIGINAL key (the change was refused).
    expect(readPin(b64(ACCOUNT_ID))?.pinnedSignaturePubkeyB64).toBe(
      original.signaturePubkeyB64,
    );
  });
});

describe("G-30 #2 — rotation routes through the chain verifier", () => {
  /** Mint a real supersession proof old→new and drive a verified
   *  rotation through gate 1, returning the bundle the rotation served.
   *  The default gate-2 verdict is supplied via the chain mock. */
  async function driveRotation(
    chainVerdict: RotationChainResult,
    oldSeed: number,
    newSeed: number,
  ): Promise<{ accountIdB64: string; newSig: string }> {
    const oldPeer = identityFromSeed(oldSeed);
    const newPeer = identityFromSeed(newSeed);
    const accountIdB64 = b64(ACCOUNT_ID);

    // First contact TOFU-pins the OLD key (a non-rotation; routes
    // through verifyIdentityInLog → "off").
    verifyIdentityInLogMock.mockResolvedValue(result("off"));
    await gate({
      signature_pubkey: oldPeer.signaturePubkeyB64,
      x25519_pubkey: oldPeer.x25519PubkeyB64, key_generation: 1
    });
    verifyIdentityInLogMock.mockClear();

    // Mint a genuine proof so gate 1 re-pins (does NOT refuse).
    const now = BigInt(Math.floor(Date.now() / 1000));
    const proof = wasm.signSupersessionProof(
      oldPeer.ed25519Seed,
      newPeer.ed25519Seed,
      now,
      now + PROOF_WINDOW_SECS,
    );

    // The rotation's gate-2 verdict comes from the chain mock.
    verifyRotationChainMock.mockResolvedValue(chainVerdict);

    await gate({
      signature_pubkey: newPeer.signaturePubkeyB64,
      x25519_pubkey: newPeer.x25519PubkeyB64, key_generation: 1,
      supersession_proof: b64(proof),
    });
    return { accountIdB64, newSig: newPeer.signaturePubkeyB64 };
  }

  it("a verified rotation routes through verifyRotationChain, NOT verifyIdentityInLog", async () => {
    const { newSig } = await driveRotation(
      chainResult("confirmed", false, 5),
      61,
      62,
    );
    // The chain verifier ran for the rotation; the single-key check did not.
    expect(verifyRotationChainMock).toHaveBeenCalledTimes(1);
    expect(verifyIdentityInLogMock).not.toHaveBeenCalled();
    // Gate 1 re-pinned to the new key (untouched by this sprint).
    expect(readPin(b64(ACCOUNT_ID))?.pinnedSignaturePubkeyB64).toBe(newSig);
  });

  it("a confirmed chain records the proven log version on the pin", async () => {
    const { accountIdB64 } = await driveRotation(
      chainResult("confirmed", false, 9),
      63,
      64,
    );
    expect(readLogStatus(accountIdB64)).toBe("confirmed");
    expect(readLogVersion(accountIdB64)).toBe(9);
  });

  it("passes the OLD pinned key + the pinned log-version floor into the chain", async () => {
    // Seed a chain-version floor on the original pin before the rotation
    // by confirming a prior chain... simpler: assert the ctx the mock saw.
    await driveRotation(chainResult("confirmed", false, 3), 65, 66);
    const ctx = verifyRotationChainMock.mock.calls[0]?.[2];
    expect(ctx).toBeDefined();
    // The OLD key (seed 65) is what gate 1 had pinned before re-pinning.
    const oldPeer = identityFromSeed(65);
    expect(b64(ctx?.pinnedOldPubkey as Uint8Array)).toBe(
      oldPeer.signaturePubkeyB64,
    );
    // No prior chain confirmation → the floor defaults to 0.
    expect(ctx?.pinnedLogVersion).toBe(0);
    expect(ctx?.supersessionProofB64.length).toBeGreaterThan(0);
  });

  it('a chain "mismatch" is recorded + surfaced but does NOT block, and does NOT record a version', async () => {
    const { accountIdB64 } = await driveRotation(
      chainResult("mismatch", false, null),
      67,
      68,
    );
    expect(readLogStatus(accountIdB64)).toBe("mismatch");
    // mismatch carries version=null → no version recorded.
    expect(readLogVersion(accountIdB64)).toBeUndefined();
  });

  it('a chain "rollback" HARD-FAILS with LogRollbackDetected', async () => {
    const oldPeer = identityFromSeed(71);
    const newPeer = identityFromSeed(72);
    verifyIdentityInLogMock.mockResolvedValue(result("off"));
    await gate({
      signature_pubkey: oldPeer.signaturePubkeyB64,
      x25519_pubkey: oldPeer.x25519PubkeyB64, key_generation: 1
    });
    const now = BigInt(Math.floor(Date.now() / 1000));
    const proof = wasm.signSupersessionProof(
      oldPeer.ed25519Seed,
      newPeer.ed25519Seed,
      now,
      now + PROOF_WINDOW_SECS,
    );
    verifyRotationChainMock.mockResolvedValue(chainResult("rollback"));
    let thrown: unknown;
    try {
      await gate({
        signature_pubkey: newPeer.signaturePubkeyB64,
        x25519_pubkey: newPeer.x25519PubkeyB64, key_generation: 1,
        supersession_proof: b64(proof),
      });
    } catch (e) {
      thrown = e;
    }
    expect(thrown).toBeInstanceOf(LogRollbackDetected);
  });

  it("a non-rotation (unchanged key) still routes through verifyIdentityInLog", async () => {
    const peer = identityFromSeed(73);
    verifyIdentityInLogMock.mockResolvedValue(result("confirmed", false));
    // First sight: TOFU + single-key check.
    await gate({
      signature_pubkey: peer.signaturePubkeyB64,
      x25519_pubkey: peer.x25519PubkeyB64, key_generation: 1
    });
    expect(verifyIdentityInLogMock).toHaveBeenCalledTimes(1);
    expect(verifyRotationChainMock).not.toHaveBeenCalled();

    // Re-fetch the SAME key: still a non-rotation (no key change).
    await gate({
      signature_pubkey: peer.signaturePubkeyB64,
      x25519_pubkey: peer.x25519PubkeyB64, key_generation: 1
    });
    expect(verifyIdentityInLogMock).toHaveBeenCalledTimes(2);
    expect(verifyRotationChainMock).not.toHaveBeenCalled();
  });

  it("a changed key with NO proof stays a non-rotation refusal (gate 1, chain never runs)", async () => {
    const oldPeer = identityFromSeed(74);
    const newPeer = identityFromSeed(75);
    verifyIdentityInLogMock.mockResolvedValue(result("off"));
    await gate({
      signature_pubkey: oldPeer.signaturePubkeyB64,
      x25519_pubkey: oldPeer.x25519PubkeyB64, key_generation: 1
    });
    verifyIdentityInLogMock.mockClear();
    // A changed key WITHOUT a proof → gate 1 refuses BEFORE gate 2.
    let thrown: unknown;
    try {
      await gate({
        signature_pubkey: newPeer.signaturePubkeyB64,
        x25519_pubkey: newPeer.x25519PubkeyB64, key_generation: 1
      });
    } catch (e) {
      thrown = e;
    }
    expect(thrown).toBeInstanceOf(SupersessionRefused);
    // The rotation chain never ran: with no proof this is not a rotation.
    expect(verifyRotationChainMock).not.toHaveBeenCalled();
  });

  it("the chain floor reflects a previously-confirmed version on the SECOND rotation", async () => {
    // Rotation 1 confirms version 4 → floor stored.
    const { accountIdB64 } = await driveRotation(
      chainResult("confirmed", false, 4),
      81,
      82,
    );
    expect(readLogVersion(accountIdB64)).toBe(4);
    verifyRotationChainMock.mockClear();

    // Rotation 2: from key (seed 82) to a third key (seed 83). Mint a
    // proof old(82)→new(83); the chain floor should now be 4.
    const midPeer = identityFromSeed(82);
    const thirdPeer = identityFromSeed(83);
    const now = BigInt(Math.floor(Date.now() / 1000));
    const proof = wasm.signSupersessionProof(
      midPeer.ed25519Seed,
      thirdPeer.ed25519Seed,
      now,
      now + PROOF_WINDOW_SECS,
    );
    verifyRotationChainMock.mockResolvedValue(
      chainResult("confirmed", false, 5),
    );
    await gate({
      signature_pubkey: thirdPeer.signaturePubkeyB64,
      x25519_pubkey: thirdPeer.x25519PubkeyB64, key_generation: 1,
      supersession_proof: b64(proof),
    });
    const ctx = verifyRotationChainMock.mock.calls[0]?.[2];
    expect(ctx?.pinnedLogVersion).toBe(4);
    // The confirmed v5 advances the floor.
    expect(readLogVersion(accountIdB64)).toBe(5);
  });
});

describe("ADR-0073 R.2 — the refusal copy of a proof-less change with no attestation", () => {
  /** TOFU-pin an original key, then attempt an UNPROVEN changed key with no
   *  attestation and capture the thrown `SupersessionRefused`. The copy
   *  marker comes from the VERIFIED log read (`verifyIdentityInLog`). */
  async function refuseChangedKey(
    log: LogVerifyResult | Error,
  ): Promise<InstanceType<typeof SupersessionRefused>> {
    const original = identityFromSeed(31);
    const rotated = identityFromSeed(32);
    verifyIdentityInLogMock.mockResolvedValue(result("off"));
    await gate({
      signature_pubkey: original.signaturePubkeyB64,
      x25519_pubkey: original.x25519PubkeyB64, key_generation: 1
    });
    if (log instanceof Error) verifyIdentityInLogMock.mockRejectedValue(log);
    else verifyIdentityInLogMock.mockResolvedValue(log);
    let thrown: unknown;
    try {
      await gate({
        signature_pubkey: rotated.signaturePubkeyB64,
        x25519_pubkey: rotated.x25519PubkeyB64, key_generation: 2
      });
    } catch (e) {
      thrown = e;
    }
    expect(thrown).toBeInstanceOf(SupersessionRefused);
    // Nothing was re-pinned and nothing recorded.
    expect(readPin(b64(ACCOUNT_ID))?.pinnedSignaturePubkeyB64).toBe(original.signaturePubkeyB64);
    return thrown as InstanceType<typeof SupersessionRefused>;
  }

  it("the log shows the key WITHOUT a reset: loggedReset=false (unexplained)", async () => {
    const err = await refuseChangedKey(result("confirmed", false));
    expect(err.reason).toBe("proof-absent");
    expect(err.loggedReset).toBe(false);
  });

  it("the log shows a reset but no attestation is served: still refused, unconfirmed", async () => {
    const err = await refuseChangedKey(result("confirmed", true));
    expect(err.loggedReset).toBeNull();
  });

  it("the log is off / unreachable / absent: refused, unconfirmed", async () => {
    for (const status of ["off", "unreachable", "absent", "mismatch"] as const) {
      const err = await refuseChangedKey(result(status));
      expect(err.loggedReset).toBeNull();
    }
  });

  it("a throwing log read leaves the refusal standing, unconfirmed", async () => {
    const err = await refuseChangedKey(new Error("log boom"));
    expect(err.loggedReset).toBeNull();
  });
});
