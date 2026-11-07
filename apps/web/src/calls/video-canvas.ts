// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A call picture drawn into a `<canvas>`, for an engine that plays a
 * `<video>` but never puts it on screen.
 *
 * Seen 2026-11-07 on macOS 27.2 in the desktop app, and reproduced in a plain
 * macOS WebKit view with no Lakko code and no Tauri: every `<video>` played
 * (`paused=false`, `readyState=4`, frames drawable into a canvas, the
 * per-frame callback firing), yet its layer in the window's layer tree had
 * nothing in it (a `RenderVideo` layer with no hosted content), a file video
 * as much as a call picture, so the screen showed the tile's background. A
 * canvas fed from the same element with `drawImage` on each frame showed the
 * picture in the same window. Whether Safari on that Mac shows video was not
 * established.
 *
 * Where {@link videoNeedsCanvas} says so, the call screen puts a canvas with
 * the element's own class (so the same `object-fit`, mirroring and layering
 * rules apply; a canvas is a replaced element) right after the `<video>`,
 * which stays in the document and keeps playing, and draws each new frame
 * of the element into it.
 */

/** The browser this decides for, as far as it is read here. */
export interface EngineHint {
  readonly userAgent: string;
}

/**
 * Whether pictures are drawn through a canvas: in the desktop app on a
 * WebKit engine (macOS WKWebView; Linux WebKitGTK is WebKit too and a canvas
 * costs it nothing in correctness), and in Safari on a Mac. Not on WebView2
 * (Windows), whose Chromium shows video, nor in Chrome, Firefox or Edge.
 */
export function videoNeedsCanvas(desktop: boolean, engine: EngineHint): boolean {
  const ua = engine.userAgent;
  const webkit = ua.includes("AppleWebKit") && !ua.includes("Chrome/") && !ua.includes("Edg/");
  if (desktop) return webkit;
  // Safari on a Mac shows the same failure (maintainer, 2026-11-07: no
  // picture in a Safari call on the Mac where the desktop app failed, while
  // Chrome on it showed video). iPhone browsers are WebKit too, but nothing
  // has been seen to fail there and the canvas costs a phone a draw per frame,
  // so only a Mac (or an iPad that reports itself as one) is covered.
  return webkit && ua.includes("Macintosh") && ua.includes("Safari/") && !ua.includes("Firefox/");
}

/** The part of a `<video>` read here. */
export interface CanvasSourceVideo {
  readonly videoWidth: number;
  readonly videoHeight: number;
  requestVideoFrameCallback?: (callback: (now: number) => void) => number;
  cancelVideoFrameCallback?: (handle: number) => void;
}

/** The part of a 2D context used here. */
export interface CanvasContext<V> {
  drawImage(image: V, dx: number, dy: number, dw: number, dh: number): void;
}

/** The part of a `<canvas>` used here. */
export interface CanvasTarget<V> {
  width: number;
  height: number;
  getContext(kind: "2d"): CanvasContext<V> | null;
}

/** How a frame is waited for where the element has no per-frame callback. */
export interface FrameScheduler {
  request(callback: () => void): number;
  cancel(handle: number): void;
}

/**
 * Draw the frame `video` shows now into `canvas`, giving the canvas the
 * video's own size first. Nothing is drawn before the video has a size; a
 * frame that cannot be drawn (the source was swapped) leaves the last one.
 */
export function drawVideoFrame<V extends CanvasSourceVideo>(video: V, canvas: CanvasTarget<V>): void {
  const ctx = canvas.getContext("2d");
  if (ctx === null) return;
  const w = video.videoWidth;
  const h = video.videoHeight;
  if (w <= 0 || h <= 0) return;
  if (canvas.width !== w) canvas.width = w;
  if (canvas.height !== h) canvas.height = h;
  try {
    ctx.drawImage(video, 0, 0, w, h);
  } catch {
    // The next frame tries again.
  }
}

/**
 * Draw every new frame of `video` into `canvas` until the returned function
 * is called. The canvas takes the video's own size, so the stylesheet's
 * `object-fit` crops or fits it exactly as it would the video. Where the
 * element has no per-frame callback, `fallback` (animation frames in the
 * browser) paces the drawing.
 */
export function mirrorVideoToCanvas<V extends CanvasSourceVideo>(
  video: V,
  canvas: CanvasTarget<V>,
  fallback: FrameScheduler,
): () => void {
  if (canvas.getContext("2d") === null) return () => {};
  let stopped = false;
  let handle: number | null = null;
  const usesFrameCallback = typeof video.requestVideoFrameCallback === "function";
  const draw = (): void => {
    if (!stopped) drawVideoFrame(video, canvas);
  };
  const next = (): void => {
    if (stopped) return;
    handle = usesFrameCallback
      ? (video.requestVideoFrameCallback?.(() => {
          draw();
          next();
        }) ?? null)
      : fallback.request(() => {
          draw();
          next();
        });
  };
  draw();
  next();
  return () => {
    stopped = true;
    if (handle === null) return;
    if (usesFrameCallback) video.cancelVideoFrameCallback?.(handle);
    else fallback.cancel(handle);
    handle = null;
  };
}

/** Animation frames, for {@link mirrorVideoToCanvas} in the browser. */
export const ANIMATION_FRAMES: FrameScheduler = {
  request: (callback) => requestAnimationFrame(() => callback()),
  cancel: (handle) => cancelAnimationFrame(handle),
};
