// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.2 with rule 4 (ADR-0101 section 10) - the identity gate END TO
 * END for a proof-less key change: `ContactsClient.verifyBundleAccountKey`
 * gathers the attestation check, the verified log read and the seal, asks
 * `resetVerdict` (the core's table), and on `sealed` re-pins and tells the
 * reset listeners; on `unsealed` it re-pins nothing, tells nobody and
 * records that the person started over (ADR-0101 section 11).
 *
 * The seal announcements and reset seals are REAL (the core's own signers,
 * `reset-seal-fixtures.ts`).
 *
 * The wasm core cannot SIGN a key-reset attestation (signing is the auth
 * service's), so `verifyKeyResetAttestation` is stubbed here; its own
 * bindings are tested in the core. Everything else is the real wasm.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as realWasm from "../../wasm/lakko_ffi_wasm";
import type { LogVerifyResult } from "../transparency/verify";
import type { IdentityFetchResponse } from "../discovery/wire";

const verifyIdentityInLogMock =
  vi.fn<(a: Uint8Array, b: Uint8Array, deps?: unknown, seal?: unknown) => Promise<LogVerifyResult>>();
vi.mock("../transparency/verify", () => ({
  verifyIdentityInLog: (a: Uint8Array, b: Uint8Array, deps?: unknown, seal?: unknown) =>
    verifyIdentityInLogMock(a, b, deps, seal),
  verifyRotationChain: vi.fn(),
}));

const fetchDeviceAttestKeyMock = vi.fn<() => Promise<Uint8Array>>();
vi.mock("../auth/device-attest-key", () => ({
  fetchDeviceAttestKey: () => fetchDeviceAttestKeyMock(),
  pinnedDeviceAttestKey: () => null,
}));

/** What the stubbed verifier answers: the generation it "read", or a throw. */
let attested: { keyGeneration: bigint; signedAtMillis: bigint } | Error = new Error("unset");
const verifyCalls: Array<{ accountId: Uint8Array; pubkey: Uint8Array }> = [];
vi.mock("../wasm", () => ({
  loadWasm: async () =>
    new Proxy(realWasm, {
      get(target, prop, receiver) {
        if (prop === "verifyKeyResetAttestation") {
          return (_att: Uint8Array, _key: Uint8Array, accountId: Uint8Array, pubkey: Uint8Array) => {
            verifyCalls.push({ accountId, pubkey });
            if (attested instanceof Error) throw attested;
            return { ...attested, free: () => undefined };
          };
        }
        return Reflect.get(target, prop, receiver) as unknown;
      },
    }),
}));

const { ContactsClient, ContactStartedOver, SupersessionRefused, IdentityCheckInconclusive } =
  await import("./client");
const {
  bindContactsAccount,
  loadAccepted,
  loadKeyAliases,
  loadStartedOverKeys,
  loadSupersededKeys,
  readPin,
  sealSeqFloorOf,
  startedOverOf,
  upsertAccepted,
} = await import("./local-store");
import { passwordAnnouncement, passwordResetSeal } from "./reset-seal-fixtures";
const { subscribeResetAccepted } = await import("./reset-events");
const { readVerifiedBinding } = await import("./verified-binding");

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

const ACCOUNT_ID = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
const ACCOUNT_B64 = b64(ACCOUNT_ID);
const ATTEST_B64 = b64(new Uint8Array(145).fill(7));

function peer(seed: number): { seed: number; sig: string; pub: Uint8Array; x: string; xb: string; edSeed: Uint8Array } {
  const id = realWasm.Identity.fromSeed(new Uint8Array(32).fill(seed));
  return {
    seed,
    sig: b64(id.signaturePublicKey),
    pub: id.signaturePublicKey,
    x: b64(id.x25519PublicKey),
    xb: b64(realWasm.signX25519Binding(id)),
    edSeed: id.ed25519Seed,
  };
}

/** The password the account's seal is made from, unless a test says otherwise. */
const PASSWORD = 0x51;

/**
 * What a SEALED reset's bundle carries beside the attestation: the previous
 * generation's announcement, signed by the old account key, and a reset
 * seal over the new key by the password seal that announcement lists.
 */
function sealOf(
  original: { seed: number },
  fresh: { pub: Uint8Array },
  over: { fromGeneration?: bigint; seq?: bigint; sealPassword?: number } = {},
): { previous_reset_seals: string; reset_seal: string } {
  const fromGeneration = over.fromGeneration ?? 1n;
  const seq = over.seq ?? 3n;
  return {
    previous_reset_seals: b64(
      passwordAnnouncement(realWasm, {
        accountId: ACCOUNT_ID,
        signerSeed: original.seed,
        keyGeneration: fromGeneration,
        seq,
        exportKeyByte: PASSWORD,
      }),
    ),
    reset_seal: b64(
      passwordResetSeal(realWasm, {
        accountId: ACCOUNT_ID,
        announcementSeq: seq,
        fromGeneration,
        newSignaturePubkey: fresh.pub,
        exportKeyByte: over.sealPassword ?? PASSWORD,
      }),
    ),
  };
}

/** The account's CURRENT announcement, signed by `signer` at `generation`. */
function currentAnnouncement(signer: { seed: number }, generation: bigint, seq: bigint): string {
  return b64(
    passwordAnnouncement(realWasm, {
      accountId: ACCOUNT_ID,
      signerSeed: signer.seed,
      keyGeneration: generation,
      seq,
      exportKeyByte: PASSWORD,
    }),
  );
}

function gate(bundle: IdentityFetchResponse, heldKeyB64?: string): Promise<void> {
  const client = new ContactsClient();
  return (
    client as unknown as {
      verifyBundleAccountKey: (a: Uint8Array, b: IdentityFetchResponse, h?: string) => Promise<void>;
    }
  ).verifyBundleAccountKey(ACCOUNT_ID, bundle, heldKeyB64);
}

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

let accepted: unknown[] = [];
let unsubscribe: () => void = () => undefined;

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  bindContactsAccount("dmlld2VyLWFjY291bnQ=");
  verifyIdentityInLogMock.mockReset();
  verifyIdentityInLogMock.mockResolvedValue({ status: "off", isReset: null });
  fetchDeviceAttestKeyMock.mockReset();
  fetchDeviceAttestKeyMock.mockResolvedValue(new Uint8Array(32).fill(9));
  attested = { keyGeneration: 2n, signedAtMillis: 1_700_000_000_000n };
  verifyCalls.length = 0;
  accepted = [];
  unsubscribe = subscribeResetAccepted((e) => accepted.push(e));
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
});

