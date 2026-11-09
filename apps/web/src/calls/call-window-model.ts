// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The collapsed call's floating window: where it is, how big it is, and what
 * it counts (`docs/ux/calls.md` sections 4.5.1 and 4.5.2).
 *
 * Section 4.5.2 says where a window lands is a fact about one device and one
 * hand, so each client writes its own placement and the numbers in the spec
 * are the rule all three follow. This module is the web client's copy of
 * those numbers. What the window SHOWS is shared information and is not
 * decided here: the person is the shared `call_stage` rule
 * (`group-call-layout.ts` `stagePerson`), the picture size it asks the SFU for
 * is `tile_video_quality` (`video-wants.ts`), and whether the window is up at
 * all and what its one status line says is `call_bar`
 * (`call-bar-model.ts`).
 *
 * Pure: no Solid, no DOM. `CallWindow.tsx` measures the viewport and the
 * pointer and draws the answer.
 */

/** The four places the window comes to rest. */
export type CallWindowCorner = "top-left" | "top-right" | "bottom-left" | "bottom-right";

/** Gap between the window and the edges it rests against (section 4.5.2). */
export const CALL_WINDOW_EDGE_PX = 16;
/** Width of the window on a tablet, a desktop browser or a wide web window. */
export const CALL_WINDOW_WIDE_WIDTH_PX = 320;
/** A viewport whose narrow side is under this is a phone (section 4.5.2). */
export const CALL_WINDOW_PHONE_NARROW_SIDE_PX = 600;
/** On a phone the window is this fraction of the viewport width ... */
export const CALL_WINDOW_PHONE_WIDTH_FRACTION = 0.36;
/** ... and never narrower than this. */
export const CALL_WINDOW_PHONE_MIN_WIDTH_PX = 120;
/**
 * A press that moves less than this before it is released is a click, not a
 * drag: the window under it expands instead of moving. Chosen, not measured;
 * the same order as the swipe thresholds elsewhere in the call screen.
 */
export const CALL_WINDOW_DRAG_THRESHOLD_PX = 6;
/**
 * How far ahead of the release the window is projected to come to rest:
 * `velocity x this`. Section 4.5.2 measures the nearest corner from where it
 * would come to rest given how fast it was moving, so a flick reaches the
 * corner it was flicked at. Chosen, not measured.
 */
export const CALL_WINDOW_PROJECTION_MS = 180;

/** The space the window floats in, in CSS pixels. */
export interface CallWindowViewport {
  readonly width: number;
  readonly height: number;
}

/** A width and a height in CSS pixels. */
export interface CallWindowSize {
  readonly width: number;
  readonly height: number;
}

/** A point in CSS pixels, from the viewport's top left. */
export interface CallWindowPoint {
  readonly x: number;
  readonly y: number;
}

/**
 * Whether this viewport is a phone in the spec's sense: the narrow side of the
 * screen under 600. Read off the viewport because that is the screen the web
 * client has.
 */
export function isPhoneViewport(viewport: CallWindowViewport): boolean {
  return Math.min(viewport.width, viewport.height) < CALL_WINDOW_PHONE_NARROW_SIDE_PX;
}

/**
 * The window's size (section 4.5.2's table).
 *
 * Phone: 3:4 upright, 36% of the width, at least 120. Everywhere else: 16:9
 * lying, 320 wide. While the picture is a screen share: 16:9 at the same
 * width. Never larger than the viewport less its safe insets and two edge
 * gaps, so a tiny window still has the window on screen.
 */
export function callWindowSize(
  viewport: CallWindowViewport,
  showingScreen: boolean,
  insets: CallWindowInsets = NO_INSETS,
): CallWindowSize {
  const phone = isPhoneViewport(viewport);
  const preferred = phone
    ? Math.max(CALL_WINDOW_PHONE_MIN_WIDTH_PX, Math.round(viewport.width * CALL_WINDOW_PHONE_WIDTH_FRACTION))
    : CALL_WINDOW_WIDE_WIDTH_PX;
  const room = Math.max(
    0,
    Math.floor(viewport.width) - safe(insets.left) - safe(insets.right) - 2 * CALL_WINDOW_EDGE_PX,
  );
  const width = Math.max(0, Math.min(preferred, room));
  const upright = phone && !showingScreen;
  const height = upright ? Math.round((width * 4) / 3) : Math.round((width * 9) / 16);
  const roomHigh = Math.max(
    0,
    Math.floor(viewport.height) - safe(insets.top) - safe(insets.bottom) - 2 * CALL_WINDOW_EDGE_PX,
  );
  if (height <= roomHigh) return { width, height };
  // A viewport too short for the shape: keep the shape, shrink both.
  const scale = roomHigh / Math.max(1, height);
  return { width: Math.floor(width * scale), height: roomHigh };
}

