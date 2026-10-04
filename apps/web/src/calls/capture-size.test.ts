// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { CAMERA_CAPTURE_SIZE, trackConstraint } from "./call-session";

describe("trackConstraint", () => {
  it("asks the camera for 720p at 30 frames, never insisting", () => {
    expect(trackConstraint(true, null)).toEqual({
      width: { ideal: 1280 },
      height: { ideal: 720 },
      frameRate: { ideal: 30 },
    });
  });

  it("keeps the size when a camera is picked", () => {
    expect(trackConstraint(true, "cam-2")).toEqual({
      ...CAMERA_CAPTURE_SIZE,
      deviceId: { exact: "cam-2" },
    });
  });

  it("asks for nothing when video is off", () => {
    expect(trackConstraint(false, "cam-2")).toBe(false);
  });
});
