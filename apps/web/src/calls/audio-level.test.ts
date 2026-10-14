// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `audio-level.ts` over fake receivers: the conversion of the statistics to
 * the common scale, what a receiver that offers less yields, and the order
 * in which a person's paths are trusted.
 */
import { describe, expect, it } from "vitest";

import {
  energyRms,
  PathChooser,
  ReceiverLevelReader,
  STATS_LEVEL_MAX_AGE_MS,
  STATS_READ_TIMEOUT_MS,
  STUCK_ZERO_MS,
  type LevelReceiver,
  type LevelSource,
  type LevelStat,
  type PathReading,
  rmsLevel,
} from "./audio-level";

function report(stats: readonly LevelStat[]): { forEach(cb: (s: LevelStat) => void): void } {
  return { forEach: (cb) => stats.forEach(cb) };
}

/** A receiver whose sources and statistics a test sets. */
class FakeReceiver implements LevelReceiver {
  public sources: LevelSource[] | null = [];
  public contributing: LevelSource[] = [];
  public stats: LevelStat[] = [];
  public statsCalls = 0;
  public failStats = false;
  /** When set, `getStats` answers only once this is called. */
  public hold: Promise<void> | null = null;
  public getSynchronizationSources(): readonly LevelSource[] {
    if (this.sources === null) throw new Error("not supported");
    return this.sources;
  }
  public getContributingSources(): readonly LevelSource[] {
    return this.contributing;
  }
  public async getStats(): Promise<ReturnType<typeof report>> {
    this.statsCalls += 1;
    const stats = this.stats;
    if (this.hold !== null) await this.hold;
    if (this.failStats) throw new Error("closed");
    return report(stats);
  }
}

const inbound = (over: Partial<LevelStat>): LevelStat => ({ type: "inbound-rtp", kind: "audio", ...over });

describe("the common scale", () => {
  it("is the root mean square of samples in -1..1, which is what the analyser path computes", () => {
    // A full-scale square wave is 1, a half-scale one 0.5, a full-scale sine 1/sqrt(2).
    expect(rmsLevel(new Float32Array(0))).toBe(0);
    expect(rmsLevel(new Float32Array(64))).toBe(0);
    expect(rmsLevel(new Float32Array([1, -1, 1, -1]))).toBe(1);
    expect(rmsLevel(new Float32Array([0.5, -0.5, 0.5, -0.5]))).toBe(0.5);
    const sine = Float32Array.from({ length: 4800 }, (_, i) => Math.sin((2 * Math.PI * i) / 48));
    expect(rmsLevel(sine)).toBeCloseTo(Math.SQRT1_2, 6);
  });

  it("converts statistics energy to the same number for the same signal", () => {
    // webrtc-stats: totalAudioEnergy grows by (sample / full scale)^2 * its
    // duration. The same sine as above, 100 ms at 48 kHz.
    const sine = Array.from({ length: 4800 }, (_, i) => Math.sin((2 * Math.PI * i) / 48));
    const energy = sine.reduce((sum, s) => sum + (s * s) / 48_000, 0);
    const level = energyRms({ total: 3, duration: 7 }, { total: 3 + energy, duration: 7.1 });
    expect(level).toBeCloseTo(rmsLevel(Float32Array.from(sine)), 6);
  });

  it("reads nothing played as silence, clamps to one, and refuses counters that went back", () => {
    expect(energyRms({ total: 1, duration: 1 }, { total: 1, duration: 1 })).toBe(0);
    expect(energyRms({ total: 0, duration: 0 }, { total: 9, duration: 1 })).toBe(1);
    expect(energyRms({ total: 5, duration: 5 }, { total: 1, duration: 6 })).toBeNull();
    expect(energyRms({ total: 5, duration: 5 }, { total: 6, duration: 1 })).toBeNull();
  });
});