afterEach(() => {
  unsubscribe();
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
  vi.restoreAllMocks();
});

async function pinOriginal(seed: number): Promise<ReturnType<typeof peer>> {
  const original = peer(seed);
  await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, x25519_binding: original.xb, key_generation: 1 });
  expect(readPin(ACCOUNT_B64)?.acceptedKeyGeneration).toBe(1);
  return original;
}

describe("the gate accepts a SEALED reset silently", () => {
  it("re-pins, records the generation, supersedes the old key and tells the listeners", async () => {
    const original = await pinOriginal(1);
    const fresh = peer(2);
    await expect(
      gate({
        signature_pubkey: fresh.sig,
        x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
        key_generation: 2,
        key_reset_attest: ATTEST_B64,
        ...sealOf(original, fresh),
      }),
    ).resolves.toBeUndefined();
    const pin = readPin(ACCOUNT_B64);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(fresh.sig);
    expect(pin?.acceptedKeyGeneration).toBe(2);
    // The announcement that sealed the reset is the account's floor now.
    expect(sealSeqFloorOf(ACCOUNT_B64)).toBe(3n);
    expect(startedOverOf(ACCOUNT_B64)).toBeUndefined();
    expect(pin?.provenPredecessorsB64).toBeUndefined();
    expect(loadSupersededKeys().get(original.sig)).toBe(ACCOUNT_B64);
    expect(accepted).toEqual([
      {
        accountIdB64: ACCOUNT_B64,
        previousKeyB64: original.sig,
        newKeyB64: fresh.sig,
        keyGeneration: 2,
        signedAtMs: 1_700_000_000_000,
      },
    ]);
    // The verifier was asked about THIS account id and the FETCHED key.
    expect(verifyCalls).toHaveLength(1);
    expect(b64(verifyCalls[0]?.accountId ?? new Uint8Array())).toBe(ACCOUNT_B64);
    expect(b64(verifyCalls[0]?.pubkey ?? new Uint8Array())).toBe(fresh.sig);
  });

  it("refuses an attested reset whose X25519 key is not bound to the new account key", async () => {
    const original = await pinOriginal(11);
    const fresh = peer(12);
    const intruder = peer(13);
    for (const binding of [undefined, intruder.xb]) {
      await expect(
        gate({
          signature_pubkey: fresh.sig,
          // A directory pairing the new key with an X25519 key of its own.
          x25519_pubkey: intruder.x,
          ...(binding === undefined ? {} : { x25519_binding: binding }),
          key_generation: 2,
          key_reset_attest: ATTEST_B64,
          ...sealOf(original, fresh),
        }),
      ).rejects.toBeInstanceOf(SupersessionRefused);
    }
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(original.sig);
    expect(accepted).toEqual([]);
  });

  it("the same bundle observed again is the unchanged-key case: no second event", async () => {
    const original = await pinOriginal(3);
    const fresh = peer(4);
    const bundle = {
      signature_pubkey: fresh.sig,
      x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
      key_generation: 2,
      key_reset_attest: ATTEST_B64,
      ...sealOf(original, fresh),
    };
    await gate(bundle);
    await gate(bundle);
    expect(accepted).toHaveLength(1);
  });

  it("an old attestation cannot roll the contact back", async () => {
    const original = await pinOriginal(5);
    const fresh = peer(6);
    await gate({
      signature_pubkey: fresh.sig,
      x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
      key_generation: 2,
      key_reset_attest: ATTEST_B64,
      ...sealOf(original, fresh),
    });
    // The serving tier replays generation 2 for another key (or the old one).
    await expect(
      gate({
        signature_pubkey: original.sig,
        x25519_pubkey: original.x, x25519_binding: original.xb,
        key_generation: 2,
        key_reset_attest: ATTEST_B64,
      }),
    ).rejects.toBeInstanceOf(SupersessionRefused);
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(fresh.sig);
  });
});

