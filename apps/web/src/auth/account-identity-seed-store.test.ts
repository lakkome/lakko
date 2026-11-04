// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Unit tests for the ADR-0017 AccountIdentity seed store
 * (`account-identity-seed-store.ts`) plus a wasm-driven proof that a
 * signup→recover round-trip with the SAME recovery phrase yields the
 * SAME AccountIdentity pubkey — the load-bearing property of the
 * phrase-seeded-identity change (the pubkey contacts pin must not
 * churn on a routine recover).
 *
 * The vitest `node` environment has no `indexedDB`, so we install a
 * minimal in-memory shim (same spirit as the `localStorage` shim the
 * other store tests use). `crypto.subtle` + `crypto.getRandomValues`
 * ARE present in node, so the AES-GCM wrap/unwrap runs for real.
 */
import { type DeviceCipher, WebDeviceCipher } from "./device-cipher";
import {
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
} from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  ACCOUNT_IDENTITY_SEED_LENGTH,
  deleteAccountIdentitySeed,
  getAccountIdentitySeed,
  putAccountIdentitySeed,
} from "./account-identity-seed-store";

// ---------------------------------------------------------------------------
// Minimal in-memory IndexedDB shim.
//
// Implements only the surface the store uses: `indexedDB.open` with
// `onupgradeneeded` / `onsuccess`, a single object store keyed by
// `keyPath`, `transaction(store, mode)` with `oncomplete` / `onerror`,
// and `objectStore.{put,get,delete}` returning request-like objects.
// Backed by one process-wide map per (dbName, storeName) so a fresh
// `open` sees previously-written rows (matches real IDB persistence
// across `db.close()`).
// ---------------------------------------------------------------------------

type Row = { accountId: string; ciphertext: Uint8Array };

const STORES = new Map<string, Map<string, Row>>();

function storeKey(db: string, store: string): string {
  return `${db}::${store}`;
}

class FakeRequest<T> {
  result: T | undefined = undefined;
  error: Error | null = null;
  onsuccess: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onupgradeneeded: (() => void) | null = null;
  fire(): void {
    if (this.error !== null) {
      this.onerror?.();
    } else {
      this.onsuccess?.();
    }
  }
}

class FakeObjectStore {
  constructor(
    private readonly backing: Map<string, Row>,
    private readonly tx: FakeTransaction,
  ) {}
  put(row: Row): FakeRequest<void> {
    const req = new FakeRequest<void>();
    this.backing.set(row.accountId, {
      accountId: row.accountId,
      ciphertext: row.ciphertext,
    });
    this.tx.queueComplete();
    return req;
  }
  get(key: string): FakeRequest<Row | undefined> {
    const req = new FakeRequest<Row | undefined>();
    req.result = this.backing.get(key);
    queueMicrotask(() => req.fire());
    return req;
  }
  delete(key: string): FakeRequest<void> {
    const req = new FakeRequest<void>();
    this.backing.delete(key);
    this.tx.queueComplete();
    return req;
  }
}

class FakeTransaction {
  oncomplete: (() => void) | null = null;
  onerror: (() => void) | null = null;
  error: Error | null = null;
  private completed = false;
  constructor(private readonly backing: Map<string, Row>) {}
  objectStore(): FakeObjectStore {
    return new FakeObjectStore(this.backing, this);
  }
  queueComplete(): void {
    if (this.completed) return;
    this.completed = true;
    queueMicrotask(() => this.oncomplete?.());
  }
}

class FakeDatabase {
  objectStoreNames = {
    contains: (): boolean => true,
  };
  constructor(private readonly backing: Map<string, Row>) {}
  createObjectStore(): void {
    /* store already exists in our flat backing map */
  }
  transaction(): FakeTransaction {
    return new FakeTransaction(this.backing);
  }
  close(): void {
    /* no-op for the shim */
  }
}

const fakeIndexedDB = {
  open(name: string): FakeRequest<FakeDatabase> {
    const key = storeKey(name, "seeds");
    if (!STORES.has(key)) STORES.set(key, new Map());
    const backing = STORES.get(key) as Map<string, Row>;
    const req = new FakeRequest<FakeDatabase>();
    req.result = new FakeDatabase(backing);
    queueMicrotask(() => {
      // No version bump path needed — fire upgrade once then success.
      req.onupgradeneeded?.();
      req.fire();
    });
    return req;
  },
};

beforeEach(() => {
  (globalThis as unknown as { indexedDB: unknown }).indexedDB =
    fakeIndexedDB;
});

afterEach(() => {
  STORES.clear();
  delete (globalThis as unknown as { indexedDB?: unknown }).indexedDB;
});

// ---------------------------------------------------------------------------
// Device-key helper — a real non-extractable-ish AES-GCM key. In node
// `crypto.subtle.generateKey` works; extractability does not matter for
// the test (we never export it).
// ---------------------------------------------------------------------------