/** The screen's safe-area insets (notches, system bars), in CSS pixels. */
export interface CallWindowInsets {
  readonly top: number;
  readonly right: number;
  readonly bottom: number;
  readonly left: number;
}

/** No insets: a desktop browser, or a page that is not drawn under any bar. */
export const NO_INSETS: CallWindowInsets = { top: 0, right: 0, bottom: 0, left: 0 };

/** An inset that is not a finite, non-negative number is read as none. */
function safe(n: number): number {
  return Number.isFinite(n) && n > 0 ? n : 0;
}

/**
 * The window's top left when it rests in `corner`.
 *
 * 16 in from the SAFE edges (section 4.5.2: it never covers the system bars),
 * so each inset is added to the gap on its side. `topClearance` is how far
 * down the screen's own header reaches, measured from the viewport's top; the
 * top corners sit 16 below whichever is lower, the header or the top inset,
 * so the window covers neither a header nor (from the start corner) the
 * composer at the bottom.
 */
export function cornerPosition(
  corner: CallWindowCorner,
  size: CallWindowSize,
  viewport: CallWindowViewport,
  topClearance: number,
  insets: CallWindowInsets = NO_INSETS,
): CallWindowPoint {
  const left = corner === "top-left" || corner === "bottom-left";
  const top = corner === "top-left" || corner === "top-right";
  const x = left
    ? safe(insets.left) + CALL_WINDOW_EDGE_PX
    : viewport.width - safe(insets.right) - CALL_WINDOW_EDGE_PX - size.width;
  const clearTop = Math.max(safe(topClearance), safe(insets.top)) + CALL_WINDOW_EDGE_PX;
  const bottomY = viewport.height - safe(insets.bottom) - CALL_WINDOW_EDGE_PX - size.height;
  // A short viewport where the cleared top would push the window past the
  // bottom keeps it on screen rather than under the fold.
  const y = top
    ? Math.min(clearTop, Math.max(safe(insets.top) + CALL_WINDOW_EDGE_PX, bottomY))
    : bottomY;
  return { x: Math.max(0, x), y: Math.max(0, y) };
}

/**
 * Where a window released at `position`, moving at `velocity` (px per ms),
 * would come to rest if nothing caught it.
 */
export function projectedRest(position: CallWindowPoint, velocity: CallWindowPoint): CallWindowPoint {
  const vx = Number.isFinite(velocity.x) ? velocity.x : 0;
  const vy = Number.isFinite(velocity.y) ? velocity.y : 0;
  return {
    x: position.x + vx * CALL_WINDOW_PROJECTION_MS,
    y: position.y + vy * CALL_WINDOW_PROJECTION_MS,
  };
}

/**
 * The corner nearest a window whose top left is at `rest`: by which half of
 * the viewport its centre is in, across and down. A centre exactly on a
 * midline goes right and down, the start corner's column and the composer's
 * row being the two it is least surprising to land in.
 */
export function nearestCorner(
  rest: CallWindowPoint,
  size: CallWindowSize,
  viewport: CallWindowViewport,
): CallWindowCorner {
  const cx = rest.x + size.width / 2;
  const cy = rest.y + size.height / 2;
  const right = cx >= viewport.width / 2;
  const bottom = cy >= viewport.height / 2;
  if (bottom) return right ? "bottom-right" : "bottom-left";
  return right ? "top-right" : "top-left";
}

/**
 * The corner the own-camera inset sits in: the bottom corner of the window
 * away from the screen edge the window rests against (section 4.5.1).
 */
export function insetCorner(windowCorner: CallWindowCorner): "bottom-left" | "bottom-right" {
  return windowCorner === "top-right" || windowCorner === "bottom-right" ? "bottom-left" : "bottom-right";
}

/**
 * The `+N` at the bottom of the window in a group call: everyone beyond the
 * person shown and you (`+16` with 18 people). `0` means nothing is printed.
 *
 * `shownIsYou` is the window showing your own tile (you are alone), where the
 * only person to discount is you. `people` counts everyone in the call, you
 * included; a share on the picture counts as its sharer.
 */
