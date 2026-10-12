// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Reads the sound level of each person's audio in a group call and feeds
 * the shared speaking rule (`docs/ux/group-call-grid.md` sections 5.2, 7.3
 * and 10).
 *
 * What counts as sound, when a ring is on and who holds the floor is decided
 * by one rule for the three clients (`crates/lakko-ui-rules`, `speaking`,
 * over wasm; `speaking.ts` is how this client reaches it). This module reads
 * levels, tells the rule who this device's own person is, who is muted and
 * who is in the call, and hands its answers on. It decides nothing.
 *
 * Every level is measured on this device from audio it has already received,
 * decrypted and decoded. Nothing here reaches the network, and the call
 * server is never asked who is speaking.
 *
 * ## One scale, and where a level comes from
 *
 * Every level handed to the tracker is the root mean square of samples in
 * `-1..1` over about a tenth of a second (`rmsLevel` in `audio-level.ts`), for
 * everyone:
 *
 *  - **This device's own microphone**: an `AnalyserNode` on the captured
 *    stream. The root mean square of its time-domain samples is the scale by
 *    definition; its window (`TAP_WINDOW_SAMPLES`) covers most of the 100 ms
 *    between two readings, so a reading is of the interval, not of one
 *    hundredth of a second picked from it.
 *  - **Someone else**: first the same `AnalyserNode` on the stream the
 *    browser plays them from, so the remote ring and the own ring are read
 *    by one instrument; then the `stats-energy` level of the receiver that
 *    decodes them (`audio-level.ts`, which states its conversion). Both are
 *    measurements of decoded audio on this device. A path with no reading
 *    (no stream yet, an audio context the browser has not let run, no
 *    energy in the statistics) falls through to the other, and so does a
 *    path stuck at exactly zero for `STUCK_ZERO_MS` (`PathChooser`).
 *
 * The level a sender wrote into the packet header (the receiver's
 * synchronization sources) and the statistics' own `audioLevel` are printed
 * in the diagnostics line and never decide anything; `audio-level.ts` says
 * why.
 *
 * Statistics are read ten times a second only for a person the analyser
 * cannot serve, and once per diagnostics report for everyone else, so the
 * next call's log shows every path side by side.
 *
 * One timer for the call, ten readings a second, stopped by `dispose`. A
 * statistics reading is a promise: at most one is under way per person, and
 * at most `MAX_STATS_READS_PER_TICK` are started per reading, in turn, so a
 * call of fifty people costs what a call of eight does.
 *
 * ## The tracker's lifetime
 *
 * The shared tracker lives in wasm memory, which the garbage collector does
 * not reclaim, so the monitor owns exactly one at a time and frees it: when
 * the roster empties (the call is over; the next call starts from a tracker
 * that has heard nobody) and on `dispose`. It is made on the first reading
 * that needs it; while the wasm module is not ready there is none, nothing
 * is read and nothing is reported, and the next reading tries again.
 */

import {
  PathChooser,
  ReceiverLevelReader,
  rmsLevel,
  type LevelPath,
  type LevelReceiver,
  type PathReading,
} from "./audio-level";
import {
  readSpeech,
  sharedSpeakingRules,
  type SpeakingRule,
  type SpeakingRules,
  type SpeakingSnapshot,
} from "./speaking";
import type { VoiceReport } from "./voice-diagnostics";

/** How often the levels are read. */
export const SAMPLE_INTERVAL_MS = 100;
/** The most statistics readings started in one tick. Chosen, not measured. */
export const MAX_STATS_READS_PER_TICK = 8;
/**
 * How many samples an analyser tap reads: 4096 is 85 ms at 48 kHz and 93 ms
 * at 44.1 kHz, most of `SAMPLE_INTERVAL_MS`. (512, as it was, is 11 ms: one
 * syllable gap picked at random out of every tenth of a second.)
 */
export const TAP_WINDOW_SAMPLES = 4096;