async function makeDeviceKey(): Promise<DeviceCipher> {
  return new WebDeviceCipher(await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, [
    "encrypt",
    "decrypt",
  ]));
}

const ACCT = "YWNjb3VudC1pZC0wMDAwMDAwMDAwMDA="; // arbitrary base64

function freshSeed(): Uint8Array {
  return crypto.getRandomValues(
    new Uint8Array(ACCOUNT_IDENTITY_SEED_LENGTH),
  );
}

describe("account-identity-seed-store (ADR-0017)", () => {
  it("round-trips a 64-byte seed under the device key", async () => {
    const key = await makeDeviceKey();
    const seed = freshSeed();
    await putAccountIdentitySeed(ACCT, seed, key);
    const got = await getAccountIdentitySeed(ACCT, key);
    expect(got).not.toBeNull();
    expect(got).toEqual(seed);
  });

  it("returns null for an account with no cached seed", async () => {
    const key = await makeDeviceKey();
    expect(await getAccountIdentitySeed(ACCT, key)).toBeNull();
  });

  it("throws / fails decrypt under the wrong device key", async () => {
    const key = await makeDeviceKey();
    const wrongKey = await makeDeviceKey();
    await putAccountIdentitySeed(ACCT, freshSeed(), key);
    await expect(getAccountIdentitySeed(ACCT, wrongKey)).rejects.toThrow();
  });

  it("rejects a seed of the wrong length on put", async () => {
    const key = await makeDeviceKey();
    const short = crypto.getRandomValues(new Uint8Array(32));
    await expect(putAccountIdentitySeed(ACCT, short, key)).rejects.toThrow();
  });

  it("delete removes the cached seed", async () => {
    const key = await makeDeviceKey();
    await putAccountIdentitySeed(ACCT, freshSeed(), key);
    await deleteAccountIdentitySeed(ACCT);
    expect(await getAccountIdentitySeed(ACCT, key)).toBeNull();
  });

  it("overwrites a prior seed (upsert)", async () => {
    const key = await makeDeviceKey();
    const a = freshSeed();
    const b = freshSeed();
    await putAccountIdentitySeed(ACCT, a, key);
    await putAccountIdentitySeed(ACCT, b, key);
    expect(await getAccountIdentitySeed(ACCT, key)).toEqual(b);
  });
});

// ---------------------------------------------------------------------------
// The core security property of ADR-0017: a recover with the SAME
// recovery phrase reproduces the SAME AccountIdentity pubkey, because
// the identity is now derived from the (unchanged) BIP39 phrase seed,
// not the (rotated-on-recover) password export key.
// ---------------------------------------------------------------------------

describe("phrase-seeded identity stability (ADR-0017)", () => {
  beforeAll(async () => {
    // Load the wasm core from its bytes (same pattern as the
    // supersession-verify test) so `RecoveryMnemonic` / `Identity`
    // are usable in the node environment.
    const wasmPath = fileURLToPath(
      new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
    );
    await init({ module_or_path: readFileSync(wasmPath) });
  });

  it("same phrase ⇒ same AccountIdentity signature pubkey across signup→recover", () => {
    const mnemonic = wasm.RecoveryMnemonic.generate();
    const phrase = mnemonic.phrase;

    // Signup-time derivation.
    const seedAtSignup = mnemonic.toSeed();
    expect(seedAtSignup.length).toBe(ACCOUNT_IDENTITY_SEED_LENGTH);
    const idAtSignup = wasm.Identity.fromSeed(seedAtSignup);
    const pubAtSignup = idAtSignup.signaturePublicKey;

    // Recover-time derivation: the user re-types the SAME phrase, even
    // though the password (and so the export key) has changed.
    const reparsed = wasm.RecoveryMnemonic.fromPhrase(phrase);
    const seedAtRecover = reparsed.toSeed();
    const idAtRecover = wasm.Identity.fromSeed(seedAtRecover);
    const pubAtRecover = idAtRecover.signaturePublicKey;

    expect(pubAtRecover).toEqual(pubAtSignup);
    expect(seedAtRecover).toEqual(seedAtSignup);

    idAtSignup.free();
    idAtRecover.free();
    mnemonic.free();
    reparsed.free();
  });

  it("a different phrase ⇒ a different AccountIdentity pubkey", () => {
    const a = wasm.RecoveryMnemonic.generate();
    const b = wasm.RecoveryMnemonic.generate();
    const idA = wasm.Identity.fromSeed(a.toSeed());
    const idB = wasm.Identity.fromSeed(b.toSeed());
    expect(idB.signaturePublicKey).not.toEqual(idA.signaturePublicKey);
    idA.free();
    idB.free();
    a.free();
    b.free();
  });
});
