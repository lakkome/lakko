// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A kept grant is checked before it is offered
 * (`docs/ux/who-can-find-me.md` section 6.1). The match and the result are
 * the real `people.ts`; only the Delivery Service is stood in for.
 */
import { describe, expect, it, vi } from "vitest";

import { canAddContact, fallbackNormalize, localPeople, mergePeople, type LocalCandidate } from "./people";
import {
  candidatesMatching,
  firstLiveGrant,
  REACH_CHECKS_AT_ONCE,
  ReachVerdicts,
  verdictAfter,
  withVerdict,
} from "./reach-verdicts";

const cand = (over: Partial<LocalCandidate> = {}): LocalCandidate => ({
  keyB64: "KEY",
  keyHex: "aa",
  name: "Bob Lind",
  verifiedUsername: "boblind",
  isContact: false,
  accountIdHex: "11".repeat(16),
  keptGrantWires: ["STALE", "LIVE"],
  ...over,
});

const match = (candidates: readonly LocalCandidate[], query: string) =>
  localPeople(candidates, query, fallbackNormalize, null);

/** Let a check finish. */
async function settle(): Promise<void> {
  for (let i = 0; i < 10; i++) await Promise.resolve();
}

function verdicts(reachCheck: (keyB64: string, wire: string) => Promise<boolean>) {
  const onChange = vi.fn();
  return { store: new ReachVerdicts({ reachCheck, onChange }), onChange };
}

describe("firstLiveGrant", () => {
  it("gives the live grant after a stale one and asks no further", async () => {
    const check = vi.fn(async (wire: string) => wire === "LIVE");
    expect(await firstLiveGrant(["STALE", "LIVE", "NEVER"], check)).toEqual({ kind: "live", wireB64: "LIVE" });
    expect(check.mock.calls.map(([w]) => w)).toEqual(["STALE", "LIVE"]);
  });

  it("is dead when every grant is refused", async () => {
    expect(await firstLiveGrant(["A", "B"], async () => false)).toEqual({ kind: "dead" });
  });

  it("passes a failed check on: it says nothing about the grants after it", async () => {
    await expect(
      firstLiveGrant(["A"], async () => {
        throw new Error("offline");
      }),
    ).rejects.toThrow("offline");
  });
});

describe("withVerdict", () => {
  it("before an answer the person holds no grant: a name, no button, no username", () => {
    const [row] = match([withVerdict(cand(), null)], "bob");
    expect(row).toMatchObject({ name: "Bob Lind", reach: "none", username: null, grantWireB64: null });
    expect(canAddContact(row!)).toBe(false);
  });

  it("live: the grant is the one that passed, marked as checked", () => {
    const [row] = match([withVerdict(cand(), { kind: "live", wireB64: "LIVE" })], "bob");
    expect(row).toMatchObject({ reach: "grant", grantWireB64: "LIVE", grantChecked: true, username: "boblind" });
    expect(canAddContact(row!)).toBe(true);
  });

  it("live keeps the other grants: a verdict is about one moment", () => {
    expect(withVerdict(cand(), { kind: "live", wireB64: "LIVE" }).keptGrantWires).toEqual(["LIVE", "STALE"]);
  });

  it("dead: no button, no username, and matched by name only", () => {
    const dead = withVerdict(cand(), { kind: "dead" });
    expect(match([dead], "bob")[0]).toMatchObject({ reach: "none", username: null });
    expect(match([dead], "boblind")).toEqual([]);
  });

  it("unanswered: the row is what it was, with the first grant", () => {
    const c = cand();
    expect(withVerdict(c, { kind: "unanswered" })).toBe(c);
    expect(match([c], "boblind")[0]).toMatchObject({ reach: "grant", grantWireB64: "STALE" });
  });

  it("leaves a contact alone", () => {
    const c = cand({ isContact: true });
    expect(withVerdict(c, null)).toBe(c);
    expect(withVerdict(c, { kind: "dead" })).toBe(c);
  });
});

