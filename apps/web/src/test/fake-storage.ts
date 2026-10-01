// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * In-memory `Storage` and IndexedDB for the host tests (the vitest `node`
 * project has neither), with a way to walk everything written.
 *
 * The IndexedDB half implements the surface the app's stores use: `open`
 * with an upgrade callback, `objectStoreNames.contains`, `createObjectStore`
 * with a string or array key path, `deleteObjectStore`, and per-store
 * `put` / `get` / `getKey` / `delete` / `getAll` / `getAllKeys` / `count` /
 * `clear` inside a transaction whose `oncomplete` fires once every request,
 * including requests issued from another request's `onsuccess`, has run. Key
 * ranges, cursors and indexes are not implemented: a store that starts using
 * them fails loudly here rather than passing silently.
 */

/** A `Storage` over a Map, iteration order = insertion order. */
export class MemoryStorage {
  private readonly map = new Map<string, string>();
  get length(): number {
    return this.map.size;
  }
  key(index: number): string | null {
    return [...this.map.keys()][index] ?? null;
  }
  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, String(value));
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
  clear(): void {
    this.map.clear();
  }
  /** Every entry (for the inventory walk). */
  entries(): [string, string][] {
    return [...this.map.entries()];
  }
}

type KeyPath = string | string[];

interface FakeStoreState {
  keyPath: KeyPath;
  rows: Map<string, { key: unknown; value: unknown }>;
}

interface FakeDbState {
  version: number;
  stores: Map<string, FakeStoreState>;
}

function keyOf(keyPath: KeyPath, value: unknown): unknown {
  const v = value as Record<string, unknown>;
  if (Array.isArray(keyPath)) return keyPath.map((p) => v[p]);
  return v[keyPath];
}

function serialiseKey(key: unknown): string {
  if (typeof key === "object" && key !== null && !Array.isArray(key)) {
    throw new Error("fake-indexeddb: key ranges are not implemented");
  }
  return JSON.stringify(key);
}

class FakeRequest<T> {
  result: T | undefined = undefined;
  error: Error | null = null;
  transaction: FakeTransaction | null = null;
  onsuccess: ((ev?: unknown) => void) | null = null;
  onerror: ((ev?: unknown) => void) | null = null;
  onupgradeneeded: ((ev?: unknown) => void) | null = null;
  onblocked: ((ev?: unknown) => void) | null = null;
}

class FakeObjectStore {
  constructor(
    private readonly state: FakeStoreState,
    private readonly tx: FakeTransaction,
  ) {}

  private request<T>(op: () => T): FakeRequest<T> {
    const req = new FakeRequest<T>();
    this.tx.enqueue(() => {
      req.result = op();
      req.onsuccess?.({ target: req });
    });
    return req;
  }

  put(value: unknown, key?: unknown): FakeRequest<unknown> {
    this.tx.requireWrite();
    return this.request(() => {
      const k = key ?? keyOf(this.state.keyPath, value);
      this.state.rows.set(serialiseKey(k), { key: k, value: structuredClone(value) });
      return k;
    });
  }
  add(value: unknown, key?: unknown): FakeRequest<unknown> {
    return this.put(value, key);
  }
  get(key: unknown): FakeRequest<unknown> {
    return this.request(() => this.state.rows.get(serialiseKey(key))?.value);
  }
  getKey(key: unknown): FakeRequest<unknown> {
    return this.request(() => this.state.rows.get(serialiseKey(key))?.key);
  }
  delete(key: unknown): FakeRequest<undefined> {
    this.tx.requireWrite();
    return this.request(() => {
      this.state.rows.delete(serialiseKey(key));
      return undefined;
    });
  }
  getAll(query?: unknown): FakeRequest<unknown[]> {
    if (query !== undefined) throw new Error("fake-indexeddb: getAll(query) is not implemented");
    return this.request(() => [...this.state.rows.values()].map((r) => r.value));
  }
  getAllKeys(query?: unknown): FakeRequest<unknown[]> {
    if (query !== undefined) throw new Error("fake-indexeddb: getAllKeys(query) is not implemented");
    return this.request(() => [...this.state.rows.values()].map((r) => r.key));
  }
  count(): FakeRequest<number> {
    return this.request(() => this.state.rows.size);
  }
  clear(): FakeRequest<undefined> {
    this.tx.requireWrite();
    return this.request(() => {
      this.state.rows.clear();
      return undefined;
    });
  }
}

class FakeTransaction {
  oncomplete: ((ev?: unknown) => void) | null = null;
  onerror: ((ev?: unknown) => void) | null = null;
  onabort: ((ev?: unknown) => void) | null = null;
  error: Error | null = null;
  private readonly queue: (() => void)[] = [];
  private scheduled = false;

