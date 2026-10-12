// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  FLOOR_ONSET_MS,
  NOISE_EVIDENCE_MIN,
  NoiseGate,
  RING_OFF_MS,
  RING_ON_MS,
  SPEAKING_LEVEL,
  SPEAKING_LEVEL_MAX,
  SPEAKING_LEVEL_MIN,
  SPEECH_OVER_NOISE,
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

describe("this device's own voice", () => {
  // Seen on a live five-person call: the large tile never moved to the
  // person speaking. The roster starts with this device's own person, and
  // its microphone (the person talking, or the room heard through it) took
  // the floor and kept everyone else from it.
  it("draws its ring and never holds the floor against someone else", () => {
    const t = new SpeakingTracker();
    const candidates = ["b", "c"];
    for (let now = 0; now <= 10_000; now += 100) {
      t.sample("me", LOUD, now);
      t.sample("b", 0, now);
      t.sample("c", LOUD, now);
    }
    const snap = t.snapshot(candidates, 10_000);
    expect(snap.speakingIds.has("me")).toBe(true);
    expect(snap.activeSpeakerId).toBe("c");
    expect(Object.keys(snap.lastSpokeAtMs)).toEqual(["c"]);
  });

  it("leaves the floor empty when only this device speaks", () => {
    const t = new SpeakingTracker();
    feed(t, "me", LOUD, 0, 5_000);
    expect(t.snapshot(["b"], 5_000).activeSpeakerId).toBeNull();
  });
});

describe("a muted person", () => {
  it("loses the ring at once and gives up the floor, whatever the level says", () => {
    const t = new SpeakingTracker();
    feed(t, "a", LOUD, 0, FLOOR_ONSET_MS + 500);
    expect(t.snapshot(ORDER, FLOOR_ONSET_MS + 500).activeSpeakerId).toBe("a");
    t.silence("a");
    const snap = t.snapshot(ORDER, FLOOR_ONSET_MS + 600);
    expect(snap.speakingIds.has("a")).toBe(false);
    expect(snap.activeSpeakerId).toBeNull();
    // They held the floor, so the stage still remembers them.
    expect(snap.lastSpokeAtMs.a).toBeGreaterThan(0);
  });

  it("has to earn the ring again after unmuting", () => {
    const t = new SpeakingTracker();
    feed(t, "a", LOUD, 0, 1_000);
    t.silence("a");
    t.sample("a", LOUD, 1_100);
    expect(t.snapshot(ORDER, 1_100).speakingIds.has("a")).toBe(false);
    t.sample("a", LOUD, 1_100 + RING_ON_MS);
    expect(t.snapshot(ORDER, 1_100 + RING_ON_MS).speakingIds.has("a")).toBe(true);
  });
});

describe("several people", () => {
  it("hands the floor to the next speaker only once the holder has stopped", () => {
    const t = new SpeakingTracker();
    const order = ["a", "b", "c"];
    let now = 0;
    const run = (levels: Record<string, number>, forMs: number): string | null => {
      const until = now + forMs;
      let active: string | null = null;
      for (; now <= until; now += 100) {
        for (const id of order) t.sample(id, levels[id] ?? 0, now);
        active = t.snapshot(order, now).activeSpeakerId;
      }
      return active;
    };
    expect(run({ b: LOUD }, 3_000)).toBe("b");
    // c joins in over b: the stage does not flip.
    expect(run({ b: LOUD, c: LOUD }, 3_000)).toBe("b");
    // b stops; c has been speaking long enough and takes it.
    expect(run({ c: LOUD }, RING_OFF_MS + 200)).toBe("c");
    // A short pause inside c's speech does not drop the floor.
    expect(run({}, RING_OFF_MS - 300)).toBe("c");
    expect(run({ c: LOUD }, 500)).toBe("c");
    // Everyone quiet: nobody holds it, and c is the most recent.
    expect(run({}, RING_OFF_MS + 200)).toBeNull();
    const last = t.snapshot(order, now).lastSpokeAtMs;
    expect(last.c).toBeGreaterThan(last.b ?? 0);
  });
});