/** One person of the call, as the monitor needs them. */
export interface VoiceSource {
  readonly id: string;
  /** This device's own person: their ring is drawn, the floor is not theirs. */
  readonly isLocal: boolean;
  /** A muted person is never speaking. */
  readonly micOn: boolean;
  /** The stream their sound is played from (or, for this device, captured to). */
  readonly stream: MediaStream | undefined;
  /** The receiver that decodes their audio, where the call has one. */
  readonly receiver?: LevelReceiver | undefined;
}

/** Reads the level of one stream; `close` releases it. */
export interface StreamLevelTap {
  /** The level now, or `null` while the tap cannot hear anything (its audio
   *  context is not running). Never a made-up silence. */
  read(): number | null;
  close(): void;
}

/** How many readings one diagnostics report covers (2 seconds). */
export const REPORT_EVERY_TICKS = 20;

/** What the monitor takes from its surroundings; the defaults are the
 *  browser's, a test hands in its own. */
export interface SpeakingMonitorDeps {
  readonly nowMs?: () => number;
  /** Start the one timer; returns what stops it. */
  readonly every?: (ms: number, run: () => void) => () => void;
  /** Open a level tap on a stream, or `null` when this browser cannot. */
  readonly openTap?: (stream: MediaStream) => StreamLevelTap | null;
  /** Release whatever `openTap` shares between taps. */
  readonly closeTaps?: () => void;
  /** The person did something: let whatever `openTap` needs a gesture for
   *  start. */
  readonly resumeTaps?: () => void;
  /** Told every `REPORT_EVERY_TICKS` readings what each person's level was
   *  over them, and who holds the floor. Diagnostics only. */
  readonly onReport?: (voices: readonly VoiceReport[], floorId: string | null) => void;
  /** Told once whenever the source of a person's level changes. */
  readonly onPath?: (id: string, path: LevelPath) => void;
  /** Where the call's tracker comes from; the shared rule unless a test
   *  wants to see what it was asked. */
  readonly rules?: SpeakingRules;
}

interface Voice {
  source: VoiceSource;
  reader: ReceiverLevelReader | null;
  readerOf: LevelReceiver | null;
  tap: StreamLevelTap | null;
  tapOf: MediaStream | null;
  chooser: PathChooser;
  path: LevelPath | null;
  /** Readings in a row that had no source at all. */
  sourceless: number;
  /** A statistics reading is owed to the diagnostics report. */
  statsOwed: boolean;
  /** For the diagnostics report, since the last one: the last path, the
   *  loudest chosen level, the loudest analyser reading, whether the
   *  analyser was passed over as stuck at zero, whether the ring was on. */
  reportPath: LevelPath;
  reportPeak: number | null;
  reportAnalyser: number | null;
  reportAnalyserStuck: boolean;
  reportSpeaking: boolean;
}

/** After this many readings without any source, say so once (3 seconds). */
const SOURCELESS_TICKS_BEFORE_SAYING = 30;

/**
 * `AnalyserNode` taps over one shared `AudioContext`.
 *
 * A context the browser has not let start (`suspended`: made outside a
 * gesture, or put to sleep) feeds its analysers nothing, and an analyser fed
 * nothing reads zeros without an error. Zeros look exactly like a quiet
 * person, so a tap answers `null` unless the context is `running`, the
 * context is asked to resume on every gesture the call screen sees, and its
 * state is written to the console whenever it changes.
 */