describe("the statistics energy path", () => {
  it("needs two readings and is the level of what played between them", async () => {
    const receiver = new FakeReceiver();
    const reader = new ReceiverLevelReader(receiver);
    expect(reader.energyLevel(0)).toBeNull();
    receiver.stats = [inbound({ totalAudioEnergy: 1, totalSamplesDuration: 10 })];
    await reader.readStats(0);
    expect(reader.energyLevel(0)).toBeNull();
    expect(reader.takeDiagnostics().statsNote).toBe("first");
    receiver.stats = [inbound({ totalAudioEnergy: 1.004, totalSamplesDuration: 10.1 })];
    await reader.readStats(100);
    expect(reader.energyLevel(100)).toBeCloseTo(0.2, 6);
    expect(reader.takeDiagnostics()).toMatchObject({ statsNote: "ok" });
  });

  it("takes the audio record among others, by either spelling of its kind", async () => {
    const receiver = new FakeReceiver();
    const reader = new ReceiverLevelReader(receiver);
    const video: LevelStat = { type: "inbound-rtp", kind: "video", totalAudioEnergy: 99, totalSamplesDuration: 1 };
    const other: LevelStat = { type: "codec" };
    receiver.stats = [other, video, { type: "inbound-rtp", mediaType: "audio", totalAudioEnergy: 0, totalSamplesDuration: 0 }];
    await reader.readStats(0);
    receiver.stats = [other, video, { type: "inbound-rtp", mediaType: "audio", totalAudioEnergy: 0.025, totalSamplesDuration: 0.1 }];
    await reader.readStats(100);
    expect(reader.energyLevel(100)).toBeCloseTo(0.5, 6);
  });

  it("starts again when the counters go back", async () => {
    const receiver = new FakeReceiver();
    const reader = new ReceiverLevelReader(receiver);
    receiver.stats = [inbound({ totalAudioEnergy: 5, totalSamplesDuration: 50 })];
    await reader.readStats(0);
    receiver.stats = [inbound({ totalAudioEnergy: 0.001, totalSamplesDuration: 0.1 })];
    await reader.readStats(100);
    expect(reader.energyLevel(100)).toBeNull();
    receiver.stats = [inbound({ totalAudioEnergy: 0.002, totalSamplesDuration: 0.2 })];
    await reader.readStats(200);
    expect(reader.energyLevel(200)).toBeCloseTo(0.1, 6);
  });

  it("stops trusting a level nobody refreshed", async () => {
    const receiver = new FakeReceiver();
    const reader = new ReceiverLevelReader(receiver);
    receiver.stats = [inbound({ totalAudioEnergy: 0, totalSamplesDuration: 0 })];
    await reader.readStats(0);
    receiver.stats = [inbound({ totalAudioEnergy: 0.1, totalSamplesDuration: 0.1 })];
    await reader.readStats(100);
    expect(reader.energyLevel(100 + STATS_LEVEL_MAX_AGE_MS)).not.toBeNull();
    expect(reader.energyLevel(101 + STATS_LEVEL_MAX_AGE_MS)).toBeNull();
  });

  it("yields no level, and says why, where the record is missing or carries no energy", async () => {
    const receiver = new FakeReceiver();
    const reader = new ReceiverLevelReader(receiver);
    expect(reader.takeDiagnostics().statsNote).toBe("pending");
    // No inbound-rtp record at all.
    receiver.stats = [{ type: "codec" }];
    await reader.readStats(0);
    expect(reader.energyLevel(0)).toBeNull();
    expect(reader.takeDiagnostics().statsNote).toBe("no-record");
    // A record with a level of its own and no energy: the level is shown
    // raw and decides nothing.
    receiver.stats = [inbound({ audioLevel: 0.7 })];
    await reader.readStats(100);
    await reader.readStats(200);
    expect(reader.energyLevel(200)).toBeNull();
    expect(reader.takeDiagnostics()).toMatchObject({ statsNote: "no-energy", statsLevel: 0.7, energy: null });
    // Energy without a duration, or values that are not amounts.
    receiver.stats = [inbound({ totalAudioEnergy: 1 })];
    await reader.readStats(300);
    receiver.stats = [inbound({ totalAudioEnergy: Number.NaN, totalSamplesDuration: -1 })];
    await reader.readStats(400);
    expect(reader.energyLevel(400)).toBeNull();
    expect(reader.takeDiagnostics().statsNote).toBe("no-energy");
  });

  it("takes one reading at a time and survives a receiver that is gone", async () => {
    const receiver = new FakeReceiver();
    const reader = new ReceiverLevelReader(receiver);
    receiver.stats = [inbound({ totalAudioEnergy: 0, totalSamplesDuration: 0 })];
    const first = reader.readStats(0);
    expect(reader.statsInFlight(0)).toBe(true);
    await reader.readStats(0);
    await first;
    expect(receiver.statsCalls).toBe(1);
    expect(reader.statsInFlight(0)).toBe(false);
    receiver.failStats = true;
    await expect(reader.readStats(100)).resolves.toBeUndefined();
    expect(reader.takeDiagnostics().statsNote).toBe("failed");
    expect(reader.energyLevel(100)).toBeNull();
  });

  it("gives up on a reading that never comes back and ignores it if it does", async () => {
    const receiver = new FakeReceiver();
    const reader = new ReceiverLevelReader(receiver);
    let release: () => void = () => undefined;
    receiver.hold = new Promise<void>((resolve) => {
      release = resolve;
    });
    receiver.stats = [inbound({ totalAudioEnergy: 100, totalSamplesDuration: 1 })];
    const hung = reader.readStats(0);
    expect(reader.statsInFlight(STATS_READ_TIMEOUT_MS - 1)).toBe(true);
    expect(reader.statsInFlight(STATS_READ_TIMEOUT_MS)).toBe(false);
    // Two fresh readings while the first still hangs.
    receiver.hold = null;
    receiver.stats = [inbound({ totalAudioEnergy: 0, totalSamplesDuration: 0 })];
    await reader.readStats(STATS_READ_TIMEOUT_MS);
    receiver.stats = [inbound({ totalAudioEnergy: 0.004, totalSamplesDuration: 0.1 })];
    await reader.readStats(STATS_READ_TIMEOUT_MS + 100);
    expect(reader.energyLevel(STATS_READ_TIMEOUT_MS + 100)).toBeCloseTo(0.2, 6);
    // The hung one answers at last: it changes nothing.
    release();
    await hung;
    expect(reader.energyLevel(STATS_READ_TIMEOUT_MS + 100)).toBeCloseTo(0.2, 6);
    expect(receiver.statsCalls).toBe(3);
  });
});