describe("what counts as sound", () => {
  /** Feed one level every 100 ms for `ms`, starting at `from`. */
  function hold(gate: NoiseGate, level: number, from: number, ms: number): number {
    let now = from;
    for (; now < from + ms; now += 100) gate.update(level, now);
    return now;
  }

  it("starts at the fixed level before anything is known", () => {
    expect(new NoiseGate().threshold).toBeCloseTo(SPEAKING_LEVEL, 9);
    expect(new SpeakingTracker().thresholdOf("nobody")).toBe(SPEAKING_LEVEL);
  });

  it("follows a quiet room down, to a bound", () => {
    const gate = new NoiseGate();
    hold(gate, 0.002, 0, 1_000);
    expect(gate.threshold).toBeCloseTo(0.002 * SPEECH_OVER_NOISE, 9);
    hold(gate, 0.0002, 1_000, 2_000);
    expect(gate.threshold).toBe(SPEAKING_LEVEL_MIN);
  });

  it("does not empty the floor on one torn reading", () => {
    const gate = new NoiseGate();
    hold(gate, 0.006, 0, 1_000);
    const before = gate.threshold;
    gate.update(0.0002, 1_000);
    expect(gate.threshold).toBeCloseTo(before / 2, 9);
  });

  it("learns nothing from digital silence or from a reading that is not a number", () => {
    const gate = new NoiseGate();
    hold(gate, 0.01, 0, 20_000);
    const before = gate.threshold;
    hold(gate, 0, 20_000, 5_000);
    hold(gate, NOISE_EVIDENCE_MIN / 2, 25_000, 5_000);
    gate.update(Number.NaN, 30_000);
    expect(gate.threshold).toBe(before);
  });

  it("climbs slowly towards steady sound and never past the bound or the sound itself", () => {
    const gate = new NoiseGate();
    // Three seconds of speech barely move it.
    hold(gate, 0.2, 0, 3_000);
    expect(gate.threshold).toBeLessThan(SPEAKING_LEVEL * 1.4);
    // A minute of the same steady sound stops at the bound.
    hold(gate, 0.2, 3_000, 60_000);
    expect(gate.threshold).toBe(SPEAKING_LEVEL_MAX);
    // A steady hum settles the floor at the hum, not above it.
    const hum = new NoiseGate();
    hold(hum, 0.01, 0, 60_000);
    expect(hum.threshold).toBeCloseTo(0.01 * SPEECH_OVER_NOISE, 9);
  });

  it("does not take a long gap between readings for a long rise", () => {
    const gate = new NoiseGate();
    gate.update(0.2, 0);
    gate.update(0.2, 3_600_000);
    expect(gate.threshold).toBeLessThan(SPEAKING_LEVEL * 1.2);
  });
});

describe("one scale for a loud room and a quiet microphone", () => {
  /** Speech as a call delivers it: syllables around `level`, never steady. */
  function talk(t: SpeakingTracker, id: string, level: number, from: number, ms: number): number {
    let now = from;
    for (let i = 0; now < from + ms; now += 100, i += 1) {
      t.sample(id, level * (i % 3 === 0 ? 0.6 : 1), now);
    }
    return now;
  }

  it("draws the ring for speech at 0.03 over a room at 0.002, which the fixed level missed", () => {
    // The 2026-10-11 call: a phone whose speech peaked at 0.03 to 0.05.
    const t = new SpeakingTracker();
    let now = 0;
    for (; now < 3_000; now += 100) t.sample("a", 0.002, now);
    expect(t.snapshot(ORDER, now).speakingIds.has("a")).toBe(false);
    // 0.6 * 0.03 = 0.018 is under the old fixed 0.02 every third reading.
    now = talk(t, "a", 0.03, now, FLOOR_ONSET_MS + 500);
    const snap = t.snapshot(ORDER, now);
    expect(snap.speakingIds.has("a")).toBe(true);
    expect(snap.activeSpeakerId).toBe("a");
  });

  it("does not draw it for a room that idles at 0.015, and does for speech over that room", () => {
    const t = new SpeakingTracker();
    let now = 0;
    for (; now < 10_000; now += 100) {
      t.sample("a", 0.015, now);
      expect(t.snapshot(ORDER, now).speakingIds.has("a")).toBe(false);
    }
    now = talk(t, "a", 0.15, now, 1_000);
    expect(t.snapshot(ORDER, now).speakingIds.has("a")).toBe(true);
  });

  it("stops counting a steady noise that came on, within seconds", () => {
    const t = new SpeakingTracker();
    let now = 0;
    for (; now < 1_000; now += 100) t.sample("a", 0.03, now);
    expect(t.snapshot(ORDER, now).speakingIds.has("a")).toBe(true);
    for (; now < 7_000; now += 100) t.sample("a", 0.03, now);
    expect(t.snapshot(ORDER, now).speakingIds.has("a")).toBe(false);
  });

  it("keeps what it learned about the room across a mute", () => {
    const t = new SpeakingTracker();
    let now = 0;
    for (; now < 3_000; now += 100) t.sample("a", 0.002, now);
    const learned = t.thresholdOf("a");
    t.silence("a");
    expect(t.thresholdOf("a")).toBe(learned);
  });

  it("treats a level that could not be read as silence and learns nothing from it", () => {
    const t = new SpeakingTracker();
    feed(t, "a", LOUD, 0, 500);
    expect(t.snapshot(ORDER, 500).speakingIds.has("a")).toBe(true);
    const before = t.thresholdOf("a");
    for (let now = 600; now <= 600 + RING_OFF_MS; now += 100) t.sample("a", null, now);
    expect(t.snapshot(ORDER, 600 + RING_OFF_MS).speakingIds.has("a")).toBe(false);
    expect(t.thresholdOf("a")).toBe(before);
  });
});

describe("rmsLevel", () => {
  it("is zero for silence and for an empty block, and rises with the signal", () => {
    expect(rmsLevel(new Float32Array(0))).toBe(0);
    expect(rmsLevel(new Float32Array(64))).toBe(0);
    expect(rmsLevel(new Float32Array(64).fill(0.5))).toBeCloseTo(0.5);
  });
});