export function browserTaps(
  say: (line: string) => void = (line): void => {
    console.info(line);
  },
): Pick<Required<SpeakingMonitorDeps>, "openTap" | "closeTaps" | "resumeTaps"> {
  let context: AudioContext | null = null;
  let said: string | null = null;
  const sayState = (): void => {
    const state = context === null ? "none" : context.state;
    if (state === said) return;
    said = state;
    say(`[call] sound level audio context: ${state}`);
  };
  const resume = (): void => {
    const ctx = context;
    if (ctx === null || ctx.state === "running" || ctx.state === "closed") return;
    void ctx
      .resume()
      .catch(() => undefined)
      .then(sayState);
  };
  const ensure = (): AudioContext | null => {
    if (context !== null) return context;
    if (typeof AudioContext === "undefined") {
      sayState();
      return null;
    }
    try {
      const ctx = new AudioContext();
      context = ctx;
      ctx.addEventListener?.("statechange", sayState);
      sayState();
      // Made outside a gesture it may start suspended; `resumeTaps` asks
      // again on every gesture.
      resume();
      return ctx;
    } catch (err) {
      console.warn("[call] no audio context for sound levels", err);
      return null;
    }
  };
  return {
    openTap: (stream) => {
      const ctx = ensure();
      if (ctx === null) return null;
      try {
        const source = ctx.createMediaStreamSource(stream);
        const analyser = ctx.createAnalyser();
        analyser.fftSize = TAP_WINDOW_SAMPLES;
        source.connect(analyser);
        const buffer = new Float32Array(new ArrayBuffer(analyser.fftSize * 4));
        return {
          read: () => {
            // Not running: nothing is heard, which is not the same as silence.
            if (ctx.state !== "running") return null;
            analyser.getFloatTimeDomainData(buffer);
            return rmsLevel(buffer);
          },
          close: () => {
            try {
              source.disconnect();
            } catch {
              // Already gone with its stream.
            }
          },
        };
      } catch (err) {
        console.warn("[call] could not read a sound level", err);
        return null;
      }
    },
    resumeTaps: resume,
    closeTaps: () => {
      const ctx = context;
      context = null;
      if (ctx !== null) void ctx.close().catch(() => undefined);
    },
  };
}

/** Watches the people it is given until `dispose`. */
export class SpeakingMonitor {
  private readonly rules: SpeakingRules;
  /** The call's tracker; `null` before the first reading, after the roster
   *  emptied and after `dispose`. Freed wherever it is set back to `null`. */
  private tracker: SpeakingRule | null = null;
  /** This device's own person, as the roster names them. */
  private localId: string | null = null;
  private readonly voices = new Map<string, Voice>();
  private readonly nowMs: () => number;
  private readonly every: (ms: number, run: () => void) => () => void;
  private readonly openTap: (stream: MediaStream) => StreamLevelTap | null;
  private readonly closeTaps: () => void;
  private readonly resumeTaps: () => void;
  private readonly onReport: ((voices: readonly VoiceReport[], floorId: string | null) => void) | null;
  private ticksSinceReport = 0;
  private readonly onPath: (id: string, path: LevelPath) => void;
  private stopTimer: (() => void) | null = null;
  /** Everyone, in roster order. */
  private order: readonly string[] = [];
  /** Where the next tick starts handing out statistics readings. */
  private statsCursor = 0;

  public constructor(
    private readonly onSnapshot: (snapshot: SpeakingSnapshot) => void,
    deps: SpeakingMonitorDeps = {},
  ) {
    this.nowMs = deps.nowMs ?? ((): number => Date.now());
    this.every =
      deps.every ??
      ((ms, run): (() => void) => {
        const timer = setInterval(run, ms);
        return (): void => clearInterval(timer);
      });
    const taps = deps.openTap === undefined ? browserTaps() : null;
    this.openTap = deps.openTap ?? (taps as NonNullable<typeof taps>).openTap;
    this.closeTaps = deps.closeTaps ?? taps?.closeTaps ?? ((): void => undefined);
    this.resumeTaps = deps.resumeTaps ?? taps?.resumeTaps ?? ((): void => undefined);
    this.onReport = deps.onReport ?? null;
    this.rules = deps.rules ?? sharedSpeakingRules;
    this.onPath =
      deps.onPath ??
      ((id, path): void => {
        // One line per person per change of source, for the call's console:
        // which of the ways this browser took.
        console.info(`[call] sound level of ${id}: ${path}`);
      });
  }

