// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Every view of the person's own camera (own tile, one-to-one self view,
 * full-screen own camera) shows the avatar with a small spinner while the
 * background starts (UX `docs/ux/call-backgrounds.md` section 3), by one rule.
 */

import { describe, expect, it } from "vitest";

import { ownCameraPreparing, type CameraBackgroundState } from "./background-status";

describe("ownCameraPreparing", () => {
  it("is true only while the background starts and the camera itself is up", () => {
    const states: CameraBackgroundState[] = ["off", "starting", "working", "failed"];
    for (const s of states) {
      expect(ownCameraPreparing(s, false), s).toBe(s === "starting");
      // The camera's own Starting overlay wins while the camera starts.
      expect(ownCameraPreparing(s, true), s).toBe(false);
    }
  });
});
