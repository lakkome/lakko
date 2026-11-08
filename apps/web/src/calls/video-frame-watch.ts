// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Tell a caller each time a remote `<video>` shows a new frame.
 *
 * Two ways, one interface (ADR-0070 step 2: no behaviour may rest on an API
 * some browser lacks):
 *
 *   1. `requestVideoFrameCallback`, where the browser has it: fires once per
 *      presented frame and has to be asked for again each time.
 *   2. The standard receiver statistics, where it does not: the receiver of
 *      the track the element plays is asked for its statistics every
 *      {@link FRAME_STATS_POLL_MS}, and a frame is reported whenever
 *      `framesDecoded` grew. This sees decoded frames, not presented ones,
 *      and no more than one report per poll however many frames arrived.
 *
 * The caller ({@link watchVideoFrames}) does not choose; it names the element
 * and, for the second way, how to find the track the element plays. The group
 * call registers each remote video track's receiver here as the track arrives
 * ({@link registerVideoFrameSource}).
 *
 * A browser with neither (no callback, and no receiver known for the track)
 * reports no frames, and the layer display that listens (`layer-display.ts`)
 * then stays on the main track.
 */

/** The part of `HTMLVideoElement` this reads; the DOM types this project
 *  builds against may not declare it. */
interface FrameCallbackVideo {
  requestVideoFrameCallback?: (callback: (now: number, metadata?: unknown) => void) => number;
  cancelVideoFrameCallback?: (handle: number) => void;
}

/**
 * What is known of one presented frame beyond the moment it was shown: the
 * parts of the `requestVideoFrameCallback` metadata read here. Each is
 * `undefined` where the browser gives none (and always on the statistics
 * path).
 */
export interface PresentedFrame {
  /** When the frame's last packet arrived, on the `performance.now()`
   *  clock. */
  readonly receiveTime?: number | undefined;
  /** The frame's RTP timestamp. */
  readonly rtpTimestamp?: number | undefined;
}

/** The numeric fields of a callback's metadata, as a {@link PresentedFrame}. */
export function presentedFrameOf(metadata: unknown): PresentedFrame {
  if (typeof metadata !== "object" || metadata === null) return {};
  const m = metadata as Record<string, unknown>;
  const num = (v: unknown): number | undefined =>
    typeof v === "number" && Number.isFinite(v) ? v : undefined;
  return { receiveTime: num(m["receiveTime"]), rtpTimestamp: num(m["rtpTimestamp"]) };
}

/**
 * Whether a frame an element presents belongs to the stream it was last
 * given, or is a frame the track decoded before that.
 *
 * Firefox (159, checked 2026-11-07 against a loopback call) shows a remote
 * track's last decoded frame the moment an element is given the track,
 * however old that frame is, and reports it through
 * `requestVideoFrameCallback`. A track the SFU stopped forwarding (a picture
 * no tile wanted: the main or middle camera track of a person drawn small)
 * keeps the frame it last decoded, possibly minutes earlier, so a tile that
 * switched to that track showed that old frame until the first new one
 * arrived. Chromium and WebKit present nothing until a new frame is decoded.
 * The rule here is the same in every browser and asks none of them for
 * either behaviour:
 *
 *   * the first frame presented after a bind is a leftover unless its
 *     `receiveTime` is at or after the bind (a frame whose arrival time is
 *     unknown is treated as a leftover: at worst one real frame is skipped);
 *   * a frame presented again with the leftover's RTP timestamp is the same
 *     leftover;
 *   * any other frame belongs to the binding.
 *
 * A leftover is one frame: a track can only have one last frame, and every
 * frame after it was decoded after the bind. So the rule cannot hold a
 * picture back for more than one frame whatever the browser's clocks say.
 */
export class BindingFrameGate {
  private boundAt: number | null = null;
  private firstSeen = false;
  private leftoverRtp: number | undefined;
  private admittedSinceBind = false;

  /** The element was given another stream at `at` (`performance.now()`). */
  bind(at: number): void {
    this.boundAt = at;
    this.firstSeen = false;
    this.leftoverRtp = undefined;
    this.admittedSinceBind = false;
  }

  /** Whether a bind happened and no frame of it has been admitted yet. */
  awaiting(): boolean {
    return this.boundAt !== null && !this.admittedSinceBind;
  }

  /** `true` when `frame` belongs to the stream bound last. */
  admit(frame: PresentedFrame): boolean {
    if (this.boundAt === null) return true;
    if (
      this.leftoverRtp !== undefined &&
      frame.rtpTimestamp !== undefined &&
      frame.rtpTimestamp === this.leftoverRtp
    ) {
      return false;
    }
    if (!this.firstSeen) {
      this.firstSeen = true;
      if (frame.receiveTime === undefined || frame.receiveTime < this.boundAt) {
        this.leftoverRtp = frame.rtpTimestamp;
        return false;
      }
    }
    this.admittedSinceBind = true;
    return true;
  }
}

/** One entry of a statistics report, as far as this module reads it. */
type StatsEntry = Record<string, unknown>;

/** The part of `RTCRtpReceiver` the statistics path reads. `getStats()`
 *  resolves to a report whose `values()` are the entries. */
export interface FrameStatsSource {
  getStats(): Promise<{ values(): Iterable<unknown> }>;
}

/** How often the statistics path asks, milliseconds. */
export const FRAME_STATS_POLL_MS = 50;

/** The receiver of each remote video track, by track. Weak: a track that is
 *  gone takes its entry with it. */
const sources = new WeakMap<object, FrameStatsSource>();

/**
 * Remember which receiver carries `track`, so an element playing that track
 * can be watched through the receiver's statistics where the browser has no
 * per-frame callback.
 */
