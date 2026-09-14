// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 §1 — the waveform the envelope carries is at most 128 bytes, and
 * the fold keeps a short loud peak rather than averaging it away.
 */
import { describe, expect, it } from "vitest";

import { MAX_WAVEFORM_BYTES, downsampleWaveform, rmsLevel, waveformBars } from "./waveform";

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

  it("empty is absent, silence is all zeros, garbage values read as zero", () => {
    expect(downsampleWaveform([])).toBeUndefined();
    expect(Array.from(downsampleWaveform([0, 0, 0])!)).toEqual([0, 0, 0]);
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
