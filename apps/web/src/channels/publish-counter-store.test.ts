// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Unit tests for the per-(account, channel) publish-counter persistence
 * (`publish-counter-store.ts`) — the store that keeps the channel-post
 * AEAD `(key, nonce)` unique across a page reload, and keeps one
 * account's floor away from another account on the same browser. Runs under the
 * default vitest `node` environment (no `localStorage`); a minimal
 * in-memory shim is installed per test so the store's
 * `typeof localStorage` guard sees a real backend.
 *
 * The load-else-0 (`seedCounter`) + monotone write-through
 * (`issueAndAdvance`) helpers are the SAME functions the
 * `channels-state.ts` `setKeys` / `nextCounter` actions call, so these
 * tests exercise the production decision logic directly.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

/** Minimal synchronous in-memory `localStorage` Web Storage shim. */
class MemoryStorage {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.has(key) ? (this.map.get(key) as string) : null;
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
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return Array.from(this.map.keys())[i] ?? null;
  }
}

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  bindContactsAccount(ACCOUNT_A);
});

afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

import { bindContactsAccount } from "../contacts/local-store";
import {
  clearPublishCountersForAccount,
  issueAndAdvance,
  loadAllCounters,
  loadCounter,
  saveCounter,
  seedCounter,
} from "./publish-counter-store";

const CH = "00112233445566778899aabbccddeeff";
const OTHER = "ffffffffffffffffffffffffffffffff";
// Standard base64 account ids, as `AuthState` carries them (`+`, `/`, `=`
// included on purpose: the row key must survive them).
const ACCOUNT_A = "AAECAwQFBgcICQoLDA0ODw==";
const ACCOUNT_B = "+/+/+/+/+/+/+/+/+/+/+w==";
const KEY = `lakko.channels.publish-counter.v2.${ACCOUNT_A}.${CH}`;
const RETIRED_KEY = "lakko.channels.publish-counter.v1." + CH;

describe("publish-counter-store: persist / load", () => {
  it("returns null before anything is persisted", () => {
    expect(loadCounter(CH)).toBeNull();
  });

  it("round-trips a bigint counter, preserving type + precision", () => {
    saveCounter(CH, 42n);
    const loaded = loadCounter(CH);
    expect(loaded).toBe(42n);
    expect(typeof loaded).toBe("bigint");
  });

  it("round-trips a large u64-range counter without precision loss", () => {
    const big = 18_446_744_073_709_551_615n; // u64::MAX
    saveCounter(CH, big);
    expect(loadCounter(CH)).toBe(big);
  });

  it("round-trips the genesis 0n", () => {
    saveCounter(CH, 0n);
    expect(loadCounter(CH)).toBe(0n);
  });

  it("keys per channel — one channel's counter does not leak into another", () => {
    saveCounter(CH, 7n);
    expect(loadCounter(OTHER)).toBeNull();
    expect(loadCounter(CH)).toBe(7n);
  });

  it("treats a corrupt (non-numeric) entry as absent rather than throwing", () => {
    localStorage.setItem(KEY, "{ not a number");
    expect(loadCounter(CH)).toBeNull();
  });

  it("treats an empty / whitespace / signed value as absent (never a silent 0n)", () => {
    // BigInt("") and BigInt(" ") are 0n and BigInt("-1") is -1n — any of
    // which would silently REWIND a real counter, so the `^\d+$` gate
    // must reject them.
    for (const junk of ["", " ", "-1", "3.5", "0x10", "1e3"]) {
      localStorage.setItem(KEY, junk);
      expect(loadCounter(CH)).toBeNull();
    }
  });
});

describe("publish-counter-store: loadAllCounters", () => {
  it("returns an empty map when nothing is persisted", () => {
    expect(loadAllCounters().size).toBe(0);
  });

  it("loads only prefixed keys, decoded per channel", () => {
    saveCounter(CH, 3n);
    saveCounter(OTHER, 9n);
    // An unrelated localStorage key must be ignored.
    localStorage.setItem("lakko.channels.subscriber-keys.v1", "[]");
    const all = loadAllCounters();
    expect(all.size).toBe(2);
    expect(all.get(CH)).toBe(3n);
    expect(all.get(OTHER)).toBe(9n);
  });

  it("skips a corrupt prefixed entry instead of failing the whole load", () => {
    saveCounter(CH, 5n);
    localStorage.setItem(`lakko.channels.publish-counter.v2.${ACCOUNT_A}.${OTHER}`, "junk");
    const all = loadAllCounters();
    expect(all.get(CH)).toBe(5n);
    expect(all.has(OTHER)).toBe(false);
  });
});

