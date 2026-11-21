// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether this device's own share shows the call itself, the endless mirror
 * (`docs/ux/screen-share.md` section 3 and 3.1).
 *
 * The person sharing sees the live picture of their share, except when the
 * share contains the call: then the tile shows no picture and says what is
 * shared. A browser never says which window or screen it captured, so the
 * decision reads what each engine does report, most precise first, and shows
 * the picture whenever nothing reported can tell.
 *
 * {@link ownShareView} is pure; {@link browserMirrorFacts} reads the page.
 */

/** What kind of surface the person shares, as the tile names it. */
export type MirrorSurface = "tab" | "window" | "screen" | "unknown";

/** What the own share tile shows. */
export type OwnShareView =
  | { readonly kind: "picture" }
  | { readonly kind: "mirror"; readonly surface: MirrorSurface };

/** A size in pixels. */
export interface PixelSize {
  readonly width: number;
  readonly height: number;
}

/** The captured picture's size, where the browser reports it. */
export interface CapturedSize {
  readonly width?: number | undefined;
  readonly height?: number | undefined;
}

/** Everything the decision reads. Every field may be unknown. */
export interface MirrorFacts {
  /** `getSettings().displaySurface`. */
  readonly displaySurface: string | undefined;
  /** The captured picture's size (`getSettings().width` / `height`). */
  readonly captured: CapturedSize;
  /** Chromium's Capture Handle: `true` when the captured tab is this page,
   *  `false` when it is another page that set one, `undefined` otherwise. */
  readonly captureHandleIsOwn: boolean | undefined;
  /** This browser window's outer size in CSS pixels, and the device pixel
   *  ratio that turns it into device pixels. */
  readonly ownWindow: PixelSize | undefined;
  /** The size of the screen this window is on, in CSS pixels. */
  readonly ownScreen: PixelSize | undefined;
  readonly devicePixelRatio: number;
  /** `screen.isExtended`: `false` when the browser says there is only one
   *  screen (Chromium); `undefined` where the browser does not say. */
  readonly screenExtended: boolean | undefined;
}

/** Two sizes are the same within this many pixels on each side. Chosen: a
 *  window's outer size and its captured frame can differ by a border. */
export const SIZE_SLACK_PX = 2;

/** How often a live share is judged again (section 3: moving the call to
 *  another screen or switching the shared surface changes the tile). */
export const MIRROR_RECHECK_MS = 2_000;

function surfaceOf(displaySurface: string | undefined): MirrorSurface {
  switch (displaySurface) {
    case "browser":
      return "tab";
    case "window":
      return "window";
    case "monitor":
      return "screen";
    default:
      return "unknown";
  }
}

/** Whether the captured picture has `size`, at scale 1, the device pixel
 *  ratio, or its inverse (engines differ in which pixels they capture at). */
export function sameSize(
  captured: CapturedSize,
  size: PixelSize | undefined,
  devicePixelRatio: number,
): boolean {
  if (size === undefined) return false;
  const w = captured.width;
  const h = captured.height;
  if (w === undefined || h === undefined || !(w > 0) || !(h > 0)) return false;
  // A capture may come at CSS pixels, device pixels, or (WebKit on a
  // high-density screen, not measured) the other way round.
  const scales =
    devicePixelRatio > 0 && devicePixelRatio !== 1 ? [1, devicePixelRatio, 1 / devicePixelRatio] : [1];
  return scales.some(
    (s) =>
      Math.abs(w - size.width * s) <= SIZE_SLACK_PX && Math.abs(h - size.height * s) <= SIZE_SLACK_PX,
  );
}

/**
 * What the own share tile shows (section 3.1, in order):
 *
 * 1. a tab: a mirror only when Chromium's Capture Handle names this page;
 * 2. a window: a mirror when the captured picture has this window's size;
 * 3. a screen: a mirror when there is one screen, or the captured picture
 *    has this screen's size;
 * 4. no surface reported: the size checks of 2 and 3.
 *
 * In the desktop app the host supplies the screen count and the sizes
 * ({@link browserMirrorFacts}).
 *
 * Anything else shows the picture.
 */
export function ownShareView(facts: MirrorFacts): OwnShareView {
  const surface = surfaceOf(facts.displaySurface);
  const mirror: OwnShareView = { kind: "mirror", surface };
  const picture: OwnShareView = { kind: "picture" };
  const dpr = facts.devicePixelRatio;
  switch (surface) {
    case "tab":
      return facts.captureHandleIsOwn === true ? mirror : picture;
    case "window":
      return sameSize(facts.captured, facts.ownWindow, dpr) ? mirror : picture;
    case "screen":
      if (facts.screenExtended === false) return mirror;
      return sameSize(facts.captured, facts.ownScreen, dpr) ? mirror : picture;
    case "unknown":
      if (sameSize(facts.captured, facts.ownWindow, dpr)) return { kind: "mirror", surface: "window" };
      if (sameSize(facts.captured, facts.ownScreen, dpr)) return { kind: "mirror", surface: "screen" };
      return picture;
  }
}