describe("a member this device never pinned (verified bindings)", () => {
  it("THE ATTACK: a member claiming another account's id is never judged a reset of it", async () => {
    // The member's roster key is `mallory`; its profile claims ACCOUNT_ID,
    // which belongs to someone else who once reset (generation 2, a valid
    // attestation for `victim`). This device never saw ACCOUNT_ID before.
    const mallory = peer(7);
    const victim = peer(8);
    await expect(
      gate(
        {
          signature_pubkey: victim.sig,
          x25519_pubkey: victim.x, x25519_binding: victim.xb,
          key_generation: 2,
          key_reset_attest: ATTEST_B64,
        },
        mallory.sig,
      ),
    ).rejects.toBeInstanceOf(IdentityCheckInconclusive);
    expect(readPin(ACCOUNT_B64)).toBeUndefined();
    expect(readVerifiedBinding(ACCOUNT_B64)).toBeUndefined();
    expect(loadSupersededKeys().size).toBe(0);
    expect(accepted).toEqual([]);
    // The attestation was not even consulted.
    expect(verifyCalls).toHaveLength(0);
  });

  it("the same claim after this device verified the account under its real key changes nothing", async () => {
    const victim = peer(10);
    const mallory = peer(11);
    const fresh = peer(12);
    // The real member is verified first.
    await gate({ signature_pubkey: victim.sig, x25519_pubkey: victim.x, x25519_binding: victim.xb, key_generation: 1 }, victim.sig);
    expect(readVerifiedBinding(ACCOUNT_B64)?.keyB64).toBe(victim.sig);
    // The account then resets; another member claims its id.
    await expect(
      gate(
        {
          signature_pubkey: fresh.sig,
          x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
          key_generation: 2,
          key_reset_attest: ATTEST_B64,
        },
        mallory.sig,
      ),
    ).rejects.toBeInstanceOf(IdentityCheckInconclusive);
    expect(loadSupersededKeys().size).toBe(0);
    expect(readVerifiedBinding(ACCOUNT_B64)?.keyB64).toBe(victim.sig);
    expect(accepted).toEqual([]);
  });

  it("the first observation records a binding and pins nothing", async () => {
    const original = peer(9);
    await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, x25519_binding: original.xb, key_generation: 4 }, original.sig);
    expect(readPin(ACCOUNT_B64)).toBeUndefined();
    expect(readVerifiedBinding(ACCOUNT_B64)).toMatchObject({ keyB64: original.sig, keyGeneration: 4 });
    expect(accepted).toEqual([]);
  });

  it("a reset is accepted from a verified binding that names the roster key", async () => {
    const original = peer(13);
    const fresh = peer(14);
    await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, x25519_binding: original.xb, key_generation: 1 }, original.sig);
    await gate(
      {
        signature_pubkey: fresh.sig,
        x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
        key_generation: 2,
        key_reset_attest: ATTEST_B64,
        ...sealOf(original, fresh),
      },
      original.sig,
    );
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(fresh.sig);
    expect(readPin(ACCOUNT_B64)?.acceptedKeyGeneration).toBe(2);
    expect(loadSupersededKeys().get(original.sig)).toBe(ACCOUNT_B64);
    // The pin is the binding now.
    expect(readVerifiedBinding(ACCOUNT_B64)).toBeUndefined();
    expect(accepted).toHaveLength(1);
  });

  it("a reset at the binding's own generation is refused (the baseline holds)", async () => {
    const original = peer(15);
    const fresh = peer(16);
    await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, x25519_binding: original.xb, key_generation: 2 }, original.sig);
    await expect(
      gate(
        {
          signature_pubkey: fresh.sig,
          x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
          key_generation: 2,
          key_reset_attest: ATTEST_B64,
        },
        original.sig,
      ),
    ).rejects.toBeInstanceOf(SupersessionRefused);
    expect(readPin(ACCOUNT_B64)).toBeUndefined();
    expect(readVerifiedBinding(ACCOUNT_B64)?.keyB64).toBe(original.sig);
  });

  it("a binding met on the contact path becomes the pin with its baseline", async () => {
    const original = peer(17);
    await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, x25519_binding: original.xb, key_generation: 3 }, original.sig);
    await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, x25519_binding: original.xb, key_generation: 3 });
    expect(readPin(ACCOUNT_B64)).toMatchObject({ pinnedSignaturePubkeyB64: original.sig, acceptedKeyGeneration: 3 });
    expect(readVerifiedBinding(ACCOUNT_B64)).toBeUndefined();
  });

  it("on the contact path a key that changed since the binding is a change, not a first pin", async () => {
    const original = peer(18);
    const other = peer(19);
    await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, x25519_binding: original.xb, key_generation: 1 }, original.sig);
    await expect(
      gate({ signature_pubkey: other.sig, x25519_pubkey: other.x, x25519_binding: other.xb, key_generation: 1 }),
    ).rejects.toBeInstanceOf(SupersessionRefused);
    expect(readPin(ACCOUNT_B64)).toBeUndefined();
  });
});

