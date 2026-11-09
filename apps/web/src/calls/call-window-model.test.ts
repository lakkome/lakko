// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The collapsed call window's numbers (`docs/ux/calls.md` section 4.5.2):
 * its size on a phone and on a wide window, the corner it rests in, the
 * corner a drop lands in, and what it counts.
 */

import { afterEach, describe, expect, it } from "vitest";

import {
  CALL_WINDOW_DRAG_THRESHOLD_PX,
  CALL_WINDOW_EDGE_PX,
  CallWindowDrag,
  callWindowSize,
  cornerPosition,
  insetCorner,
  insetShown,
  isPhoneViewport,
  nearestCorner,
  othersBeyondShown,
  projectedRest,
  rememberCorner,
  rememberedCorner,
  resetRememberedCorner,
} from "./call-window-model";

const DESKTOP = { width: 1440, height: 900 };
const PHONE = { width: 390, height: 844 };

describe("the window's size", () => {
  it("lies 16:9 at 320 wide on a desktop window", () => {
    expect(isPhoneViewport(DESKTOP)).toBe(false);
    expect(callWindowSize(DESKTOP, false)).toEqual({ width: 320, height: 180 });
  });

  it("stands 3:4 at 36% of the width on a phone", () => {
    expect(isPhoneViewport(PHONE)).toBe(true);
    // 36% of 390 is 140.4, rounded to 140; 3:4 upright is 187.
    expect(callWindowSize(PHONE, false)).toEqual({ width: 140, height: 187 });
  });

  it("is never narrower than 120 on a phone", () => {
    expect(callWindowSize({ width: 300, height: 640 }, false).width).toBe(120);
  });

  it("is a phone by its narrow side, so a phone on its side is still a phone", () => {
    expect(isPhoneViewport({ width: 844, height: 390 })).toBe(true);
    expect(isPhoneViewport({ width: 700, height: 600 })).toBe(false);
  });

  it("lies 16:9 at the same width while the picture is a screen share", () => {
    expect(callWindowSize(PHONE, true)).toEqual({ width: 140, height: 79 });
    expect(callWindowSize(DESKTOP, true)).toEqual({ width: 320, height: 180 });
  });

  it("stays inside a viewport too small for it", () => {
    const size = callWindowSize({ width: 200, height: 100 }, false);
    expect(size.width).toBeLessThanOrEqual(200 - 2 * CALL_WINDOW_EDGE_PX);
    expect(size.height).toBeLessThanOrEqual(100 - 2 * CALL_WINDOW_EDGE_PX);
  });
});

describe("where it rests", () => {
  const size = { width: 320, height: 180 };

  it("starts top right, 16 in and 16 below the header", () => {
    expect(cornerPosition("top-right", size, DESKTOP, 64)).toEqual({
      x: 1440 - 16 - 320,
      y: 64 + 16,
    });
  });

  it("sits 16 in from the bottom and the side in a bottom corner, whatever the header", () => {
    expect(cornerPosition("bottom-left", size, DESKTOP, 64)).toEqual({ x: 16, y: 900 - 16 - 180 });
  });

  it("keeps a top corner on screen when the header leaves no room", () => {
    const short = { width: 800, height: 260 };
    const at = cornerPosition("top-left", size, short, 200);
    expect(at.y + size.height).toBeLessThanOrEqual(short.height);
  });
});

describe("where a drop lands", () => {
  const size = { width: 320, height: 180 };

  it("goes to the corner of the half the window's centre is in", () => {
    expect(nearestCorner({ x: 100, y: 100 }, size, DESKTOP)).toBe("top-left");
    expect(nearestCorner({ x: 1000, y: 100 }, size, DESKTOP)).toBe("top-right");
    expect(nearestCorner({ x: 100, y: 600 }, size, DESKTOP)).toBe("bottom-left");
    expect(nearestCorner({ x: 1000, y: 600 }, size, DESKTOP)).toBe("bottom-right");
  });

  it("reaches the corner a flick was aimed at, from short of the midline", () => {
    // Released left of the middle, moving fast to the right.
    const rest = projectedRest({ x: 500, y: 100 }, { x: 3, y: 0 });
    expect(nearestCorner({ x: 500, y: 100 }, size, DESKTOP)).toBe("top-left");
    expect(nearestCorner(rest, size, DESKTOP)).toBe("top-right");
  });

  it("reads a velocity that is not a number as standing still", () => {
    expect(projectedRest({ x: 10, y: 20 }, { x: Number.NaN, y: Number.POSITIVE_INFINITY })).toEqual({
      x: 10,
      y: 20,
    });
  });
});

