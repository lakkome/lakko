// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `audio-level.ts` over fake receivers: the two standard places a browser
 * reports the level of the audio it decodes.
 */
import { describe, expect, it } from "vitest";

import {
  ReceiverLevelReader,
  SOURCE_STALE_MS,
  STATS_LEVEL_MAX_AGE_MS,
  type LevelReceiver,
  type LevelSource,
  type LevelStat,
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
  public getSynchronizationSources(): readonly LevelSource[] {
    if (this.sources === null) throw new Error("not supported");
    return this.sources;
  }
  public getContributingSources(): readonly LevelSource[] {
    return this.contributing;
  }
  public async getStats(): Promise<ReturnType<typeof report>> {
    this.statsCalls += 1;
    if (this.failStats) throw new Error("closed");
    return report(this.stats);
  }
}

describe("the synchronization-source path", () => {
  it("reads the level the engine put on the source", () => {
    const r = new FakeReceiver();
    r.sources = [{ audioLevel: 0.3, timestamp: 1_000 }];
    const reader = new ReceiverLevelReader(r);
    expect(reader.level(0)).toBeCloseTo(0.3);
    expect(reader.path).toBe("sync-source");
    expect(reader.needsStats).toBe(false);
  });

  it("takes the loudest of several sources and clamps to one", () => {
    const r = new FakeReceiver();
    r.sources = [
      { audioLevel: 0.1, timestamp: 10 },
      { audioLevel: 7, timestamp: 11 },
    ];
    expect(new ReceiverLevelReader(r).level(0)).toBe(1);
  });

  it("falls to the contributing sources when no synchronization source has a level", () => {
    const r = new FakeReceiver();
    r.sources = [{ timestamp: 10 }];
    r.contributing = [{ audioLevel: 0.2, timestamp: 10 }];
    const reader = new ReceiverLevelReader(r);
    expect(reader.level(0)).toBeCloseTo(0.2);
    expect(reader.path).toBe("sync-source");
  });

  it("reads a source whose packets stopped as silence, by its own timestamp not standing still", () => {
    const r = new FakeReceiver();
    r.sources = [{ audioLevel: 0.4, timestamp: 5_000 }];
    const reader = new ReceiverLevelReader(r);
    expect(reader.level(0)).toBeCloseTo(0.4);
    // The browser keeps the entry and its last level for ten seconds.
    expect(reader.level(SOURCE_STALE_MS - 1)).toBeCloseTo(0.4);
    expect(reader.level(SOURCE_STALE_MS)).toBe(0);
    // A new packet: playing again.
    r.sources = [{ audioLevel: 0.25, timestamp: 5_020 }];
    expect(reader.level(SOURCE_STALE_MS + 100)).toBeCloseTo(0.25);
  });

  it("is absent where the engine lists sources without a level, or has no such method", () => {
    const r = new FakeReceiver();
    r.sources = [{ timestamp: 10 }];
    const reader = new ReceiverLevelReader(r);
    expect(reader.sourceLevel(0)).toBeNull();
    r.sources = null;
    expect(reader.sourceLevel(0)).toBeNull();
    const bare: LevelReceiver = { getStats: async () => report([]) };
    expect(new ReceiverLevelReader(bare).sourceLevel(0)).toBeNull();
  });
});