describe("the values shown and never used", () => {
  it("reports the sources' level raw, from either list, and nothing where there is none", () => {
    const receiver = new FakeReceiver();
    const reader = new ReceiverLevelReader(receiver);
    expect(reader.takeDiagnostics().sources).toBeNull();
    receiver.sources = [{ timestamp: 1 }];
    expect(reader.takeDiagnostics().sources).toBeNull();
    receiver.sources = [{ audioLevel: 0.03, timestamp: 1 }, { audioLevel: 0.2, timestamp: 2 }];
    receiver.contributing = [{ audioLevel: 0.1 }];
    expect(reader.takeDiagnostics().sources).toBe(0.2);
    // An engine without the method.
    receiver.sources = null;
    expect(reader.takeDiagnostics().sources).toBe(0.1);
  });

  it("never lets a loud source or a loud statistics level stand in for energy", async () => {
    const receiver = new FakeReceiver();
    const reader = new ReceiverLevelReader(receiver);
    receiver.sources = [{ audioLevel: 1, timestamp: 1 }];
    receiver.stats = [inbound({ audioLevel: 1 })];
    await reader.readStats(0);
    await reader.readStats(100);
    expect(reader.energyLevel(100)).toBeNull();
  });

  it("hands over the loudest energy level since it was last asked", async () => {
    const receiver = new FakeReceiver();
    const reader = new ReceiverLevelReader(receiver);
    let energy = 0;
    let duration = 0;
    let at = 0;
    for (const level of [0, 0.3, 0.1]) {
      energy += level * level * 0.1;
      duration += 0.1;
      receiver.stats = [inbound({ totalAudioEnergy: energy, totalSamplesDuration: duration, audioLevel: level })];
      await reader.readStats(at);
      at += 100;
    }
    expect(reader.takeDiagnostics().energy).toBeCloseTo(0.3, 6);
    expect(reader.takeDiagnostics().energy).toBeNull();
  });
});

