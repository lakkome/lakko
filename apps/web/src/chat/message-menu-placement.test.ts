// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The web menu opens at the pointer, over the message, like Telegram's
 * (`docs/ux/message-actions.md` section 7). These tests pin the geometry that
 * makes that true and nothing about the bubble - the module no longer knows
 * where the bubble is, on purpose.
 */
import { describe, expect, it } from "vitest";

import {
  messageMenuLayout,
  type MenuSize,
  type MessageMenuLayout,
  type MessageMenuLayoutInput,
  type SurfacePosition,
} from "./message-menu-placement";

const ROW: MenuSize = { width: 234, height: 34 };
const CARD: MenuSize = { width: 118, height: 104 };
const BETWEEN = 8;
const MARGIN = 8;
const VP = { width: 1280, height: 800 };

function layout(over: Partial<MessageMenuLayoutInput>): MessageMenuLayout {
  return messageMenuLayout({
    pointer: { x: 400, y: 300 },
    row: ROW,
    card: CARD,
    viewport: VP,
    between: BETWEEN,
    margin: MARGIN,
    ...over,
  });
}

function inside(at: SurfacePosition, size: MenuSize, vp: { width: number; height: number }): boolean {
  return (
    at.left >= MARGIN &&
    at.top >= MARGIN &&
    at.left + size.width <= vp.width - MARGIN &&
    at.top + size.height <= vp.height - MARGIN
  );
}

describe("messageMenuLayout", () => {
  it("puts the card's top-left corner at the pointer, with the row above it", () => {
    const l = layout({});
    expect(l.card).toEqual({ left: 400, top: 300 });
    expect(l.rowAt).toBe("above");
    // The row sits the gap above the card, sharing its left edge.
    expect(l.row.left).toBe(400);
    expect(l.row.top).toBe(300 - BETWEEN - ROW.height);
    expect(l.block.align).toBe("left");
  });

  it("flips to open leftward near the right edge, with the card's right edge at the pointer", () => {
    const l = layout({ pointer: { x: 1200, y: 300 } });
    expect(l.block.align).toBe("right");
    // The block's right edge is the pointer; each surface ends there.
    expect(l.row.left + ROW.width).toBe(1200);
    expect(l.card!.left + CARD.width).toBe(1200);
    expect(l.card!.top).toBe(300);
  });

  it("puts the row below the card when there is no room above", () => {
    const l = layout({ pointer: { x: 400, y: 20 } });
    expect(l.rowAt).toBe("below");
    expect(l.card!.top).toBe(20);
    expect(l.row.top).toBe(20 + CARD.height + BETWEEN);
  });

  it("lifts the whole block near the bottom edge, in one piece, order kept", () => {
    const l = layout({ pointer: { x: 400, y: 790 } });
    expect(inside(l.row, ROW, VP)).toBe(true);
    expect(inside(l.card!, CARD, VP)).toBe(true);
    // Still row above card, still one gap between them: the block moved, the
    // surfaces did not rearrange.
    expect(l.rowAt).toBe("above");
    expect(l.card!.top - (l.row.top + ROW.height)).toBe(BETWEEN);
  });

  it("draws the row alone, at the pointer, when there is no card", () => {
    const l = layout({ card: null });
    expect(l.rowAt).toBe("alone");
    expect(l.card).toBeNull();
    expect(l.row).toEqual({ left: 400, top: 300 });
    expect(l.block.height).toBe(ROW.height);
  });

  it("lands in the same place when reopened on the same message", () => {
    const a = layout({ pointer: { x: 640, y: 420 } });
    const b = layout({ pointer: { x: 640, y: 420 } });
    expect(b).toEqual(a);
  });

  it("keeps both surfaces inside the viewport, sharing an edge, from every pointer", () => {
    for (const vp of [VP, { width: 390, height: 844 }, { width: 2560, height: 1440 }]) {
      for (let x = 0; x <= vp.width; x += Math.max(1, Math.floor(vp.width / 16))) {
        for (let y = 0; y <= vp.height; y += Math.max(1, Math.floor(vp.height / 16))) {
          const l = layout({ pointer: { x, y }, viewport: vp });
          expect(inside(l.row, ROW, vp), `row at ${x},${y} in ${vp.width}x${vp.height}`).toBe(true);
          expect(inside(l.card!, CARD, vp), `card at ${x},${y} in ${vp.width}x${vp.height}`).toBe(true);
          // One gap between the two surfaces, whichever is on top.
          const upper = l.rowAt === "above" ? { at: l.row, size: ROW } : { at: l.card!, size: CARD };
          const lower = l.rowAt === "above" ? { at: l.card!, size: CARD } : { at: l.row, size: ROW };
          expect(lower.at.top - (upper.at.top + upper.size.height)).toBe(BETWEEN);
          // Shared edge: left edges when opening rightward, right edges otherwise.
          if (l.block.align === "left") expect(l.row.left).toBe(l.card!.left);
          else expect(l.row.left + ROW.width).toBe(l.card!.left + CARD.width);
        }
      }
    }
  });

  it("is anchored to the pointer whenever the viewport allows it", () => {
    // Away from every edge the card is exactly at the pointer; that is the
    // whole point, and a test on the clamped corners alone would not notice a
    // regression that quietly offset every menu by a fixed amount.
    for (const [x, y] of [
      [300, 300],
      [640, 400],
      [900, 200],
    ] as const) {
      const l = layout({ pointer: { x, y } });
      expect(l.card).toEqual({ left: x, top: y });
    }
  });
});