  /**
   * The people of the call, in roster order. Call it whenever a person, a
   * stream, a receiver or a mute changes.
   */
  public sync(people: readonly VoiceSource[]): void {
    this.order = people.map((p) => p.id);
    const present = new Set(this.order);
    for (const [id, voice] of this.voices) {
      if (!present.has(id)) {
        voice.tap?.close();
        this.voices.delete(id);
      }
    }
    this.localId = people.find((p) => p.isLocal)?.id ?? null;
    if (people.length === 0) {
      // The call is over: the next one starts from a tracker that has heard
      // nobody, and nothing is held in wasm memory between calls.
      this.freeTracker();
    } else {
      const tracker = this.ensureTracker();
      tracker?.setLocal(this.localId);
      tracker?.keepOnly([...this.order]);
    }
    for (const p of people) {
      let voice = this.voices.get(p.id);
      if (voice === undefined) {
        voice = {
          source: p,
          reader: null,
          readerOf: null,
          tap: null,
          tapOf: null,
          chooser: new PathChooser(),
          path: null,
          sourceless: 0,
          statsOwed: true,
          reportPath: "none",
          reportPeak: null,
          reportAnalyser: null,
          reportAnalyserStuck: false,
          reportSpeaking: false,
        };
        this.voices.set(p.id, voice);
      }
      voice.source = p;
      const receiver = p.isLocal ? undefined : p.receiver;
      if ((receiver ?? null) !== voice.readerOf) {
        voice.readerOf = receiver ?? null;
        voice.reader = receiver === undefined ? null : new ReceiverLevelReader(receiver);
        voice.statsOwed = true;
      }
      if (voice.tap !== null && voice.tapOf !== (p.stream ?? null)) {
        voice.tap.close();
        voice.tap = null;
        voice.tapOf = null;
      }
      if (!p.micOn) this.tracker?.silence(p.id);
    }
    if (this.voices.size > 0 && this.stopTimer === null) {
      this.stopTimer = this.every(SAMPLE_INTERVAL_MS, () => this.tick());
    } else if (this.voices.size === 0 && this.stopTimer !== null) {
      this.stopTimer();
      this.stopTimer = null;
    }
  }

  /**
   * The person did something on the call screen (a press, a key). A browser
   * lets an audio context start only on such a gesture, so this is when a
   * suspended one is asked again.
   */
  public userGesture(): void {
    this.resumeTaps();
  }

  /** Stop reading and release the audio graph. */
  public dispose(): void {
    this.stopTimer?.();
    this.stopTimer = null;
    for (const voice of this.voices.values()) voice.tap?.close();
    this.voices.clear();
    this.order = [];
    this.localId = null;
    this.freeTracker();
    this.closeTaps();
  }

  /** The call's tracker, made on first use and told who this device is;
   *  `null` while the wasm module is not ready. */
  private ensureTracker(): SpeakingRule | null {
    if (this.tracker !== null) return this.tracker;
    try {
      const tracker = this.rules.create();
      tracker.setLocal(this.localId);
      this.tracker = tracker;
    } catch {
      this.tracker = null;
    }
    return this.tracker;
  }

  private freeTracker(): void {
    const tracker = this.tracker;
    this.tracker = null;
    try {
      tracker?.free();
    } catch {
      // Already freed with the module; nothing is held either way.
    }
  }

  /** The level of the stream the person is played from; `null` when there
   *  is none to read or the tap cannot hear (never a made-up silence). */
  private tapLevel(voice: Voice): number | null {
    const stream = voice.source.stream;
    if (stream === undefined) return null;
    if (voice.tap === null) {
      // A stream can gain its audio track after its video one; look again
      // on every reading until it has one.
      if (stream.getAudioTracks().length === 0) return null;
      voice.tap = this.openTap(stream);
      voice.tapOf = stream;
      if (voice.tap === null) return null;
    }
    return voice.tap.read();
  }