describe("the gate still fails closed", () => {
  it("an attestation that does not verify is refused and nothing changes", async () => {
    const original = await pinOriginal(11);
    const fresh = peer(12);
    attested = new Error("bad signature");
    const thrown = await gate({
      signature_pubkey: fresh.sig,
      x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
      key_generation: 2,
      key_reset_attest: ATTEST_B64,
    }).catch((e: unknown) => e);
    expect(thrown).toBeInstanceOf(SupersessionRefused);
    expect((thrown as InstanceType<typeof SupersessionRefused>).loggedReset).toBeNull();
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(original.sig);
    expect(loadSupersededKeys().size).toBe(0);
    expect(accepted).toEqual([]);
  });

  it("the attested generation must be the bundle's", async () => {
    await pinOriginal(13);
    const fresh = peer(14);
    await expect(
      gate({
        signature_pubkey: fresh.sig,
        x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
        key_generation: 3,
        key_reset_attest: ATTEST_B64,
      }),
    ).rejects.toBeInstanceOf(SupersessionRefused);
  });

  it("a log that shows the key without a reset refuses as unexplained", async () => {
    await pinOriginal(15);
    const fresh = peer(16);
    verifyIdentityInLogMock.mockResolvedValue({ status: "confirmed", isReset: false });
    const thrown = await gate({
      signature_pubkey: fresh.sig,
      x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
      key_generation: 2,
      key_reset_attest: ATTEST_B64,
    }).catch((e: unknown) => e);
    expect(thrown).toBeInstanceOf(SupersessionRefused);
    expect((thrown as InstanceType<typeof SupersessionRefused>).loggedReset).toBe(false);
  });

  it("a change that came with a proof which fails is never treated as a reset", async () => {
    const original = await pinOriginal(17);
    const fresh = peer(18);
    const thrown = await gate({
      signature_pubkey: fresh.sig,
      x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
      key_generation: 2,
      key_reset_attest: ATTEST_B64,
      supersession_proof: b64(new Uint8Array(145).fill(1)),
    }).catch((e: unknown) => e);
    expect(thrown).toBeInstanceOf(SupersessionRefused);
    expect((thrown as InstanceType<typeof SupersessionRefused>).reason).toBe("proof-invalid");
    expect(verifyCalls).toHaveLength(0);
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(original.sig);
  });
});

describe("no verdict changes nothing", () => {
  it("no attest key: inconclusive, the pin stays", async () => {
    const original = await pinOriginal(21);
    const fresh = peer(22);
    fetchDeviceAttestKeyMock.mockRejectedValue(new Error("jwks down"));
    await expect(
      gate({
        signature_pubkey: fresh.sig,
        x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
        key_generation: 2,
        key_reset_attest: ATTEST_B64,
      }),
    ).rejects.toBeInstanceOf(IdentityCheckInconclusive);
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(original.sig);
    expect(accepted).toEqual([]);
  });

  it("an unreachable log: inconclusive, the pin stays", async () => {
    const original = await pinOriginal(23);
    const fresh = peer(24);
    verifyIdentityInLogMock.mockResolvedValue({ status: "unreachable", isReset: null });
    await expect(
      gate({
        signature_pubkey: fresh.sig,
        x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
        key_generation: 2,
        key_reset_attest: ATTEST_B64,
      }),
    ).rejects.toBeInstanceOf(IdentityCheckInconclusive);
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(original.sig);
  });
});

