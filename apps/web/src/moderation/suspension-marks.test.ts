// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 step 4 - the standing service (cache, one request in flight per
 * prefix, failure back-off, `unknown` on any core error, nothing asked when
 * unpinned), the reactive read, and the mark sites' decisions. The core's
 * prefix and its refusal of an answer that does not check are asked of the
 * REAL wasm; the cache logic uses a fake core, since no snapshot can be built
 * on this side.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import {
  FAILURE_BACKOFF_MS,
  MIN_REUSE_MS,
  SuspensionMarks,
  answerLogSize,
  fetchLogEntry,
  installOwnAccountKeys,
  installSuspensionMarks,
  isSuspendedKeyB64,
  standingWithin,
  standingsWithin,
  suspendedAmong,
  type StandingWasm,
  type SuspensionMarksDeps,
} from "./suspension-marks";
import { dropsRequestFrom, inviteOffersAdd, personMark, withoutSuspended } from "./suspension-rules";
import {
  MODERATOR_LIST_CACHE_MS,
  cachedModeratorList,
  forgetModeratorList,
  rememberModeratorList,
} from "./report-api";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

afterEach(() => {
  installSuspensionMarks(null);
  installOwnAccountKeys(() => []);
});

const LOG_KEY = new Uint8Array(32).fill(7);
const SUSPENDED = new Uint8Array(32).fill(1);
const FREE = new Uint8Array(32).fill(2);

/** A fake core: the prefix is the key's first byte; the answer is JSON
 *  naming the suspended keys' first bytes and the reuse time. */
function fakeWasm(): StandingWasm & { checks: number; proofs: string[] } {
  const w = {
    checks: 0,
    proofs: [] as string[],
    moderationBucketPrefix(key: Uint8Array): string {
      if (key.length !== 32) throw new Error("invalid: key");
      return (key[0] ?? 0).toString(16).padStart(4, "0");
    },
    moderationAccountStanding(answerJson: string, key: Uint8Array, _log: Uint8Array, _w: Uint8Array, _t: number, nowMs: bigint) {
      w.checks += 1;
      const a = JSON.parse(answerJson) as {
        suspended: number[];
        reuseUntilMs: number;
        untilMs?: number;
        bad?: boolean;
        noIndex?: boolean;
      };
      if (a.bad === true) throw new Error("bad-root-signature: no");
      const hit = a.suspended.includes(key[0] ?? -1) && (a.untilMs === undefined || a.untilMs > Number(nowMs));
      return {
        suspended: hit,
        untilMs: hit && a.untilMs !== undefined ? BigInt(a.untilMs) : undefined,
        reuseUntilMs: BigInt(a.reuseUntilMs),
        logIndex: hit && a.noIndex !== true ? 3n : undefined,
      };
    },
    /** Proves when the entry body is the one {@link harness} serves and the
     *  answer does not say `unsigned`. */
    moderationProveAccountStanding(
      answerJson: string,
      entryJson: string,
      list: Uint8Array,
      rootKey: Uint8Array,
      key: Uint8Array,
      _nowMs: bigint,
    ): void {
      w.proofs.push(entryJson);
      const a = JSON.parse(answerJson) as { unsigned?: boolean };
      if (a.unsigned === true) throw new Error("not-authorised: no moderator signed it");
      if (entryJson !== ENTRY || list[0] !== 9 || rootKey !== ROOT_KEY || key.length !== 32) {
        throw new Error("entry-mismatch: no");
      }
    },
  };
  return w;
}

const ROOT_KEY = new Uint8Array(32).fill(8);
const ENTRY = JSON.stringify({ index: 3, leaf_b64: "", audit_path_b64: [], log_size: 9 });

interface Harness {
  marks: SuspensionMarks;
  fetches: string[];
  /** `index/size` of every log-entry request. */
  entries: string[];
  list: { value: Uint8Array | null | Error };
  entry: { value: string | Error };
  clock: { now: number };
  body: { value: string | Error };
  release: () => void;
}

