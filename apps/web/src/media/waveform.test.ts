// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 §1 — the waveform the envelope carries is at most 128 bytes, and
 * the fold keeps a short loud peak rather than averaging it away.
 */
import { describe, expect, it } from "vitest";

import { MAX_WAVEFORM_BYTES, barIsPlayed, downsampleWaveform, rmsLevel, waveformBars } from "./waveform";

describe("downsampleWaveform", () => {
  it("never exceeds the envelope bound, whatever the take length", () => {
    for (const n of [1, 127, 128, 129, 1000, 18_000]) {
      const out = downsampleWaveform(new Array<number>(n).fill(0.3));
      expect(out).toBeDefined();
      expect(out!.length).toBeLessThanOrEqual(MAX_WAVEFORM_BYTES);
      expect(out!.length).toBe(Math.min(n, MAX_WAVEFORM_BYTES));
    }
  });

  it("an explicit larger bucket count is still capped", () => {
    expect(downsampleWaveform(new Array<number>(500).fill(1), 400)!.length).toBe(MAX_WAVEFORM_BYTES);
  });

  it("normalises the loudest bucket to 255", () => {
    const out = downsampleWaveform([0.1, 0.2, 0.4])!;
    expect(Array.from(out)).toEqual([64, 128, 255]);
  });

  it("keeps a single loud sample inside a long quiet bucket", () => {
    const levels = new Array<number>(1280).fill(0.01);
    levels[645] = 0.9;
    const out = downsampleWaveform(levels)!;
    expect(out[64]).toBe(255);
    expect(out[0]).toBeLessThan(10);
  });

  it("empty is absent, and so is an entirely silent take", () => {
    expect(downsampleWaveform([])).toBeUndefined();
    expect(downsampleWaveform([0, 0, 0])).toBeUndefined();
  });

  /**
   * The reason the rule above is the right way round rather than arbitrary:
   * `waveformBars` floors a zero row at 0.08 and draws an ABSENT waveform at
   * 0.15, so recording "we measured silence" would draw flatter than having
   * measured nothing at all. At 0.08 a bar is wider than it is tall, which is
   * what turned a waveform into a row of dots.
   */
  it("the absent placeholder stands taller than a floored zero row would", () => {
    const absent = waveformBars(undefined, 8)[0] ?? 0;
    const zeros = waveformBars(new Uint8Array(8), 8)[0] ?? 0;
    expect(absent).toBeGreaterThan(zeros);
  });

  it("one silent sample does not silence a take that has sound", () => {
    expect(downsampleWaveform([0, 0.5, 0])).toBeDefined();
  });

  it("garbage values read as zero without silencing the take", () => {
    expect(Array.from(downsampleWaveform([Number.NaN, -1, 0.5, Infinity])!)).toEqual([0, 0, 255, 0]);
  });
});

describe("display helpers", () => {
  it("resamples to the requested bar count and floors silence", () => {
    const bars = waveformBars(new Uint8Array([0, 255]), 4);
    expect(bars).toHaveLength(4);
    expect(bars[0]).toBeCloseTo(0.08);
    expect(bars[3]).toBe(1);
    expect(waveformBars(undefined, 3)).toEqual([0.15, 0.15, 0.15]);
  });

  it("computes an RMS level in 0..1", () => {
    expect(rmsLevel([])).toBe(0);
    expect(rmsLevel([0.5, -0.5])).toBeCloseTo(0.5);
    expect(rmsLevel([4, 4])).toBe(1);
  });
});

/**
 * UX spec section 2 — the waveform is the progress indicator, so which bars
 * are behind the playhead is a rule, not a detail. It lives in one place
 * because the bubble and the composer's preview player had drawn the same
 * picture by two different rules.
 */
describe("barIsPlayed", () => {
  it("fills a bar once the playhead passes its midpoint", () => {
    // Four bars: midpoints at 12.5%, 37.5%, 62.5%, 87.5% of the duration.
    const at = (ms: number): boolean[] => [0, 1, 2, 3].map((i) => barIsPlayed(i, 4, ms, 1000));
    expect(at(0)).toEqual([false, false, false, false]);
    expect(at(124)).toEqual([false, false, false, false]);
    expect(at(125)).toEqual([true, false, false, false]);
    expect(at(500)).toEqual([true, true, false, false]);
    expect(at(1000)).toEqual([true, true, true, true]);
  });

  it("fills nothing without a playhead", () => {
    // No duration yet (metadata still loading), a degenerate bar count, or a
    // position that is not a number: draw the unplayed track, never a full
    // bar on a message that has not started.
    expect(barIsPlayed(0, 4, 500, 0)).toBe(false);
    expect(barIsPlayed(0, 4, 500, Number.NaN)).toBe(false);
    expect(barIsPlayed(0, 0, 500, 1000)).toBe(false);
    expect(barIsPlayed(0, 4, Number.NaN, 1000)).toBe(false);
  });

  it("is monotone in the position and in the index", () => {
    for (let i = 0; i < 40; i += 1) {
      for (let ms = 0; ms <= 1000; ms += 50) {
        if (barIsPlayed(i, 40, ms, 1000)) {
          expect(barIsPlayed(i, 40, ms + 50, 1000)).toBe(true);
          if (i > 0) expect(barIsPlayed(i - 1, 40, ms, 1000)).toBe(true);
        }
      }
    }
  });
});