export function othersBeyondShown(people: number, shownIsYou: boolean): number {
  const total = Math.max(0, Math.floor(people));
  return Math.max(0, shownIsYou ? total - 1 : total - 2);
}

/**
 * Whether the own-camera inset is drawn: only while your camera sends, and
 * not when the picture already is you (section 4.5.1).
 */
export function insetShown(ownCameraSending: boolean, shownIsYou: boolean): boolean {
  return ownCameraSending && !shownIsYou;
}

// ---- the corner kept for the session ---------------------------------------

/**
 * Section 4.5.2: the corner the window lands in is kept while the app runs and
 * is where the next collapsed call opens. Kept in memory, not in storage:
 * "while the app runs" is a page load, and a placement is never shared with
 * another device.
 */
let keptCorner: CallWindowCorner = "top-right";

/** The corner the next collapsed call opens in. */
export function rememberedCorner(): CallWindowCorner {
  return keptCorner;
}

/** Keep `corner` as where the next collapsed call opens. */
export function rememberCorner(corner: CallWindowCorner): void {
  keptCorner = corner;
}

/** Back to the start corner; for tests. */
export function resetRememberedCorner(): void {
  keptCorner = "top-right";
}

// ---- the drag --------------------------------------------------------------

/** What a pointer release on the window means. */
export type CallWindowRelease =
  | { readonly kind: "click" }
  | { readonly kind: "drop"; readonly corner: CallWindowCorner };

/**
 * One press on the window, from pointer down to pointer up.
 *
 * Until the pointer has moved {@link CALL_WINDOW_DRAG_THRESHOLD_PX} it is a
 * click and the window does not move, so a slightly shaky click still brings
 * the call back. Past it, the window follows the pointer, and on release it
 * goes to the corner nearest where it would come to rest. The velocity is
 * taken over the last stretch of movement, not the whole drag, so a slow drag
 * that ends in a flick is read as the flick.
 */
export class CallWindowDrag {
  private readonly startPointer: CallWindowPoint;
  private readonly startWindow: CallWindowPoint;
  private dragging = false;
  private current: CallWindowPoint;
  /** Recent pointer samples, newest last, for the release velocity. */
  private samples: { readonly x: number; readonly y: number; readonly t: number }[] = [];

  public constructor(pointer: CallWindowPoint, windowAt: CallWindowPoint, timeMs: number) {
    this.startPointer = pointer;
    this.startWindow = windowAt;
    this.current = windowAt;
    this.samples.push({ x: pointer.x, y: pointer.y, t: timeMs });
  }

  /** The pointer moved. Returns where the window is now, or `null` while the
   *  press is still a click and the window stays where it was. */
  public move(pointer: CallWindowPoint, timeMs: number): CallWindowPoint | null {
    this.samples.push({ x: pointer.x, y: pointer.y, t: timeMs });
    // A velocity over the last ~100 ms; older samples say nothing about the
    // release.
    while (this.samples.length > 2 && timeMs - (this.samples[0]?.t ?? timeMs) > 100) {
      this.samples.shift();
    }
    const dx = pointer.x - this.startPointer.x;
    const dy = pointer.y - this.startPointer.y;
    if (!this.dragging && Math.hypot(dx, dy) < CALL_WINDOW_DRAG_THRESHOLD_PX) return null;
    this.dragging = true;
    this.current = { x: this.startWindow.x + dx, y: this.startWindow.y + dy };
    return this.current;
  }

  /** Whether the press has become a drag. */
  public isDragging(): boolean {
    return this.dragging;
  }

  /** The pointer was released at `timeMs`. A pointer that stood still for a
   *  moment before letting go was not flicking: no velocity. */
  public release(size: CallWindowSize, viewport: CallWindowViewport, timeMs: number): CallWindowRelease {
    if (!this.dragging) return { kind: "click" };
    const first = this.samples[0];
    const last = this.samples[this.samples.length - 1];
    let velocity: CallWindowPoint = { x: 0, y: 0 };
    const fresh = last !== undefined && timeMs - last.t <= 100;
    if (fresh && first !== undefined && last !== undefined && last.t > first.t) {
      const dt = last.t - first.t;
      velocity = { x: (last.x - first.x) / dt, y: (last.y - first.y) / dt };
    }
    const rest = projectedRest(this.current, velocity);
    return { kind: "drop", corner: nearestCorner(rest, size, viewport) };
  }
}
