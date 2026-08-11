// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0042 — `push/group-routing-store` round-trip.
 *
 * Pins the three things the store owns:
 *   1. the device-key-encrypted `push_routing_secret` round-trips
 *      (put → get returns the exact 32 bytes), and `hasGroupRoutingSecret`
 *      reflects presence without the device key;
 *   2. the plaintext `push_gid → (account, group_id)` REVERSE INDEX
 *      resolves via `lookupPushRoute` — the map the Sprint-4 silent-push
 *      decrypt reverse-resolves — and `clearAllGroupRouting` wipes both;
 *   3. the durable `distributed` flag: `getGroupRoutingState` separates
 *      `absent` / `pending` / `distributed`, `markGroupRoutingDistributed`
 *      flips a pending row, `markGroupRoutingPending` flips it BACK (what
 *      re-arms the retry after a failed redistribute), both are no-ops on an
 *      absent row, and a LEGACY row carrying no flag reads as `distributed`
 *      (so an upgrade does not re-broadcast for every group).
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
  getGroupRoutingState,
  hasGroupRoutingSecret,
  lookupPushRoute,
  markGroupRoutingDistributed,
  markGroupRoutingPending,
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
      {
        accountId: ACCT,
        groupIdB64: GROUP,
        secret,
        pushGidHex: PUSH_GID_HEX,
        distributed: true,
      },
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
        distributed: true,
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
        distributed: true,
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
        distributed: true,
      },
      deviceKey,
    );

    await clearAllGroupRouting();

    expect(await hasGroupRoutingSecret(ACCT, GROUP)).toBe(false);
    expect(await lookupPushRoute(PUSH_GID_HEX)).toBeNull();
  });
});

describe("group-routing-store — the durable `distributed` flag", () => {
  async function put(distributed: boolean): Promise<void> {
    await putGroupRouting(
      {
        accountId: ACCT,
        groupIdB64: GROUP,
        secret: new Uint8Array(32).fill(3),
        pushGidHex: PUSH_GID_HEX,
        distributed,
      },
      await makeDeviceKey(),
    );
  }

  /** Strip the field from the stored row — the on-disk shape a build that
   *  predates the flag left behind. */
  function stripDistributedField(): void {
    const rows = DBS.get("lakko-push-routing")?.stores.get("secrets");
    expect(rows).toBeDefined();
    for (const row of (rows as Map<string, Row>).values()) {
      delete row.distributed;
    }
  }

  it("reports `absent` for a group with no secret", async () => {
    expect(await getGroupRoutingState(ACCT, GROUP)).toBe("absent");
  });

  it("reports `pending` for a secret persisted before its broadcast landed", async () => {
    await put(false);
    expect(await getGroupRoutingState(ACCT, GROUP)).toBe("pending");
    // Presence alone cannot see the difference — that is the whole defect.
    expect(await hasGroupRoutingSecret(ACCT, GROUP)).toBe(true);
  });

  it("markGroupRoutingDistributed flips pending -> distributed, keeping the secret intact", async () => {
    const deviceKey = await makeDeviceKey();
    const secret = crypto.getRandomValues(new Uint8Array(32));
    await putGroupRouting(
      {
        accountId: ACCT,
        groupIdB64: GROUP,
        secret,
        pushGidHex: PUSH_GID_HEX,
        distributed: false,
      },
      deviceKey,
    );

    await markGroupRoutingDistributed(ACCT, GROUP);

    expect(await getGroupRoutingState(ACCT, GROUP)).toBe("distributed");
    // The read-modify-write must not disturb the ciphertext.
    const got = await getGroupRoutingSecret(ACCT, GROUP, deviceKey);
    expect(Array.from(got as Uint8Array)).toEqual(Array.from(secret));
    // Idempotent.
    await markGroupRoutingDistributed(ACCT, GROUP);
    expect(await getGroupRoutingState(ACCT, GROUP)).toBe("distributed");
  });

  it("markGroupRoutingDistributed on an absent row is a no-op (never resurrects a wiped secret)", async () => {
    await markGroupRoutingDistributed(ACCT, GROUP);
    expect(await getGroupRoutingState(ACCT, GROUP)).toBe("absent");
    expect(await hasGroupRoutingSecret(ACCT, GROUP)).toBe(false);
  });

  it("markGroupRoutingPending flips distributed -> pending, keeping the secret intact", async () => {
    // The flag is per-(account, group), not per-member, so a FAILED
    // redistribute after a landed mint-broadcast must be able to re-arm the
    // owner's re-broadcast branch — else the newly-added member, whose only
    // source of the secret is that redistribute, defers push forever.
    const deviceKey = await makeDeviceKey();
    const secret = crypto.getRandomValues(new Uint8Array(32));
    await putGroupRouting(
      {
        accountId: ACCT,
        groupIdB64: GROUP,
        secret,
        pushGidHex: PUSH_GID_HEX,
        distributed: true,
      },
      deviceKey,
    );

    await markGroupRoutingPending(ACCT, GROUP);

    expect(await getGroupRoutingState(ACCT, GROUP)).toBe("pending");
    // The read-modify-write must not disturb the ciphertext.
    const got = await getGroupRoutingSecret(ACCT, GROUP, deviceKey);
    expect(Array.from(got as Uint8Array)).toEqual(Array.from(secret));
    // Idempotent, and the pair round-trips back.
    await markGroupRoutingPending(ACCT, GROUP);
    expect(await getGroupRoutingState(ACCT, GROUP)).toBe("pending");
    await markGroupRoutingDistributed(ACCT, GROUP);
    expect(await getGroupRoutingState(ACCT, GROUP)).toBe("distributed");
  });

  it("markGroupRoutingPending on an absent row is a no-op (never resurrects a wiped secret)", async () => {
    await markGroupRoutingPending(ACCT, GROUP);
    expect(await getGroupRoutingState(ACCT, GROUP)).toBe("absent");
    expect(await hasGroupRoutingSecret(ACCT, GROUP)).toBe(false);
    expect(await lookupPushRoute(PUSH_GID_HEX)).toBeNull();
  });

  it("markGroupRoutingPending downgrades a LEGACY (flag-less) row too", async () => {
    // A legacy row only READS as distributed — that is an inference, not a
    // recorded value — so a failed send on such a group must still re-arm the
    // retry rather than being swallowed.
    await put(true);
    stripDistributedField();
    expect(await getGroupRoutingState(ACCT, GROUP)).toBe("distributed");

    await markGroupRoutingPending(ACCT, GROUP);
    expect(await getGroupRoutingState(ACCT, GROUP)).toBe("pending");
  });

  it("a LEGACY row with no `distributed` field reads as distributed", async () => {
    await put(true);
    stripDistributedField();
    // Those groups were provisioned by the old code and work today; reading
    // the missing field as `pending` would re-broadcast for every one of
    // them on the first launch after an upgrade.
    expect(await getGroupRoutingState(ACCT, GROUP)).toBe("distributed");
  });
});
