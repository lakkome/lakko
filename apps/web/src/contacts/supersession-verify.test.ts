// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * G-4 (ADR-0016) — verify-on-fetch decision-table tests for
 * `ContactsClient.verifyBundleAccountKey`, the fail-closed gate
 * that re-pins a peer's AccountIdentity key only when a valid
 * supersession proof authorises the change.
 *
 * Drives the REAL wasm core (`signSupersessionProof` /
 * `verifySupersession`) so the proofs are genuine, and exercises
 * the private gate method directly (a runtime cast — TS `private`
 * is compile-time only) against an in-memory `localStorage` shim
 * backing the pin store.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { ContactsClient, SupersessionRefused } from "./client";
import { bindContactsAccount, loadFlaggedContacts, readKeyState, readPin } from "./local-store";
import type { IdentityFetchResponse } from "../discovery/wire";

/** In-memory `localStorage` shim (vitest `node` environment has
 *  none). The pin store reads `localStorage` lazily per call. */
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

/** Standard-alphabet base64 of raw bytes (matches the wire). */
function b64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i] ?? 0);
  return btoa(s);
}

const WINDOW_SECS = 14n * 24n * 60n * 60n; // emit window used by the client

/** Build a peer's identity from a fixed seed → its bundle pubkeys. */
function identityFromSeed(seed: number): {
  ed25519Seed: Uint8Array;
  signaturePubkeyB64: string;
  x25519PubkeyB64: string;
} {
  const id = wasm.Identity.fromSeed(new Uint8Array(32).fill(seed));
  return {
    ed25519Seed: id.ed25519Seed,
    signaturePubkeyB64: b64(id.signaturePublicKey),
    x25519PubkeyB64: b64(id.x25519PublicKey),
  };
}

/** A 16-byte fixed account_id for the peer under test. */
const ACCOUNT_ID = new Uint8Array([
  1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
]);

/** Invoke the private gate method on a fresh client. Returns the
 *  promise so callers can assert resolve / reject. */
function gate(bundle: IdentityFetchResponse): Promise<void> {
  const client = new ContactsClient();
  // Runtime cast to reach the private method — `private` is a
  // compile-time fence only, the method exists on the instance.
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
});

afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

