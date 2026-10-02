// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  FLOOR_ONSET_MS,
  RING_OFF_MS,
  RING_ON_MS,
  SPEAKING_LEVEL,
  SpeakingTracker,
  rmsLevel,
} from "./speaking";

const LOUD = SPEAKING_LEVEL * 5;
const ORDER = ["a", "b"];

/** Feed `id` one level every 100 ms over `[from, to]`. */
function feed(t: SpeakingTracker, id: string, level: number, from: number, to: number): void {
  for (let now = from; now <= to; now += 100) t.sample(id, level, now);
}

describe("the ring", () => {
  it("comes on after a short sound and goes off after a pause", () => {
    const t = new SpeakingTracker();
    t.sample("a", LOUD, 0);
    expect(t.snapshot(ORDER, 0).speakingIds.has("a")).toBe(false);
    t.sample("a", LOUD, RING_ON_MS);
    expect(t.snapshot(ORDER, RING_ON_MS).speakingIds.has("a")).toBe(true);
    t.sample("a", 0, RING_ON_MS + 100);
    expect(t.snapshot(ORDER, RING_ON_MS + 100).speakingIds.has("a")).toBe(true);
    t.sample("a", 0, RING_ON_MS + 100 + RING_OFF_MS);
    expect(t.snapshot(ORDER, RING_ON_MS + 100 + RING_OFF_MS).speakingIds.has("a")).toBe(false);
  });

  it("is on for everyone who is speaking at once", () => {
    const t = new SpeakingTracker();
    feed(t, "a", LOUD, 0, 500);
    feed(t, "b", LOUD, 0, 500);
    expect([...t.snapshot(ORDER, 500).speakingIds].sort()).toEqual(["a", "b"]);
  });
});

describe("the floor", () => {
  it("is not taken by a cough", () => {
    const t = new SpeakingTracker();
    feed(t, "a", LOUD, 0, 600);
    expect(t.snapshot(ORDER, 600).activeSpeakerId).toBeNull();
  });

  it("goes to someone who keeps speaking, and is remembered after they stop", () => {
    const t = new SpeakingTracker();
    feed(t, "a", LOUD, 0, FLOOR_ONSET_MS);
    const during = t.snapshot(ORDER, FLOOR_ONSET_MS);
    expect(during.activeSpeakerId).toBe("a");
    feed(t, "a", 0, FLOOR_ONSET_MS + 100, FLOOR_ONSET_MS + 100 + RING_OFF_MS);
    const after = t.snapshot(ORDER, FLOOR_ONSET_MS + 100 + RING_OFF_MS);
    expect(after.activeSpeakerId).toBeNull();
    expect(after.lastSpokeAtMs["a"]).toBe(FLOOR_ONSET_MS);
  });

  it("stays with whoever holds it while a second person talks over them", () => {
    const t = new SpeakingTracker();
    feed(t, "b", LOUD, 0, FLOOR_ONSET_MS);
    expect(t.snapshot(ORDER, FLOOR_ONSET_MS).activeSpeakerId).toBe("b");
    // `a` is earlier in the roster and now qualifies too; `b` keeps the floor.
    feed(t, "a", LOUD, FLOOR_ONSET_MS, 2 * FLOOR_ONSET_MS + 200);
    feed(t, "b", LOUD, FLOOR_ONSET_MS, 2 * FLOOR_ONSET_MS + 200);
    expect(t.snapshot(ORDER, 2 * FLOOR_ONSET_MS + 200).activeSpeakerId).toBe("b");
  });

  it("forgets someone who left the call", () => {
    const t = new SpeakingTracker();
    feed(t, "a", LOUD, 0, FLOOR_ONSET_MS);
    t.snapshot(ORDER, FLOOR_ONSET_MS);
    t.keepOnly(new Set(["b"]));
    const snap = t.snapshot(["b"], FLOOR_ONSET_MS + 100);
    expect(snap.activeSpeakerId).toBeNull();
    expect(snap.lastSpokeAtMs["a"]).toBeUndefined();
    expect(snap.speakingIds.size).toBe(0);
  });
});

describe("rmsLevel", () => {
  it("is zero for silence and for an empty block, and rises with the signal", () => {
    expect(rmsLevel(new Float32Array(0))).toBe(0);
    expect(rmsLevel(new Float32Array(64))).toBe(0);
    expect(rmsLevel(new Float32Array(64).fill(0.5))).toBeCloseTo(0.5);
  });
});