  constructor(
    private readonly db: FakeDbState,
    private readonly names: readonly string[],
    readonly mode: string,
  ) {
    this.schedule();
  }

  requireWrite(): void {
    if (this.mode !== "readwrite" && this.mode !== "versionchange") {
      throw new Error("fake-indexeddb: write in a readonly transaction");
    }
  }

  objectStore(name: string): FakeObjectStore {
    if (!this.names.includes(name)) throw new Error(`fake-indexeddb: store ${name} not in transaction`);
    const state = this.db.stores.get(name);
    if (state === undefined) throw new Error(`fake-indexeddb: no store ${name}`);
    return new FakeObjectStore(state, this);
  }

  enqueue(op: () => void): void {
    this.queue.push(op);
    this.schedule();
  }

  private schedule(): void {
    if (this.scheduled) return;
    this.scheduled = true;
    setTimeout(() => this.drain(), 0);
  }

  private drain(): void {
    this.scheduled = false;
    while (this.queue.length > 0) {
      const op = this.queue.shift() as () => void;
      op();
    }
    // A request's `onsuccess` may have queued more; commit only once a turn
    // passes with nothing new.
    setTimeout(() => {
      if (this.queue.length > 0) {
        this.schedule();
        return;
      }
      this.oncomplete?.({ target: this });
    }, 0);
  }
}

class FakeDatabase {
  onversionchange: ((ev?: unknown) => void) | null = null;
  onclose: ((ev?: unknown) => void) | null = null;
  constructor(
    readonly name: string,
    private readonly state: FakeDbState,
  ) {}
  get version(): number {
    return this.state.version;
  }
  get objectStoreNames(): { contains(n: string): boolean; readonly length: number } {
    const names = [...this.state.stores.keys()];
    return { contains: (n: string) => names.includes(n), length: names.length };
  }
  createObjectStore(name: string, opts?: { keyPath?: KeyPath }): void {
    if (opts?.keyPath === undefined) throw new Error("fake-indexeddb: out-of-line keys are not implemented");
    this.state.stores.set(name, { keyPath: opts.keyPath, rows: new Map() });
  }
  deleteObjectStore(name: string): void {
    this.state.stores.delete(name);
  }
  transaction(names: string | string[], mode = "readonly"): FakeTransaction {
    return new FakeTransaction(this.state, Array.isArray(names) ? names : [names], mode);
  }
  close(): void {
    /* nothing held */
  }
}

/** The fake factory; `dbs` is the whole state, for seeding and walking. */
export class FakeIndexedDb {
  readonly dbs = new Map<string, FakeDbState>();

  open(name: string, version?: number): FakeRequest<FakeDatabase> {
    const req = new FakeRequest<FakeDatabase>();
    setTimeout(() => {
      let state = this.dbs.get(name);
      const fresh = state === undefined;
      if (state === undefined) {
        state = { version: 0, stores: new Map() };
        this.dbs.set(name, state);
      }
      const target = version ?? Math.max(state.version, 1);
      const db = new FakeDatabase(name, state);
      req.result = db;
      if (fresh || target > state.version) {
        state.version = target;
        req.onupgradeneeded?.({ target: req });
      }
      req.onsuccess?.({ target: req });
    }, 0);
    return req;
  }

  deleteDatabase(name: string): FakeRequest<undefined> {
    const req = new FakeRequest<undefined>();
    setTimeout(() => {
      this.dbs.delete(name);
      req.onsuccess?.({ target: req });
    }, 0);
    return req;
  }

  /** Empty every store, keeping the schemas. */
  clearRows(): void {
    for (const db of this.dbs.values()) for (const s of db.stores.values()) s.rows.clear();
  }

  /** Put a row straight into `db`/`store` (the store must exist). */
  seed(db: string, store: string, value: unknown): void {
    const s = this.dbs.get(db)?.stores.get(store);
    if (s === undefined) throw new Error(`fake-indexeddb: seed into missing ${db}/${store}`);
    const key = keyOf(s.keyPath, value);
    s.rows.set(serialiseKey(key), { key, value: structuredClone(value) });
  }

  /** Every row as `"<db>/<store>"` plus the JSON of its key and value. */
  walk(): { where: string; text: string }[] {
    const out: { where: string; text: string }[] = [];
    for (const [dbName, db] of this.dbs) {
      for (const [storeName, store] of db.stores) {
        for (const { key, value } of store.rows.values()) {
          out.push({ where: `${dbName}/${storeName}`, text: `${JSON.stringify(key)} ${JSON.stringify(value)}` });
        }
      }
    }
    return out;
  }
}
