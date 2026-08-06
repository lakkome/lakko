// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0042 — `push/group-routing-store` round-trip.
 *
 * Pins the two things the store owns:
 *   1. the device-key-encrypted `push_routing_secret` round-trips
 *      (put → get returns the exact 32 bytes), and `hasGroupRoutingSecret`
 *      reflects presence without the device key;
 *   2. the plaintext `push_gid → (account, group_id)` REVERSE INDEX
 *      resolves via `lookupPushRoute` — the map the Sprint-4 silent-push
 *      decrypt reverse-resolves — and `clearAllGroupRouting` wipes both.
 *
 * A small in-memory IndexedDB shim (multi-store + `getKey`) + the real
 * `crypto.subtle` (Node WebCrypto) exercise the actual store code path.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

// ---------------------------------------------------------------------------
// In-memory IndexedDB shim (multi-store, getKey, clear).
// ---------------------------------------------------------------------------

type Row = Record<string, unknown>;

interface DbState {
  stores: Map<string, Map<string, Row>>;
  keyPaths: Map<string, string>;
}

const DBS = new Map<string, DbState>();

class FakeReq<T> {
  result: T | undefined = undefined;
  error: Error | null = null;
  onsuccess: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onupgradeneeded: (() => void) | null = null;
}

class FakeStore {
  constructor(
    private readonly backing: Map<string, Row>,
    private readonly keyPath: string,
    private readonly tx: FakeTx,
  ) {}
  put(row: Row): FakeReq<void> {
    const req = new FakeReq<void>();
    this.tx.enqueue(() => {
      this.backing.set(String(row[this.keyPath]), row);
    });
    return req;
  }
  get(key: string): FakeReq<Row | undefined> {
    const req = new FakeReq<Row | undefined>();
    this.tx.enqueue(() => {
      req.result = this.backing.get(key);
      req.onsuccess?.();
    });
    return req;
  }
  getKey(key: string): FakeReq<string | undefined> {
    const req = new FakeReq<string | undefined>();
    this.tx.enqueue(() => {
      req.result = this.backing.has(key) ? key : undefined;
      req.onsuccess?.();
    });
    return req;
  }
  clear(): FakeReq<void> {
    const req = new FakeReq<void>();
    this.tx.enqueue(() => {
      this.backing.clear();
    });
    return req;
  }
}

class FakeTx {
  oncomplete: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  error: Error | null = null;
  private pending = 0;
  private done = false;
  constructor(private readonly state: DbState) {}
  objectStore(name: string): FakeStore {
    const backing = this.state.stores.get(name);
    const keyPath = this.state.keyPaths.get(name);
    if (backing === undefined || keyPath === undefined) {
      throw new Error(`no such store ${name}`);
    }
    return new FakeStore(backing, keyPath, this);
  }
  enqueue(op: () => void): void {
    this.pending += 1;
    queueMicrotask(() => {
      op();
      this.pending -= 1;
      if (this.pending === 0) this.settle();
    });
  }
  private settle(): void {
    queueMicrotask(() => {
      if (!this.done) {
        this.done = true;
        this.oncomplete?.();
      }
    });
  }
}

class FakeDb {
  objectStoreNames = {
    contains: (name: string): boolean => this.state.stores.has(name),
  };
  constructor(private readonly state: DbState) {}
  createObjectStore(name: string, opts: { keyPath: string }): void {
    if (!this.state.stores.has(name)) this.state.stores.set(name, new Map());
    this.state.keyPaths.set(name, opts.keyPath);
  }
  transaction(_names: string | string[], _mode: string): FakeTx {
    return new FakeTx(this.state);
  }
  close(): void {
    /* no-op */
  }
}

const fakeIndexedDB = {
  open(name: string, _version: number): FakeReq<FakeDb> {
    let state = DBS.get(name);
    const fresh = state === undefined;
    if (state === undefined) {
      state = { stores: new Map(), keyPaths: new Map() };
      DBS.set(name, state);
    }
    const req = new FakeReq<FakeDb>();
    req.result = new FakeDb(state);
    queueMicrotask(() => {
      if (fresh) req.onupgradeneeded?.();
      req.onsuccess?.();
    });
    return req;
  },
};