describe("which path decides", () => {
  const both = (analyser: number | null, energy: number | null): PathReading[] => [
    { path: "analyser", level: analyser },
    { path: "stats-energy", level: energy },
  ];

  it("is the first in order that has a reading", () => {
    const chooser = new PathChooser();
    expect(chooser.choose(both(0.2, 0.5), 0)).toEqual({ path: "analyser", level: 0.2 });
    expect(chooser.choose(both(null, 0.5), 100)).toEqual({ path: "stats-energy", level: 0.5 });
    expect(chooser.choose(both(Number.NaN, 0.5), 200)).toEqual({ path: "stats-energy", level: 0.5 });
    expect(chooser.choose(both(null, null), 300)).toEqual({ path: "none", level: null });
    expect(chooser.choose([], 400)).toEqual({ path: "none", level: null });
  });

  it("passes over a path stuck at zero, after the bound and not before", () => {
    const chooser = new PathChooser();
    // The analyser reads zero while the statistics hear speech.
    expect(chooser.choose(both(0, 0.3), 0).path).toBe("analyser");
    expect(chooser.choose(both(0, 0.3), STUCK_ZERO_MS - 100).path).toBe("analyser");
    expect(chooser.stuck("analyser", STUCK_ZERO_MS - 100)).toBe(false);
    expect(chooser.choose(both(0, 0.3), STUCK_ZERO_MS)).toEqual({ path: "stats-energy", level: 0.3 });
    expect(chooser.stuck("analyser", STUCK_ZERO_MS)).toBe(true);
    // One reading above zero and the analyser decides again, with a fresh bound.
    expect(chooser.choose(both(0.1, 0.3), STUCK_ZERO_MS + 100).path).toBe("analyser");
    expect(chooser.choose(both(0, 0.3), STUCK_ZERO_MS + 200).path).toBe("analyser");
    expect(chooser.choose(both(0, 0.3), 2 * STUCK_ZERO_MS + 200).path).toBe("stats-energy");
  });

  it("answers silence from the first path when every path is stuck at zero", () => {
    const chooser = new PathChooser();
    chooser.choose(both(0, 0), 0);
    expect(chooser.choose(both(0, 0), 10 * STUCK_ZERO_MS)).toEqual({ path: "analyser", level: 0 });
    // The moment the second hears something, it decides.
    expect(chooser.choose(both(0, 0.2), 10 * STUCK_ZERO_MS + 100)).toEqual({ path: "stats-energy", level: 0.2 });
  });

  it("keeps a path stuck at zero when it is the only one with a reading", () => {
    const chooser = new PathChooser();
    chooser.choose(both(0, null), 0);
    expect(chooser.choose(both(0, null), 5 * STUCK_ZERO_MS)).toEqual({ path: "analyser", level: 0 });
  });

  it("does not count a path that could not be read as reading zero", () => {
    const chooser = new PathChooser();
    chooser.choose(both(0, 0.3), 0);
    chooser.choose(both(null, 0.3), STUCK_ZERO_MS - 100);
    // The zero before the gap does not carry over it.
    expect(chooser.choose(both(0, 0.3), STUCK_ZERO_MS).path).toBe("analyser");
  });
});
