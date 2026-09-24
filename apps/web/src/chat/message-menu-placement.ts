// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where a message's action menu goes, `docs/ux/client-redesign.md` section 5.2:
 * "The menu also never overlaps the bubble's own controls, whatever opens it."
 *
 * The rule the old strip broke was not a styling one. It was positioned INSIDE
 * the bubble (`right: 0.4rem` / `left: 0.4rem` on a `position: absolute` box)
 * and revealed on hover, so on a voice message it landed on the play button:
 * pressing play hit the strip's React button instead and the message paused.
 * A position expressed relative to the bubble's own box cannot be made safe by
 * choosing better numbers, because the bubble's contents vary - a voice bubble
 * has a play button at one edge, a waveform across the middle and a speed chip
 * at the other; a video note is a circle whose whole surface is the tap
 * target; a photo fills its bubble edge to edge. Any inside-the-box coordinate
 * is one layout change away from covering something again.
 *
 * So this module answers the question the only way that stays true: the menu
 * is placed OUTSIDE the bubble's bounding rectangle. It never asks what is in
 * the bubble, so it cannot be wrong about a bubble kind it has not met.
 *
 * Preference order, first fit wins:
 *
 *   1. the empty side of the row - right of an incoming bubble, left of an
 *      outgoing one (a bubble is capped at 78% of the row, so on a wide
 *      viewport there is always somewhere to go);
 *   2. the other side;
 *   3. above the bubble's top edge;
 *   4. below its bottom edge;
 *   5. `clamped` - the one case where overlap is unavoidable, and it is
 *      reported rather than hidden. It needs a bubble that is both too wide
 *      for either side and too tall for above or below, which on a phone a
 *      long text message reaches. The menu is then clamped into the viewport
 *      at the pointer. Even there it cannot repeat the reported defect: it
 *      exists only after a deliberate right-click or long-press, and the next
 *      press anywhere outside it dismisses it rather than activating what is
 *      underneath.
 *
 * Pure and total: no DOM, no clock, no reads of anything the caller did not
 * pass in, and every branch is reachable from a unit test. The component that
 * uses it measures the two rectangles and does as it is told.
 */

/** The part of a DOM rect this decision needs. Viewport coordinates. */
export interface MenuAnchorRect {
  readonly left: number;
  readonly top: number;
  readonly right: number;
  readonly bottom: number;
}

export interface MenuSize {
  readonly width: number;
  readonly height: number;
}

export interface MenuViewport {
  readonly width: number;
  readonly height: number;
}

/** Which of the five rules produced the answer. `clamped` is the only one that
 *  may overlap the bubble; the rest are outside it by construction. */
export type MenuSide = "away" | "near" | "above" | "below" | "clamped";

export interface MenuPlacement {
  readonly left: number;
  readonly top: number;
  readonly side: MenuSide;
}

export interface MenuPlacementInput {
  /** The bubble the menu belongs to, in viewport coordinates. */
  readonly bubble: MenuAnchorRect;
  /** Where the gesture happened. A keyboard trigger has no pointer, so the
   *  caller passes the bubble's own top-left corner and the menu opens there. */
  readonly pointer: { readonly x: number; readonly y: number };
  /** The measured menu, after it has rendered once invisibly. */
  readonly menu: MenuSize;
  readonly viewport: MenuViewport;
  /** The side of the row the bubble is NOT on: `right` for an incoming
   *  bubble, `left` for an outgoing one. Tried first because it is the empty
   *  half of the row, so the menu covers no other message either. */
  readonly away: "left" | "right";
  /** Space between the bubble's edge and the menu. */
  readonly gap: number;
  /** Space kept clear at the viewport edges. */
  readonly margin: number;
}

function clamp(value: number, low: number, high: number): number {
  // `high < low` when the menu is larger than the space it must fit in; the
  // low edge wins, so the menu stays reachable rather than being pushed off
  // the top or the left where nothing could dismiss it.
  if (high < low) return low;
  return Math.min(high, Math.max(low, value));
}

export function messageMenuPlacement(input: MenuPlacementInput): MenuPlacement {
  const { bubble, pointer, menu, viewport, gap, margin } = input;
  const maxLeft = viewport.width - menu.width - margin;
  const maxTop = viewport.height - menu.height - margin;
  const verticalAtPointer = clamp(pointer.y, margin, maxTop);
  const horizontalAtPointer = clamp(pointer.x, margin, maxLeft);

  const rightLeft = bubble.right + gap;
  const leftLeft = bubble.left - gap - menu.width;
  const fitsRight = rightLeft >= margin && rightLeft + menu.width + margin <= viewport.width;
  const fitsLeft = leftLeft >= margin && leftLeft + menu.width + margin <= viewport.width;

  const away = input.away === "right" ? fitsRight : fitsLeft;
  const near = input.away === "right" ? fitsLeft : fitsRight;
  if (away) {
    return {
      left: input.away === "right" ? rightLeft : leftLeft,
      top: verticalAtPointer,
      side: "away",
    };
  }
  if (near) {
    return {
      left: input.away === "right" ? leftLeft : rightLeft,
      top: verticalAtPointer,
      side: "near",
    };
  }

  const aboveTop = bubble.top - gap - menu.height;
  if (aboveTop >= margin) {
    return { left: horizontalAtPointer, top: aboveTop, side: "above" };
  }
  const belowTop = bubble.bottom + gap;
  if (belowTop + menu.height + margin <= viewport.height) {
    return { left: horizontalAtPointer, top: belowTop, side: "below" };
  }
  return { left: horizontalAtPointer, top: verticalAtPointer, side: "clamped" };
}

/** Whether a placement's rectangle intersects the bubble's. The property every
 *  non-`clamped` answer must satisfy; exported so the caller and the tests ask
 *  the same question rather than two spellings of it. */
export function placementOverlapsBubble(
  placement: MenuPlacement,
  menu: MenuSize,
  bubble: MenuAnchorRect,
): boolean {
  const right = placement.left + menu.width;
  const bottom = placement.top + menu.height;
  return (
    placement.left < bubble.right &&
    right > bubble.left &&
    placement.top < bubble.bottom &&
    bottom > bubble.top
  );
}
