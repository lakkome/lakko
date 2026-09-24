// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  messageMenuPlacement,
  placementOverlapsBubble,
  type MenuAnchorRect,
  type MenuPlacementInput,
  type MenuSize,
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