function harness(
  opts: { logKey?: Uint8Array | null; rootKey?: Uint8Array | null; gate?: boolean; core?: StandingWasm } = {},
): Harness {
  const fetches: string[] = [];
  const entries: string[] = [];
  const list: { value: Uint8Array | null | Error } = { value: new Uint8Array([9]) };
  const entry: { value: string | Error } = { value: ENTRY };
  const clock = { now: 1_000_000 };
  const body: { value: string | Error } = {
    value: answer([1], clock.now + 5 * 60_000),
  };
  let open: () => void = () => {};
  let gate: Promise<void> = Promise.resolve();
  if (opts.gate === true) gate = new Promise((r) => (open = r));
  const core = opts.core ?? fakeWasm();
  const deps: SuspensionMarksDeps = {
    wasm: async () => core,
    fetchBucket: async (prefix) => {
      fetches.push(prefix);
      await gate;
      if (body.value instanceof Error) throw body.value;
      return body.value;
    },
    logKey: opts.logKey === undefined ? LOG_KEY : opts.logKey,
    witnessKeys: [],
    witnessThreshold: 1,
    now: () => clock.now,
    rootKey: opts.rootKey === undefined ? ROOT_KEY : opts.rootKey,
    fetchEntry: async (index, size) => {
      entries.push(`${index}/${size}`);
      if (entry.value instanceof Error) throw entry.value;
      return entry.value;
    },
    moderatorList: async () => {
      if (list.value instanceof Error) throw list.value;
      return list.value;
    },
  };
  return { marks: new SuspensionMarks(deps), fetches, entries, list, entry, clock, body, release: () => open() };
}

/** A fake answer naming the suspended keys' first bytes, with a log head of
 *  nine leaves. */
function answer(suspended: number[], reuseUntilMs: number, extra: Record<string, unknown> = {}): string {
  return JSON.stringify({ suspended, reuseUntilMs, root: { log_size: 9 }, ...extra });
}