/** Two views are the same. */
export function sameView(a: OwnShareView | null, b: OwnShareView | null): boolean {
  if (a === null || b === null) return a === b;
  if (a.kind !== b.kind) return false;
  return a.kind === "picture" || (b.kind === "mirror" && a.surface === b.surface);
}

/** The handle this page sets for Chromium's Capture Handle; random per page
 *  load, never sent anywhere. */
let ownHandle: string | null = null;

/**
 * Let Chromium tell a capture of this very tab (section 3.1, rule 2). Called
 * before the picker opens. Where the API is missing nothing happens.
 */
export function setOwnCaptureHandle(): void {
  if (ownHandle !== null) return;
  try {
    const md = globalThis.navigator?.mediaDevices as
      | (MediaDevices & { setCaptureHandleConfig?: (c: unknown) => void })
      | undefined;
    if (typeof md?.setCaptureHandleConfig !== "function") return;
    const bytes = new Uint8Array(16);
    globalThis.crypto.getRandomValues(bytes);
    const handle = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
    md.setCaptureHandleConfig({ handle, exposeOrigin: false, permittedOrigins: [globalThis.location.origin] });
    ownHandle = handle;
  } catch {
    // A browser that refuses leaves rule 2 unanswered.
  }
}

/** Rule 2's answer for `track`. */
export function captureHandleIsOwn(track: MediaStreamTrack): boolean | undefined {
  try {
    const t = track as MediaStreamTrack & { getCaptureHandle?: () => { handle?: string } | null };
    if (typeof t.getCaptureHandle !== "function") return undefined;
    const h = t.getCaptureHandle();
    if (h === null || h === undefined || typeof h.handle !== "string" || h.handle === "") return undefined;
    return ownHandle !== null && h.handle === ownHandle;
  } catch {
    return undefined;
  }
}

/** The page's facts for `track`. Never throws. */
export function browserMirrorFacts(
  track: MediaStreamTrack,
  host: {
    readonly screens: number | undefined;
    readonly window: PixelSize | undefined;
    readonly screen: PixelSize | undefined;
  } | undefined,
): MirrorFacts {
  let settings: MediaTrackSettings & { displaySurface?: string } = {};
  try {
    settings = track.getSettings();
  } catch {
    settings = {};
  }
  const w = globalThis.window;
  const scr = globalThis.screen as (Screen & { isExtended?: boolean }) | undefined;
  const ownWindow =
    w !== undefined && w.outerWidth > 0 && w.outerHeight > 0
      ? { width: w.outerWidth, height: w.outerHeight }
      : undefined;
  const ownScreen =
    scr !== undefined && scr.width > 0 && scr.height > 0 ? { width: scr.width, height: scr.height } : undefined;
  const pageExtended = typeof scr?.isExtended === "boolean" ? scr.isExtended : undefined;
  const hostExtended = host?.screens === undefined ? undefined : host.screens > 1;
  return {
    displaySurface: typeof settings.displaySurface === "string" ? settings.displaySurface : undefined,
    captured: { width: settings.width, height: settings.height },
    captureHandleIsOwn: captureHandleIsOwn(track),
    // The host's sizes are in device pixels already; `sameSize` also tries
    // them at scale 1.
    ownWindow: host?.window ?? ownWindow,
    ownScreen: host?.screen ?? ownScreen,
    devicePixelRatio: w?.devicePixelRatio ?? 1,
    screenExtended: hostExtended ?? pageExtended,
  };
}

/** One log line of what the decision read: kinds and sizes, no identifier. */
export function mirrorFactsLine(facts: MirrorFacts, view: OwnShareView): string {
  const size = (s: CapturedSize | undefined): string =>
    s === undefined || s.width === undefined || s.height === undefined ? "-" : `${s.width}x${s.height}`;
  return [
    `surface=${facts.displaySurface ?? "-"}`,
    `captured=${size(facts.captured)}`,
    `window=${size(facts.ownWindow)}`,
    `screen=${size(facts.ownScreen)}`,
    `dpr=${facts.devicePixelRatio}`,
    `extended=${facts.screenExtended ?? "-"}`,
    `handle=${facts.captureHandleIsOwn ?? "-"}`,
    `view=${view.kind === "picture" ? "picture" : `mirror-${view.surface}`}`,
  ].join(" ");
}
