// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Any `<video>` a person is meant to see, drawn through a canvas where the
 * engine plays video but does not put it on screen (`calls/video-canvas.ts`:
 * the desktop app on macOS WebKit).
 *
 * The call screen lays a canvas carrying the video's own class over each
 * picture (`routes/Call.tsx`). Elsewhere a video is styled by selectors a
 * canvas would not match (`.lakko-vn-bubble video`), may carry native
 * controls, and sits in boxes of every kind, so this does it the other way
 * round: the canvas goes BEHIND the video, in the same box, and the video
 * stays exactly where and what it was. A video the engine does not put on
 * screen paints nothing, so the frames show through it, while whatever the
 * element does paint stays above them: its native controls, its poster
 * before the first frame. Everything a player does (play, pause, seek, end,
 * its own buttons and progress ring) keeps working on the element.
 *
 * What is changed on the page to get there, and undone at the end:
 *   * the canvas, inserted just before the video, absolutely positioned on the
 *     video's box (its offset in the parent), with the video's size, `object-fit`,
 *     `object-position`, corner radius, transform, opacity and visibility, and
 *     the video's background colour, which the video itself gives up
 *     (a black video background would otherwise cover the frames);
 *   * the video, if it was not positioned, becomes `position: relative`
 *     (which moves nothing) so it paints above the canvas;
 *   * the parent, if it was not positioned, becomes `position: relative`, so
 *     the canvas is placed in and clipped by the same box as the video.
 */

import { onCleanup } from "solid-js";

import { isDesktop } from "../platform";
import { ANIMATION_FRAMES, drawVideoFrame, mirrorVideoToCanvas, videoNeedsCanvas } from "../calls/video-canvas";

/** Whether this build draws pictures through a canvas. Decided once. */
export const PICTURES_THROUGH_CANVAS = videoNeedsCanvas(isDesktop(), {
  userAgent: typeof navigator === "undefined" ? "" : navigator.userAgent,
});

/** How often the canvas is laid onto the video's box again, milliseconds,
 *  besides when the video or its parent changes size. Chosen, not measured. */
export const OVERLAY_RELAYOUT_MS = 250;

/** The video's box and the computed styles the canvas copies. */
export interface VideoBox {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
  /** `false` when the video takes no part in layout (`display: none`). */
  readonly shown: boolean;
  readonly objectFit: string;
  readonly objectPosition: string;
  readonly borderRadius: string;
  readonly transform: string;
  readonly transformOrigin: string;
  readonly opacity: string;
  readonly visibility: string;
  readonly zIndex: string;
  readonly backgroundColor: string;
}

/** The inline style the canvas takes for `box`. */
export function overlayStyle(box: VideoBox): Record<string, string> {
  return {
    position: "absolute",
    left: `${box.left}px`,
    top: `${box.top}px`,
    width: `${box.width}px`,
    height: `${box.height}px`,
    margin: "0",
    "box-sizing": "border-box",
    display: box.shown ? "block" : "none",
    "object-fit": box.objectFit,
    "object-position": box.objectPosition,
    "border-radius": box.borderRadius,
    transform: box.transform,
    "transform-origin": box.transformOrigin,
    opacity: box.opacity,
    visibility: box.visibility,
    "z-index": box.zIndex,
    "background-color": box.backgroundColor,
    "pointer-events": "none",
  };
}

/** Whether a computed `position` leaves an element unpositioned. */
export function isStatic(position: string): boolean {
  return position === "" || position === "static";
}

/** Whether a computed colour paints nothing. */
export function isTransparent(color: string): boolean {
  const c = color.replace(/\s+/g, "").toLowerCase();
  return c === "" || c === "transparent" || c === "rgba(0,0,0,0)";
}

function readBox(video: HTMLVideoElement): VideoBox {
  const s = getComputedStyle(video);
  return {
    left: video.offsetLeft,
    top: video.offsetTop,
    width: video.offsetWidth,
    height: video.offsetHeight,
    shown: s.display !== "none" && video.isConnected,
    objectFit: s.objectFit,
    objectPosition: s.objectPosition,
    borderRadius: s.borderRadius,
    transform: s.transform,
    transformOrigin: s.transformOrigin,
    opacity: s.opacity,
    visibility: s.visibility,
    zIndex: s.zIndex,
    backgroundColor: s.backgroundColor,
  };
}

