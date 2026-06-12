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
} from "../transparency/verify";
import type { IdentityFetchResponse } from "../discovery/wire";

// Mock the transparency verifier — the contacts client calls
// `verifyIdentityInLog` (gate 2) and `lookupServedKeyReset` (the gate-1
// S4 enrichment); we control both per test.
const verifyIdentityInLogMock = vi.fn<
  (a: Uint8Array, b: Uint8Array) => Promise<LogVerifyResult>
>();
const lookupServedKeyResetMock = vi.fn<
  (a: Uint8Array, b: Uint8Array) => Promise<boolean | null>
>();
vi.mock("../transparency/verify", () => ({
  verifyIdentityInLog: (a: Uint8Array, b: Uint8Array) =>
    verifyIdentityInLogMock(a, b),
  lookupServedKeyReset: (a: Uint8Array, b: Uint8Array) =>
    lookupServedKeyResetMock(a, b),
}));

/** Helper — wrap a bare status into the `{status, isReset}` shape the
 *  client now consumes. `isReset` defaults to `null` (no latest read). */
function result(
  status: LogVerifyStatus,
  isReset: boolean | null = null,
): LogVerifyResult {
  return { status, isReset };
}

// Import AFTER the mock is registered.
const { ContactsClient, SupersessionRefused, LogRollbackDetected } =
  await import("./client");
const { readPin, readLogStatus, readLogReset } = await import("./local-store");

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
} {
  const id = wasm.Identity.fromSeed(new Uint8Array(32).fill(seed));
  return {
    signaturePubkeyB64: b64(id.signaturePublicKey),
    x25519PubkeyB64: b64(id.x25519PublicKey),
  };
}

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
  verifyIdentityInLogMock.mockReset();
  lookupServedKeyResetMock.mockReset();
  // Default the gate-1 enrichment lookup to "unknown" unless a test
  // overrides it; gate 1 throws regardless of this value.
  lookupServedKeyResetMock.mockResolvedValue(null);
  // Silence the best-effort console.warn calls during the suite.
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
});

afterEach(() => {
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
        x25519_pubkey: peer.x25519PubkeyB64,
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
        x25519_pubkey: peer.x25519PubkeyB64,
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
        x25519_pubkey: peer.x25519PubkeyB64,
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
          x25519_pubkey: peer.x25519PubkeyB64,
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
        x25519_pubkey: peer.x25519PubkeyB64,
      }),
    ).resolves.toBeUndefined();
    // No status recorded (the check threw), but the pin is set and the
    // path completed — the additive check never breaks the primary.
    expect(readPin(b64(ACCOUNT_ID))?.pinnedSignaturePubkeyB64).toBe(
      peer.signaturePubkeyB64,
    );
  });

  it("does NOT weaken the supersession gate: a changed key with no proof still refuses BEFORE the log check", async () => {
    const original = identityFromSeed(21);
    const rotated = identityFromSeed(22);
    // First contact TOFU-pins the original key (log says off).
    verifyIdentityInLogMock.mockResolvedValue(result("off"));
    await gate({
      signature_pubkey: original.signaturePubkeyB64,
      x25519_pubkey: original.x25519PubkeyB64,
    });
    verifyIdentityInLogMock.mockClear();

    // A changed key with NO supersession proof must fail-closed via
    // the supersession gate — the log check must NOT even run.
    let thrown: unknown;
    try {
      await gate({
        signature_pubkey: rotated.signaturePubkeyB64,
        x25519_pubkey: rotated.x25519PubkeyB64,
      });
    } catch (e) {
      thrown = e;
    }
    expect(thrown).toBeInstanceOf(SupersessionRefused);
    expect((thrown as InstanceType<typeof SupersessionRefused>).reason).toBe(
      "proof-absent",
    );
    // The additive log check never ran — gate 1 short-circuited.
    expect(verifyIdentityInLogMock).not.toHaveBeenCalled();
    // The pin still holds the ORIGINAL key (the change was refused).
    expect(readPin(b64(ACCOUNT_ID))?.pinnedSignaturePubkeyB64).toBe(
      original.signaturePubkeyB64,
    );
  });
});

describe("KT-log S4 — SupersessionRefused.loggedReset enrichment", () => {
  /** TOFU-pin an original key, then attempt an UNPROVEN changed key and
   *  capture the thrown `SupersessionRefused`. `loggedReset` is driven
   *  by the mocked `lookupServedKeyReset` result for that test. */
  async function refuseChangedKey(
    loggedReset: boolean | null,
  ): Promise<InstanceType<typeof SupersessionRefused>> {
    const original = identityFromSeed(31);
    const rotated = identityFromSeed(32);
    verifyIdentityInLogMock.mockResolvedValue(result("off"));
    await gate({
      signature_pubkey: original.signaturePubkeyB64,
      x25519_pubkey: original.x25519PubkeyB64,
    });
    // The gate-1 enrichment lookup returns the requested marker.
    lookupServedKeyResetMock.mockResolvedValue(loggedReset);
    let thrown: unknown;
    try {
      await gate({
        signature_pubkey: rotated.signaturePubkeyB64,
        x25519_pubkey: rotated.x25519PubkeyB64,
      });
    } catch (e) {
      thrown = e;
    }
    expect(thrown).toBeInstanceOf(SupersessionRefused);
    return thrown as InstanceType<typeof SupersessionRefused>;
  }

  it("attaches loggedReset=true when the log shows an owner reset", async () => {
    const err = await refuseChangedKey(true);
    expect(err.reason).toBe("proof-absent");
    expect(err.loggedReset).toBe(true);
  });

  it("attaches loggedReset=false when the log shows NO owner reset", async () => {
    const err = await refuseChangedKey(false);
    expect(err.loggedReset).toBe(false);
  });

  it("attaches loggedReset=null when the log is off/unreachable/absent", async () => {
    const err = await refuseChangedKey(null);
    expect(err.loggedReset).toBeNull();
  });

  it("the gate still THROWS regardless of the log lookup (gate unweakened)", async () => {
    // Even with loggedReset=true (a confirmed reset), the refusal stands —
    // the log only colours the message, it never authorises the new key.
    const err = await refuseChangedKey(true);
    expect(err).toBeInstanceOf(SupersessionRefused);
  });

  it("a throwing enrichment lookup leaves loggedReset null (best-effort, refusal stands)", async () => {
    const original = identityFromSeed(41);
    const rotated = identityFromSeed(42);
    verifyIdentityInLogMock.mockResolvedValue(result("off"));
    await gate({
      signature_pubkey: original.signaturePubkeyB64,
      x25519_pubkey: original.x25519PubkeyB64,
    });
    // The enrichment lookup itself throws — must not break the refusal.
    lookupServedKeyResetMock.mockRejectedValue(new Error("log boom"));
    let thrown: unknown;
    try {
      await gate({
        signature_pubkey: rotated.signaturePubkeyB64,
        x25519_pubkey: rotated.x25519PubkeyB64,
      });
    } catch (e) {
      thrown = e;
    }
    expect(thrown).toBeInstanceOf(SupersessionRefused);
    expect(
      (thrown as InstanceType<typeof SupersessionRefused>).loggedReset,
    ).toBeNull();
  });
});
