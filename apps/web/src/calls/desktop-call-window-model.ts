// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The collapsed call as a window of the operating system, in the desktop app
 * (`docs/ux/calls.md` section 4.5.4a).
 *
 * The call's media live in the main window's page and a second webview cannot
 * receive a `MediaStream`. So the main page relays what the window shows: a
 * small description ({@link CallWindowView}) whenever it changes, and the
 * picture and the own-camera inset as compressed images at a modest rate,
 * drawn from the very `<video>` elements the in-app window would show. The
 * host (`apps/desktop/src-tauri/src/call_window.rs`) holds the latest of each
 * and hands them to the second window, which draws section 4.5.1's layout
 * from them and sends back two things: a press of the mute button, and a
 * click anywhere else.
 *
 * Pure: no Solid, no DOM beyond the structural shapes below, so the rules are
 * unit-tested. The main page's glue is `desktop-call-window-relay.ts`; the
 * second window's page is `DesktopCallWindowPage.tsx`.
 */

/** Width of the OS window in points (section 4.5.2, a desktop window). */
export const OS_CALL_WINDOW_WIDTH = 320;
/** Height of the OS window in points: 16:9. */
export const OS_CALL_WINDOW_HEIGHT = 180;

/**
 * How often a picture frame is sent: 10 a second. Chosen, not measured: a
 * talking face in a 320-point window reads as moving at this rate, and the
 * cost (an encode in the main page, a copy through the host) stays small.
 */
export const FRAME_INTERVAL_MS = 100;
/**
 * The image's pixels per point: the main page's device pixel ratio, at most 2,
 * so the picture is 640x360 on a Retina screen. Chosen, not measured.
 */
export const MAX_FRAME_SCALE = 2;
/** JPEG quality of a frame. Chosen, not measured. */
export const FRAME_JPEG_QUALITY = 0.72;
/**
 * The inset's share of the window's width and its shape: the in-app window's
 * landscape inset (`styles.css` `.lk-call-window[data-shape="landscape"]
 * .lk-call-window-inset`), 22 percent wide, 4:3.
 */
export const INSET_WIDTH_FRACTION = 0.22;
export const INSET_ASPECT = 4 / 3;

/** What the picture is, as the window draws it. */
export type CallWindowPicture =
  /** An image the main page sends: a camera, a photo, a shared screen. */
  | { readonly kind: "frame" }
  /** A camera that is on with no picture yet: the tile's blank state. */
  | { readonly kind: "blank" }
  /** No photo, no camera: the initials on the person's colour. */
  | { readonly kind: "initials"; readonly text: string; readonly tone: 1 | 2 | 3 | 4 };

/** The mute button as the call screen draws it (`call-controls.ts`). */
export interface CallWindowMute {
  readonly icon: string;
  readonly tone: string;
  readonly pressed: boolean | null;
  readonly label: string;
  readonly disabled: boolean;
}

/**
 * Everything the OS window shows besides the images, already in the
 * interface language. Decided in the main page by the same rules the in-app
 * window uses; the second window decides nothing.
 */
export interface CallWindowView {
  /** The state or the running time (`call_bar`). */
  readonly status: string;
  readonly encrypted: boolean;
  /** The name under the picture. */
  readonly name: string;
  /** `+N`; `0` prints nothing. */
  readonly others: number;
  readonly mute: CallWindowMute | null;
  readonly picture: CallWindowPicture;
  /** Your own camera's inset is drawn. */
  readonly inset: boolean;
  /** What a screen reader says for the window (the in-app window's button). */
  readonly spoken: string;
  /** The live region's line. */
  readonly announced: string;
  /** The window's region name. */
  readonly region: string;
  /** The interface language and direction, for the window's document. */
  readonly lang: string;
  readonly dir: "ltr" | "rtl";
}

const MAX_TEXT = 512;

function text(v: unknown): string | null {
  return typeof v === "string" && v.length <= MAX_TEXT ? v : null;
}

function parseMute(v: unknown): CallWindowMute | null | undefined {
  if (v === null) return null;
  if (typeof v !== "object" || v === undefined) return undefined;
  const o = v as Record<string, unknown>;
  const icon = text(o.icon);
  const tone = text(o.tone);
  const label = text(o.label);
  if (icon === null || tone === null || label === null) return undefined;
  if (typeof o.disabled !== "boolean") return undefined;
  if (o.pressed !== null && typeof o.pressed !== "boolean") return undefined;
  return { icon, tone, label, disabled: o.disabled, pressed: o.pressed };
}