describe("the standing service", () => {
  it("asks nothing and answers unknown when no log key is pinned", async () => {
    const h = harness({ logKey: null });
    expect(h.marks.enabled).toBe(false);
    expect(await h.marks.standing(SUSPENDED)).toBe("unknown");
    expect(h.fetches).toEqual([]);
  });

  it("reads suspended and not from the checked answer", async () => {
    const h = harness();
    expect(await h.marks.standing(SUSPENDED)).toBe("suspended");
    expect(await h.marks.standing(FREE)).toBe("not");
  });

  it("reuses an answer per prefix until its reuse time, then asks again", async () => {
    const h = harness();
    await h.marks.standing(SUSPENDED);
    await h.marks.standing(SUSPENDED);
    expect(h.fetches).toEqual(["0001"]);
    h.clock.now += 5 * 60_000 - 1;
    await h.marks.standing(SUSPENDED);
    expect(h.fetches).toHaveLength(1);
    h.clock.now += 1;
    await h.marks.standing(SUSPENDED);
    expect(h.fetches).toHaveLength(2);
  });

  it("keeps an answer whose reuse time already passed for the minimum", async () => {
    const h = harness();
    h.body.value = JSON.stringify({ suspended: [], reuseUntilMs: h.clock.now - 1 });
    await h.marks.standing(FREE);
    h.clock.now += MIN_REUSE_MS - 1;
    await h.marks.standing(FREE);
    expect(h.fetches).toHaveLength(1);
    h.clock.now += 1;
    await h.marks.standing(FREE);
    expect(h.fetches).toHaveLength(2);
  });

  it("sends one request per prefix while it is out", async () => {
    const h = harness({ gate: true });
    const a = h.marks.standing(SUSPENDED);
    const b = h.marks.standing(SUSPENDED);
    await new Promise((r) => setTimeout(r, 0));
    expect(h.fetches).toEqual(["0001"]);
    h.release();
    expect(await a).toBe("suspended");
    expect(await b).toBe("suspended");
    expect(h.fetches).toHaveLength(1);
  });

  it("does not ask a failed prefix again for ten minutes", async () => {
    const h = harness();
    h.body.value = new Error("network");
    expect(await h.marks.standing(SUSPENDED)).toBe("unknown");
    h.body.value = answer([1], h.clock.now + 60 * 60_000);
    h.clock.now += FAILURE_BACKOFF_MS - 1;
    expect(await h.marks.standing(SUSPENDED)).toBe("unknown");
    expect(h.fetches).toHaveLength(1);
    h.clock.now += 1;
    expect(await h.marks.standing(SUSPENDED)).toBe("suspended");
    expect(h.fetches).toHaveLength(2);
  });

  it("treats an answer that does not check as a failure, not a cache entry", async () => {
    const h = harness();
    h.body.value = JSON.stringify({ suspended: [1], reuseUntilMs: h.clock.now + 60_000, bad: true });
    expect(await h.marks.standing(SUSPENDED)).toBe("unknown");
    expect(await h.marks.standing(SUSPENDED)).toBe("unknown");
    expect(h.fetches).toHaveLength(1);
  });

  it("is unknown for a key the core refuses, with no request", async () => {
    const h = harness();
    expect(await h.marks.standing(new Uint8Array(5))).toBe("unknown");
    expect(h.fetches).toEqual([]);
  });

  it("is due again when a suspension's end comes before the reuse time", async () => {
    const h = harness();
    h.body.value = answer([1], h.clock.now + 10 * 60_000, { untilMs: h.clock.now + 90_000 });
    const r = await h.marks.check(SUSPENDED);
    expect(r.standing).toBe("suspended");
    expect(r.recheckAtMs).toBe(h.clock.now + 90_000);
    h.clock.now += 90_000;
    expect(await h.marks.standing(SUSPENDED)).toBe("not");
  });

  it("is unknown on any error of the REAL core (an answer that is no snapshot)", async () => {
    const h = harness({ core: wasm as unknown as StandingWasm });
    h.body.value = JSON.stringify({ period: 1, prefix: "0000" });
    expect(await h.marks.standing(SUSPENDED)).toBe("unknown");
    // The real core named the bucket the request went to.
    expect(h.fetches).toEqual([wasm.moderationBucketPrefix(SUSPENDED)]);
    expect(h.fetches[0]).toMatch(/^[0-9a-f]{4}$/);
  });
});

describe("the reactive read", () => {
  it("marks a suspended key once its standing is in, never this account's own", async () => {
    const h = harness();
    installSuspensionMarks(h.marks);
    const k = base64Encode(SUSPENDED);
    expect(isSuspendedKeyB64(k)).toBe(false);
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));
    expect(isSuspendedKeyB64(k)).toBe(true);
    expect(isSuspendedKeyB64(base64Encode(FREE))).toBe(false);
    installOwnAccountKeys(() => [k]);
    expect(isSuspendedKeyB64(k)).toBe(false);
  });

  it("asks nothing when unpinned", async () => {
    const h = harness({ logKey: null });
    installSuspensionMarks(h.marks);
    expect(isSuspendedKeyB64(base64Encode(SUSPENDED))).toBe(false);
    await new Promise((r) => setTimeout(r, 0));
    expect(h.fetches).toEqual([]);
  });

  it("waits for a request's standing only up to its bound", async () => {
    const h = harness({ gate: true });
    installSuspensionMarks(h.marks);
    expect(await standingWithin(base64Encode(SUSPENDED), 10)).toBe("unknown");
    h.release();
    expect(await standingWithin(base64Encode(SUSPENDED), 1_000)).toBe("suspended");
  });
});