describe("a proven rotation never enters the reset path", () => {
  it("re-pins by the proof, asks no attestation, supersedes nothing", async () => {
    const original = await pinOriginal(31);
    const fresh = peer(32);
    const now = BigInt(Math.floor(Date.now() / 1000));
    const proof = realWasm.signSupersessionProof(original.edSeed, fresh.edSeed, now, now + 86_400n);
    verifyIdentityInLogMock.mockResolvedValue({ status: "off", isReset: null });
    const { verifyRotationChain } = await import("../transparency/verify");
    (verifyRotationChain as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      status: "off",
      isReset: null,
      version: null,
    });
    await gate({
      signature_pubkey: fresh.sig,
      x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
      key_generation: 2,
      key_reset_attest: ATTEST_B64,
      supersession_proof: b64(proof),
    });
    const pin = readPin(ACCOUNT_B64);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(fresh.sig);
    expect(pin?.provenPredecessorsB64).toEqual([original.sig]);
    // The attestation beside the proof is read for ONE thing: the
    // generation the rotation moved the account to (ADR-0101 section 10),
    // so the next reset's seal is checked against that generation.
    expect(verifyCalls).toHaveLength(1);
    expect(pin?.acceptedKeyGeneration).toBe(2);
    expect(loadSupersededKeys().size).toBe(0);
    expect(accepted).toEqual([]);
  });

  it("without a verified attestation the accepted generation stays", async () => {
    const original = await pinOriginal(33);
    const fresh = peer(34);
    const now = BigInt(Math.floor(Date.now() / 1000));
    const proof = realWasm.signSupersessionProof(original.edSeed, fresh.edSeed, now, now + 86_400n);
    const { verifyRotationChain } = await import("../transparency/verify");
    (verifyRotationChain as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      status: "off",
      isReset: null,
      version: null,
    });
    for (const attestation of [undefined, ATTEST_B64]) {
      // The second pass carries an attestation the verifier refuses.
      attested = new Error("bad signature");
      await gate({
        signature_pubkey: fresh.sig,
        x25519_pubkey: fresh.x, x25519_binding: fresh.xb,
        key_generation: 2,
        ...(attestation === undefined ? {} : { key_reset_attest: attestation }),
        supersession_proof: b64(proof),
      });
      expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(fresh.sig);
      expect(readPin(ACCOUNT_B64)?.acceptedKeyGeneration).toBe(1);
    }
  });

  it("a reset AFTER a proven rotation is sealed by the rotated key's announcement", async () => {
    const original = await pinOriginal(35);
    const rotated = peer(36);
    const reset = peer(37);
    const now = BigInt(Math.floor(Date.now() / 1000));
    const proof = realWasm.signSupersessionProof(original.edSeed, rotated.edSeed, now, now + 86_400n);
    const { verifyRotationChain } = await import("../transparency/verify");
    (verifyRotationChain as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      status: "off",
      isReset: null,
      version: null,
    });
    await gate({
      signature_pubkey: rotated.sig,
      x25519_pubkey: rotated.x, x25519_binding: rotated.xb,
      key_generation: 2,
      key_reset_attest: ATTEST_B64,
      supersession_proof: b64(proof),
    });
    // The reset leaves generation 2, the one the rotation moved to.
    attested = { keyGeneration: 3n, signedAtMillis: 1_700_000_100_000n };
    await gate({
      signature_pubkey: reset.sig,
      x25519_pubkey: reset.x, x25519_binding: reset.xb,
      key_generation: 3,
      key_reset_attest: ATTEST_B64,
      ...sealOf(rotated, reset, { fromGeneration: 2n, seq: 9n }),
    });
    const pin = readPin(ACCOUNT_B64);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(reset.sig);
    expect(pin?.acceptedKeyGeneration).toBe(3);
    expect(loadSupersededKeys().get(rotated.sig)).toBe(ACCOUNT_B64);
    expect(loadSupersededKeys().get(original.sig)).toBe(ACCOUNT_B64);
    expect(accepted).toHaveLength(1);
  });
});

// ----------------------------------------------------------------------------
// ADR-0101 section 10 and 11: rule 4 at the gate
// ----------------------------------------------------------------------------

function resetBundle(
  fresh: ReturnType<typeof peer>,
  extra: Partial<IdentityFetchResponse> = {},
): IdentityFetchResponse {
  return {
    signature_pubkey: fresh.sig,
    x25519_pubkey: fresh.x,
    x25519_binding: fresh.xb,
    key_generation: 2,
    key_reset_attest: ATTEST_B64,
    ...extra,
  };
}

/** Nothing of an accepted reset happened: no re-pin, no superseded key, no
 *  reset time (what every return of ADR-0101 sections 4 to 9 reads), no event. */
function expectNothingMoved(original: ReturnType<typeof peer>): void {
  const pin = readPin(ACCOUNT_B64);
  expect(pin?.pinnedSignaturePubkeyB64).toBe(original.sig);
  expect(pin?.acceptedKeyGeneration).toBe(1);
  expect(pin?.supersededKeysB64).toBeUndefined();
  expect(pin?.resetSignedAtMs).toBeUndefined();
  expect(loadSupersededKeys().size).toBe(0);
  expect(accepted).toEqual([]);
}

