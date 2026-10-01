// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { FORK_GRACE_MS, forkVerdict, type ForkVerdict } from "./fork-verdict";

const T0 = 1_000_000;

describe("forkVerdict", () => {
  it("is in sync when the DS names no epoch", () => {
    expect(forkVerdict(3n, true, undefined, undefined, T0)).toEqual({ kind: "in-sync" });
  });

  it("is in sync when the DS is not ahead", () => {
    expect(forkVerdict(3n, true, 3n, undefined, T0)).toEqual({ kind: "in-sync" });
    expect(forkVerdict(3n, true, 2n, undefined, T0)).toEqual({ kind: "in-sync" });
  });

  it("never counts a fetch with rows as drained", () => {
    // The caller passes drained = false whenever rows came back, applied or
    // not; the DS being ahead does not matter then.
    expect(forkVerdict(3n, false, 4n, undefined, T0)).toEqual({ kind: "in-sync" });
    expect(forkVerdict(3n, false, 4n, T0, T0 + FORK_GRACE_MS * 10)).toEqual({ kind: "in-sync" });
  });

  it("starts suspecting on the first drained poll behind the DS", () => {
    expect(forkVerdict(3n, true, 4n, undefined, T0)).toEqual({ kind: "suspect", since: T0 });
  });

  it("keeps the original start time inside the grace window", () => {
    expect(forkVerdict(3n, true, 4n, T0, T0 + FORK_GRACE_MS - 1)).toEqual({
      kind: "suspect",
      since: T0,
    });
  });

  it("says forked once the grace has run out", () => {
    expect(forkVerdict(3n, true, 4n, T0, T0 + FORK_GRACE_MS)).toEqual({ kind: "forked" });
    expect(forkVerdict(3n, true, 5n, T0, T0 + FORK_GRACE_MS * 3)).toEqual({ kind: "forked" });
  });

  it("clears the timer on any in-sync poll, so the grace starts over", () => {
    // Drive the verdict the way the poll loop does: the caller keeps `since`
    // from a suspect verdict and drops it on anything else.
    let since: number | undefined;
    const poll = (drained: boolean, latest: bigint | undefined, local: bigint, now: number): ForkVerdict => {
      const v = forkVerdict(local, drained, latest, since, now);
      since = v.kind === "suspect" ? v.since : undefined;
      return v;
    };
    expect(poll(true, 4n, 3n, T0).kind).toBe("suspect");
    // A sibling's commit arrived: rows came back.
    expect(poll(false, 4n, 3n, T0 + 60_000).kind).toBe("in-sync");
    expect(since).toBeUndefined();
    // Behind again: the window restarts from here, not from T0.
    expect(poll(true, 5n, 4n, T0 + 100_000)).toEqual({ kind: "suspect", since: T0 + 100_000 });
    expect(poll(true, 5n, 4n, T0 + 100_000 + FORK_GRACE_MS - 1).kind).toBe("suspect");
    // The epoch caught up.
    expect(poll(true, 5n, 5n, T0 + 100_000 + FORK_GRACE_MS).kind).toBe("in-sync");
    expect(since).toBeUndefined();
  });

  it("goes suspect then forked across the grace", () => {
    let since: number | undefined;
    const seen: string[] = [];
    for (let t = 0; t <= FORK_GRACE_MS; t += 25_000) {
      const v = forkVerdict(7n, true, 8n, since, T0 + t);
      since = v.kind === "suspect" ? v.since : undefined;
      seen.push(v.kind);
    }
    // 0, 25, 50, 75, 100 s: suspect; the next poll past 120 s is forked.
    expect(seen).toEqual(["suspect", "suspect", "suspect", "suspect", "suspect"]);
    expect(forkVerdict(7n, true, 8n, T0, T0 + 125_000)).toEqual({ kind: "forked" });
  });
});