describe("a batch of requests", () => {
  it("asks every sender at once under ONE bound, not one bound per request", async () => {
    const h = harness({ gate: true });
    installSuspensionMarks(h.marks);
    const keys = [1, 2, 3, 4, 5].map((b) => base64Encode(new Uint8Array(32).fill(b)));
    const started = Date.now();
    const out = await standingsWithin(keys, 50);
    const took = Date.now() - started;
    // Every prefix was asked before the bound, all at once.
    expect([...h.fetches].sort()).toEqual(["0001", "0002", "0003", "0004", "0005"]);
    // One bound for the batch: five sequential bounds would be 250 ms.
    expect(took).toBeLessThan(200);
    for (const k of keys) expect(out.get(k)).toBe("unknown");
    h.release();
  });

  it("reads each answered sender's standing, a late one as unknown", async () => {
    const h = harness();
    installSuspensionMarks(h.marks);
    const s = base64Encode(SUSPENDED);
    const f = base64Encode(FREE);
    const out = await standingsWithin([s, f, s], 1_000);
    expect(out.get(s)).toBe("suspended");
    expect(out.get(f)).toBe("not");
    expect(h.fetches.sort()).toEqual(["0001", "0002"]);
  });

  it("asks nothing for this account's own key or when unpinned", async () => {
    const own = base64Encode(SUSPENDED);
    installOwnAccountKeys(() => [own]);
    const h = harness();
    installSuspensionMarks(h.marks);
    expect((await standingsWithin([own], 1_000)).get(own)).toBe("unknown");
    expect(h.fetches).toEqual([]);
    const u = harness({ logKey: null });
    installSuspensionMarks(u.marks);
    expect((await standingsWithin([base64Encode(FREE)], 1_000)).get(base64Encode(FREE))).toBe("unknown");
    expect(u.fetches).toEqual([]);
  });
});

describe("a held link-answer request, decided later", () => {
  it("is dropped only when its sender is now verified suspended", async () => {
    const h = harness();
    installSuspensionMarks(h.marks);
    const s = base64Encode(SUSPENDED);
    const f = base64Encode(FREE);
    const out = await suspendedAmong([s, f], 1_000);
    expect([...out]).toEqual([s]);
  });

  it("proceeds when the check times out or cannot be read", async () => {
    const slow = harness({ gate: true });
    installSuspensionMarks(slow.marks);
    expect((await suspendedAmong([base64Encode(SUSPENDED)], 20)).size).toBe(0);
    slow.release();
    const broken = harness();
    broken.body.value = new Error("network");
    installSuspensionMarks(broken.marks);
    expect((await suspendedAmong([base64Encode(SUSPENDED)], 1_000)).size).toBe(0);
  });

  it("catches a suspension written after the request arrived", async () => {
    const h = harness();
    installSuspensionMarks(h.marks);
    const k = base64Encode(SUSPENDED);
    // At arrival the snapshot named nobody.
    h.body.value = JSON.stringify({ suspended: [], reuseUntilMs: h.clock.now + 60_000 });
    expect((await standingsWithin([k], 1_000)).get(k)).toBe("not");
    // A later snapshot names them; the decision asks again once it is due.
    h.clock.now += 60_000;
    h.body.value = answer([1], h.clock.now + 60_000);
    expect([...(await suspendedAmong([k], 1_000))]).toEqual([k]);
  });
});

