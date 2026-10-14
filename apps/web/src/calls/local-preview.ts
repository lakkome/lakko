// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The person's own camera on their own screen: how a `<video>` is bound to
 * it, the same way in every browser.
 *
 * Seen on the first call Safari was ever in: the person's own tile was an
 * empty dark box while the other two people saw their video and they saw
 * both of them. The other browsers showed their own preview in the same
 * build. Every own-camera element of the call screen was bound the same way,
 * and that way rested on three things an engine is free not to do:
 *
 *   1. `srcObject` was assigned inside the element's `ref`, which runs when
 *      the element is created, BEFORE it is in the document, and playback
 *      was left to the `autoplay` attribute. Whether an element given its
 *      stream while detached starts by itself once inserted is the engine's
 *      choice. (The remote pictures, which did show in Safari, are bound
 *      after insertion.)
 *   2. Nothing ever called `play()`.
 *   3. The call keeps ONE `MediaStream` object for the whole call and swaps
 *      the camera track inside it (camera turned on mid-call, camera
 *      flipped). The signal that carries the stream does not change when
 *      its content does, so the element was never bound again; that it
 *      follows a track added to a stream it already plays is again the
 *      engine's choice.
 *
 * The rule here asks none of that of the engine:
 *
 *   * the element is bound only once it is in the document;
 *   * it plays a stream of its OWN, made from the camera track alone, and a
 *     new one whenever the camera track is another object, so a new camera
 *     is always a new `srcObject`;
 *   * `play()` is called after every bind, and again on each look for as
 *     long as the element is paused while it has a live track;
 *   * the element is looked at on a slow timer ({@link PREVIEW_CHECK_MS}) as
 *     well as when the stream signal changes, because the track swap inside
 *     the stream is announced by nothing.
 *
 * Which of the three it was in Safari, or whether it was something else (the
 * camera track itself not producing frames there once it has clones for the
 * light and middle senders), is not known: no Safari was run. The line
 * {@link describeLocalPreview} writes a little after the bind says which.
 *
 * Structural types, no DOM: the binder is run under a test with stand-ins.
 */

/** The part of a camera `MediaStreamTrack` read here. */
export interface PreviewTrack {
  readonly readyState?: string;
  readonly muted?: boolean;
  readonly enabled?: boolean;
}

/** The part of a `MediaStream` read here. */
export interface PreviewSource<T extends PreviewTrack> {
  getVideoTracks(): T[];
}

/** The part of a `<video>` element used here. */
export interface PreviewElement<S> {
  srcObject: S | null;
  readonly paused?: boolean;
  readonly readyState?: number;
  readonly videoWidth?: number;
  readonly isConnected?: boolean;
  play?: () => Promise<void> | void;
}

/** How often an own-camera element is looked at again, milliseconds. Chosen,
 *  not measured: a camera swapped inside the stream shows within this. */
export const PREVIEW_CHECK_MS = 500;

/** How long after a bind the diagnostic line is written, milliseconds: long
 *  enough for a camera's first frame. Chosen, not measured. */
export const PREVIEW_DIAG_AFTER_MS = 2000;

/**
 * Keeps one own-camera element playing the camera of the stream it is given.
 * `S` is the stream type the element plays, `T` the track type.
 */
export class LocalPreviewBinder<T extends PreviewTrack, S> {
  private bound: T | null = null;
  private binds = 0;
  private playCalls = 0;
  private playRefusals = 0;

  /**
   * @param streamOf makes the stream the element plays from one camera track
   *   (`new MediaStream([track])` in the browser).
   */
  constructor(private readonly streamOf: (track: T) => S) {}

  /**
   * Look at the element and the source and put them right. Returns `true`
   * when the element was given another stream (or none) by this call.
   *
   * An element that is not in the document is left alone: it is bound on the
   * first look after it is.
   */
  sync(el: PreviewElement<S>, source: PreviewSource<T> | null | undefined): boolean {
    if (el.isConnected === false) return false;
    const track = source?.getVideoTracks()[0] ?? null;
    let rebound = false;
    if (track !== this.bound) {
      this.bound = track;
      el.srcObject = track === null ? null : this.streamOf(track);
      this.binds += 1;
      rebound = true;
    }
    if (track !== null && (rebound || el.paused === true)) this.play(el);
    return rebound;
  }

  /** The camera track the element plays now. */
  track(): T | null {
    return this.bound;
  }

  /** The element is gone: forget what it played. */
  reset(): void {
    this.bound = null;
  }

  /** How often the element was bound, `play()` called, and `play()`
   *  refused, for the diagnostic line. */
  counts(): { readonly binds: number; readonly playCalls: number; readonly playRefusals: number } {
    return { binds: this.binds, playCalls: this.playCalls, playRefusals: this.playRefusals };
  }

  private play(el: PreviewElement<S>): void {
    if (typeof el.play !== "function") return;
    this.playCalls += 1;
    try {
      const result = el.play();
      if (result !== undefined && typeof result.then === "function") {
        result.then(undefined, () => {
          // Refused or interrupted (a newer bind): the next look tries again.
          this.playRefusals += 1;
        });
      }
    } catch {
      this.playRefusals += 1;
    }
  }
}

/** One track's state as `ready=<live|ended|-> muted=<..> enabled=<..>`. */
export function describeTrackState(track: PreviewTrack | null | undefined): string {
  if (track === null || track === undefined) return "ready=- muted=- enabled=-";
  return (
    `ready=${track.readyState ?? "-"} muted=${track.muted === undefined ? "-" : String(track.muted)}` +
    ` enabled=${track.enabled === undefined ? "-" : String(track.enabled)}`
  );
}

/**
 * The own-camera element and its track in one line, for the console.
 * `where` names the element (`tile`, `self`, `fill`).
 *
 * How to read it when the own picture is dark:
 *   * `el_ready=0` or `paused=true` with `track ready=live muted=false`: the
 *     element is not playing a camera that works; `play_refused` says
 *     whether the engine refused `play()`.
 *   * `track ... muted=true` or `ready=ended`: the camera track itself gives
 *     no frames (read with the `call local camera` line of the same call,
 *     which has the copies sent as the light and middle pictures).
 *   * `el_ready=4 paused=false width>0` with a live unmuted track: the
 *     element plays; the darkness is in how it is drawn (styles), not in the
 *     media.
 */
export function describeLocalPreview(
  where: string,
  el: PreviewElement<unknown>,
  track: PreviewTrack | null,
  counts: { readonly binds: number; readonly playCalls: number; readonly playRefusals: number },
): string {
  return (
    `call local preview ${where}: el_ready=${el.readyState ?? "-"}` +
    ` paused=${el.paused === undefined ? "-" : String(el.paused)} width=${el.videoWidth ?? "-"}` +
    ` binds=${counts.binds} play_calls=${counts.playCalls} play_refused=${counts.playRefusals}` +
    ` track ${describeTrackState(track)}`
  );
}

/**
 * The camera track and the copies of it that are sent, in one line: the main
 * track (the one the own preview plays) and each clone a further sender
 * carries, `-` for one that does not exist.
 */
export function describeLocalCamera(
  main: PreviewTrack | null | undefined,
  copies: ReadonlyArray<readonly [string, PreviewTrack | null | undefined]>,
): string {
  return (
    `call local camera: main ${describeTrackState(main)}` +
    copies.map(([name, track]) => ` | ${name} ${describeTrackState(track)}`).join("")
  );
}