describe("an UNSEALED reset is a new account under the same username", () => {
  it("an attested reset with no seal: nothing is re-pinned or told, the fact is recorded", async () => {
    const original = await pinOriginal(41);
    const fresh = peer(42);
    const thrown = await gate(resetBundle(fresh)).catch((e: unknown) => e);
    expect(thrown).toBeInstanceOf(ContactStartedOver);
    // Neither the fail-closed refusal nor "no verdict yet".
    expect(thrown).not.toBeInstanceOf(SupersessionRefused);
    expect(thrown).not.toBeInstanceOf(IdentityCheckInconclusive);
    expectNothingMoved(original);
    expect(startedOverOf(ACCOUNT_B64)).toMatchObject({
      newKeyB64: fresh.sig,
      keyGeneration: 2,
      signedAtMs: 1_700_000_000_000,
    });
    expect(loadStartedOverKeys().get(original.sig)).toBe(ACCOUNT_B64);
  });

  it("asked again, the answer and the record are the same", async () => {
    const original = await pinOriginal(43);
    const fresh = peer(44);
    await expect(gate(resetBundle(fresh))).rejects.toBeInstanceOf(ContactStartedOver);
    const first = startedOverOf(ACCOUNT_B64);
    await expect(gate(resetBundle(fresh))).rejects.toBeInstanceOf(ContactStartedOver);
    expect(startedOverOf(ACCOUNT_B64)).toEqual(first);
    expectNothingMoved(original);
  });

  it("THE OPERATOR'S RESET: an announcement and a seal under keys of its own move nothing", async () => {
    const original = await pinOriginal(45);
    const fresh = peer(46);
    const operator = peer(47);
    // Consistent with each other, signed by a key the device never held.
    await expect(gate(resetBundle(fresh, sealOf(operator, fresh)))).rejects.toBeInstanceOf(ContactStartedOver);
    expectNothingMoved(original);
  });

  it("a seal by a password the account never announced moves nothing", async () => {
    const original = await pinOriginal(48);
    const fresh = peer(49);
    await expect(
      gate(resetBundle(fresh, sealOf(original, fresh, { sealPassword: 0x77 }))),
    ).rejects.toBeInstanceOf(ContactStartedOver);
    expectNothingMoved(original);
  });

  it("with a key log: a seal whose announcement the log does not show is unsealed", async () => {
    const original = await pinOriginal(50);
    const fresh = peer(51);
    verifyIdentityInLogMock.mockResolvedValue({ status: "confirmed", isReset: true, sealLog: "not-proven" });
    await expect(gate(resetBundle(fresh, sealOf(original, fresh)))).rejects.toBeInstanceOf(ContactStartedOver);
    expectNothingMoved(original);
    // The log was asked about exactly the announcement the directory served.
    const asked = verifyIdentityInLogMock.mock.calls.at(-1)?.[3] as { previousAnnouncement: Uint8Array };
    expect(b64(asked.previousAnnouncement)).toBe(sealOf(original, fresh).previous_reset_seals);
  });

  it("with a key log that cannot be read for the announcement: no verdict, nothing recorded", async () => {
    const original = await pinOriginal(52);
    const fresh = peer(53);
    verifyIdentityInLogMock.mockResolvedValue({ status: "confirmed", isReset: true, sealLog: "unreachable" });
    await expect(gate(resetBundle(fresh, sealOf(original, fresh)))).rejects.toBeInstanceOf(
      IdentityCheckInconclusive,
    );
    expectNothingMoved(original);
    expect(startedOverOf(ACCOUNT_B64)).toBeUndefined();
  });

  it("with a key log that proves the announcement: sealed", async () => {
    const original = await pinOriginal(54);
    const fresh = peer(55);
    verifyIdentityInLogMock.mockResolvedValue({ status: "confirmed", isReset: true, sealLog: "proven" });
    await gate(resetBundle(fresh, sealOf(original, fresh)));
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(fresh.sig);
    expect(accepted).toHaveLength(1);
  });

  it("a reset that later PROVES sealed is the same person after all: re-pinned, the fact cleared", async () => {
    const original = await pinOriginal(56);
    const fresh = peer(57);
    // The directory first serves the reset stripped of its seal.
    await expect(gate(resetBundle(fresh))).rejects.toBeInstanceOf(ContactStartedOver);
    expect(loadStartedOverKeys().size).toBe(1);
    await gate(resetBundle(fresh, sealOf(original, fresh)));
    const pin = readPin(ACCOUNT_B64);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(fresh.sig);
    expect(pin?.startedOver).toBeUndefined();
    expect(loadStartedOverKeys().size).toBe(0);
    expect(loadSupersededKeys().get(original.sig)).toBe(ACCOUNT_B64);
    expect(accepted).toHaveLength(1);
  });

  it("a person this device knows only by a verified binding: the binding becomes the pin, unmoved", async () => {
    const original = peer(58);
    const fresh = peer(59);
    await gate(
      { signature_pubkey: original.sig, x25519_pubkey: original.x, x25519_binding: original.xb, key_generation: 1 },
      original.sig,
    );
    expect(readPin(ACCOUNT_B64)).toBeUndefined();
    await expect(gate(resetBundle(fresh), original.sig)).rejects.toBeInstanceOf(ContactStartedOver);
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(original.sig);
    expect(startedOverOf(ACCOUNT_B64)?.newKeyB64).toBe(fresh.sig);
    expect(loadSupersededKeys().size).toBe(0);
    expect(accepted).toEqual([]);
  });

  it("the new account rotates by proof before the person decided: still started over, the record follows", async () => {
    const original = await pinOriginal(60);
    const fresh = peer(61);
    const rotated = peer(62);
    await expect(gate(resetBundle(fresh))).rejects.toBeInstanceOf(ContactStartedOver);
    const now = BigInt(Math.floor(Date.now() / 1000));
    const proof = realWasm.signSupersessionProof(fresh.edSeed, rotated.edSeed, now, now + 86_400n);
    await expect(
      gate(resetBundle(rotated, { key_generation: 3, supersession_proof: b64(proof) })),
    ).rejects.toBeInstanceOf(ContactStartedOver);
    expect(startedOverOf(ACCOUNT_B64)?.newKeyB64).toBe(rotated.sig);
    expectNothingMoved(original);
  });

  it("a proof the started-over key did not sign is the ordinary fail-closed refusal", async () => {
    const original = await pinOriginal(63);
    const fresh = peer(64);
    const other = peer(65);
    const third = peer(66);
    await expect(gate(resetBundle(fresh))).rejects.toBeInstanceOf(ContactStartedOver);
    const now = BigInt(Math.floor(Date.now() / 1000));
    const proof = realWasm.signSupersessionProof(other.edSeed, third.edSeed, now, now + 86_400n);
    const thrown = await gate(resetBundle(third, { key_generation: 3, supersession_proof: b64(proof) })).catch(
      (e: unknown) => e,
    );
    expect(thrown).toBeInstanceOf(SupersessionRefused);
    expect(startedOverOf(ACCOUNT_B64)?.newKeyB64).toBe(fresh.sig);
    expectNothingMoved(original);
  });
});