function parsePicture(v: unknown): CallWindowPicture | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  if (o.kind === "frame") return { kind: "frame" };
  if (o.kind === "blank") return { kind: "blank" };
  if (o.kind === "initials") {
    const t = text(o.text);
    const tone = o.tone;
    if (t === null || (tone !== 1 && tone !== 2 && tone !== 3 && tone !== 4)) return null;
    return { kind: "initials", text: t, tone };
  }
  return null;
}

/**
 * Read a view the host handed the second window. Anything malformed is
 * `null`, and the window then shows nothing rather than half a state.
 */
export function parseCallWindowView(v: unknown): CallWindowView | null {
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  const status = text(o.status);
  const name = text(o.name);
  const spoken = text(o.spoken);
  const announced = text(o.announced);
  const region = text(o.region);
  const lang = text(o.lang);
  const mute = parseMute(o.mute);
  const picture = parsePicture(o.picture);
  const others = o.others;
  if (
    status === null ||
    name === null ||
    spoken === null ||
    announced === null ||
    region === null ||
    lang === null ||
    mute === undefined ||
    picture === null ||
    typeof o.encrypted !== "boolean" ||
    typeof o.inset !== "boolean" ||
    typeof others !== "number" ||
    !Number.isInteger(others) ||
    others < 0 ||
    (o.dir !== "ltr" && o.dir !== "rtl")
  ) {
    return null;
  }
  return {
    status,
    encrypted: o.encrypted,
    name,
    others,
    mute,
    picture,
    inset: o.inset,
    spoken,
    announced,
    region,
    lang,
    dir: o.dir,
  };
}

/** Whether two views would draw the same; the page posts only a change.
 *  Compared with the keys sorted, so the order they were written in does not
 *  count as a change. */
export function sameView(a: CallWindowView | null, b: CallWindowView | null): boolean {
  return canonical(a) === canonical(b);
}

function canonical(v: unknown): string {
  return JSON.stringify(v, (_key, value: unknown) => {
    if (value === null || typeof value !== "object" || Array.isArray(value)) return value;
    const o = value as Record<string, unknown>;
    return Object.fromEntries(Object.keys(o).sort().map((k) => [k, o[k]]));
  });
}

// ---- drawing a video into a frame -------------------------------------------

/** A source rectangle in the video's pixels and the target in the canvas's. */
export interface DrawPlan {
  readonly sx: number;
  readonly sy: number;
  readonly sw: number;
  readonly sh: number;
  readonly dx: number;
  readonly dy: number;
  readonly dw: number;
  readonly dh: number;
}

/**
 * How a `srcW` x `srcH` picture fills a `dstW` x `dstH` image. `cover` crops
 * it to fill (a camera, as a tile draws it with `object-fit: cover`);
 * `contain` letterboxes it whole (a shared screen, which must not lose its
 * edges). `null` while the source has no size yet.
 */
export function drawPlan(
  srcW: number,
  srcH: number,
  dstW: number,
  dstH: number,
  fit: "cover" | "contain",
): DrawPlan | null {
  if (!(srcW > 0 && srcH > 0 && dstW > 0 && dstH > 0)) return null;
  const srcAspect = srcW / srcH;
  const dstAspect = dstW / dstH;
  if (fit === "cover") {
    if (srcAspect > dstAspect) {
      const sw = srcH * dstAspect;
      return { sx: (srcW - sw) / 2, sy: 0, sw, sh: srcH, dx: 0, dy: 0, dw: dstW, dh: dstH };
    }
    const sh = srcW / dstAspect;
    return { sx: 0, sy: (srcH - sh) / 2, sw: srcW, sh, dx: 0, dy: 0, dw: dstW, dh: dstH };
  }
  if (srcAspect > dstAspect) {
    const dh = dstW / srcAspect;
    return { sx: 0, sy: 0, sw: srcW, sh: srcH, dx: 0, dy: (dstH - dh) / 2, dw: dstW, dh };
  }
  const dw = dstH * srcAspect;
  return { sx: 0, sy: 0, sw: srcW, sh: srcH, dx: (dstW - dw) / 2, dy: 0, dw, dh: dstH };
}

