// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The join-attribution read rule shared by the three clients: read only when
 * the pending joiner set differs from the set the map was fetched for, never
 * more than once per 10 s per group, and clear with no read when nothing is
 * pending. iOS read the whole knock queue some 45 times a minute before it.
 */
import { describe, expect, it } from "vitest";

import { JoinAttributionGate } from "./join-attribution-gate";

const T0 = 1_000_000;

describe("JoinAttributionGate", () => {
  it("an empty pending set clears with no read", () => {
    const gate = new JoinAttributionGate();
    expect(gate.decide("g", [], T0)).toEqual({ kind: "clear" });
  });

  it("reads once for a new set, then keeps it however often it is asked", () => {
    const gate = new JoinAttributionGate();
    expect(gate.decide("g", ["a"], T0)).toEqual({ kind: "fetch" });
    gate.fetched("g", ["a"]);
    // Every poll re-asks (the panel re-reads its pending set on each bump).
    for (let t = T0 + 1_000; t < T0 + 60_000; t += 1_000) {
      expect(gate.decide("g", ["a"], t).kind).toBe("keep");
    }
  });

  it("the set is compared as a set, not as an ordered list", () => {
    const gate = new JoinAttributionGate();
    gate.decide("g", ["a", "b"], T0);
    gate.fetched("g", ["a", "b"]);
    expect(gate.decide("g", ["b", "a", "a"], T0 + 20_000).kind).toBe("keep");
  });

  it("a changed set inside 10 s of the last read waits out the floor, then reads", () => {
    const gate = new JoinAttributionGate();
    gate.decide("g", ["a"], T0);
    gate.fetched("g", ["a"]);
    expect(gate.decide("g", ["a", "b"], T0 + 4_000)).toEqual({ kind: "wait", retryInMs: 6_000 });
    expect(gate.decide("g", ["a", "b"], T0 + 10_000)).toEqual({ kind: "fetch" });
  });

  it("the floor is per group", () => {
    const gate = new JoinAttributionGate();
    expect(gate.decide("g1", ["a"], T0).kind).toBe("fetch");
    expect(gate.decide("g2", ["a"], T0 + 1).kind).toBe("fetch");
  });

  it("a failed read is retried after the floor, never sooner", () => {
    const gate = new JoinAttributionGate();
    expect(gate.decide("g", ["a"], T0).kind).toBe("fetch");
    // no `fetched`: the read failed
    expect(gate.decide("g", ["a"], T0 + 2_000).kind).toBe("wait");
    expect(gate.decide("g", ["a"], T0 + 10_000).kind).toBe("fetch");
  });

  it("a remounted view re-reads, subject to the floor", () => {
    const gate = new JoinAttributionGate();
    gate.decide("g", ["a"], T0);
    gate.fetched("g", ["a"]);
    gate.forget("g");
    expect(gate.decide("g", ["a"], T0 + 3_000).kind).toBe("wait");
    expect(gate.decide("g", ["a"], T0 + 10_000).kind).toBe("fetch");
  });

  it("clearing and re-adding the same key reads again", () => {
    const gate = new JoinAttributionGate();
    gate.decide("g", ["a"], T0);
    gate.fetched("g", ["a"]);
    expect(gate.decide("g", [], T0 + 11_000).kind).toBe("clear");
    expect(gate.decide("g", ["a"], T0 + 22_000).kind).toBe("fetch");
  });
});