describe("a press on the window", () => {
  const size = { width: 320, height: 180 };

  it("is a click while it moves less than the threshold", () => {
    const drag = new CallWindowDrag({ x: 1200, y: 100 }, { x: 1104, y: 80 }, 0);
    expect(drag.move({ x: 1200 + CALL_WINDOW_DRAG_THRESHOLD_PX - 2, y: 100 }, 16)).toBeNull();
    expect(drag.isDragging()).toBe(false);
    expect(drag.release(size, DESKTOP, 32)).toEqual({ kind: "click" });
  });

  it("follows the pointer once dragged, and drops in the nearest corner", () => {
    const drag = new CallWindowDrag({ x: 1200, y: 100 }, { x: 1104, y: 80 }, 0);
    const at = drag.move({ x: 300, y: 700 }, 400);
    expect(at).toEqual({ x: 1104 - 900, y: 80 + 600 });
    expect(drag.isDragging()).toBe(true);
    expect(drag.release(size, DESKTOP, 410)).toEqual({ kind: "drop", corner: "bottom-left" });
  });

  it("takes no flick from a pointer that stood still before letting go", () => {
    const flicked = new CallWindowDrag({ x: 200, y: 100 }, { x: 100, y: 80 }, 0);
    const held = new CallWindowDrag({ x: 200, y: 100 }, { x: 100, y: 80 }, 0);
    for (const drag of [flicked, held]) {
      // A fast move to the right that ends left of the middle.
      drag.move({ x: 450, y: 100 }, 20);
      drag.move({ x: 500, y: 100 }, 40);
    }
    // Let go at once: the flick carries it over the middle.
    expect(flicked.release(size, DESKTOP, 45)).toEqual({ kind: "drop", corner: "top-right" });
    // Held still for a second first: it lands by where it is.
    expect(held.release(size, DESKTOP, 1040)).toEqual({ kind: "drop", corner: "top-left" });
  });

  it("reads the flick from the last stretch of the drag", () => {
    const drag = new CallWindowDrag({ x: 200, y: 100 }, { x: 100, y: 80 }, 0);
    // A slow drag to the middle ...
    drag.move({ x: 400, y: 100 }, 500);
    drag.move({ x: 500, y: 100 }, 1000);
    // ... then a quick flick to the right.
    drag.move({ x: 560, y: 100 }, 1020);
    drag.move({ x: 620, y: 100 }, 1040);
    expect(drag.release(size, DESKTOP, 1045)).toEqual({ kind: "drop", corner: "top-right" });
  });
});

describe("what the window counts and draws", () => {
  it("prints +16 with 18 people: everyone beyond the shown person and you", () => {
    expect(othersBeyondShown(18, false)).toBe(16);
  });

  it("prints nothing in a call of two, or for someone alone", () => {
    expect(othersBeyondShown(2, false)).toBe(0);
    expect(othersBeyondShown(1, true)).toBe(0);
  });

  it("counts everyone else when the picture is you", () => {
    expect(othersBeyondShown(3, true)).toBe(2);
  });

  it("draws the inset only while your camera sends and the picture is not you", () => {
    expect(insetShown(true, false)).toBe(true);
    expect(insetShown(false, false)).toBe(false);
    expect(insetShown(true, true)).toBe(false);
  });

  it("puts the inset in the bottom corner away from the edge", () => {
    expect(insetCorner("top-right")).toBe("bottom-left");
    expect(insetCorner("bottom-right")).toBe("bottom-left");
    expect(insetCorner("top-left")).toBe("bottom-right");
    expect(insetCorner("bottom-left")).toBe("bottom-right");
  });
});

describe("the corner kept for the session", () => {
  afterEach(() => {
    resetRememberedCorner();
  });

  it("starts top right and keeps where the last drop landed", () => {
    expect(rememberedCorner()).toBe("top-right");
    rememberCorner("bottom-left");
    expect(rememberedCorner()).toBe("bottom-left");
  });
});

describe("the safe area", () => {
  const size = { width: 140, height: 187 };
  // A phone with a notch at the top and a home indicator at the bottom.
  const insets = { top: 47, right: 0, bottom: 34, left: 0 };

  it("rests 16 in from the safe edges, never over a system bar", () => {
    const top = cornerPosition("top-right", size, PHONE, 0, insets);
    expect(top.y).toBe(47 + CALL_WINDOW_EDGE_PX);
    const bottom = cornerPosition("bottom-left", size, PHONE, 0, insets);
    expect(bottom.y + size.height).toBe(PHONE.height - 34 - CALL_WINDOW_EDGE_PX);
  });

  it("keeps to the header when the header reaches below the top inset", () => {
    expect(cornerPosition("top-left", size, PHONE, 100, insets).y).toBe(100 + CALL_WINDOW_EDGE_PX);
  });

  it("keeps clear of side insets in landscape", () => {
    const landscape = { width: 844, height: 390 };
    const side = { top: 0, right: 47, bottom: 21, left: 47 };
    const s = callWindowSize(landscape, false, side);
    expect(cornerPosition("top-left", s, landscape, 0, side).x).toBe(47 + CALL_WINDOW_EDGE_PX);
    const right = cornerPosition("bottom-right", s, landscape, 0, side);
    expect(right.x + s.width).toBe(844 - 47 - CALL_WINDOW_EDGE_PX);
    expect(right.y + s.height).toBe(390 - 21 - CALL_WINDOW_EDGE_PX);
  });

  it("shrinks to fit a viewport whose insets leave little room", () => {
    const s = callWindowSize({ width: 300, height: 200 }, false, { top: 40, right: 0, bottom: 40, left: 0 });
    expect(s.height).toBeLessThanOrEqual(200 - 80 - 2 * CALL_WINDOW_EDGE_PX);
  });

  it("reads an inset that is not a number as none", () => {
    const odd = { top: Number.NaN, right: -5, bottom: Number.POSITIVE_INFINITY, left: 0 };
    expect(cornerPosition("bottom-right", size, PHONE, 0, odd)).toEqual(
      cornerPosition("bottom-right", size, PHONE, 0),
    );
  });
});
