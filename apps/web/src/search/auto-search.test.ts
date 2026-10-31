// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import {
  AutoSearch,
  MAX_REMEMBERED_SEARCHES,
  NETWORK_SEARCH_DEBOUNCE_MS,
  NETWORK_SEARCH_MIN_CHARS,
  SEARCH_ANSWER_KEPT_MS,
  SearchMemory,
  networkQueryOf,
  type NetworkQueryRules,
  type NetworkView,
  type Timers,
} from "./auto-search";
import { fallbackNormalize } from "./people";
import type { PeopleSearchOutcome } from "./people-search";

/** Usernames are `[a-z0-9_]{3,32}`; a name term is a word of 3+ letters. */
const rules: NetworkQueryRules = {
  canonicalUsername: (typed) => {
    const bare = typed.replace(/^@+/, "").toLowerCase();
    return /^[a-z0-9_]{3,32}$/.test(bare) ? bare : null;
  },
  queryTerm: (q) => q.split(/\s+/).find((w) => /^\p{L}{3,}$/u.test(w)) ?? null,
  normalize: fallbackNormalize,
};

const found = (name: string): PeopleSearchOutcome => ({
  people: [{ keyB64: name, keyHex: name, accountIdHex: null, name, username: null, reach: "open", grantWireB64: null }],
  channel: null,
  state: { kind: "found" },
});
const notFound: PeopleSearchOutcome = { people: [], channel: null, state: { kind: "not-found" } };
const failed: PeopleSearchOutcome = { people: [], channel: null, state: { kind: "failed" } };
const outOfSearches: PeopleSearchOutcome = { people: [], channel: null, state: { kind: "allowance-used" } };

/** A hand-driven clock. */
function fakeTimers(): Timers & { advance(ms: number): void; pending(): number } {
  let now = 0;
  let next = 1;
  const queue = new Map<number, { at: number; fn: () => void }>();
  return {
    set: (fn, ms) => {
      const id = next++;
      queue.set(id, { at: now + ms, fn });
      return id;
    },
    clear: (h) => {
      queue.delete(h as number);
    },
    advance(ms) {
      now += ms;
      for (const [id, t] of [...queue]) {
        if (t.at <= now) {
          queue.delete(id);
          t.fn();
        }
      }
    },
    pending: () => queue.size,
  };
}

function harness(answer: (query: string) => Promise<PeopleSearchOutcome>, memory = new SearchMemory()) {
  const timers = fakeTimers();
  const views: NetworkView[] = [];
  const run = vi.fn(answer);
  const auto = new AutoSearch({ run, memory, timers, onChange: (v) => views.push(v) });
  const type = (text: string): void => auto.input(networkQueryOf(text, rules));
  const last = (): NetworkView => views[views.length - 1] ?? { outcome: null, searching: false };
  return { auto, timers, run, type, last, memory };
}

/** Let the awaited answer land. */
const settle = async (): Promise<void> => {
  for (let i = 0; i < 5; i++) await Promise.resolve();
};

describe("networkQueryOf (UX 6.1: when it asks the network)", () => {
  it("asks nothing under three characters", () => {
    expect(NETWORK_SEARCH_MIN_CHARS).toBe(3);
    expect(networkQueryOf("an", rules)).toBeNull();
    expect(networkQueryOf("ann", rules)).not.toBeNull();
  });

  it("ignores a leading @ - for the count and for the text asked", () => {
    expect(networkQueryOf("@an", rules)).toBeNull();
    expect(networkQueryOf("@anna", rules)).toEqual({ query: "anna", key: "anna" });
  });

  it("asks nothing for a text that is neither a username nor has a 3-letter word", () => {
    expect(networkQueryOf("a b c d", rules)).toBeNull();
    expect(networkQueryOf("?!?", rules)).toBeNull();
  });

  it("treats case and repeated spaces as the same search", () => {
    expect(networkQueryOf("Anna  Weber", rules)?.key).toBe(networkQueryOf("anna weber", rules)?.key);
  });
});

describe("AutoSearch: waiting for the pause", () => {
  it("says a question is coming from the moment a new text is typed", async () => {
    const h = harness(async (q) => found(q));
    h.type("anna");
    expect(h.last().waiting).toBe(true);
    expect(h.last().searching).toBe(false);
    h.timers.advance(NETWORK_SEARCH_DEBOUNCE_MS);
    expect(h.last().waiting).toBeUndefined();
    expect(h.last().searching).toBe(true);
    await settle();
    expect(h.last().waiting).toBeUndefined();
  });

  it("does not wait on a text that asks nothing, or on a remembered answer", async () => {
    const h = harness(async (q) => found(q));
    h.type("an");
    expect(h.last().waiting).toBeUndefined();
    h.type("anna");
    h.timers.advance(NETWORK_SEARCH_DEBOUNCE_MS);
    await settle();
    h.type("an");
    h.type("anna");
    expect(h.last().waiting).toBeUndefined();
  });
});

