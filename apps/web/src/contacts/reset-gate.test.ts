// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 R.2 - the identity gate END TO END for a proof-less key change:
 * `ContactsClient.verifyBundleAccountKey` gathers the attestation check and
 * the verified log read, asks `resetVerdict`, and on `accepted` re-pins and
 * tells the reset listeners; nothing else.
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

const verifyIdentityInLogMock = vi.fn<(a: Uint8Array, b: Uint8Array) => Promise<LogVerifyResult>>();
vi.mock("../transparency/verify", () => ({
  verifyIdentityInLog: (a: Uint8Array, b: Uint8Array) => verifyIdentityInLogMock(a, b),
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

const { ContactsClient, SupersessionRefused, IdentityCheckInconclusive } = await import("./client");
const { bindContactsAccount, loadSupersededKeys, readPin } = await import("./local-store");
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
const ATTEST_B64 = b64(new Uint8Array(177).fill(7));

function peer(seed: number): { sig: string; x: string; edSeed: Uint8Array } {
  const id = realWasm.Identity.fromSeed(new Uint8Array(32).fill(seed));
  return { sig: b64(id.signaturePublicKey), x: b64(id.x25519PublicKey), edSeed: id.ed25519Seed };
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
  await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, key_generation: 1 });
  expect(readPin(ACCOUNT_B64)?.acceptedKeyGeneration).toBe(1);
  return original;
}

describe("the gate accepts an attested reset silently", () => {
  it("re-pins, records the generation, supersedes the old key and tells the listeners", async () => {
    const original = await pinOriginal(1);
    const fresh = peer(2);
    await expect(
      gate({
        signature_pubkey: fresh.sig,
        x25519_pubkey: fresh.x,
        key_generation: 2,
        key_reset_attest: ATTEST_B64,
      }),
    ).resolves.toBeUndefined();
    const pin = readPin(ACCOUNT_B64);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(fresh.sig);
    expect(pin?.acceptedKeyGeneration).toBe(2);
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

  it("the same bundle observed again is the unchanged-key case: no second event", async () => {
    await pinOriginal(3);
    const fresh = peer(4);
    const bundle = {
      signature_pubkey: fresh.sig,
      x25519_pubkey: fresh.x,
      key_generation: 2,
      key_reset_attest: ATTEST_B64,
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
      x25519_pubkey: fresh.x,
      key_generation: 2,
      key_reset_attest: ATTEST_B64,
    });
    // The serving tier replays generation 2 for another key (or the old one).
    await expect(
      gate({
        signature_pubkey: original.sig,
        x25519_pubkey: original.x,
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
          x25519_pubkey: victim.x,
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
    await gate({ signature_pubkey: victim.sig, x25519_pubkey: victim.x, key_generation: 1 }, victim.sig);
    expect(readVerifiedBinding(ACCOUNT_B64)?.keyB64).toBe(victim.sig);
    // The account then resets; another member claims its id.
    await expect(
      gate(
        {
          signature_pubkey: fresh.sig,
          x25519_pubkey: fresh.x,
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
    await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, key_generation: 4 }, original.sig);
    expect(readPin(ACCOUNT_B64)).toBeUndefined();
    expect(readVerifiedBinding(ACCOUNT_B64)).toMatchObject({ keyB64: original.sig, keyGeneration: 4 });
    expect(accepted).toEqual([]);
  });

  it("a reset is accepted from a verified binding that names the roster key", async () => {
    const original = peer(13);
    const fresh = peer(14);
    await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, key_generation: 1 }, original.sig);
    await gate(
      {
        signature_pubkey: fresh.sig,
        x25519_pubkey: fresh.x,
        key_generation: 2,
        key_reset_attest: ATTEST_B64,
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
    await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, key_generation: 2 }, original.sig);
    await expect(
      gate(
        {
          signature_pubkey: fresh.sig,
          x25519_pubkey: fresh.x,
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
    await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, key_generation: 3 }, original.sig);
    await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, key_generation: 3 });
    expect(readPin(ACCOUNT_B64)).toMatchObject({ pinnedSignaturePubkeyB64: original.sig, acceptedKeyGeneration: 3 });
    expect(readVerifiedBinding(ACCOUNT_B64)).toBeUndefined();
  });

  it("on the contact path a key that changed since the binding is a change, not a first pin", async () => {
    const original = peer(18);
    const other = peer(19);
    await gate({ signature_pubkey: original.sig, x25519_pubkey: original.x, key_generation: 1 }, original.sig);
    await expect(
      gate({ signature_pubkey: other.sig, x25519_pubkey: other.x, key_generation: 1 }),
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
      x25519_pubkey: fresh.x,
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
        x25519_pubkey: fresh.x,
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
      x25519_pubkey: fresh.x,
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
      x25519_pubkey: fresh.x,
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
        x25519_pubkey: fresh.x,
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
        x25519_pubkey: fresh.x,
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
      x25519_pubkey: fresh.x,
      key_generation: 2,
      key_reset_attest: ATTEST_B64,
      supersession_proof: b64(proof),
    });
    const pin = readPin(ACCOUNT_B64);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(fresh.sig);
    expect(pin?.provenPredecessorsB64).toEqual([original.sig]);
    expect(verifyCalls).toHaveLength(0);
    expect(loadSupersededKeys().size).toBe(0);
    expect(accepted).toEqual([]);
  });
});
