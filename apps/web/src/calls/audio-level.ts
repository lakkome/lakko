// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The sound level of one person's audio, read from the receiver the browser
 * decodes it with (`docs/ux/group-call-grid.md` sections 7.3 and 10).
 *
 * Everything here is measured by this device from audio it has already
 * received, decrypted and decoded. Nothing is asked of the call server, and
 * the audio-level RTP header extension (RFC 6464), which would carry the
 * level to the server in the clear, is not what this reads.
 *
 * Two standard sources, tried in this order for each reading:
 *
 *  1. `RTCRtpReceiver.getSynchronizationSources()` (then
 *     `getContributingSources()`): an `audioLevel` per source, read without a
 *     promise. An engine fills it only when it has a level for the source;
 *     where it is absent this path yields nothing and the next one is used.
 *  2. The receiver's `inbound-rtp` statistics: the growth of
 *     `totalAudioEnergy` over the growth of `totalSamplesDuration` since the
 *     previous reading, which is the root mean square of everything played
 *     in between; where an engine reports no energy, its `audioLevel`.
 *
 * A receiver that offers neither answers `"none"`, and the caller falls back
 * to reading the played stream itself (`speaking-monitor.ts`).
 *
 * Pure: no DOM and no clock. The receiver is a structural type so the whole
 * thing runs over fakes in a test.
 */

/** Where a level came from. */
export type LevelPath = "sync-source" | "stats-energy" | "stats-level" | "none";

/** One entry of `getSynchronizationSources()` as far as this reads it. */
export interface LevelSource {
  readonly audioLevel?: number | undefined;
  readonly timestamp?: number | undefined;
}

/** One statistics record as far as this reads it. */
export interface LevelStat {
  readonly type?: string;
  readonly kind?: string;
  readonly mediaType?: string;
  readonly audioLevel?: number;
  readonly totalAudioEnergy?: number;
  readonly totalSamplesDuration?: number;
}

/** A statistics report: anything with the `forEach` of `RTCStatsReport`. */
export interface LevelStatsReport {
  forEach(callback: (stat: LevelStat) => void): void;
}

/** The part of `RTCRtpReceiver` this reads (`RTCRtpReceiver` satisfies it). */
export interface LevelReceiver {
  getSynchronizationSources?(): readonly LevelSource[];
  getContributingSources?(): readonly LevelSource[];
  getStats(): Promise<LevelStatsReport>;
}

/**
 * A source whose timestamp has not moved for this long is no longer playing
 * (the browser keeps a source listed, with its last level, for ten seconds
 * after its last packet). Its level then reads as silence. Chosen, not
 * measured: longer than the 400 ms between the packets of a silent Opus
 * stream with discontinuous transmission.
 */
export const SOURCE_STALE_MS = 1_000;

/** A statistics level older than this is not used any more. Chosen. */
export const STATS_LEVEL_MAX_AGE_MS = 1_000;

