// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { avatarVideoPlays, pointerHovers } from "./avatar-video";

describe("avatarVideoPlays (docs/ux/profile-screen.md section 7)", () => {
  it("plays always in a large view", () => {
    for (const hovering of [false, true]) {
      for (const canHover of [false, true]) {
        expect(avatarVideoPlays(true, hovering, canHover)).toBe(true);
      }
    }
  });

  it("plays in a list row only while hovered on a device that can hover", () => {
    expect(avatarVideoPlays("hover", true, true)).toBe(true);
    expect(avatarVideoPlays("hover", false, true)).toBe(false);
    expect(avatarVideoPlays("hover", true, false)).toBe(false);
    expect(avatarVideoPlays("hover", false, false)).toBe(false);
  });

  it("plays in the header while hovered, or always on a touch-only device", () => {
    expect(avatarVideoPlays("header", true, true)).toBe(true);
    expect(avatarVideoPlays("header", false, true)).toBe(false);
    expect(avatarVideoPlays("header", false, false)).toBe(true);
  });

  it("never plays elsewhere", () => {
    expect(avatarVideoPlays(undefined, true, true)).toBe(false);
    expect(avatarVideoPlays(false, true, true)).toBe(false);
  });

  it("a touch is not hovering", () => {
    expect(pointerHovers("touch")).toBe(false);
    expect(pointerHovers("mouse")).toBe(true);
    expect(pointerHovers("pen")).toBe(true);
  });
});
