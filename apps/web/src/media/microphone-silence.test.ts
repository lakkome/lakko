// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX spec §2 "Hearing nothing". The fold is four lines, so what these tests are
 * really pinning is the two constants and the three rules around them — the
 * warning arrives at two seconds and not before, anything heard clears it
 * outright, and the floor sits where a dead meter falls under it while one bit
 * of real signal does not.
 *
 * The floor's two sides are asserted as the arithmetic the doc comment claims,
 * not as literals, so a change to the constant that breaks the claim fails here
 * rather than quietly widening what counts as silence.
 */
import { describe, expect, it } from "vitest";

import {
  QUIET_WATCH,
  SILENCE_LEVEL_FLOOR,
  SILENCE_WARNING_MS,
  hasInputSound,
  observeLevel,
} from "./microphone-silence";

/** One least-significant bit of 16-bit PCM: the quietest non-silence. */
const ONE_PCM_BIT = 1 / 32767;
/** What an iOS meter reports for digital silence, on this same 0..1 scale. */
const IOS_DIGITAL_SILENCE = 1e-8;

describe("the floor", () => {
  it("counts the meter's own silence as silence on every platform", () => {
    expect(hasInputSound(0)).toBe(false);
    expect(hasInputSound(IOS_DIGITAL_SILENCE)).toBe(false);
  });

  it("counts one bit of real signal as sound", () => {
    expect(hasInputSound(ONE_PCM_BIT)).toBe(true);
  });

  it("sits in the gap the doc comment claims, two orders either side", () => {
    expect(SILENCE_LEVEL_FLOOR).toBeGreaterThan(IOS_DIGITAL_SILENCE * 50);
    expect(SILENCE_LEVEL_FLOOR).toBeLessThan(ONE_PCM_BIT / 10);
  });

  it("reads a nonsense measurement as silence, never as sound", () => {
    expect(hasInputSound(Number.NaN)).toBe(false);
    expect(hasInputSound(Number.NEGATIVE_INFINITY)).toBe(false);
    expect(hasInputSound(-1)).toBe(false);
  });
});

describe("the fold", () => {
  const silentRun = (fromMs: number, toMs: number, step = 100): boolean => {
    let w = QUIET_WATCH;
    for (let t = fromMs; t <= toMs; t += step) w = observeLevel(w, 0, t);
    return w.warns;
  };

  it("says nothing before two seconds and says so at exactly two", () => {
    expect(silentRun(0, SILENCE_WARNING_MS - 100)).toBe(false);
    expect(silentRun(0, SILENCE_WARNING_MS)).toBe(true);
  });

  it("says nothing on the very first silent sample", () => {
    expect(observeLevel(QUIET_WATCH, 0, 0).warns).toBe(false);
    expect(observeLevel(QUIET_WATCH, 0, 9_999).warns).toBe(false);
  });

  it("one audible sample clears a warning that had already arrived", () => {
    let w = QUIET_WATCH;
    for (let t = 0; t <= SILENCE_WARNING_MS; t += 100) w = observeLevel(w, 0, t);
    expect(w.warns).toBe(true);
    w = observeLevel(w, 0.4, SILENCE_WARNING_MS + 100);
    expect(w).toEqual(QUIET_WATCH);
  });

  it("times a later silence from when it began, not from the take's start", () => {
    // Sound until 5s, then silence: the warning is due at 7s, not at 2s.
    let w = QUIET_WATCH;
    for (let t = 0; t < 5_000; t += 100) w = observeLevel(w, 0.3, t);
    for (let t = 5_000; t < 7_000; t += 100) w = observeLevel(w, 0, t);
    expect(w.warns).toBe(false);
    expect(observeLevel(w, 0, 7_000).warns).toBe(true);
  });

  it("a clock that goes backwards delays the warning rather than raising one", () => {
    let w = observeLevel(QUIET_WATCH, 0, 10_000);
    w = observeLevel(w, 0, 0);
    expect(w.warns).toBe(false);
  });
});