describe("verifyBundleAccountKey (G-4, fail-closed)", () => {
  it("first-ever observation TOFU-pins the key", async () => {
    const peer = identityFromSeed(11);
    const accountIdB64 = b64(ACCOUNT_ID);
    expect(readPin(accountIdB64)).toBeUndefined();
    await expect(
      gate({
        signature_pubkey: peer.signaturePubkeyB64,
        x25519_pubkey: peer.x25519PubkeyB64, key_generation: 1
      }),
    ).resolves.toBeUndefined();
    const pin = readPin(accountIdB64);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(peer.signaturePubkeyB64);
    expect(pin?.rotationMarker).toBe(0);
  });

  it("an unchanged (equal) pin passes without a proof", async () => {
    const peer = identityFromSeed(22);
    const bundle: IdentityFetchResponse = {
      signature_pubkey: peer.signaturePubkeyB64,
      x25519_pubkey: peer.x25519PubkeyB64, key_generation: 1
    };
    // First call TOFU-pins.
    await gate(bundle);
    // Second call with the SAME key passes (no proof needed).
    await expect(gate(bundle)).resolves.toBeUndefined();
  });

  it("a changed key WITH a valid proof re-pins and advances the marker", async () => {
    const oldPeer = identityFromSeed(33);
    const newPeer = identityFromSeed(34);
    const accountIdB64 = b64(ACCOUNT_ID);

    // TOFU-pin the OLD key.
    await gate({
      signature_pubkey: oldPeer.signaturePubkeyB64,
      x25519_pubkey: oldPeer.x25519PubkeyB64, key_generation: 1
    });

    // Mint a real proof old -> new.
    const now = BigInt(Math.floor(Date.now() / 1000));
    const expires = now + WINDOW_SECS;
    const proof = wasm.signSupersessionProof(
      oldPeer.ed25519Seed,
      newPeer.ed25519Seed,
      now,
      expires,
    );

    // Fetch the NEW key WITH the proof → re-pin.
    await expect(
      gate({
        signature_pubkey: newPeer.signaturePubkeyB64,
        x25519_pubkey: newPeer.x25519PubkeyB64, key_generation: 1,
        supersession_proof: b64(proof),
      }),
    ).resolves.toBeUndefined();

    const pin = readPin(accountIdB64);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(newPeer.signaturePubkeyB64);
    // The marker advanced to the proof's expires_at.
    expect(pin?.rotationMarker).toBe(Number(expires));
  });

  it("ADR-0066: a PROVEN rotation at a higher generation is followed silently, and the old key is the proven predecessor", async () => {
    const oldPeer = identityFromSeed(41);
    const newPeer = identityFromSeed(42);
    const accountIdB64 = b64(ACCOUNT_ID);
    await gate({
      signature_pubkey: oldPeer.signaturePubkeyB64,
      x25519_pubkey: oldPeer.x25519PubkeyB64, key_generation: 1
    });
    const now = BigInt(Math.floor(Date.now() / 1000));
    const proof = wasm.signSupersessionProof(oldPeer.ed25519Seed, newPeer.ed25519Seed, now, now + WINDOW_SECS);
    // What the proven-rotation arm serves: the new key, the proof, and the
    // generation the attestation moved the account to. No throw, no prompt.
    await expect(
      gate({
        signature_pubkey: newPeer.signaturePubkeyB64,
        x25519_pubkey: newPeer.x25519PubkeyB64, key_generation: 2,
        supersession_proof: b64(proof),
      }),
    ).resolves.toBeUndefined();
    const pin = readPin(accountIdB64);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(newPeer.signaturePubkeyB64);
    expect(pin?.provenPredecessorsB64).toEqual([oldPeer.signaturePubkeyB64]);
    // Nothing of the ADR-0065 phase-2 "keys reset" state: a proven rotation
    // is not a reset.
    expect(pin?.keyReset).toBeUndefined();
    expect(readKeyState(accountIdB64)).toBe("normal");
    expect(loadFlaggedContacts()).toEqual([]);
  });

  it("ADR-0066: a changed key WITHOUT a proof is refused and leaves NO predecessor", async () => {
    const oldPeer = identityFromSeed(43);
    const newPeer = identityFromSeed(44);
    const accountIdB64 = b64(ACCOUNT_ID);
    await gate({
      signature_pubkey: oldPeer.signaturePubkeyB64,
      x25519_pubkey: oldPeer.x25519PubkeyB64, key_generation: 1
    });
    await expect(
      gate({
        signature_pubkey: newPeer.signaturePubkeyB64,
        x25519_pubkey: newPeer.x25519PubkeyB64, key_generation: 2,
      }),
    ).rejects.toBeInstanceOf(SupersessionRefused);
    const pin = readPin(accountIdB64);
    expect(pin?.pinnedSignaturePubkeyB64).toBe(oldPeer.signaturePubkeyB64);
    expect(pin?.provenPredecessorsB64).toBeUndefined();
  });

  it("a changed key WITHOUT a proof fails closed (no blind TOFU)", async () => {
    const oldPeer = identityFromSeed(44);
    const newPeer = identityFromSeed(45);
    const accountIdB64 = b64(ACCOUNT_ID);

    await gate({
      signature_pubkey: oldPeer.signaturePubkeyB64,
      x25519_pubkey: oldPeer.x25519PubkeyB64, key_generation: 1
    });

    let thrown: unknown;
    try {
      await gate({
        signature_pubkey: newPeer.signaturePubkeyB64,
        x25519_pubkey: newPeer.x25519PubkeyB64,
        key_generation: 1,
        // no supersession_proof
      });
    } catch (err) {
      thrown = err;
    }
    expect(thrown).toBeInstanceOf(SupersessionRefused);
    expect((thrown as SupersessionRefused).reason).toBe("proof-absent");
    // The pin is UNCHANGED — the old key is still trusted.
    expect(readPin(accountIdB64)?.pinnedSignaturePubkeyB64).toBe(
      oldPeer.signaturePubkeyB64,
    );
  });

  it("a changed key with an INVALID proof fails closed", async () => {
    const oldPeer = identityFromSeed(55);
    const newPeer = identityFromSeed(56);
    const wrongOldPeer = identityFromSeed(99);
    const accountIdB64 = b64(ACCOUNT_ID);

    await gate({
      signature_pubkey: oldPeer.signaturePubkeyB64,
      x25519_pubkey: oldPeer.x25519PubkeyB64, key_generation: 1
    });

    // Proof signed by a DIFFERENT old key — does not verify against
    // the pinned old key.
    const now = BigInt(Math.floor(Date.now() / 1000));
    const badProof = wasm.signSupersessionProof(
      wrongOldPeer.ed25519Seed,
      newPeer.ed25519Seed,
      now,
      now + WINDOW_SECS,
    );

    let thrown: unknown;
    try {
      await gate({
        signature_pubkey: newPeer.signaturePubkeyB64,
        x25519_pubkey: newPeer.x25519PubkeyB64, key_generation: 1,
        supersession_proof: b64(badProof),
      });
    } catch (err) {
      thrown = err;
    }
    expect(thrown).toBeInstanceOf(SupersessionRefused);
    expect((thrown as SupersessionRefused).reason).toBe("proof-invalid");
    expect(readPin(accountIdB64)?.pinnedSignaturePubkeyB64).toBe(
      oldPeer.signaturePubkeyB64,
    );
  });

  it("a valid proof that authorises a DIFFERENT key than served fails closed", async () => {
    const oldPeer = identityFromSeed(66);
    const realNewPeer = identityFromSeed(67);
    const servedImposter = identityFromSeed(68);
    const accountIdB64 = b64(ACCOUNT_ID);

    await gate({
      signature_pubkey: oldPeer.signaturePubkeyB64,
      x25519_pubkey: oldPeer.x25519PubkeyB64, key_generation: 1
    });

    // A genuine proof old -> realNew, but the bundle serves a third
    // (imposter) key. The proof verifies but the authorised key !=
    // served key → defence-in-depth refusal.
    const now = BigInt(Math.floor(Date.now() / 1000));
    const proof = wasm.signSupersessionProof(
      oldPeer.ed25519Seed,
      realNewPeer.ed25519Seed,
      now,
      now + WINDOW_SECS,
    );

    let thrown: unknown;
    try {
      await gate({
        signature_pubkey: servedImposter.signaturePubkeyB64,
        x25519_pubkey: servedImposter.x25519PubkeyB64, key_generation: 1,
        supersession_proof: b64(proof),
      });
    } catch (err) {
      thrown = err;
    }
    expect(thrown).toBeInstanceOf(SupersessionRefused);
    expect((thrown as SupersessionRefused).reason).toBe("key-mismatch");
    expect(readPin(accountIdB64)?.pinnedSignaturePubkeyB64).toBe(
      oldPeer.signaturePubkeyB64,
    );
  });
});
