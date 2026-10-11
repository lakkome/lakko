// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The sound level of one other person's audio (`docs/ux/group-call-grid.md`
 * sections 7.3 and 10): which measurements exist, which of them may decide
 * the speaking ring, and in what order.
 *
 * ## One scale
 *
 * Every level that reaches the ring is the root mean square of samples in
 * `-1..1` over about a tenth of a second (`speaking.ts`). Two measurements
 * are on that scale and are of the audio THIS device decoded and plays:
 *
 *  1. **`analyser`**: an `AnalyserNode` on the stream the browser plays
 *     (`speaking-monitor.ts`). The root mean square of its time-domain
 *     samples is the scale by definition, and it is the same instrument that
 *     reads this device's own microphone, so the two cannot disagree.
 *  2. **`stats-energy`**: the receiver's `inbound-rtp` statistics. W3C
 *     webrtc-stats defines `totalAudioEnergy` as the sum over received
 *     samples of `(sample / full scale)^2 * duration` and
 *     `totalSamplesDuration` as the sum of the durations, and gives
 *     `sqrt(delta energy / delta duration)` as the root mean square over the
 *     interval between two readings. That is the scale with no conversion.
 *     (Recalled, not verified here: libwebrtc adds the square of each 10 ms
 *     frame's LARGEST sample rather than of every sample, so in Chromium and
 *     Safari this reads at or above the true value, never below. It errs
 *     towards drawing the ring, which is why it is second and not first.)
 *
 * They are tried in that order. A path that has no reading falls through to
 * the next, and so does a path that has read exactly zero for
 * `STUCK_ZERO_MS`: an analyser the browser feeds nothing, or statistics that
 * never count energy, read zero without an error, and zero is exactly
 * "nobody is speaking". No evidence that packets arrive is needed for that
 * rule: a silence that is real is silence on the next path too.
 *
 * ## What is read for the diagnostics line only, and why
 *
 *  - **`sources`**: `RTCRtpReceiver.getSynchronizationSources()[].audioLevel`.
 *    Where the audio-level header extension (RFC 6464) is negotiated, an
 *    engine fills this from the byte the SENDER wrote into the packet header
 *    (W3C webrtc-pc: converted as `10^(-dBov / 20)`, 127 read as 0), and that
 *    byte crosses the call server outside the end-to-end encryption, so the
 *    server can read and rewrite it. Section 7.3 forbids a level the server
 *    supplies. The conversion does land on the common scale (dBov is the
 *    root mean square relative to full scale), which is why it is printed
 *    beside the others, but it is not a measurement by this device and never
 *    decides the ring. On the 2026-10-11 call it was the only path Chromium
 *    used; Firefox, which does not take that extension on a receiver, had
 *    nothing on it.
 *  - **`stats-level`**: the `audioLevel` of the same statistics record. The
 *    specification leaves its window to the engine (libwebrtc reports a
 *    decaying peak, not a root mean square), so it cannot be put on the
 *    common scale reliably and never decides the ring.
 *
 * Pure: no DOM and no clock. The receiver is a structural type so the whole
 * thing runs over fakes in a test.
 */

/** A measurement that may decide the ring, or none. */
export type LevelPath = "analyser" | "stats-energy" | "none";

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

/** A statistics level older than this is not used any more. Chosen. */
export const STATS_LEVEL_MAX_AGE_MS = 1_000;
/** A statistics reading that has not come back after this long is given up
 *  on and another may start. Chosen. */
export const STATS_READ_TIMEOUT_MS = 3_000;
/**
 * A path that has read exactly zero for this long is not trusted while
 * another path has a reading: the bound on how long a dead measurement can
 * hide a person who is speaking. Chosen: long enough that a pause between
 * sentences on a noise-gated microphone does not flip the path, short enough
 * that the ring is late by no more than this once.
 */
export const STUCK_ZERO_MS = 1_500;
/** At or below this a reading is "exactly zero" (digital silence). */
export const ZERO_LEVEL = 1e-6;