describe("candidatesMatching", () => {
  it("includes a person matched by a username the row does not show yet", () => {
    const candidates = [cand(), cand({ keyHex: "bb", name: "Ann", verifiedUsername: null })];
    expect(candidatesMatching(candidates, "boblind", fallbackNormalize, null).map((c) => c.keyHex)).toEqual(["aa"]);
  });
});

describe("ReachVerdicts", () => {
  it("records a live verdict, with the wire that passed", async () => {
    const { store, onChange } = verdicts(async (_key, wire) => wire === "LIVE");
    const c = cand();
    store.ask([c], "bob");
    await settle();
    expect(store.verdictOf(c)).toEqual({ kind: "live", wireB64: "LIVE" });
    expect(onChange).toHaveBeenCalledTimes(1);
    expect(store.apply([c])[0]?.keptGrantWires).toEqual(["LIVE", "STALE"]);
  });

  it("records dead when all are refused", async () => {
    const { store } = verdicts(async () => false);
    const c = cand();
    store.ask([c], "bob");
    await settle();
    expect(store.verdictOf(c)).toEqual({ kind: "dead" });
    expect(store.apply([c])[0]?.keptGrantWires).toEqual([]);
  });

  it("records no verdict on a throw, and the next ask checks again", async () => {
    const check = vi.fn(async () => {
      throw new Error("offline");
    });
    const { store } = verdicts(check);
    const c = cand();
    store.ask([c], "bob");
    await settle();
    expect(store.verdictOf(c)).toEqual({ kind: "unanswered" });
    expect(store.apply([c])[0]).toBe(c);
    store.ask([c], "bob");
    await settle();
    expect(check).toHaveBeenCalledTimes(2);
  });

  it("does not ask again for the same text once there is a verdict", async () => {
    const check = vi.fn(async () => false);
    const { store } = verdicts(check);
    const c = cand();
    store.ask([c], "bob");
    await settle();
    store.ask([c], "bob");
    await settle();
    expect(check).toHaveBeenCalledTimes(2); // the two grants, once each
  });

  it("a check answers for one text: the next text asks again, live and dead alike", async () => {
    let reachable = true;
    const check = vi.fn(async () => reachable);
    const { store } = verdicts(check);
    const live = cand({ keptGrantWires: ["ONE"] });
    store.ask([live], "bo");
    await settle();
    expect(store.verdictOf(live)).toEqual({ kind: "live", wireB64: "ONE" });

    // They left the group: the same session, a new text.
    reachable = false;
    store.ask([live], "bob");
    await settle();
    expect(store.verdictOf(live)).toEqual({ kind: "dead" });

    // They came back: a dead verdict is asked about too.
    reachable = true;
    store.ask([live], "bobl");
    await settle();
    expect(store.verdictOf(live)).toEqual({ kind: "live", wireB64: "ONE" });
  });

  it("the last answer stays on screen until the new one arrives", async () => {
    let answer: (ok: boolean) => void = () => {};
    const check = vi.fn(async () => true);
    const { store } = verdicts(check);
    const c = cand({ keptGrantWires: ["ONE"] });
    store.ask([c], "bo");
    await settle();
    check.mockImplementation(() => new Promise<boolean>((resolve) => (answer = resolve)));
    store.ask([c], "bob");
    expect(store.apply([c])[0]).toMatchObject({ keptGrantWires: ["ONE"], grantChecked: true });
    answer(false);
    await settle();
    expect(store.apply([c])[0]?.keptGrantWires).toEqual([]);
  });

  it("a check that could not be made does not replace a verdict held", async () => {
    let offline = false;
    const check = vi.fn(async () => {
      if (offline) throw new Error("offline");
      return false;
    });
    const { store } = verdicts(check);
    const c = cand({ keptGrantWires: ["ONE"] });
    store.ask([c], "bo");
    await settle();
    expect(store.verdictOf(c)).toEqual({ kind: "dead" });
    offline = true;
    store.ask([c], "bob");
    await settle();
    expect(store.verdictOf(c)).toEqual({ kind: "dead" });
  });

  it("verdictAfter: unanswered changes nothing held, and any answer replaces", () => {
    const live = { kind: "live", wireB64: "A" } as const;
    expect(verdictAfter(live, { kind: "unanswered" })).toBe(live);
    expect(verdictAfter({ kind: "dead" }, { kind: "unanswered" })).toEqual({ kind: "dead" });
    expect(verdictAfter(null, { kind: "unanswered" })).toEqual({ kind: "unanswered" });
    expect(verdictAfter(live, { kind: "dead" })).toEqual({ kind: "dead" });
  });

  it("checks a few people at a time and gets to everyone", async () => {
    const answers: ((ok: boolean) => void)[] = [];
    const check = vi.fn(() => new Promise<boolean>((resolve) => answers.push(resolve)));
    const { store } = verdicts(check);
    const many = Array.from({ length: REACH_CHECKS_AT_ONCE + 2 }, (_, i) =>
      cand({ keyHex: `k${i}`, keptGrantWires: ["ONE"] }),
    );
    store.ask(many, "b");
    expect(check).toHaveBeenCalledTimes(REACH_CHECKS_AT_ONCE);
    while (answers.length > 0) {
      answers.shift()!(true);
      await settle();
    }
    expect(check).toHaveBeenCalledTimes(many.length);
  });

  it("clear forgets what was held: the next text starts from no answer", async () => {
    const { store } = verdicts(async () => true);
    const c = cand({ keptGrantWires: ["ONE"] });
    store.ask([c], "bo");
    await settle();
    store.clear();
    expect(store.verdictOf(c)).toBeNull();
  });

  it("runs no concurrent duplicate for one person", async () => {
    let release: (ok: boolean) => void = () => {};
    const check = vi.fn(() => new Promise<boolean>((resolve) => (release = resolve)));
    const { store } = verdicts(check);
    const c = cand({ keptGrantWires: ["ONE"] });
    store.ask([c], "bo");
    store.ask([c], "bob");
    expect(check).toHaveBeenCalledTimes(1);
    release(true);
    await settle();
  });

  it("never checks a contact or a person with no grant", async () => {
    const check = vi.fn(async () => true);
    const { store } = verdicts(check);
    store.ask([cand({ isContact: true }), cand({ keyHex: "bb", keptGrantWires: [] })], "bob");
    await settle();
    expect(check).not.toHaveBeenCalled();
  });

  it("a new grant voids the verdict: the person is asked about again", async () => {
    const check = vi.fn(async (_key: string, wire: string) => wire === "NEW");
    const { store } = verdicts(check);
    const before = cand({ keptGrantWires: ["OLD"] });
    store.ask([before], "bob");
    await settle();
    expect(store.verdictOf(before)).toEqual({ kind: "dead" });

    const after = cand({ keptGrantWires: ["OLD", "NEW"] });
    expect(store.verdictOf(after)).toBeNull();
    store.ask([after], "bob");
    await settle();
    expect(store.verdictOf(after)).toEqual({ kind: "live", wireB64: "NEW" });
  });

  it("a send refused by the reach check records dead", async () => {
    const { store, onChange } = verdicts(async () => true);
    const c = cand();
    store.ask([c], "bob");
    await settle();
    store.refused(c.keyHex);
    expect(store.verdictOf(c)).toEqual({ kind: "dead" });
    expect(onChange).toHaveBeenCalledTimes(2);
  });

  it("a stale local grant does not win the merge over the live one the network checked", async () => {
    const { store } = verdicts(async () => {
      throw new Error("offline");
    });
    const c = cand({ keptGrantWires: ["STALE"] });
    store.ask([c], "bob");
    await settle();
    const [local] = match(store.apply([c]), "bob");
    const merged = mergePeople(
      [[local!], [{ ...local!, grantWireB64: "LIVE", grantChecked: true }]],
      null,
    );
    expect(merged[0]?.grantWireB64).toBe("LIVE");
  });
});