/** The image size for a window `width` x `height` points wide on a screen of
 *  `devicePixelRatio`. */
export function frameSize(
  width: number,
  height: number,
  devicePixelRatio: number,
): { readonly width: number; readonly height: number } {
  const ratio = Number.isFinite(devicePixelRatio) && devicePixelRatio > 0 ? devicePixelRatio : 1;
  const scale = Math.min(MAX_FRAME_SCALE, Math.max(1, ratio));
  return { width: Math.round(width * scale), height: Math.round(height * scale) };
}

/** The structural part of a `<video>` the picking rule reads. */
export interface VideoLike {
  readonly readyState: number;
  readonly videoWidth: number;
  readonly videoHeight: number;
  readonly dataset: { readonly [key: string]: string | undefined };
}

/** `HTMLMediaElement.HAVE_CURRENT_DATA`. */
const HAVE_CURRENT_DATA = 2;

/**
 * The video a tile is showing, out of the elements it holds: a remote person's
 * tile layers a full and a light picture and marks the one underneath
 * `data-shown="false"` (`routes/Call.tsx` `RemoteVideo`). The first element
 * that is not marked hidden and has a frame to draw; `null` when none has.
 */
export function pickShownVideo<V extends VideoLike>(videos: readonly V[]): V | null {
  for (const v of videos) {
    if (v.dataset["shown"] === "false") continue;
    if (v.readyState < HAVE_CURRENT_DATA || v.videoWidth <= 0 || v.videoHeight <= 0) continue;
    return v;
  }
  return null;
}

// ---- the send schedule -------------------------------------------------------

/**
 * When a frame of one slot is sent. At most one every
 * {@link FRAME_INTERVAL_MS}; never while the previous one of that slot is
 * still being encoded or posted, so a slow encode or a busy host drops frames
 * instead of queueing them; and a still image (a photo) only when it changed.
 */
export class FrameSchedule {
  private lastSentAt = Number.NEGATIVE_INFINITY;
  private inFlight = false;
  private lastStill: string | null = null;

  /** Whether to capture a moving source (a video) now. */
  public dueForVideo(nowMs: number): boolean {
    if (this.inFlight) return false;
    if (nowMs - this.lastSentAt < FRAME_INTERVAL_MS) return false;
    return true;
  }

  /** Whether to capture a still source with `key` (its URL) now. */
  public dueForStill(key: string): boolean {
    if (this.inFlight) return false;
    return key !== this.lastStill;
  }

  /** A capture started; `still` is the key of a still source, else `null`. */
  public started(nowMs: number, still: string | null): void {
    this.inFlight = true;
    this.lastSentAt = nowMs;
    this.lastStill = still;
  }

  /** The capture and its post are over, sent or not. */
  public finished(): void {
    this.inFlight = false;
  }

  /** The source went away: the next one, even the same photo, is sent. */
  public reset(): void {
    this.lastStill = null;
    this.lastSentAt = Number.NEGATIVE_INFINITY;
  }
}

// ---- a press on the OS window ---------------------------------------------------

/**
 * A press that moves less than this before release is a click (section
 * 4.5.4a: clicking brings the call back); past it, the OS moves the window.
 * The in-app window's threshold (`call-window-model.ts`).
 */
export const OS_WINDOW_DRAG_THRESHOLD_PX = 6;

/** Whether a press that has moved `dx`, `dy` since it began is now a drag. */
export function pressIsDrag(dx: number, dy: number): boolean {
  return Math.hypot(dx, dy) >= OS_WINDOW_DRAG_THRESHOLD_PX;
}

/**
 * Which bottom corner the inset sits in: away from the screen edge the window
 * is nearer to (section 4.5.1), from where the OS window is on its screen.
 */
export function osInsetCorner(
  windowLeft: number,
  windowWidth: number,
  screenLeft: number,
  screenWidth: number,
): "bottom-left" | "bottom-right" {
  const centre = windowLeft + windowWidth / 2;
  return centre >= screenLeft + screenWidth / 2 ? "bottom-left" : "bottom-right";
}