function isLevel(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

/** The loudest `audioLevel` among `sources`, with the newest timestamp among
 *  those that carry a level; `null` when none carries one. */
function loudest(
  sources: readonly LevelSource[] | undefined,
): { readonly level: number; readonly timestamp: number | null } | null {
  if (sources === undefined) return null;
  let level: number | null = null;
  let timestamp: number | null = null;
  for (const source of sources) {
    if (!isLevel(source.audioLevel)) continue;
    level = level === null ? source.audioLevel : Math.max(level, source.audioLevel);
    if (typeof source.timestamp === "number" && Number.isFinite(source.timestamp)) {
      timestamp = timestamp === null ? source.timestamp : Math.max(timestamp, source.timestamp);
    }
  }
  return level === null ? null : { level: Math.min(1, level), timestamp };
}

/** Reads one receiver. Keeps what a reading needs from the previous one. */
export class ReceiverLevelReader {
  /** The newest source timestamp seen, and when (caller's clock) it last moved. */
  private sourceTimestamp: number | null = null;
  private sourceMovedAtMs: number | null = null;
  /** The energy counters of the previous statistics reading. */
  private energy: { readonly total: number; readonly duration: number } | null = null;
  /** The level of the last statistics reading and when it was taken. */
  private statsLevel: { readonly level: number; readonly atMs: number } | null = null;
  /** Statistics were read and carried no level of either kind. */
  private statsEmpty = false;
  private statsPath: LevelPath = "none";
  private reading = false;
  private lastPath: LevelPath = "none";

  public constructor(private readonly receiver: LevelReceiver) {}

  /** The path the last answer of {@link level} came from. */
  public get path(): LevelPath {
    return this.lastPath;
  }

  /** Whether a statistics reading is under way. */
  public get statsInFlight(): boolean {
    return this.reading;
  }

  /**
   * Whether the receiver has been asked both ways and offers no level at
   * all, so the caller should read the played stream instead.
   */
  public get unsupported(): boolean {
    return this.statsEmpty && this.lastPath === "none";
  }

  /**
   * The level from the receiver's sources, `0..1`, or `null` when the engine
   * puts no level on them. `nowMs` is the caller's clock; the sources' own
   * timestamps are only compared with each other, never with it.
   */
  public sourceLevel(nowMs: number): number | null {
    let found: ReturnType<typeof loudest> = null;
    try {
      found =
        loudest(this.receiver.getSynchronizationSources?.()) ??
        loudest(this.receiver.getContributingSources?.());
    } catch {
      found = null;
    }
    if (found === null) return null;
    if (found.timestamp === null) return found.level;
    if (this.sourceTimestamp === null || found.timestamp > this.sourceTimestamp) {
      this.sourceTimestamp = found.timestamp;
      this.sourceMovedAtMs = nowMs;
      return found.level;
    }
    // The same packet as before: still playing, or stopped a while ago.
    const movedAt = this.sourceMovedAtMs ?? nowMs;
    return nowMs - movedAt >= SOURCE_STALE_MS ? 0 : found.level;
  }

  /**
   * Take one statistics reading. Resolves once it is folded in; never
   * rejects. One at a time: a call while one is under way does nothing.
   */
  public async readStats(nowMs: number): Promise<void> {
    if (this.reading) return;
    this.reading = true;
    try {
      const report = await this.receiver.getStats();
      this.foldStats(report, nowMs);
    } catch {
      // A receiver that is gone has no level; the next reading says so.
    } finally {
      this.reading = false;
    }
  }

  /** Fold one statistics report in. */
  private foldStats(report: LevelStatsReport, nowMs: number): void {
    let audio: LevelStat | null = null;
    report.forEach((stat) => {
      if (audio !== null || stat.type !== "inbound-rtp") return;
      const kind = stat.kind ?? stat.mediaType;
      if (kind !== undefined && kind !== "audio") return;
      audio = stat;
    });
    const stat = audio as LevelStat | null;
    if (stat === null) return;
    if (isLevel(stat.totalAudioEnergy) && isLevel(stat.totalSamplesDuration)) {
      const before = this.energy;
      this.energy = { total: stat.totalAudioEnergy, duration: stat.totalSamplesDuration };
      this.statsEmpty = false;
      if (before === null) return;
      const energy = stat.totalAudioEnergy - before.total;
      const duration = stat.totalSamplesDuration - before.duration;
      // Counters that went back belong to a new stream: start again from here.
      if (energy < 0 || duration < 0) return;
      // Nothing played since the last reading is silence.
      const level = duration > 0 ? Math.min(1, Math.sqrt(energy / duration)) : 0;
      this.statsLevel = { level, atMs: nowMs };
      this.statsPath = "stats-energy";
      return;
    }
    if (isLevel(stat.audioLevel)) {
      this.statsEmpty = false;
      this.statsLevel = { level: Math.min(1, stat.audioLevel), atMs: nowMs };
      this.statsPath = "stats-level";
      return;
    }
    this.statsEmpty = true;
  }

  /**
   * The person's level now, `0..1`, or `null` while there is none to give
   * (nothing read yet, or the receiver offers none). Does not start a
   * statistics reading: the caller does, with {@link readStats}, when
   * {@link needsStats} says so.
   */
  public level(nowMs: number): number | null {
    const fromSource = this.sourceLevel(nowMs);
    if (fromSource !== null) {
      this.lastPath = "sync-source";
      return fromSource;
    }
    const stats = this.statsLevel;
    if (stats !== null && nowMs - stats.atMs <= STATS_LEVEL_MAX_AGE_MS) {
      this.lastPath = this.statsPath;
      return stats.level;
    }
    this.lastPath = "none";
    return null;
  }

  /** Whether the last {@link level} had to do without the sources, so a
   *  statistics reading is what keeps it going. */
  public get needsStats(): boolean {
    return this.lastPath !== "sync-source" && !this.statsEmpty;
  }
}
