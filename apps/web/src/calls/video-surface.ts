// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Call pictures kept in the page in Firefox, out of the compositor's own
 * video surfaces.
 *
 * Seen 2026-11-07 on test.lakko.me in Firefox 159 on macOS, in a group call:
 * for one screen refresh (16.7 ms) part of a remote picture turned into a
 * flat white rectangle with a light grey band across its top, then the
 * picture came back. The white area was cut along a 1024 device-pixel grid
 * and the name strip drawn over the picture stayed in place over it, so the
 * frame the decoder produced was not the cause: the browser's compositor
 * presented that part of the window without the picture for one refresh. In
 * the recording seven of the eight flashes fell on the very refresh in which
 * the speaking ring of one tile went on or off.
 *
 * Reproduced in Firefox 159.0.1 on this machine with the production call
 * screen, two remote pictures from an in-page WebRTC loopback and the
 * speaking rings switched every 300 ms: 13 and 10 such flashes in two runs
 * of 90 s, the same white rectangle with the same grey band; one 30 s run
 * with the rings left alone showed none. With
 * `gfx.webrender.debug.surface-promotion-logging` on, Firefox's WebRender
 * logged that it promotes one call video to an "underlay" compositor surface
 * and refuses the other for the underlay limit, and which one it refused
 * changed with the ring cycle. With every call `<video>` kept off those
 * surfaces (the logged reason becomes "is not on a root tile cache"), three
 * runs of 90 s with the same rings showed no flash. Not seen on a live call.
 *
 * What keeps a video off them here is a filter that is not the identity
 * (an identity filter such as `brightness(1)` was measured to be dropped,
 * and the video promoted again): `contrast(1.0001)`, a change no screen can
 * show. The cost is that Firefox draws each call picture into the page's own
 * tiles on every frame instead of handing it to the operating system's
 * compositor. Chrome and Safari were not seen to flicker and keep their own
 * path; the desktop app on WebKit draws pictures through a canvas
 * (`video-canvas.ts`).
 */

/** The browser this decides for, as far as it is read here. */
export interface EngineHint {
  readonly userAgent: string;
}

/** The attribute on `<html>` the stylesheet reads. */
export const VIDEO_SURFACE_ATTRIBUTE = "data-video-surfaces";

/** Its value when call pictures are kept in the page. */
export const VIDEO_SURFACES_IN_PAGE = "in-page";

/**
 * Whether call pictures are kept off the compositor's video surfaces: in a
 * browser built on Gecko (Firefox on a computer or on Android). Firefox on an
 * iPhone or iPad is WebKit and says `FxiOS`, not `Firefox/`; Chromium and
 * WebKit browsers say "like Gecko" but never `Firefox/`.
 */
export function callVideoKeptInPage(engine: EngineHint): boolean {
  const ua = engine.userAgent;
  return ua.includes("Firefox/") && !ua.includes("AppleWebKit") && !ua.includes("FxiOS");
}

/** The part of the root element this writes. */
export interface AttributeTarget {
  setAttribute(name: string, value: string): void;
}

/** Mark the document so the stylesheet keeps call pictures in the page
 *  where {@link callVideoKeptInPage} says so. Nothing is written otherwise. */
export function bootstrapVideoSurfaces(root: AttributeTarget, engine: EngineHint): void {
  if (callVideoKeptInPage(engine)) root.setAttribute(VIDEO_SURFACE_ATTRIBUTE, VIDEO_SURFACES_IN_PAGE);
}