describe("the person accepts the new account (UX 16.11)", () => {
  it("only a request from exactly the started-over key moves the pin, and nothing else moves", async () => {
    const original = await pinOriginal(70);
    const fresh = peer(71);
    const intruder = peer(72);
    upsertAccepted({
      signaturePubkeyB64: original.sig,
      displayName: "Anna",
      acceptedAtMs: 1,
      accountIdHex: Array.from(ACCOUNT_ID, (b) => b.toString(16).padStart(2, "0")).join(""),
      groupIdB64: "b2xkLWNoYXQ=",
    });
    const client = new ContactsClient();
    // Before the device noticed anything, no key can be adopted.
    expect(client.adoptStartedOverRequest(ACCOUNT_ID, fresh.pub)).toBe(false);
    await expect(gate(resetBundle(fresh))).rejects.toBeInstanceOf(ContactStartedOver);
    // Another key under the same account id is not the new account.
    expect(client.adoptStartedOverRequest(ACCOUNT_ID, intruder.pub)).toBe(false);
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(original.sig);

    expect(client.adoptStartedOverRequest(ACCOUNT_ID, fresh.pub)).toBe(true);
    const pin = readPin(ACCOUNT_B64);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(fresh.sig);
    expect(pin?.acceptedKeyGeneration).toBe(2);
    expect(pin?.startedOver).toBeUndefined();
    // Nothing an ACCEPTED reset writes: no superseded key, no reset time.
    expect(pin?.supersededKeysB64).toBeUndefined();
    expect(pin?.resetSignedAtMs).toBeUndefined();
    expect(loadSupersededKeys().size).toBe(0);
    expect(accepted).toEqual([]);
    // The old conversation stays closed, and keeps the person's name.
    expect(loadStartedOverKeys().get(original.sig)).toBe(ACCOUNT_B64);
    expect(loadKeyAliases().get(original.sig)).toBe(fresh.sig);
    // One row, under the new key, with its rename; the old chat link and
    // the "an older one-to-one is still theirs" alias are gone.
    const rows = loadAccepted();
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ signaturePubkeyB64: fresh.sig, displayName: "Anna" });
    expect(rows[0]?.groupIdB64).toBeUndefined();
    expect(rows[0]?.previousSignaturePubkeysB64 ?? []).not.toContain(original.sig);
    // From here the published key is the pin: the gate passes.
    await expect(gate(resetBundle(fresh))).resolves.toBeUndefined();
    expect(accepted).toEqual([]);
  });
});