describe("a suspension needs a moderator's signature (F2)", () => {
  it("acts only once the entry and the moderator list prove it, at the answer head's size", async () => {
    const h = harness();
    const core = fakeWasm();
    const p = harness({ core });
    expect(await p.marks.standing(SUSPENDED)).toBe("suspended");
    expect(p.entries).toEqual(["3/9"]);
    expect(core.proofs).toEqual([ENTRY]);
    // A key the snapshot does not name needs no proof.
    expect(await h.marks.standing(FREE)).toBe("not");
    expect(h.entries).toEqual([]);
  });

  it("keeps the proof with the answer, and proves again for a new answer", async () => {
    const h = harness();
    await h.marks.standing(SUSPENDED);
    await h.marks.standing(SUSPENDED);
    expect(h.entries).toHaveLength(1);
    h.clock.now += 5 * 60_000;
    h.body.value = answer([1], h.clock.now + 5 * 60_000);
    expect(await h.marks.standing(SUSPENDED)).toBe("suspended");
    expect(h.fetches).toHaveLength(2);
    expect(h.entries).toHaveLength(2);
  });

  it("is unknown when no moderator root key is pinned, with no entry asked", async () => {
    const h = harness({ rootKey: null });
    expect(await h.marks.standing(SUSPENDED)).toBe("unknown");
    expect(h.entries).toEqual([]);
  });

  it("is unknown when the bucket names no log entry", async () => {
    const h = harness();
    h.body.value = answer([1], h.clock.now + 60_000, { noIndex: true });
    expect(await h.marks.standing(SUSPENDED)).toBe("unknown");
    expect(h.entries).toEqual([]);
  });

  it("is unknown when the answer carries no head size", async () => {
    const h = harness();
    h.body.value = JSON.stringify({ suspended: [1], reuseUntilMs: h.clock.now + 60_000 });
    expect(await h.marks.standing(SUSPENDED)).toBe("unknown");
    expect(h.entries).toEqual([]);
  });

  it("is unknown when no listed moderator signed it", async () => {
    const h = harness();
    h.body.value = answer([1], h.clock.now + 60_000, { unsigned: true });
    expect(await h.marks.standing(SUSPENDED)).toBe("unknown");
  });

  it("is unknown when the entry or the list cannot be had, and a failed proof is kept with the answer", async () => {
    const noEntry = harness();
    noEntry.entry.value = new Error("network");
    expect(await noEntry.marks.standing(SUSPENDED)).toBe("unknown");
    noEntry.entry.value = ENTRY;
    expect(await noEntry.marks.standing(SUSPENDED)).toBe("unknown");
    expect(noEntry.entries).toHaveLength(1);
    noEntry.clock.now += 5 * 60_000;
    expect(await noEntry.marks.standing(SUSPENDED)).toBe("suspended");

    const noList = harness();
    noList.list.value = null;
    expect(await noList.marks.standing(SUSPENDED)).toBe("unknown");
    const brokenList = harness();
    brokenList.list.value = new Error("network");
    expect(await brokenList.marks.standing(SUSPENDED)).toBe("unknown");
    const wrongEntry = harness();
    wrongEntry.entry.value = "{}";
    expect(await wrongEntry.marks.standing(SUSPENDED)).toBe("unknown");
  });

  it("is unknown on a core without the proof", async () => {
    const core = fakeWasm() as Partial<ReturnType<typeof fakeWasm>>;
    delete core.moderationProveAccountStanding;
    const h = harness({ core: core as StandingWasm });
    expect(await h.marks.standing(SUSPENDED)).toBe("unknown");
  });

  it("the REAL core refuses an entry that proves nothing", () => {
    expect(() =>
      wasm.moderationProveAccountStanding(
        answer([1], 0),
        ENTRY,
        new Uint8Array([9]),
        ROOT_KEY,
        SUSPENDED,
        1_000_000n,
      ),
    ).toThrow(/^[a-z-]+:/);
  });

  it("an unproven suspension marks nothing, drops nothing and hides nothing at any site", async () => {
    const h = harness();
    h.body.value = answer([1], h.clock.now + 60 * 60_000, { unsigned: true });
    installSuspensionMarks(h.marks);
    const k = base64Encode(SUSPENDED);
    expect(isSuspendedKeyB64(k)).toBe(false);
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));
    // Banner, card line, member marker, search filter, held requests.
    expect(isSuspendedKeyB64(k)).toBe(false);
    // Contact-request drop and invite Add refusal.
    const standing = await standingWithin(k, 1_000);
    expect(standing).toBe("unknown");
    expect(dropsRequestFrom(standing)).toBe(false);
    expect(inviteOffersAdd(false, isSuspendedKeyB64(k))).toBe(true);
    // A batch of requests and a held link-answer request.
    expect((await standingsWithin([k], 1_000)).get(k)).toBe("unknown");
    expect((await suspendedAmong([k], 1_000)).size).toBe(0);
    expect(withoutSuspended([{ keyB64: k }], isSuspendedKeyB64)).toHaveLength(1);
  });

  it("a proven suspension acts at every site", async () => {
    const h = harness();
    installSuspensionMarks(h.marks);
    const k = base64Encode(SUSPENDED);
    expect(await standingWithin(k, 1_000)).toBe("suspended");
    expect(dropsRequestFrom(await standingWithin(k, 1_000))).toBe(true);
    isSuspendedKeyB64(k);
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));
    expect(isSuspendedKeyB64(k)).toBe(true);
    expect(inviteOffersAdd(false, isSuspendedKeyB64(k))).toBe(false);
    expect(withoutSuspended([{ keyB64: k }], isSuspendedKeyB64)).toHaveLength(0);
    expect([...(await suspendedAmong([k], 1_000))]).toEqual([k]);
  });

  it("holds the moderator list in memory, one request at a time", async () => {
    forgetModeratorList();
    let asked = 0;
    const fetchList = async (): Promise<Uint8Array | null> => {
      asked += 1;
      return new Uint8Array([9]);
    };
    const [a, b] = await Promise.all([cachedModeratorList(fetchList, 0), cachedModeratorList(fetchList, 0)]);
    expect(a).toEqual(new Uint8Array([9]));
    expect(b).toEqual(new Uint8Array([9]));
    expect(asked).toBe(1);
    await cachedModeratorList(fetchList, MODERATOR_LIST_CACHE_MS - 1);
    expect(asked).toBe(1);
    await cachedModeratorList(fetchList, MODERATOR_LIST_CACHE_MS);
    expect(asked).toBe(2);
    forgetModeratorList();
    rememberModeratorList(new Uint8Array([4]), 0);
    expect(await cachedModeratorList(fetchList, 1)).toEqual(new Uint8Array([4]));
    expect(asked).toBe(2);
    forgetModeratorList();
  });

  it("reads the head size of an answer", () => {
    expect(answerLogSize(answer([], 0))).toBe(9n);
    expect(answerLogSize("{}")).toBeNull();
    expect(answerLogSize("nope")).toBeNull();
    expect(answerLogSize(JSON.stringify({ root: { log_size: -1 } }))).toBeNull();
  });

  it("asks the log entry at the index and the head size, anonymously", async () => {
    const seen: string[] = [];
    const original = globalThis.fetch;
    globalThis.fetch = (async (url: string) => {
      seen.push(url);
      return new Response(ENTRY, { status: 200 });
    }) as typeof fetch;
    try {
      expect(await fetchLogEntry(3n, 9n, "/m")).toBe(ENTRY);
      expect(seen).toEqual(["/m/moderation/log/entry/3?size=9"]);
      await expect(fetchLogEntry(9n, 9n, "/m")).rejects.toThrow();
    } finally {
      globalThis.fetch = original;
    }
  });
});

describe("the mark sites' decisions", () => {
  it("a block wins over a suspension on the banner and the card", () => {
    expect(personMark(true, true)).toBe("blocked");
    expect(personMark(true, false)).toBe("blocked");
    expect(personMark(false, true)).toBe("suspended");
    expect(personMark(false, false)).toBeNull();
  });

  it("drops a request only on a verified suspension", () => {
    expect(dropsRequestFrom("suspended")).toBe(true);
    expect(dropsRequestFrom("unknown")).toBe(false);
    expect(dropsRequestFrom("not")).toBe(false);
  });

  it("search keeps every row but the suspended ones, in order", () => {
    const rows = [{ keyB64: "a" }, { keyB64: "s" }, { keyB64: "b" }];
    expect(withoutSuspended(rows, (k) => k === "s").map((r) => r.keyB64)).toEqual(["a", "b"]);
  });

  it("an invite from a suspended account offers no Add", () => {
    expect(inviteOffersAdd(false, false)).toBe(true);
    expect(inviteOffersAdd(false, true)).toBe(false);
    expect(inviteOffersAdd(true, false)).toBe(false);
  });
});