describe("publish-counter-store: seedCounter (load-else-0)", () => {
  it("prefers the live in-memory value over the persisted floor", () => {
    saveCounter(CH, 2n);
    expect(seedCounter(10n, CH)).toBe(10n);
  });

  it("falls back to the persisted floor when in-memory is absent (reload)", () => {
    saveCounter(CH, 8n);
    expect(seedCounter(undefined, CH)).toBe(8n);
  });

  it("starts a genuinely new channel at 0 (no in-memory, no persisted)", () => {
    expect(seedCounter(undefined, CH)).toBe(0n);
  });

  it("preserves a live 0n rather than treating it as absent", () => {
    saveCounter(CH, 5n);
    // In-memory 0n must win over `??` — a brand-new channel seeded at 0
    // this session must not silently jump to the persisted 5n.
    expect(seedCounter(0n, CH)).toBe(0n);
  });
});

describe("publish-counter-store: issueAndAdvance (monotone write-through)", () => {
  it("issues the pre-increment value and persists value+1 before returning", () => {
    const r = issueAndAdvance(0n, CH);
    expect(r.value).toBe(0n);
    expect(r.advanced).toBe(1n);
    // Write-through: the advanced floor is durable immediately.
    expect(loadCounter(CH)).toBe(1n);
  });

  it("never re-issues a value across sequential calls", () => {
    let inMem: bigint | undefined = undefined;
    const issued: bigint[] = [];
    for (let i = 0; i < 5; i += 1) {
      const r = issueAndAdvance(inMem, CH);
      issued.push(r.value);
      inMem = r.advanced;
    }
    expect(issued).toEqual([0n, 1n, 2n, 3n, 4n]);
    expect(loadCounter(CH)).toBe(5n);
  });

  it("refuses to issue a value whose advanced floor did not land", () => {
    saveCounter(CH, 3n);
    // A write that is skipped without an error (the Forget-device gate on
    // `setItem`) or that throws (quota): nothing is issued either way.
    const storage = localStorage as unknown as MemoryStorage;
    const original = storage.setItem.bind(storage);
    storage.setItem = (): void => undefined;
    expect(() => issueAndAdvance(3n, CH)).toThrow("not persisted");
    storage.setItem = (): void => {
      throw new Error("QuotaExceededError");
    };
    expect(() => issueAndAdvance(3n, CH)).toThrow("not persisted");
    storage.setItem = original;
    expect(loadCounter(CH)).toBe(3n);
    expect(issueAndAdvance(3n, CH).value).toBe(3n);
  });

  it("refuses to issue a value with no storage at all", () => {
    delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
    expect(() => issueAndAdvance(0n, CH)).toThrow("not persisted");
  });

  it("resumes from the persisted floor when in-memory is lost", () => {
    saveCounter(CH, 5n);
    const r = issueAndAdvance(undefined, CH);
    expect(r.value).toBe(5n); // NOT 0 — no rewind
    expect(loadCounter(CH)).toBe(6n);
  });
});

describe("publish-counter-store: reload regression (the bug this closes)", () => {
  it("does NOT rewind the counter to 0 after a simulated reload", () => {
    // Session 1: fresh channel, publish 5 posts.
    let map = new Map<string, bigint>();
    map.set(CH, seedCounter(map.get(CH), CH)); // setKeys load-else-0 -> 0n
    const sessionOne: bigint[] = [];
    for (let i = 0; i < 5; i += 1) {
      const r = issueAndAdvance(map.get(CH), CH);
      sessionOne.push(r.value);
      map.set(CH, r.advanced);
    }
    expect(sessionOne).toEqual([0n, 1n, 2n, 3n, 4n]);

    // Simulated page RELOAD: in-memory map is rebuilt from persistence,
    // the channel content seed is unchanged (stable MLS exporter).
    map = loadAllCounters();
    expect(map.get(CH)).toBe(5n);

    // setKeys runs again on re-open — must NOT clobber to 0.
    map.set(CH, seedCounter(map.get(CH), CH));
    expect(map.get(CH)).toBe(5n);

    // Next post after reload issues 5, never a re-used 0..4.
    const afterReload = issueAndAdvance(map.get(CH), CH);
    expect(afterReload.value).toBe(5n);
    expect(sessionOne).not.toContain(afterReload.value);
  });
});