describe("adding the new account by @name or from a search result (UX 16.11, alignment item D)", () => {
  it("the keys of the new account are adopted as a new contact; another key under the account id is not", async () => {
    await pinOriginal(73);
    const fresh = peer(74);
    const intruder = peer(75);
    const client = new ContactsClient();
    (client as unknown as { discovery: { identityFetch: () => Promise<IdentityFetchResponse> } }).discovery = {
      identityFetch: async () => resetBundle(fresh),
    };
    // A search result naming some other key under the account id: refused.
    await expect(client.refuseContactRequestToUnverifiedKeys(ACCOUNT_ID, intruder.pub)).rejects.toBeInstanceOf(
      ContactStartedOver,
    );
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).not.toBe(intruder.sig);
    // The new account's own keys: adopted, the old keys left behind (the old
    // one-to-one stays closed).
    const original = readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64;
    await expect(client.refuseContactRequestToUnverifiedKeys(ACCOUNT_ID, fresh.pub)).resolves.toBeUndefined();
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(fresh.sig);
    expect(original !== undefined && loadStartedOverKeys().has(original)).toBe(true);
    expect(accepted).toEqual([]);
  });

  it("a request by @name goes to the new account, which becomes the contact's key", async () => {
    await pinOriginal(76);
    const fresh = peer(77);
    const client = new ContactsClient();
    const sealedTo: string[] = [];
    Object.assign(client as unknown as Record<string, unknown>, {
      discovery: {
        lookup: async () => ({ account: ACCOUNT_ID, normalisedUsername: "anna", mailboxBinding: null }),
        identityFetch: async () => resetBundle(fresh),
      },
      composeAndPublishContactRequest: async (_i: unknown, signaturePub: Uint8Array) => {
        sealedTo.push(b64(signaturePub));
        return { messageId: 1, createdAt: "" };
      },
      reportUsernameResolved: () => undefined,
    });
    await client.sendContactRequest(null, "anna", new Uint8Array(0));
    expect(sealedTo).toEqual([fresh.sig]);
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(fresh.sig);
  });
});

describe("the seq floor (ADR-0101 section 10, the peer's check 1)", () => {
  it("is raised by the account's own current announcement, and never lowered", async () => {
    const original = await pinOriginal(80);
    const bundle = (seq: bigint): IdentityFetchResponse => ({
      signature_pubkey: original.sig,
      x25519_pubkey: original.x,
      x25519_binding: original.xb,
      key_generation: 1,
      reset_seals: currentAnnouncement(original, 1n, seq),
    });
    await gate(bundle(5n));
    expect(sealSeqFloorOf(ACCOUNT_B64)).toBe(5n);
    await gate(bundle(9n));
    expect(sealSeqFloorOf(ACCOUNT_B64)).toBe(9n);
    await gate(bundle(4n));
    expect(sealSeqFloorOf(ACCOUNT_B64)).toBe(9n);
  });

  it("ignores an announcement another key signed, or one that names another account", async () => {
    const original = await pinOriginal(81);
    const other = peer(82);
    const base = {
      signature_pubkey: original.sig,
      x25519_pubkey: original.x,
      x25519_binding: original.xb,
      key_generation: 1,
    };
    await gate({ ...base, reset_seals: currentAnnouncement(other, 1n, 50n) });
    await gate({
      ...base,
      reset_seals: b64(
        passwordAnnouncement(realWasm, {
          accountId: new Uint8Array(16).fill(0x77),
          signerSeed: original.seed,
          keyGeneration: 1n,
          seq: 60n,
          exportKeyByte: PASSWORD,
        }),
      ),
    });
    await gate({ ...base, reset_seals: "bm90LWFuLWFubm91bmNlbWVudA==" });
    expect(sealSeqFloorOf(ACCOUNT_B64)).toBe(0n);
  });

  it("a reset sealed against an OLDER announcement than the device has seen is unsealed", async () => {
    const original = await pinOriginal(83);
    const fresh = peer(84);
    // The device saw seq 9 (say, after a password change removed an old seal).
    await gate({
      signature_pubkey: original.sig,
      x25519_pubkey: original.x,
      x25519_binding: original.xb,
      key_generation: 1,
      reset_seals: currentAnnouncement(original, 1n, 9n),
    });
    // The reset names the older announcement, seq 3, and a seal it listed.
    await expect(gate(resetBundle(fresh, sealOf(original, fresh, { seq: 3n })))).rejects.toBeInstanceOf(
      ContactStartedOver,
    );
    expectNothingMoved(original);
    // At the floor it is sealed.
    await gate(resetBundle(fresh, sealOf(original, fresh, { seq: 9n })));
    expect(readPin(ACCOUNT_B64)?.pinnedSignaturePubkeyB64).toBe(fresh.sig);
  });
});
