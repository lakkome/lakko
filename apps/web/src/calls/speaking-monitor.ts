// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Reads the sound level of each person's audio in a group call and feeds
 * `SpeakingTracker` (`docs/ux/group-call-grid.md` section 7.3).
 *
 * The level is read from the `MediaStream` the browser plays: audio that has
 * already been received, decrypted and decoded on this device. Nothing here
 * reaches the network, and the call server is never asked who is speaking.
 *
 * One `AnalyserNode` per person, read ten times a second. A browser that
 * refuses an `AudioContext`, or leaves it suspended, simply yields no levels:
 * no ring is drawn and the large tile falls back to the roster order, which
 * is the call as it was before this existed.
 */

import { rmsLevel, SpeakingTracker, type SpeakingSnapshot } from "./speaking";

/** How often the levels are read. */
const SAMPLE_INTERVAL_MS = 100;

interface Tap {
  readonly stream: MediaStream;
  readonly source: MediaStreamAudioSourceNode;
  readonly analyser: AnalyserNode;
  readonly buffer: Float32Array<ArrayBuffer>;
}

/** Watches the streams it is given until `dispose`. */
export class SpeakingMonitor {
  private readonly tracker = new SpeakingTracker();
  private readonly taps = new Map<string, Tap>();
  private context: AudioContext | null = null;
  private timer: ReturnType<typeof setInterval> | null = null;
  private order: readonly string[] = [];

  public constructor(private readonly onSnapshot: (snapshot: SpeakingSnapshot) => void) {}

  /**
   * The people of the call and the stream each one's sound arrives on, in
   * roster order. Call it whenever either changes.
   */
  public sync(people: readonly { readonly id: string; readonly stream: MediaStream | undefined }[]): void {
    this.order = people.map((p) => p.id);
    const present = new Set(this.order);
    for (const [id, tap] of this.taps) {
      const now = people.find((p) => p.id === id)?.stream;
      if (now !== tap.stream) this.drop(id);
    }
    this.tracker.keepOnly(present);
    for (const p of people) {
      if (p.stream === undefined || this.taps.has(p.id)) continue;
      if (p.stream.getAudioTracks().length === 0) continue;
      const ctx = this.ensureContext();
      if (ctx === null) return;
      try {
        const source = ctx.createMediaStreamSource(p.stream);
        const analyser = ctx.createAnalyser();
        analyser.fftSize = 512;
        source.connect(analyser);
        this.taps.set(p.id, {
          stream: p.stream,
          source,
          analyser,
          buffer: new Float32Array(new ArrayBuffer(analyser.fftSize * 4)),
        });
      } catch (err) {
        console.warn("[call] could not read a sound level", err);
      }
    }
    if (this.taps.size > 0 && this.timer === null) {
      this.timer = setInterval(() => this.tick(), SAMPLE_INTERVAL_MS);
    }
  }

  /** Stop reading and release the audio graph. */
  public dispose(): void {
    if (this.timer !== null) clearInterval(this.timer);
    this.timer = null;
    for (const id of [...this.taps.keys()]) this.drop(id);
    const ctx = this.context;
    this.context = null;
    if (ctx !== null) void ctx.close().catch(() => undefined);
  }

  private ensureContext(): AudioContext | null {
    if (this.context !== null) return this.context;
    if (typeof AudioContext === "undefined") return null;
    try {
      this.context = new AudioContext();
      // A context made outside a gesture starts suspended in some browsers;
      // the call was started by one, so this normally succeeds at once.
      void this.context.resume().catch(() => undefined);
      return this.context;
    } catch (err) {
      console.warn("[call] no audio context for sound levels", err);
      return null;
    }
  }

  private drop(id: string): void {
    const tap = this.taps.get(id);
    if (tap === undefined) return;
    try {
      tap.source.disconnect();
    } catch {
      // Already gone with its stream.
    }
    this.taps.delete(id);
  }

  private tick(): void {
    const now = Date.now();
    for (const [id, tap] of this.taps) {
      tap.analyser.getFloatTimeDomainData(tap.buffer);
      this.tracker.sample(id, rmsLevel(tap.buffer), now);
    }
    this.onSnapshot(this.tracker.snapshot(this.order, now));
  }
}