  /** One reading of everyone. Public for a test to drive; the timer calls it. */
  public tick(): void {
    const ids = this.order;
    if (ids.length === 0) return;
    const tracker = this.ensureTracker();
    // No rule to ask: nothing is read, and the next reading tries again.
    if (tracker === null) return;
    const now = this.nowMs();
    let budget = MAX_STATS_READS_PER_TICK;
    let nextCursor = this.statsCursor;
    for (let step = 0; step < ids.length; step += 1) {
      const index = (this.statsCursor + step) % ids.length;
      const id = ids[index] as string;
      const voice = this.voices.get(id);
      if (voice === undefined) continue;
      if (!voice.source.micOn) {
        tracker.silence(id);
        voice.reportPath = "none";
        continue;
      }
      // Every path that can be read now, in order of preference; the
      // chooser skips one that cannot be read or is stuck at zero.
      const fromTap = this.tapLevel(voice);
      const reader = voice.reader;
      const readings: PathReading[] = [{ path: "analyser", level: fromTap }];
      if (reader !== null) {
        readings.push({ path: "stats-energy", level: reader.energyLevel(now) });
      }
      const { level, path } = voice.chooser.choose(readings, now);
      const tapStuck = fromTap !== null && voice.chooser.stuck("analyser", now);
      if (fromTap !== null) voice.reportAnalyser = Math.max(voice.reportAnalyser ?? 0, fromTap);
      if (tapStuck) voice.reportAnalyserStuck = true;
      // The statistics keep a person going whom the analyser cannot serve,
      // and are read once per report for everyone, for the diagnostics.
      if (
        reader !== null &&
        (fromTap === null || tapStuck || voice.statsOwed) &&
        !reader.statsInFlight(now) &&
        budget > 0
      ) {
        budget -= 1;
        nextCursor = (index + 1) % ids.length;
        voice.statsOwed = false;
        void reader.readStats(now);
      }
      // Say where a person's level comes from when that changes, and say
      // once when a few seconds have passed with no source at all.
      if (path === "none") {
        voice.sourceless += 1;
        if (voice.sourceless === SOURCELESS_TICKS_BEFORE_SAYING && voice.path !== "none") {
          voice.path = "none";
          this.onPath(id, "none");
        }
      } else {
        voice.sourceless = 0;
        if (path !== voice.path) {
          voice.path = path;
          this.onPath(id, path);
        }
      }
      voice.reportPath = path;
      if (level !== null) voice.reportPeak = Math.max(voice.reportPeak ?? 0, level);
      tracker.sample(id, level, now);
    }
    this.statsCursor = budget === 0 ? nextCursor : 0;
    // The whole roster, in order: the rule knows who this device is and
    // passes them over itself.
    tracker.decideFloor([...ids], now);
    const snapshot = readSpeech(tracker);
    for (const id of snapshot.speakingIds) {
      const voice = this.voices.get(id);
      if (voice !== undefined) voice.reportSpeaking = true;
    }
    this.onSnapshot(snapshot);
    this.ticksSinceReport += 1;
    if (this.ticksSinceReport >= REPORT_EVERY_TICKS) {
      this.ticksSinceReport = 0;
      this.report(tracker, snapshot.activeSpeakerId);
    }
  }

  /** Hand the diagnostics of the readings since the last report over. */
  private report(tracker: SpeakingRule, floorId: string | null): void {
    const voices: VoiceReport[] = [];
    for (const id of this.order) {
      const voice = this.voices.get(id);
      if (voice === undefined) continue;
      const micOn = voice.source.micOn;
      voices.push({
        id,
        isLocal: voice.source.isLocal,
        path: micOn ? voice.reportPath : "none",
        peak: voice.reportPeak,
        speaking: voice.reportSpeaking,
        threshold: tracker.threshold(id),
        analyser: voice.reportAnalyser,
        analyserStuck: voice.reportAnalyserStuck,
        receiver: voice.reader?.takeDiagnostics() ?? null,
      });
      voice.reportPeak = null;
      voice.reportAnalyser = null;
      voice.reportAnalyserStuck = false;
      voice.reportSpeaking = false;
      voice.statsOwed = micOn;
    }
    this.onReport?.(voices, floorId);
  }
}
