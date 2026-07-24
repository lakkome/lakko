// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0040 (G-35) — the durable pending-commit outbox store round-trip + its
 * fail-closed durable write.
 *
 * The vitest `node` environment has no `indexedDB`, so we install a small
 * in-memory multi-store shim (the `lakko-mls` DB carries two stores — `snapshots`
 * + `commit-outbox` — and supports multi-store atomic transactions). The shim
 * covers exactly the surface the outbox + snapshot helpers use: `open` with
 * `onupgradeneeded`/`onsuccess`, `transaction(store | [stores], mode)` with
 * `oncomplete`/`onabort`/`onerror`, and `put`/`get`/`getAll`/`delete`. An
 * error-injection knob drives the fail-closed test (a write that aborts the tx
 * must make `putEntryDurable` REJECT, not swallow).
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

// ---------------------------------------------------------------------------
// In-memory IndexedDB shim (multi-store, atomic-ish).
// ---------------------------------------------------------------------------

type Row = Record<string, unknown>;

interface DbState {
  stores: Map<string, Map<string, Row>>;
  keyPaths: Map<string, string>;
}

const DBS = new Map<string, DbState>();

/** Set to a store name to make the NEXT `put` into that store abort its tx. */
let failPutStore: string | null = null;

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
    private readonly name: string,
    private readonly tx: FakeTx,
  ) {}
  put(row: Row): FakeReq<void> {
    const req = new FakeReq<void>();
    if (failPutStore === this.name) {
      failPutStore = null;
      this.tx.fail(new Error("simulated IDB put failure"));
      return req;
    }
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
  getAll(): FakeReq<Row[]> {
    const req = new FakeReq<Row[]>();
    this.tx.enqueue(() => {
      req.result = Array.from(this.backing.values());
      req.onsuccess?.();
    });
    return req;
  }
  delete(key: string): FakeReq<void> {
    const req = new FakeReq<void>();
    this.tx.enqueue(() => {
      this.backing.delete(key);
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
  private failed = false;
  constructor(private readonly state: DbState) {}
  objectStore(name: string): FakeStore {
    const backing = this.state.stores.get(name);
    const keyPath = this.state.keyPaths.get(name);
    if (backing === undefined || keyPath === undefined) {
      throw new Error(`no such store ${name}`);
    }
    return new FakeStore(backing, keyPath, name, this);
  }
  enqueue(op: () => void): void {
    this.pending += 1;
    queueMicrotask(() => {
      if (this.failed) return;
      try {
        op();
      } catch (err) {
        this.fail(err as Error);
        return;
      }
      this.pending -= 1;
      if (this.pending === 0) this.settle();
    });
  }
  private settle(): void {
    queueMicrotask(() => {
      if (!this.done && !this.failed) {
        this.done = true;
        this.oncomplete?.();
      }
    });
  }
  fail(err: Error): void {
    if (this.failed || this.done) return;
    this.failed = true;
    this.error = err;
    queueMicrotask(() => {
      this.onerror?.();
      this.onabort?.();
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
  transaction(names: string | string[], _mode: string): FakeTx {
    void names;
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
    const db = new FakeDb(state);
    req.result = db;
    queueMicrotask(() => {
      // Fire the upgrade once (fresh db) so the stores get created, then success.
      if (fresh) req.onupgradeneeded?.();
      req.onsuccess?.();
    });
    return req;
  },
};

beforeEach(() => {
  DBS.clear();
  failPutStore = null;
  (globalThis as unknown as { indexedDB: unknown }).indexedDB = fakeIndexedDB;
});

afterEach(() => {
  delete (globalThis as unknown as { indexedDB?: unknown }).indexedDB;
});

import {
  deleteEntry,
  deleteOutboxForAccount,
  loadUnacked,
  markAcked,
  putEntryDurable,
  type PendingCommitEntry,
} from "./commit-outbox";

function entry(over: Partial<PendingCommitEntry> = {}): PendingCommitEntry {
  return {
    id: "id-1",
    accountId: "acct-a",
    groupId: "grp-1",
    targetEpoch: "6",
    commitB64: "AQID",
    acked: false,
    ...over,
  };
}

describe("commit-outbox store — round-trip", () => {
  it("puts then loads an un-acked entry by group", async () => {
    await putEntryDurable(entry({ id: "a", groupId: "g1" }));
    await putEntryDurable(entry({ id: "b", groupId: "g2" }));

    const g1 = await loadUnacked("g1");
    expect(g1.map((e) => e.id)).toEqual(["a"]);

    const all = await loadUnacked();
    expect(all.map((e) => e.id).sort()).toEqual(["a", "b"]);
  });

  it("markAcked flips the flag + records ids, so the sweep skips it", async () => {
    await putEntryDurable(entry({ id: "a", groupId: "g1" }));
    await markAcked("a", 42, 43);

    // No longer un-acked.
    expect(await loadUnacked("g1")).toEqual([]);
    // The ids landed.
    const all = await loadUnacked();
    expect(all).toEqual([]); // acked -> filtered out of un-acked

    // markAcked on a missing id is a graceful no-op (a concurrent delete raced).
    await expect(markAcked("does-not-exist")).resolves.toBeUndefined();
  });

  it("deleteEntry removes it", async () => {
    await putEntryDurable(entry({ id: "a", groupId: "g1" }));
    await deleteEntry("a");
    expect(await loadUnacked()).toEqual([]);
  });

  it("deleteOutboxForAccount purges only the target account's entries", async () => {
    await putEntryDurable(entry({ id: "a", accountId: "acct-a" }));
    await putEntryDurable(entry({ id: "b", accountId: "acct-b" }));
    await deleteOutboxForAccount("acct-a");
    const all = await loadUnacked();
    expect(all.map((e) => e.id)).toEqual(["b"]);
  });

  it("preserves the two-artifact fields (add / approve coverage)", async () => {
    await putEntryDurable(
      entry({
        id: "w",
        groupId: "g1",
        secondArtifactB64: "V0VMQ09NRQ==",
        secondArtifactKind: "welcome",
        secondArtifactMailboxB64: "TUFJTA==",
      }),
    );
    const loaded = await loadUnacked("g1");
    const e = loaded[0];
    expect(e).toBeDefined();
    expect(e?.secondArtifactKind).toBe("welcome");
    expect(e?.secondArtifactB64).toBe("V0VMQ09NRQ==");
    expect(e?.secondArtifactMailboxB64).toBe("TUFJTA==");
  });
});

describe("commit-outbox store — fail-closed durable write", () => {
  it("putEntryDurable THROWS when the IDB transaction aborts", async () => {
    failPutStore = "commit-outbox";
    await expect(putEntryDurable(entry({ id: "x" }))).rejects.toThrow();
    // Nothing landed.
    failPutStore = null;
    expect(await loadUnacked()).toEqual([]);
  });
});