describe("AutoSearch.recheck", () => {
  const isAnna = (o: PeopleSearchOutcome): boolean => o.people.some((p) => p.name === "anna");

  it("asks at once for the text standing when its answer is false", async () => {
    const h = harness(async (q) => found(q));
    h.type("anna");
    h.timers.advance(NETWORK_SEARCH_DEBOUNCE_MS);
    await settle();
    expect(h.run).toHaveBeenCalledTimes(1);
    h.auto.recheck(isAnna);
    await settle();
    expect(h.run).toHaveBeenCalledTimes(2);
    expect(h.last().outcome?.state.kind).toBe("found");
  });

  it("leaves a true answer and asks nothing", async () => {
    const h = harness(async (q) => found(q));
    h.type("anna");
    h.timers.advance(NETWORK_SEARCH_DEBOUNCE_MS);
    await settle();
    h.auto.recheck(() => false);
    await settle();
    expect(h.run).toHaveBeenCalledTimes(1);
  });

  it("forgets a false answer for a text not on screen", async () => {
    const h = harness(async (q) => found(q));
    h.type("anna");
    h.timers.advance(NETWORK_SEARCH_DEBOUNCE_MS);
    await settle();
    h.type("bert");
    h.timers.advance(NETWORK_SEARCH_DEBOUNCE_MS);
    await settle();
    h.auto.recheck(isAnna);
    await settle();
    expect(h.memory.get("anna")).toBeUndefined();
    expect(h.memory.get("bert")).toBeDefined();
    expect(h.run).toHaveBeenCalledTimes(2);
  });
});

describe("AutoSearch", () => {
  it("asks after 500 ms without typing, once, with the last text", async () => {
    expect(NETWORK_SEARCH_DEBOUNCE_MS).toBe(500);
    const h = harness(async (q) => found(q));
    h.type("ann");
    h.timers.advance(300);
    h.type("anna");
    h.timers.advance(499);
    expect(h.run).not.toHaveBeenCalled();
    h.timers.advance(1);
    expect(h.run).toHaveBeenCalledTimes(1);
    expect(h.run).toHaveBeenCalledWith("anna");
    expect(h.last().searching).toBe(true);
    await settle();
    expect(h.last()).toEqual({ outcome: found("anna"), searching: false });
  });

  it("never asks the network under three characters", () => {
    const h = harness(async (q) => found(q));
    h.type("a");
    h.type("an");
    h.timers.advance(5_000);
    expect(h.run).not.toHaveBeenCalled();
    expect(h.last()).toEqual({ outcome: null, searching: false });
  });

  it("does not ask again for a text already asked in this session", async () => {
    const h = harness(async (q) => found(q));
    h.type("anna");
    h.timers.advance(500);
    await settle();
    h.type("annab");
    h.type("anna");
    h.timers.advance(5_000);
    await settle();
    expect(h.run).toHaveBeenCalledTimes(1);
    // ...and the remembered answer shows at once, without the spinner.
    expect(h.last()).toEqual({ outcome: found("anna"), searching: false });
  });

  it("remembers across fields of one session (a shared memory)", async () => {
    const memory = new SearchMemory();
    const first = harness(async (q) => found(q), memory);
    first.type("anna");
    first.timers.advance(500);
    await settle();
    const second = harness(async (q) => found(q), memory);
    second.type("anna");
    second.timers.advance(500);
    expect(second.run).not.toHaveBeenCalled();
    expect(second.last().outcome).toEqual(found("anna"));
  });

  it("keeps the earlier results while the new answer is on its way", async () => {
    let release: ((o: PeopleSearchOutcome) => void) | null = null;
    const h = harness(
      (q) =>
        q === "ann"
          ? Promise.resolve(found("ann"))
          : new Promise((resolve) => {
              release = resolve;
            }),
    );
    h.type("ann");
    h.timers.advance(500);
    await settle();
    h.type("anna");
    expect(h.last().outcome).toEqual(found("ann"));
    h.timers.advance(500);
    expect(h.last()).toEqual({ outcome: found("ann"), searching: true });
    release!(found("anna"));
    await settle();
    expect(h.last()).toEqual({ outcome: found("anna"), searching: false });
  });

  it("hides the People part when the text drops under three characters", async () => {
    const h = harness(async (q) => found(q));
    h.type("anna");
    h.timers.advance(500);
    await settle();
    h.type("an");
    expect(h.last()).toEqual({ outcome: null, searching: false });
  });

  it("does not show an answer that arrives after the text changed", async () => {
    let release: ((o: PeopleSearchOutcome) => void) | null = null;
    const h = harness(
      (q) =>
        q === "anna"
          ? new Promise((resolve) => {
              release = resolve;
            })
          : Promise.resolve(found(q)),
    );
    h.type("anna");
    h.timers.advance(500);
    h.type("bob");
    release!(found("anna"));
    await settle();
    expect(h.last().outcome).toBeNull();
    // ...but it is remembered: typing "anna" again shows it without asking.
    h.type("anna");
    expect(h.last().outcome).toEqual(found("anna"));
    expect(h.run).toHaveBeenCalledTimes(1);
  });

  it("shows not found and remembers it", async () => {
    const h = harness(async () => notFound);
    h.type("nobody");
    h.timers.advance(500);
    await settle();
    expect(h.last().outcome?.state).toEqual({ kind: "not-found" });
    h.type("nobod");
    h.type("nobody");
    h.timers.advance(500);
    expect(h.run).toHaveBeenCalledTimes(1);
  });

  it("does not remember a network failure: typing again retries", async () => {
    const answers = [failed, found("anna")];
    const h = harness(async () => answers.shift() ?? notFound);
    h.type("anna");
    h.timers.advance(500);
    await settle();
    expect(h.last().outcome?.state).toEqual({ kind: "failed" });
    // The same search typed again (a trailing space) is the retry.
    h.type("anna ");
    h.timers.advance(500);
    await settle();
    expect(h.run).toHaveBeenCalledTimes(2);
    expect(h.last().outcome).toEqual(found("anna"));
  });

  it("does not remember out of searches either", async () => {
    const h = harness(async () => outOfSearches);
    h.type("anna");
    h.timers.advance(500);
    await settle();
    expect(h.last().outcome?.state).toEqual({ kind: "allowance-used" });
    expect(h.memory.get("anna")).toBeUndefined();
  });

  it("reads a thrown search as a network failure, never as not found", async () => {
    const h = harness(async () => {
      throw new Error("boom");
    });
    h.type("anna");
    h.timers.advance(500);
    await settle();
    expect(h.last().outcome?.state).toEqual({ kind: "failed" });
  });

  it("asks nothing after it is disposed", () => {
    const h = harness(async (q) => found(q));
    h.type("anna");
    h.auto.dispose();
    h.timers.advance(5_000);
    expect(h.run).not.toHaveBeenCalled();
  });
});

