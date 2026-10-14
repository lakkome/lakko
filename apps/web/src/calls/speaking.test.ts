// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `speaking.ts` over the real wasm: how this client reaches the shared
 * speaking rule and what it reads back.
 *
 * The rule's own cases (the noise floor, the ring's timing, the floor's
 * onset) are tested where the rule is written, in Rust
 * (`crates/lakko-ui-rules/src/speaking.rs`). What is here is the boundary:
 * that the snapshot is the tracker's answers and nothing else, and the
 * places where the shared rule answers differently from the TypeScript copy
 * it replaced, so a client that still expected the old answer fails here.
 */
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

import { readSpeech, sharedSpeakingRules, type SpeakingRule } from "./speaking";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const made: SpeakingRule[] = [];
function tracker(): SpeakingRule {
  const t = sharedSpeakingRules.create();
  made.push(t);
  return t;
}
afterEach(() => {
  for (const t of made.splice(0)) t.free();
});

const loud = (): number => wasm.speakingLevel() * 5;

/** Feed `id` one level every 100 ms over `[from, to]`. */
function feed(t: SpeakingRule, id: string, level: number | null, from: number, to: number): void {
  for (let now = from; now <= to; now += 100) t.sample(id, level, now);
}

describe("the snapshot", () => {
  it("is empty for a call nobody has been heard in", () => {
    const snap = readSpeech(tracker());
    expect([...snap.speakingIds]).toEqual([]);
    expect(snap.activeSpeakerId).toBeNull();
    expect(snap.lastSpokeAtMs).toEqual({});
  });

  it("carries the rings, the floor and when each person last held it", () => {
    const t = tracker();
    const onset = wasm.speakingFloorOnsetMs();
    feed(t, "b", loud(), 0, onset + 500);
    feed(t, "a", loud(), 0, 500);
    expect(t.decideFloor(["a", "b"], onset + 500)).toBe("b");
    const snap = readSpeech(t);
    expect(snap.speakingIds.has("a")).toBe(true);
    expect(snap.speakingIds.has("b")).toBe(true);
    expect(snap.activeSpeakerId).toBe("b");
    expect(snap.lastSpokeAtMs).toEqual({ b: onset + 500 });
    expect(t.lastHeldFloorMs("b")).toBe(onset + 500);
    expect(t.lastHeldFloorMs("a")).toBeUndefined();
  });

  it("pairs every floor holder with their own time", () => {
    const t = tracker();
    const onset = wasm.speakingFloorOnsetMs();
    const off = wasm.speakingRingOffMs();
    // z holds the floor first, then a: the lists are sorted by id, so the
    // order of holding is not the order of the lists.
    feed(t, "z", loud(), 0, onset + 300);
    t.decideFloor(["a", "z"], onset + 300);
    const zAt = onset + 300;
    const aFrom = zAt + 100;
    feed(t, "z", 0, aFrom, aFrom + off + 200);
    feed(t, "a", loud(), aFrom, aFrom + onset + 300);
    const aAt = aFrom + onset + 300;
    expect(t.decideFloor(["a", "z"], aAt)).toBe("a");
    expect(t.floorHolders()).toEqual(["a", "z"]);
    expect(readSpeech(t).lastSpokeAtMs).toEqual({ a: aAt, z: zAt });
  });
});

describe("what the shared rule answers that the copy it replaced did not", () => {
  it("lists the people speaking sorted by id, not in the order they were heard", () => {
    const t = tracker();
    feed(t, "z", loud(), 0, 500);
    feed(t, "m", loud(), 0, 500);
    feed(t, "a", loud(), 0, 500);
    expect(t.speaking()).toEqual(["a", "m", "z"]);
  });

  it("does not let two readings at one instant pull the noise floor down twice", () => {
    const once = tracker();
    const twice = tracker();
    once.sample("a", 0.001, 1_000);
    twice.sample("a", 0.001, 1_000);
    twice.sample("a", 0.001, 1_000);
    expect(twice.threshold("a")).toBe(once.threshold("a"));
    // And it did come down from where a person starts.
    expect(once.threshold("a")).toBeLessThan(wasm.speakingLevel());
  });

  it("gives the floor up at once when its holder is muted, before the next decision", () => {
    const t = tracker();
    const onset = wasm.speakingFloorOnsetMs();
    feed(t, "b", loud(), 0, onset + 500);
    expect(t.decideFloor(["b"], onset + 500)).toBe("b");
    t.silence("b");
    expect(t.floor()).toBeUndefined();
    expect(t.isSpeaking("b")).toBe(false);
    // Muted, not gone: the large tile still has them to fall back on.
    expect(readSpeech(t).lastSpokeAtMs).toEqual({ b: onset + 500 });
  });

  it("gives the floor up at once when its holder leaves, with the memory of it", () => {
    const t = tracker();
    const onset = wasm.speakingFloorOnsetMs();
    feed(t, "b", loud(), 0, onset + 500);
    t.decideFloor(["b"], onset + 500);
    t.forget("b");
    expect(t.floor()).toBeUndefined();
    expect(readSpeech(t).lastSpokeAtMs).toEqual({});
  });

  it("reads a level above full scale as full scale, and a reading that is no number as silence", () => {
    const t = tracker();
    feed(t, "a", 7, 0, 500);
    feed(t, "b", Number.NaN, 0, 500);
    feed(t, "c", null, 0, 500);
    expect(t.speaking()).toEqual(["a"]);
    expect(t.threshold("a")).toBeLessThanOrEqual(wasm.speakingLevelMax());
    // Nothing was learned from what could not be read.
    expect(t.threshold("c")).toBe(wasm.speakingLevel());
  });

  it("passes this device's own person over for the floor wherever they stand in the roster", () => {
    const t = tracker();
    const onset = wasm.speakingFloorOnsetMs();
    t.setLocal("me");
    feed(t, "me", loud(), 0, onset + 500);
    expect(t.decideFloor(["me", "b"], onset + 500)).toBeUndefined();
    expect(t.isSpeaking("me")).toBe(true);
    feed(t, "b", loud(), onset + 600, 2 * onset + 1_200);
    feed(t, "me", loud(), onset + 600, 2 * onset + 1_200);
    expect(t.decideFloor(["me", "b"], 2 * onset + 1_200)).toBe("b");
  });
});

describe("the numbers the rule is read with", () => {
  it("are the rule's own, and in the order the rule needs them", () => {
    expect(wasm.speakingLevelMin()).toBeLessThan(wasm.speakingLevel());
    expect(wasm.speakingLevel()).toBeLessThan(wasm.speakingLevelMax());
    expect(wasm.speakingRingOnMs()).toBeLessThan(wasm.speakingRingOffMs());
    expect(wasm.speakingRingOffMs()).toBeLessThan(wasm.speakingFloorOnsetMs());
    expect(tracker().threshold("nobody")).toBe(wasm.speakingLevel());
  });
});
