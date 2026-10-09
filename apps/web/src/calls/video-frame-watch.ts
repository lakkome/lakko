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
  requestVideoFrameCallback?: (callback: (now: number) => void) => number;
  cancelVideoFrameCallback?: (handle: number) => void;
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
 * Call `onFrame(now)` for the frames `el` shows, `now` on the
 * `performance.now()` clock. Returns the function that stops it.
 *
 * With `requestVideoFrameCallback`: once per presented frame. Without it and
 * with `trackOf`: from the statistics of the receiver registered for the
 * track ({@link watchDecodedFrames}). With neither: nothing, and a no-op.
 */
export function watchVideoFrames(
  el: object,
  onFrame: (now: number) => void,
  trackOf?: TrackOf,
): () => void {
  const video = el as FrameCallbackVideo;
  if (typeof video.requestVideoFrameCallback !== "function") {
    if (trackOf === undefined) return () => {};
    return watchDecodedFrames(() => videoFrameSourceOf(trackOf()), onFrame);
  }
  let stopped = false;
  let handle = 0;
  const request = (): void => {
    handle = video.requestVideoFrameCallback?.call(video, (now: number) => {
      if (stopped) return;
      onFrame(now);
      request();
    }) ?? 0;
  };
  request();
  return () => {
    stopped = true;
    video.cancelVideoFrameCallback?.call(video, handle);
  };
}