function isAmount(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

/**
 * The root mean square of what played between two statistics readings, on
 * the common scale: `sqrt(delta totalAudioEnergy / delta totalSamplesDuration)`
 * (W3C webrtc-stats, `totalAudioEnergy`). `null` when the counters went back
 * (a new stream: start again). Nothing played in between is silence.
 */
export function energyRms(
  before: { readonly total: number; readonly duration: number },
  after: { readonly total: number; readonly duration: number },
): number | null {
  const energy = after.total - before.total;
  const duration = after.duration - before.duration;
  if (energy < 0 || duration < 0) return null;
  return duration > 0 ? Math.min(1, Math.sqrt(energy / duration)) : 0;
}

/** What the statistics of a receiver have yielded so far. */
export type StatsNote =
  /** Not asked yet, or the first answer is still under way. */
  | "pending"
  /** `getStats` rejected. */
  | "failed"
  /** The report had no `inbound-rtp` audio record. */
  | "no-record"
  /** The record carries no `totalAudioEnergy` / `totalSamplesDuration`. */
  | "no-energy"
  /** One reading of the counters: a level needs two. */
  | "first"
  | "ok";

/** The raw values of the paths that never decide the ring, and of the energy
 *  path, since the last {@link ReceiverLevelReader.takeDiagnostics}. */
export interface ReceiverDiagnostics {
  /** The loudest `stats-energy` level folded in, or `null`. */
  readonly energy: number | null;
  readonly statsNote: StatsNote;
  /** The last `audioLevel` of the statistics record, raw, or `null`. */
  readonly statsLevel: number | null;
  /** The loudest `audioLevel` on the receiver's sources now, raw, or `null`. */
  readonly sources: number | null;
}

/** Reads one receiver's statistics. Keeps what a reading needs from the
 *  previous one. */
export class ReceiverLevelReader {
  /** The energy counters of the previous statistics reading. */
  private counters: { readonly total: number; readonly duration: number } | null = null;
  /** The level of the last statistics reading and when it was taken. */
  private energy: { readonly level: number; readonly atMs: number } | null = null;
  private note: StatsNote = "pending";
  /** When the reading under way started, or `null`. */
  private readingSinceMs: number | null = null;
  /** Counts readings, so one given up on cannot fold in late. */
  private generation = 0;
  private peakEnergy: number | null = null;
  private rawStatsLevel: number | null = null;

  public constructor(private readonly receiver: LevelReceiver) {}

  /** Whether a statistics reading is under way and not yet given up on. */
  public statsInFlight(nowMs: number): boolean {
    return this.readingSinceMs !== null && nowMs - this.readingSinceMs < STATS_READ_TIMEOUT_MS;
  }

  /**
   * Take one statistics reading. Resolves once it is folded in; never
   * rejects. One at a time: a call while one is under way does nothing,
   * unless that one has been out for `STATS_READ_TIMEOUT_MS`.
   */
  public async readStats(nowMs: number): Promise<void> {
    if (this.statsInFlight(nowMs)) return;
    this.generation += 1;
    const mine = this.generation;
    this.readingSinceMs = nowMs;
    try {
      const report = await this.receiver.getStats();
      if (mine !== this.generation) return;
      this.foldStats(report, nowMs);
    } catch {
      // A receiver that is gone has no level; the note says so.
      if (mine === this.generation) this.note = "failed";
    } finally {
      if (mine === this.generation) this.readingSinceMs = null;
    }
  }

  /** Fold one statistics report in. */
  private foldStats(report: LevelStatsReport, nowMs: number): void {
    let audio: LevelStat | null = null;
    try {
      report.forEach((stat) => {
        if (audio !== null || stat.type !== "inbound-rtp") return;
        const kind = stat.kind ?? stat.mediaType;
        if (kind !== undefined && kind !== "audio") return;
        audio = stat;
      });
    } catch {
      this.note = "failed";
      return;
    }
    const stat = audio as LevelStat | null;
    if (stat === null) {
      this.note = "no-record";
      return;
    }
    this.rawStatsLevel = isAmount(stat.audioLevel) ? stat.audioLevel : null;
    if (!isAmount(stat.totalAudioEnergy) || !isAmount(stat.totalSamplesDuration)) {
      this.note = "no-energy";
      return;
    }
    const before = this.counters;
    this.counters = { total: stat.totalAudioEnergy, duration: stat.totalSamplesDuration };
    if (before === null) {
      this.note = "first";
      return;
    }
    const level = energyRms(before, this.counters);
    // Counters that went back belong to a new stream: start again from here.
    if (level === null) {
      this.note = "first";
      return;
    }
    this.note = "ok";
    this.energy = { level, atMs: nowMs };
    this.peakEnergy = Math.max(this.peakEnergy ?? 0, level);
  }

  /**
   * The `stats-energy` level now, on the common scale, or `null` while there
   * is none (nothing read twice yet, the engine reports no energy, or the
   * last reading is older than `STATS_LEVEL_MAX_AGE_MS`). Does not start a
   * reading: the caller does, with {@link readStats}.
   */
  public energyLevel(nowMs: number): number | null {
    const energy = this.energy;
    if (energy === null || nowMs - energy.atMs > STATS_LEVEL_MAX_AGE_MS) return null;
    return energy.level;
  }

  /** The loudest `audioLevel` on the receiver's sources, raw; `null` where
   *  the engine puts none there. Diagnostics only (see the file comment). */
  private sourcesLevel(): number | null {
    let loudest: number | null = null;
    for (const list of ["getSynchronizationSources", "getContributingSources"] as const) {
      try {
        for (const source of this.receiver[list]?.() ?? []) {
          if (!isAmount(source.audioLevel)) continue;
          loudest = Math.max(loudest ?? 0, source.audioLevel);
        }
      } catch {
        // An engine without the method has no level there.
      }
    }
    return loudest;
  }

  /** The raw values since the last call, for the `voices` line. */
  public takeDiagnostics(): ReceiverDiagnostics {
    const taken: ReceiverDiagnostics = {
      energy: this.peakEnergy,
      statsNote: this.note,
      statsLevel: this.rawStatsLevel,
      sources: this.sourcesLevel(),
    };
    this.peakEnergy = null;
    return taken;
  }
}

/** One path's reading at one moment; `null` is "could not be read". */
export interface PathReading {
  readonly path: Exclude<LevelPath, "none">;
  readonly level: number | null;
}

/** What {@link PathChooser.choose} answers. */
export interface ChosenLevel {
  readonly path: LevelPath;
  /** `null` only with the path `none`. */
  readonly level: number | null;
}

/**
 * Picks, among the readings of one person in their order of preference, the
 * one the ring is decided from.
 *
 *  - A path with no reading is skipped.
 *  - A path that has read exactly zero for `STUCK_ZERO_MS` is skipped while
 *    a later path has a reading that is not itself stuck at zero.
 *  - When every path that has a reading is stuck at zero, the first of them
 *    answers: that is silence.
 *
 * Pure: the caller supplies the time.
 */
export class PathChooser {
  private readonly zeroSince = new Map<string, number>();

  /** Whether `path` has read exactly zero for `STUCK_ZERO_MS` as of the
   *  last {@link choose}. */
  public stuck(path: Exclude<LevelPath, "none">, nowMs: number): boolean {
    const since = this.zeroSince.get(path);
    return since !== undefined && nowMs - since >= STUCK_ZERO_MS;
  }

  public choose(readings: readonly PathReading[], nowMs: number): ChosenLevel {
    for (const { path, level } of readings) {
      // A path that cannot be read is not a path reading zero.
      if (level === null || !Number.isFinite(level) || level > ZERO_LEVEL) {
        this.zeroSince.delete(path);
      } else if (!this.zeroSince.has(path)) {
        this.zeroSince.set(path, nowMs);
      }
    }
    const readable = readings.filter(
      (r): r is PathReading & { level: number } => r.level !== null && Number.isFinite(r.level),
    );
    const chosen = readable.find((r) => !this.stuck(r.path, nowMs)) ?? readable[0];
    return chosen === undefined ? { path: "none", level: null } : chosen;
  }
}