describe("a remembered answer ages (UX 6.1)", () => {
  it("is reused until SEARCH_ANSWER_KEPT_MS and asked again after", () => {
    let now = 0;
    const m = new SearchMemory(10, () => now);
    m.remember("a", found("a"));
    now = SEARCH_ANSWER_KEPT_MS - 1;
    expect(m.get("a")).toBeDefined();
    now = SEARCH_ANSWER_KEPT_MS;
    expect(m.get("a")).toBeUndefined();
  });

  it("a not-found ages the same way", () => {
    let now = 0;
    const m = new SearchMemory(10, () => now);
    m.remember("a", notFound);
    now = SEARCH_ANSWER_KEPT_MS;
    expect(m.get("a")).toBeUndefined();
  });

  it("the age is read when a text is asked: an old answer is asked again, and nothing asks by itself", async () => {
    let now = 0;
    const h = harness(async (q) => found(q), new SearchMemory(10, () => now));
    h.type("anna");
    h.timers.advance(NETWORK_SEARCH_DEBOUNCE_MS);
    await settle();
    expect(h.run).toHaveBeenCalledTimes(1);

    // Young: the same text again is answered from memory.
    h.type("anna ");
    h.timers.advance(NETWORK_SEARCH_DEBOUNCE_MS);
    await settle();
    expect(h.run).toHaveBeenCalledTimes(1);

    // Time passes with the text standing: nothing asks by itself.
    now = SEARCH_ANSWER_KEPT_MS * 2;
    h.timers.advance(SEARCH_ANSWER_KEPT_MS * 2);
    await settle();
    expect(h.run).toHaveBeenCalledTimes(1);

    // The next time the text is asked, the answer is too old.
    h.type("anna  ");
    h.timers.advance(NETWORK_SEARCH_DEBOUNCE_MS);
    await settle();
    expect(h.run).toHaveBeenCalledTimes(2);
  });
});

describe("SearchMemory", () => {
  it("forgets the oldest answer beyond its bound", () => {
    const m = new SearchMemory(2);
    m.remember("a", notFound);
    m.remember("b", notFound);
    m.remember("c", notFound);
    expect(m.get("a")).toBeUndefined();
    expect(m.get("c")).toEqual(notFound);
    expect(MAX_REMEMBERED_SEARCHES).toBeGreaterThan(0);
  });

  it("drops a remembered answer when the same text later fails", () => {
    const m = new SearchMemory();
    m.remember("a", found("a"));
    m.remember("a", failed);
    expect(m.get("a")).toBeUndefined();
  });
});
