// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  messageMenuLayout,
  messageMenuPlacement,
  placementOverlapsBubble,
  surfaceOverlapsBubble,
  type MenuAnchorRect,
  type MenuPlacementInput,
  type MenuSize,
  type MessageMenuLayout,
  type MessageMenuLayoutInput,
  type SurfacePosition,
} from "./message-menu-placement";

const MENU: MenuSize = { width: 220, height: 160 };

function place(over: Partial<MenuPlacementInput>): ReturnType<typeof messageMenuPlacement> {
  const base: MenuPlacementInput = {
    bubble: { left: 40, top: 300, right: 400, bottom: 380 },
    pointer: { x: 200, y: 340 },
    menu: MENU,
    viewport: { width: 1200, height: 800 },
    away: "right",
    gap: 8,
    margin: 8,
  };
  return messageMenuPlacement({ ...base, ...over });
}

describe("messageMenuPlacement", () => {
  it("puts the menu on the empty side of the row for an incoming bubble", () => {
    const p = place({});
    expect(p.side).toBe("away");
    // Right of the bubble's right edge, by the gap.
    expect(p.left).toBe(408);
  });

  it("puts it on the other empty side for an outgoing bubble", () => {
    const p = place({
      bubble: { left: 800, top: 300, right: 1160, bottom: 380 },
      away: "left",
      pointer: { x: 900, y: 340 },
    });
    expect(p.side).toBe("away");
    expect(p.left).toBe(800 - 8 - MENU.width);
  });

  it("opens at the pointer vertically, so it lands where the gesture was", () => {
    expect(place({ pointer: { x: 200, y: 340 } }).top).toBe(340);
    expect(place({ pointer: { x: 200, y: 120 } }).top).toBe(120);
  });

  it("falls back to the near side when the away side has no room", () => {
    // A bubble pushed hard right: nothing fits to its right, plenty to its left.
    const p = place({
      bubble: { left: 700, top: 300, right: 1120, bottom: 380 },
      away: "right",
    });
    expect(p.side).toBe("near");
    expect(p.left).toBe(700 - 8 - MENU.width);
  });

  it("goes above the bubble when neither side fits", () => {
    // Phone width: a bubble that spans nearly the row leaves no side room.
    const p = place({
      viewport: { width: 380, height: 800 },
      bubble: { left: 12, top: 400, right: 300, bottom: 470 },
      pointer: { x: 150, y: 430 },
      away: "right",
    });
    expect(p.side).toBe("above");
    expect(p.top).toBe(400 - 8 - MENU.height);
  });

  it("goes below when there is no room above either", () => {
    const p = place({
      viewport: { width: 380, height: 800 },
      bubble: { left: 12, top: 40, right: 300, bottom: 200 },
      pointer: { x: 150, y: 120 },
      away: "right",
    });
    expect(p.side).toBe("below");
    expect(p.top).toBe(208);
  });

  it("reports `clamped` rather than pretending, when the bubble leaves nowhere", () => {
    // A long text message on a phone: too wide for either side, too tall for
    // above or below. This is the ONE case the module admits an overlap.
    const p = place({
      viewport: { width: 380, height: 700 },
      bubble: { left: 12, top: 20, right: 360, bottom: 690 },
      pointer: { x: 150, y: 400 },
      away: "right",
    });
    expect(p.side).toBe("clamped");
  });

  it("keeps the menu inside the viewport on every branch", () => {
    const viewports = [
      { width: 1400, height: 900 },
      { width: 380, height: 700 },
      { width: 768, height: 420 },
    ];
    for (const viewport of viewports) {
      for (const away of ["left", "right"] as const) {
        for (let top = 0; top < viewport.height; top += 37) {
          for (let left = 0; left < viewport.width; left += 53) {
            const bubble: MenuAnchorRect = {
              left,
              top,
              right: Math.min(viewport.width, left + 300),
              bottom: Math.min(viewport.height, top + 90),
            };
            const p = messageMenuPlacement({
              bubble,
              pointer: { x: bubble.left + 10, y: bubble.top + 10 },
              menu: MENU,
              viewport,
              away,
              gap: 8,
              margin: 8,
            });
            // A menu pushed off the top or left could not be dismissed by a
            // press on its own backdrop-covered edge, so both stay >= 0.
            expect(p.left).toBeGreaterThanOrEqual(0);
            expect(p.top).toBeGreaterThanOrEqual(0);
          }
        }
      }
    }
  });

  it("never overlaps the bubble unless it says `clamped`", () => {
    // The property the module exists for, over a grid rather than one case:
    // the defect was a menu sitting on a bubble's own play button.
    const viewports = [
      { width: 1400, height: 900 },
      { width: 380, height: 700 },
      { width: 900, height: 500 },
    ];
    let sawAway = false;
    let sawAbove = false;
    for (const viewport of viewports) {
      for (const away of ["left", "right"] as const) {
        for (let top = 0; top + 40 < viewport.height; top += 29) {
          for (let left = 0; left + 40 < viewport.width; left += 41) {
            for (const [w, h] of [
              [280, 70],
              [140, 300],
              [340, 520],
            ] as const) {
              const bubble: MenuAnchorRect = {
                left,
                top,
                right: Math.min(viewport.width, left + w),
                bottom: Math.min(viewport.height, top + h),
              };
              const p = messageMenuPlacement({
                bubble,
                pointer: { x: bubble.left + 5, y: bubble.top + 5 },
                menu: MENU,
                viewport,
                away,
                gap: 8,
                margin: 8,
              });
              if (p.side === "away") sawAway = true;
              if (p.side === "above") sawAbove = true;
              if (p.side === "clamped") continue;
              expect(placementOverlapsBubble(p, MENU, bubble)).toBe(false);
            }
          }
        }
      }
    }
    // The grid actually exercises the branches it claims to.
    expect(sawAway).toBe(true);
    expect(sawAbove).toBe(true);
  });

  it("detects an overlap the way the defect looked: a menu inside the bubble", () => {
    const bubble: MenuAnchorRect = { left: 40, top: 300, right: 400, bottom: 380 };
    // The old strip: inside the box, 0.4rem from an edge, 14px above the top.
    expect(
      placementOverlapsBubble({ left: 46, top: 292, side: "clamped" }, MENU, bubble),
    ).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Two surfaces: `message-actions.md` sections 2 and 7. The reaction row and the
// action card are separate floating surfaces with a gap, the row above the card
// or below it when there is no room above, both inside the viewport, neither
// over the bubble.
// ---------------------------------------------------------------------------

/** Stand-ins for seven emoji and a plus, and a three-row card - not measured.
 *  What matters is the shape: the row is the wider and the shorter, as it is
 *  in the real menu, so the edge-sharing branch has something to do. */
const ROW: MenuSize = { width: 234, height: 34 };
const CARD: MenuSize = { width: 118, height: 104 };
const BETWEEN = 8;

function layout(over: Partial<MessageMenuLayoutInput>): MessageMenuLayout {
  const base: MessageMenuLayoutInput = {
    bubble: { left: 40, top: 300, right: 400, bottom: 380 },
    pointer: { x: 200, y: 340 },
    row: ROW,
    card: CARD,
    viewport: { width: 1200, height: 800 },
    away: "right",
    gap: 8,
    between: BETWEEN,
    margin: 8,
  };
  return messageMenuLayout({ ...base, ...over });
}

function inside(at: SurfacePosition, size: MenuSize, vp: { width: number; height: number }): boolean {
  return at.left >= 0 && at.top >= 0 && at.left + size.width <= vp.width && at.top + size.height <= vp.height;
}

function overlaps(a: SurfacePosition, as: MenuSize, b: SurfacePosition, bs: MenuSize): boolean {
  return (
    a.left < b.left + bs.width &&
    a.left + as.width > b.left &&
    a.top < b.top + bs.height &&
    a.top + as.height > b.top
  );
}

describe("messageMenuLayout", () => {
  it("floats the row above the card, the gap between them, the card at the pointer", () => {
    const l = layout({});
    expect(l.rowAt).toBe("above");
    const card = l.card as SurfacePosition;
    expect(card.top).toBe(340);
    // The gap is between the row's bottom edge and the card's top edge - two
    // surfaces, not one box with a border inside it.
    expect(card.top - (l.row.top + ROW.height)).toBe(BETWEEN);
  });

  it("puts the row below the card when there is no room above", () => {
    // A message at the top of the conversation: the pointer is closer to the
    // viewport's top than the row and the gap are tall.
    const l = layout({
      bubble: { left: 40, top: 10, right: 400, bottom: 90 },
      pointer: { x: 200, y: 30 },
    });
    expect(l.rowAt).toBe("below");
    const card = l.card as SurfacePosition;
    expect(l.row.top - (card.top + CARD.height)).toBe(BETWEEN);
    expect(l.row.top).toBeGreaterThan(card.top);
  });

  it("shares the edge that faces the bubble", () => {
    // Right of an incoming bubble: left edges together, next to the message.
    const r = layout({});
    expect((r.card as SurfacePosition).left).toBe(r.row.left);
    expect(r.row.left).toBe(408);
    // Left of an outgoing bubble: right edges together, so the wider row does
    // not reach toward the bubble past the card.
    const l = layout({
      bubble: { left: 800, top: 300, right: 1160, bottom: 380 },
      away: "left",
      pointer: { x: 900, y: 340 },
    });
    expect(l.row.left + ROW.width).toBe(800 - 8);
    expect((l.card as SurfacePosition).left + CARD.width).toBe(800 - 8);
  });

  it("draws the row alone when there is no card", () => {
    const l = layout({ card: null });
    expect(l.rowAt).toBe("alone");
    expect(l.card).toBeNull();
    expect(l.row).toEqual({ left: 408, top: 340 });
  });

  it("lands in the same place when reopened on the same message", () => {
    // The anchor is the gesture, never the previous menu: nothing the first
    // answer produced is an input to the second.
    const first = layout({ pointer: { x: 210, y: 350 } });
    const again = layout({ pointer: { x: 210, y: 350 } });
    expect(again).toEqual(first);
  });

  it("keeps both surfaces inside the viewport at every edge and corner", () => {
    const viewport = { width: 900, height: 600 };
    const points = [
      { x: 4, y: 4 }, // top-left corner
      { x: 896, y: 4 }, // top-right
      { x: 4, y: 596 }, // bottom-left
      { x: 896, y: 596 }, // bottom-right
      { x: 450, y: 2 }, // top edge
      { x: 450, y: 598 }, // bottom edge
      { x: 1, y: 300 }, // left edge
      { x: 899, y: 300 }, // right edge
    ];
    const rowAts = new Set<string>();
    const sides = new Set<string>();
    for (const pt of points) {
      for (const [away, wide] of [
        ["left", false],
        ["right", false],
        // A bubble spanning nearly the row leaves no room beside it, so the
        // footprint opens at the pointer above or below it - the case where a
        // row wider than the card could run off the right edge.
        ["left", true],
        ["right", true],
      ] as const) {
        // A bubble under the pointer, clipped to the viewport.
        const bubble: MenuAnchorRect = {
          left: wide ? 20 : Math.max(0, pt.x - 60),
          top: Math.max(0, pt.y - 20),
          right: wide ? viewport.width - 20 : Math.min(viewport.width, pt.x + 60),
          bottom: Math.min(viewport.height, pt.y + 20),
        };
        const l = layout({ bubble, pointer: pt, viewport, away });
        sides.add(l.block.side);
        rowAts.add(l.rowAt);
        expect(inside(l.row, ROW, viewport), `row at ${JSON.stringify(pt)} ${away}`).toBe(true);
        expect(inside(l.card as SurfacePosition, CARD, viewport), `card at ${JSON.stringify(pt)} ${away}`)
          .toBe(true);
        // Two surfaces: they never touch, whichever way round they stack.
        expect(overlaps(l.row, ROW, l.card as SurfacePosition, CARD)).toBe(false);
      }
    }
    // The corners reach both arrangements and both kinds of footprint, so this
    // is not one branch checked eight times.
    expect(rowAts).toEqual(new Set(["above", "below"]));
    expect(sides.has("near") || sides.has("away")).toBe(true);
    expect(sides.has("above") || sides.has("below")).toBe(true);
  });

  it("never puts either surface over the bubble unless it says `clamped`", () => {
    // The rule the card already obeyed, now asked of the row as well.
    const viewports = [
      { width: 1400, height: 900 },
      { width: 380, height: 700 },
      { width: 900, height: 500 },
    ];
    let sawAlignRight = false;
    let sawBelow = false;
    for (const viewport of viewports) {
      for (const away of ["left", "right"] as const) {
        for (let top = 0; top + 40 < viewport.height; top += 29) {
          for (let left = 0; left + 40 < viewport.width; left += 41) {
            for (const [w, h] of [
              [280, 70],
              [140, 300],
              [340, 520],
            ] as const) {
              const bubble: MenuAnchorRect = {
                left,
                top,
                right: Math.min(viewport.width, left + w),
                bottom: Math.min(viewport.height, top + h),
              };
              const l = layout({
                bubble,
                pointer: { x: bubble.left + 5, y: bubble.top + 5 },
                viewport,
                away,
              });
              const card = l.card as SurfacePosition;
              // Inside the viewport on every branch, `clamped` included.
              expect(inside(l.row, ROW, viewport)).toBe(true);
              expect(inside(card, CARD, viewport)).toBe(true);
              if (l.block.side === "clamped") continue;
              if (card.left + CARD.width === l.row.left + ROW.width && card.left !== l.row.left) {
                sawAlignRight = true;
              }
              if (l.rowAt === "below") sawBelow = true;
              expect(surfaceOverlapsBubble(l.row, ROW, bubble)).toBe(false);
              expect(surfaceOverlapsBubble(card, CARD, bubble)).toBe(false);
            }
          }
        }
      }
    }
    // The grid reaches the two cases that could put the row on the bubble:
    // the footprint left of it (where the wider row hangs toward it), and the
    // row stacked below the card.
    expect(sawAlignRight).toBe(true);
    expect(sawBelow).toBe(true);
  });
});
