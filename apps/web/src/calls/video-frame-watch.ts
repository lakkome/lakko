// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Tell a caller each time a `<video>` presents a frame.
 *
 * `requestVideoFrameCallback` fires once per presented frame and has to be
 * asked for again each time. Feature-detected: a browser without it reports
 * no frames, and the layer display that listens (`layer-display.ts`) then
 * stays on the full track.
 */

/** The part of `HTMLVideoElement` this reads; the DOM types this project
 *  builds against may not declare it. */
interface FrameCallbackVideo {
  requestVideoFrameCallback?: (callback: (now: number) => void) => number;
  cancelVideoFrameCallback?: (handle: number) => void;
}

/** Whether `el` can report its presented frames. */
export function canWatchVideoFrames(el: object): boolean {
  return typeof (el as FrameCallbackVideo).requestVideoFrameCallback === "function";
}

/**
 * Call `onFrame(now)` for every frame `el` presents, `now` on the
 * `performance.now()` clock. Returns the function that stops it. Does nothing
 * (and returns a no-op) where the browser cannot report frames.
 */
export function watchVideoFrames(el: object, onFrame: (now: number) => void): () => void {
  const video = el as FrameCallbackVideo;
  if (typeof video.requestVideoFrameCallback !== "function") return () => {};
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