/**
 * Draw `video` through a canvas behind it, for as long as the current Solid
 * owner lives. Call it from the element's `ref`; a no-op where the engine puts
 * video on screen itself.
 */
export function paintVideoBehind(video: HTMLVideoElement): void {
  if (!PICTURES_THROUGH_CANVAS) return;
  const canvas = document.createElement("canvas");
  canvas.setAttribute("aria-hidden", "true");
  canvas.dataset["videoCanvas"] = "true";
  let parent: HTMLElement | null = null;
  let parentPositioned = false;
  let videoPositioned = false;
  /** The video's own background colour, read once before it is taken off. */
  let backgroundColor: string | null = null;
  /** The video's inline background before it was taken off; `null` when it
   *  was not. */
  let videoInlineBackground: string | null = null;
  let disposed = false;
  let stopFrames: (() => void) | null = null;

  const relayout = (): void => {
    if (disposed) return;
    const p = video.parentElement;
    if (p === null || !video.isConnected) return;
    if (p !== parent) {
      parent = p;
      if (isStatic(getComputedStyle(p).position)) {
        p.style.position = "relative";
        parentPositioned = true;
      }
    }
    if (canvas.parentElement !== p || canvas.nextSibling !== video) p.insertBefore(canvas, video);
    const vs = getComputedStyle(video);
    if (isStatic(vs.position)) {
      video.style.position = "relative";
      videoPositioned = true;
    }
    if (backgroundColor === null) {
      // The colour moves to the canvas: a black video background would
      // otherwise cover the frames behind it.
      backgroundColor = vs.backgroundColor;
      if (!isTransparent(backgroundColor)) {
        videoInlineBackground = video.style.backgroundColor;
        video.style.backgroundColor = "transparent";
      }
    }
    const style = overlayStyle({ ...readBox(video), backgroundColor });
    Object.assign(canvas.style, toCamel(style));
    stopFrames ??= mirrorVideoToCanvas<HTMLVideoElement>(video, canvas, ANIMATION_FRAMES);
  };

  // A paused element presents no new frame, so the frame it shows after a
  // load, a seek or a new source is drawn on those events.
  const redraw = (): void => {
    if (!disposed) drawVideoFrame<HTMLVideoElement>(video, canvas);
  };
  const restart = (): void => {
    stopFrames?.();
    stopFrames = null;
    const ctx = canvas.getContext("2d");
    ctx?.clearRect(0, 0, canvas.width, canvas.height);
    relayout();
  };
  const events: Array<[string, () => void]> = [
    ["loadeddata", redraw],
    ["seeked", redraw],
    ["playing", restart],
    ["emptied", restart],
  ];
  for (const [name, fn] of events) video.addEventListener(name, fn);

  const resize = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(relayout);
  resize?.observe(video);
  const mutation =
    typeof MutationObserver === "undefined" ? null : new MutationObserver(relayout);
  mutation?.observe(video, { attributes: true, attributeFilter: ["class", "style", "hidden"] });
  const timer = setInterval(relayout, OVERLAY_RELAYOUT_MS);
  queueMicrotask(relayout);

  onCleanup(() => {
    disposed = true;
    clearInterval(timer);
    resize?.disconnect();
    mutation?.disconnect();
    for (const [name, fn] of events) video.removeEventListener(name, fn);
    stopFrames?.();
    canvas.remove();
    if (videoPositioned) video.style.position = "";
    if (videoInlineBackground !== null) video.style.backgroundColor = videoInlineBackground;
    if (parentPositioned && parent !== null) parent.style.position = "";
  });
}

function toCamel(style: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(style)) {
    out[k.replace(/-([a-z])/g, (_m, c: string) => c.toUpperCase())] = v;
  }
  return out;
}
