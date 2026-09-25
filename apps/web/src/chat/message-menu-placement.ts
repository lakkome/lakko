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
 * uses it measures the rectangles and does as it is told.
 *
 * ## Two surfaces, one footprint
 *
 * `docs/ux/message-actions.md` sections 2 and 7: the reaction row and the
 * action card are two separate floating surfaces with a visible gap between
 * them, the row above the card or below it when there is no room above, both
 * fully inside the viewport and neither over the bubble's own controls.
 *
 * `messageMenuLayout` does not grow a second copy of the rule above for the
 * row. It stacks the two surfaces into ONE footprint - the row, the gap, the
 * card - and hands that footprint to `messageMenuPlacement` as if it were a
 * single menu. Everything the rule guarantees of a rectangle it places (inside
 * the viewport, outside the bubble unless `clamped`) is then true of the
 * footprint, and so of each surface inside it, by containment. A second
 * placement for the row would have to re-prove both properties and would
 * disagree with the first at exactly the edges where it matters.
 *
 * Inside the footprint the two surfaces share the edge that faces the bubble:
 * left edges when the footprint is right of the bubble or above / below it
 * (where it opens at the pointer), right edges when it is left of the bubble.
 * Without that, the wider row would hang past the narrower card toward the
 * bubble - still inside the footprint, but the card would float away from the
 * message it belongs to.
 *
 * The card opens at the pointer, and the row goes above it when there is room
 * between the pointer and the top of the viewport for the row and the gap;
 * otherwise below. That decision reads the pointer and the sizes only, never a
 * previous menu, which is why reopening on the same message lands in the same
 * place.
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

/** Where the reaction row sits relative to the action card. `alone` when there
 *  is no card to sit relative to: a message with nothing to copy and nothing
 *  to edit offers reactions only. */
export type ReactionRowAt = "above" | "below" | "alone";

export interface SurfacePosition {
  readonly left: number;
  readonly top: number;
}

export interface MessageMenuLayout {
  /** The footprint of both surfaces and the gap between them, placed by
   *  `messageMenuPlacement`. `side` is that rule's answer for the footprint. */
  readonly block: MenuPlacement & MenuSize;
  readonly rowAt: ReactionRowAt;
  /** Viewport coordinates of the reaction row. */
  readonly row: SurfacePosition;
  /** Viewport coordinates of the action card, `null` when there is none. */
  readonly card: SurfacePosition | null;
}

export interface MessageMenuLayoutInput {
  readonly bubble: MenuAnchorRect;
  readonly pointer: { readonly x: number; readonly y: number };
  /** The measured reaction row. */
  readonly row: MenuSize;
  /** The measured action card, or `null` when it is not drawn. */
  readonly card: MenuSize | null;
  readonly viewport: MenuViewport;
  readonly away: "left" | "right";
  /** Space between the footprint and the bubble. */
  readonly gap: number;
  /** Space between the reaction row and the action card. */
  readonly between: number;
  /** Space kept clear at the viewport edges. */
  readonly margin: number;
}

export function messageMenuLayout(input: MessageMenuLayoutInput): MessageMenuLayout {
  const { bubble, pointer, row, card, viewport, away, gap, between, margin } = input;

  if (card === null) {
    const p = messageMenuPlacement({ bubble, pointer, menu: row, viewport, away, gap, margin });
    return {
      block: { ...p, width: row.width, height: row.height },
      rowAt: "alone",
      row: { left: p.left, top: p.top },
      card: null,
    };
  }

  const rowAt: ReactionRowAt = pointer.y - between - row.height >= margin ? "above" : "below";
  const size: MenuSize = {
    width: Math.max(row.width, card.width),
    height: row.height + between + card.height,
  };
  // The footprint's top such that the CARD lands at the pointer: with the row
  // above, that is the row's height and the gap higher.
  const anchorY = rowAt === "above" ? pointer.y - between - row.height : pointer.y;
  const p = messageMenuPlacement({
    bubble,
    pointer: { x: pointer.x, y: anchorY },
    menu: size,
    viewport,
    away,
    gap,
    margin,
  });

  // Share the edge that faces the bubble; see the module comment.
  const alignRight = (p.side === "away" || p.side === "near") && p.left < bubble.left;
  const leftOf = (w: number): number => (alignRight ? p.left + size.width - w : p.left);

  const rowTop = rowAt === "above" ? p.top : p.top + card.height + between;
  const cardTop = rowAt === "above" ? p.top + row.height + between : p.top;
  return {
    block: { ...p, ...size },
    rowAt,
    row: { left: leftOf(row.width), top: rowTop },
    card: { left: leftOf(card.width), top: cardTop },
  };
}

/** Whether a surface of `size` at `at` intersects the bubble. The same question
 *  as `placementOverlapsBubble`, asked of one surface of the layout. */
export function surfaceOverlapsBubble(
  at: SurfacePosition,
  size: MenuSize,
  bubble: MenuAnchorRect,
): boolean {
  return placementOverlapsBubble({ ...at, side: "clamped" }, size, bubble);
}
