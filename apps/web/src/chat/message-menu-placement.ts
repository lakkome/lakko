// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where a message's action menu goes on web: at the pointer, over the message,
 * the way Telegram's does. `docs/ux/message-actions.md` section 7.
 *
 * ## What this used to be, and why it changed
 *
 * The first version of this module placed the menu OUTSIDE the bubble's
 * rectangle - beside it, then above or below it - and overlapped the bubble
 * only as a reported last resort. That answered a real defect: a strip that
 * appeared on HOVER, positioned inside the bubble, landed on a voice message's
 * play button and stole the click. But the cure outlived its disease. The
 * hover strip is gone (`client-redesign.md`: the menu opens on a deliberate
 * gesture and on nothing else), and a menu that a person has just right-clicked
 * for is not going to intercept a play they are not about to press. What the
 * outside-the-bubble rule DID do, on a wide viewport with a bubble at the far
 * edge, was put the menu somewhere off to the side and make the person travel
 * to it - which the maintainer saw next to Telegram and called out by name
 * (2026-09-26).
 *
 * So the rule is now the one every desktop context menu follows: the card's
 * near corner is at the pointer, the reaction row floats above it (below when
 * there is no room above), both surfaces share the edge that meets the pointer,
 * and the only thing that moves the block is the viewport's edge. Whether the
 * block covers the bubble is not a question this module asks.
 *
 * Pure and total: no DOM, no clock, nothing read that the caller did not pass.
 * Reopening on the same message with the same pointer lands in the same place,
 * because nothing here remembers a previous menu.
 */

export interface MenuSize {
  readonly width: number;
  readonly height: number;
}

export interface MenuViewport {
  readonly width: number;
  readonly height: number;
}

export interface SurfacePosition {
  readonly left: number;
  readonly top: number;
}

/** Where the reaction row sits relative to the action card. `alone` when there
 *  is no card to sit relative to: a message with nothing to copy and nothing
 *  to edit offers reactions only. */
export type ReactionRowAt = "above" | "below" | "alone";

/**
 * Which vertical edge of the block sits at the pointer's x. `left` is the
 * ordinary case - the block opens to the right of the pointer. `right` is the
 * flip near the viewport's right edge: the block opens to the left instead,
 * with its right edge at the pointer, which is what keeps a menu opened on an
 * outgoing bubble at the far right from being pushed away from where the
 * person clicked.
 */
export type MenuAlign = "left" | "right";

export interface MessageMenuLayout {
  /** The footprint of both surfaces and the gap between them. */
  readonly block: SurfacePosition & MenuSize & { readonly align: MenuAlign };
  readonly rowAt: ReactionRowAt;
  /** Viewport coordinates of the reaction row. */
  readonly row: SurfacePosition;
  /** Viewport coordinates of the action card, `null` when there is none. */
  readonly card: SurfacePosition | null;
}

export interface MessageMenuLayoutInput {
  /** Where the gesture happened. A keyboard trigger has no pointer, so the
   *  caller passes the bubble's own top-left corner and the menu opens there. */
  readonly pointer: { readonly x: number; readonly y: number };
  /** The measured reaction row. */
  readonly row: MenuSize;
  /** The measured action card, or `null` when it is not drawn. */
  readonly card: MenuSize | null;
  readonly viewport: MenuViewport;
  /** Space between the reaction row and the action card. */
  readonly between: number;
  /** Space kept clear at the viewport edges. */
  readonly margin: number;
}

function clamp(value: number, low: number, high: number): number {
  // `high < low` when the block is larger than the space it must fit in; the
  // low edge wins, so the block stays reachable rather than being pushed off
  // the top or the left where nothing could dismiss it.
  if (high < low) return low;
  return Math.min(high, Math.max(low, value));
}

export function messageMenuLayout(input: MessageMenuLayoutInput): MessageMenuLayout {
  const { pointer, row, card, viewport, between, margin } = input;

  const width = Math.max(row.width, card?.width ?? 0);
  const height = card === null ? row.height : row.height + between + card.height;

  // Horizontal: open to the right of the pointer when the block fits there,
  // else flip and open to the left with the block's right edge at the pointer.
  // Then clamp, for a viewport narrower than the block either way.
  const fitsRight = pointer.x + width + margin <= viewport.width;
  const align: MenuAlign = fitsRight ? "left" : "right";
  const left = clamp(
    align === "left" ? pointer.x : pointer.x - width,
    margin,
    viewport.width - width - margin,
  );

  // Vertical: the CARD lands at the pointer; the row goes above it when there
  // is room for the row and the gap between the pointer and the top edge,
  // otherwise below. A block that would run off the bottom is lifted just
  // enough, in one piece, so the order of the two surfaces never changes.
  const rowAt: ReactionRowAt =
    card === null ? "alone" : pointer.y - between - row.height >= margin ? "above" : "below";
  const wantedTop = rowAt === "above" ? pointer.y - between - row.height : pointer.y;
  const top = clamp(wantedTop, margin, viewport.height - height - margin);

  // Both surfaces share the edge that meets the pointer, so the narrower one
  // does not float away from where the gesture was.
  const leftOf = (w: number): number => (align === "left" ? left : left + width - w);

  if (card === null) {
    return {
      block: { left, top, width, height, align },
      rowAt,
      row: { left: leftOf(row.width), top },
      card: null,
    };
  }
  const rowTop = rowAt === "above" ? top : top + card.height + between;
  const cardTop = rowAt === "above" ? top + row.height + between : top;
  return {
    block: { left, top, width, height, align },
    rowAt,
    row: { left: leftOf(row.width), top: rowTop },
    card: { left: leftOf(card.width), top: cardTop },
  };
}