describe("publish-counter-store: per account", () => {
  it("files the row under the account and the channel", () => {
    saveCounter(CH, 4n);
    expect(localStorage.getItem(KEY)).toBe("4");
  });

  it("keeps two accounts' counters for one channel apart", () => {
    saveCounter(CH, 7n, ACCOUNT_A);
    saveCounter(CH, 3n, ACCOUNT_B);
    expect(loadCounter(CH, ACCOUNT_A)).toBe(7n);
    expect(loadCounter(CH, ACCOUNT_B)).toBe(3n);
    expect(loadAllCounters(ACCOUNT_A).get(CH)).toBe(7n);
    expect(loadAllCounters(ACCOUNT_B).get(CH)).toBe(3n);
  });

  it("account B never starts from account A's floor", () => {
    let inMem: bigint | undefined;
    for (let i = 0; i < 5; i += 1) inMem = issueAndAdvance(inMem, CH).advanced;
    expect(loadCounter(CH)).toBe(5n);

    bindContactsAccount(ACCOUNT_B);
    expect(loadCounter(CH)).toBeNull();
    expect(loadAllCounters().size).toBe(0);
    expect(seedCounter(undefined, CH)).toBe(0n);
    const first = issueAndAdvance(undefined, CH);
    expect(first.value).toBe(0n);
    // A's floor is untouched by B's post.
    expect(loadCounter(CH, ACCOUNT_A)).toBe(5n);
  });

  it("an id that prefixes another account's id never reads that account's rows", () => {
    const shorter = "AAEC";
    const longer = "AAECAw==";
    saveCounter(CH, 9n, longer);
    expect(loadAllCounters(shorter).size).toBe(0);
    expect(loadCounter(CH, shorter)).toBeNull();
  });

  it("never reads the retired device-wide v1 row", () => {
    localStorage.setItem(RETIRED_KEY, "99");
    expect(loadCounter(CH)).toBeNull();
    expect(loadAllCounters().size).toBe(0);
    expect(issueAndAdvance(undefined, CH).value).toBe(0n);
  });

  it("with no account: nothing is read, a save is a no-op, nothing is issued", () => {
    saveCounter(CH, 6n);
    bindContactsAccount(null);
    expect(loadCounter(CH)).toBeNull();
    expect(loadAllCounters().size).toBe(0);
    const before = (localStorage as unknown as MemoryStorage).length;
    saveCounter(OTHER, 2n);
    expect((localStorage as unknown as MemoryStorage).length).toBe(before);
    expect(() => issueAndAdvance(undefined, CH)).toThrow();
    // The account's floor did not move.
    expect(loadCounter(CH, ACCOUNT_A)).toBe(6n);
  });
});

describe("publish-counter-store: clearPublishCountersForAccount (Forget device / Delete account)", () => {
  it("removes only the wiped account's counters and the retired rows", () => {
    saveCounter(CH, 3n, ACCOUNT_A);
    saveCounter(OTHER, 9n, ACCOUNT_A);
    saveCounter(CH, 4n, ACCOUNT_B);
    localStorage.setItem(RETIRED_KEY, "42");
    localStorage.setItem("lakko.channels.subscriber-keys.v1", "[]");

    clearPublishCountersForAccount(ACCOUNT_A);

    expect(loadCounter(CH, ACCOUNT_A)).toBeNull();
    expect(loadCounter(OTHER, ACCOUNT_A)).toBeNull();
    expect(loadCounter(CH, ACCOUNT_B)).toBe(4n);
    expect(localStorage.getItem(RETIRED_KEY)).toBeNull();
    expect(localStorage.getItem("lakko.channels.subscriber-keys.v1")).toBe("[]");
  });
});