beforeEach(() => {
  DBS.clear();
  (globalThis as unknown as { indexedDB: unknown }).indexedDB = fakeIndexedDB;
});

afterEach(() => {
  delete (globalThis as unknown as { indexedDB?: unknown }).indexedDB;
});

import {
  clearAllGroupRouting,
  getGroupRoutingSecret,
  hasGroupRoutingSecret,
  lookupPushRoute,
  putGroupRouting,
} from "./group-routing-store";

async function makeDeviceKey(): Promise<CryptoKey> {
  return crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, [
    "encrypt",
    "decrypt",
  ]);
}

const ACCT = "YWNjb3VudC1pZC0wMDAwMDAwMDAwMDA=";
const GROUP = "Z3JvdXAtaWQtYWFhYWFhYWFhYWFh";
const PUSH_GID_HEX =
  "0011223344556677889900aabbccddeeff0011223344556677889900aabbccdd";

describe("group-routing-store — round-trip", () => {
  it("put then get round-trips the raw secret (device-key-encrypted)", async () => {
    const deviceKey = await makeDeviceKey();
    const secret = crypto.getRandomValues(new Uint8Array(32));

    await putGroupRouting(
      { accountId: ACCT, groupIdB64: GROUP, secret, pushGidHex: PUSH_GID_HEX },
      deviceKey,
    );

    const got = await getGroupRoutingSecret(ACCT, GROUP, deviceKey);
    expect(got).not.toBeNull();
    expect(Array.from(got as Uint8Array)).toEqual(Array.from(secret));
  });

  it("getGroupRoutingSecret returns null for an unknown group", async () => {
    const deviceKey = await makeDeviceKey();
    expect(await getGroupRoutingSecret(ACCT, "Zm9v", deviceKey)).toBeNull();
  });

  it("hasGroupRoutingSecret reflects presence without a device key", async () => {
    const deviceKey = await makeDeviceKey();
    expect(await hasGroupRoutingSecret(ACCT, GROUP)).toBe(false);
    await putGroupRouting(
      {
        accountId: ACCT,
        groupIdB64: GROUP,
        secret: new Uint8Array(32).fill(9),
        pushGidHex: PUSH_GID_HEX,
      },
      deviceKey,
    );
    expect(await hasGroupRoutingSecret(ACCT, GROUP)).toBe(true);
  });

  it("lookupPushRoute maps push_gid -> (account, group_id) [the reverse index]", async () => {
    const deviceKey = await makeDeviceKey();
    await putGroupRouting(
      {
        accountId: ACCT,
        groupIdB64: GROUP,
        secret: new Uint8Array(32).fill(1),
        pushGidHex: PUSH_GID_HEX,
      },
      deviceKey,
    );

    const route = await lookupPushRoute(PUSH_GID_HEX);
    expect(route).not.toBeNull();
    expect(route?.accountId).toBe(ACCT);
    expect(route?.groupIdB64).toBe(GROUP);
    // The reverse index is keyed by push_gid, NOT the group_id.
    expect(route?.pushGidHex).toBe(PUSH_GID_HEX);

    expect(await lookupPushRoute("deadbeef")).toBeNull();
  });

  it("clearAllGroupRouting wipes secrets + the reverse index", async () => {
    const deviceKey = await makeDeviceKey();
    await putGroupRouting(
      {
        accountId: ACCT,
        groupIdB64: GROUP,
        secret: new Uint8Array(32).fill(2),
        pushGidHex: PUSH_GID_HEX,
      },
      deviceKey,
    );

    await clearAllGroupRouting();

    expect(await hasGroupRoutingSecret(ACCT, GROUP)).toBe(false);
    expect(await lookupPushRoute(PUSH_GID_HEX)).toBeNull();
  });
});
