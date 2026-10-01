// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { CAMERA_UNAVAILABLE_NOTICE, planCallStart } from "./call-start-flow";

describe("planCallStart", () => {
  it("dials a voice call immediately, with no screen in between", () => {
    // Section 3.1: "Tapping the phone button places the call immediately. No
    // preparation screen, no 'check your microphone', no empty camera box."
    const plan = planCallStart("audio", true);
    expect(plan).toEqual({ kind: "dial", media: { audio: true, video: false }, notice: null });
  });

  it("dials a video call immediately too, carrying the camera", () => {
    // Was "shows the camera check before a video call" until 2026-09-21.
    // Section 3.2 now: one press of a call button is one call, for both kinds.
    expect(planCallStart("video", true)).toEqual({
      kind: "dial",
      media: { audio: true, video: true },
      notice: null,
    });
  });

  it("never asks anyone a question before their call", () => {
    for (const kind of ["audio", "video"] as const) {
      for (const cameraAvailable of [true, false]) {
        expect(planCallStart(kind, cameraAvailable).kind).toBe("dial");
      }
    }
  });

  it("places a voice call and says so when there is no camera", () => {
    // Section 3.2: "If the device has no camera, the video button starts a
    // voice call and says so in one line."
    const plan = planCallStart("video", false);
    expect(plan.media).toEqual({ audio: true, video: false });
    expect(plan.notice).toBe(CAMERA_UNAVAILABLE_NOTICE);
  });

  it("says nothing when the call is the call that was asked for", () => {
    expect(planCallStart("audio", true).notice).toBeNull();
    expect(planCallStart("video", true).notice).toBeNull();
  });

  it("keeps that one line to one sentence with no jargon", () => {
    expect(CAMERA_UNAVAILABLE_NOTICE().split(".").filter((s) => s.trim() !== "")).toHaveLength(1);
    expect(CAMERA_UNAVAILABLE_NOTICE()).not.toMatch(/getUserMedia|NotFoundError|track/u);
  });

  it("captures audio on every call it plans, muted or not", () => {
    // `audio: false` would mean no audio transceiver at all, so the call could
    // never be unmuted without renegotiating.
    for (const kind of ["audio", "video"] as const) {
      for (const cameraAvailable of [true, false]) {
        expect(planCallStart(kind, cameraAvailable).media.audio).toBe(true);
      }
    }
  });
});