describe("the statistics path", () => {
  const inbound = (energy: number, duration: number, extra: LevelStat = {}): LevelStat[] => [
    { type: "codec" },
    { type: "inbound-rtp", kind: "video", totalAudioEnergy: 99, totalSamplesDuration: 1 },
    { type: "inbound-rtp", kind: "audio", totalAudioEnergy: energy, totalSamplesDuration: duration, ...extra },
  ];

  it("is the root mean square of what played between two readings", async () => {
    const r = new FakeReceiver();
    r.sources = [];
    const reader = new ReceiverLevelReader(r);
    expect(reader.level(0)).toBeNull();
    expect(reader.needsStats).toBe(true);
    r.stats = inbound(1, 10);
    await reader.readStats(0);
    // One reading has nothing to compare with.
    expect(reader.level(0)).toBeNull();
    // 0.1 s played at level 0.5: energy grows by 0.5 * 0.5 * 0.1.
    r.stats = inbound(1 + 0.025, 10.1);
    await reader.readStats(100);
    expect(reader.level(100)).toBeCloseTo(0.5);
    expect(reader.path).toBe("stats-energy");
    // Silence played: duration grows, energy does not.
    r.stats = inbound(1.025, 10.2);
    await reader.readStats(200);
    expect(reader.level(200)).toBe(0);
    // Nothing played at all since: silence too.
    await reader.readStats(300);
    expect(reader.level(300)).toBe(0);
  });

  it("starts again when the counters go back", async () => {
    const r = new FakeReceiver();
    const reader = new ReceiverLevelReader(r);
    r.stats = inbound(5, 50);
    await reader.readStats(0);
    r.stats = inbound(0.1, 1);
    await reader.readStats(100);
    expect(reader.level(100)).toBeNull();
    r.stats = inbound(0.1 + 0.04 * 0.1, 1.1);
    await reader.readStats(200);
    expect(reader.level(200)).toBeCloseTo(0.2);
  });

  it("uses the reported level where an engine reports no energy", async () => {
    const r = new FakeReceiver();
    const reader = new ReceiverLevelReader(r);
    r.stats = [{ type: "inbound-rtp", mediaType: "audio", audioLevel: 0.3 }];
    await reader.readStats(0);
    expect(reader.level(0)).toBeCloseTo(0.3);
    expect(reader.path).toBe("stats-level");
  });

  it("stops trusting a level nobody refreshed", async () => {
    const r = new FakeReceiver();
    const reader = new ReceiverLevelReader(r);
    r.stats = [{ type: "inbound-rtp", kind: "audio", audioLevel: 0.3 }];
    await reader.readStats(0);
    expect(reader.level(STATS_LEVEL_MAX_AGE_MS)).toBeCloseTo(0.3);
    expect(reader.level(STATS_LEVEL_MAX_AGE_MS + 1)).toBeNull();
  });

  it("says so when the receiver offers no level either way", async () => {
    const r = new FakeReceiver();
    const reader = new ReceiverLevelReader(r);
    r.stats = [{ type: "inbound-rtp", kind: "audio" }];
    expect(reader.level(0)).toBeNull();
    expect(reader.unsupported).toBe(false);
    await reader.readStats(0);
    expect(reader.level(0)).toBeNull();
    expect(reader.unsupported).toBe(true);
    expect(reader.needsStats).toBe(false);
  });

  it("keeps asking while the receiver has no audio record yet", async () => {
    const r = new FakeReceiver();
    const reader = new ReceiverLevelReader(r);
    r.stats = [{ type: "transport" }];
    await reader.readStats(0);
    expect(reader.level(0)).toBeNull();
    expect(reader.unsupported).toBe(false);
    expect(reader.needsStats).toBe(true);
  });

  it("takes one reading at a time and survives a receiver that is gone", async () => {
    const r = new FakeReceiver();
    const reader = new ReceiverLevelReader(r);
    const first = reader.readStats(0);
    expect(reader.statsInFlight).toBe(true);
    await reader.readStats(0);
    await first;
    expect(r.statsCalls).toBe(1);
    expect(reader.statsInFlight).toBe(false);
    r.failStats = true;
    await expect(reader.readStats(100)).resolves.toBeUndefined();
    expect(reader.level(100)).toBeNull();
  });

  it("prefers the sources whenever they carry a level", async () => {
    const r = new FakeReceiver();
    const reader = new ReceiverLevelReader(r);
    r.stats = [{ type: "inbound-rtp", kind: "audio", audioLevel: 0.9 }];
    await reader.readStats(0);
    r.sources = [{ audioLevel: 0.1, timestamp: 1 }];
    expect(reader.level(0)).toBeCloseTo(0.1);
    expect(reader.path).toBe("sync-source");
  });
});