export function registerVideoFrameSource(track: object, source: FrameStatsSource): void {
  sources.set(track, source);
}

/** The receiver registered for `track`, if any. */
export function videoFrameSourceOf(track: object | undefined): FrameStatsSource | undefined {
  return track === undefined ? undefined : sources.get(track);
}

/** How to find the track an element plays now; it may change, and may be
 *  absent before the person's video arrives. */
export type TrackOf = () => object | undefined;

function hasFrameCallback(el: object): boolean {
  return typeof (el as FrameCallbackVideo).requestVideoFrameCallback === "function";
}

/**
 * Whether the frames `el` shows can be reported now: the browser has the
 * per-frame callback, or the track `el` plays has a registered receiver to
 * read statistics from.
 */
export function canWatchVideoFrames(el: object, trackOf?: TrackOf): boolean {
  if (hasFrameCallback(el)) return true;
  return trackOf !== undefined && videoFrameSourceOf(trackOf()) !== undefined;
}

/**
 * `framesDecoded` of the video `inbound-rtp` entry of a receiver's report, or
 * `null` when the report has none (nothing received yet, or an engine that
 * does not count decoded frames).
 */
export function framesDecodedOf(entries: Iterable<unknown>): number | null {
  for (const raw of entries) {
    if (typeof raw !== "object" || raw === null) continue;
    const e = raw as StatsEntry;
    if (e["type"] !== "inbound-rtp") continue;
    // `kind` is the standard name; `mediaType` is what older engines call it.
    const kind = e["kind"] ?? e["mediaType"];
    if (kind !== undefined && kind !== "video") continue;
    const v = e["framesDecoded"];
    if (typeof v === "number" && Number.isFinite(v)) return v;
  }
  return null;
}

/**
 * Whether a new frame was decoded since the last reading. The first reading
 * of a stream is only a starting point: frames it already counts were decoded
 * at a time nobody observed. A count that went backwards (the receiver was
 * replaced) is a starting point again.
 */
export class DecodedFrameCounter {
  private last: number | null = null;

  /** Take a reading; `true` when it shows at least one new frame. */
  observe(framesDecoded: number | null): boolean {
    if (framesDecoded === null) return false;
    const before = this.last;
    this.last = framesDecoded;
    return before !== null && framesDecoded > before;
  }

  /** Forget the stream: the next reading is a starting point. */
  reset(): void {
    this.last = null;
  }
}

/** The timer and the clock the statistics path uses; replaced in tests. */
export interface FrameStatsDeps {
  setInterval(fn: () => void, ms: number): unknown;
  clearInterval(handle: unknown): void;
  /** The `performance.now()` clock. */
  now(): number;
}

const browserDeps: FrameStatsDeps = {
  setInterval: (fn, ms) => setInterval(fn, ms),
  clearInterval: (handle) => clearInterval(handle as ReturnType<typeof setInterval>),
  now: () => performance.now(),
};

/**
 * The statistics path: every {@link FRAME_STATS_POLL_MS} ask the receiver
 * `sourceOf()` names for its statistics and call `onFrame(now)` when
 * `framesDecoded` grew since the last answer. A poll is skipped while the
 * previous one has not answered. When `sourceOf()` names a different
 * receiver than before, counting starts again. Returns the function that
 * stops it; nothing is reported after that.
 */
export function watchDecodedFrames(
  sourceOf: () => FrameStatsSource | undefined,
  onFrame: (now: number) => void,
  deps: FrameStatsDeps = browserDeps,
): () => void {
  const counter = new DecodedFrameCounter();
  let stopped = false;
  let asking = false;
  let current: FrameStatsSource | undefined;
  const poll = (): void => {
    if (stopped || asking) return;
    const source = sourceOf();
    if (source !== current) {
      current = source;
      counter.reset();
    }
    if (source === undefined) return;
    asking = true;
    void (async (): Promise<void> => {
      try {
        const report = await source.getStats();
        if (stopped || source !== current) return;
        if (counter.observe(framesDecodedOf(report.values()))) onFrame(deps.now());
      } catch {
        // A receiver that cannot answer reports no frame this time.
      } finally {
        asking = false;
      }
    })();
  };
  const handle = deps.setInterval(poll, FRAME_STATS_POLL_MS);
  return () => {
    stopped = true;
    deps.clearInterval(handle);
  };
}

/**
 * Call `onFrame(now, frame)` for the frames `el` shows, `now` on the
 * `performance.now()` clock and `frame` what the browser said of the frame
 * ({@link PresentedFrame}; empty on the statistics path). Returns the
 * function that stops it.
 *
 * With `requestVideoFrameCallback`: once per presented frame. Without it and
 * with `trackOf`: from the statistics of the receiver registered for the
 * track ({@link watchDecodedFrames}). With neither: nothing, and a no-op.
 */
export function watchVideoFrames(
  el: object,
  onFrame: (now: number, frame: PresentedFrame) => void,
  trackOf?: TrackOf,
): () => void {
  const video = el as FrameCallbackVideo;
  if (typeof video.requestVideoFrameCallback !== "function") {
    if (trackOf === undefined) return () => {};
    return watchDecodedFrames(
      () => videoFrameSourceOf(trackOf()),
      (now) => onFrame(now, {}),
    );
  }
  let stopped = false;
  let handle = 0;
  const request = (): void => {
    handle = video.requestVideoFrameCallback?.call(video, (now: number, metadata?: unknown) => {
      if (stopped) return;
      onFrame(now, presentedFrameOf(metadata));
      request();
    }) ?? 0;
  };
  request();
  return () => {
    stopped = true;
    video.cancelVideoFrameCallback?.call(video, handle);
  };
}
