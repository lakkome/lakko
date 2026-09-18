// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  CAMERA_UNAVAILABLE_NOTICE,
  cameraCheckCallAccessibleName,
  cameraCheckCallLabel,
  cameraCheckControls,
  cameraCheckResult,
  planCallStart,
} from "./call-start-flow";
import { callKindWord } from "./call-media-choice";

describe("planCallStart", () => {
  it("dials a voice call immediately, with no screen in between", () => {
    // Section 3.1: "Tapping the phone button places the call immediately. No
    // preparation screen, no 'check your microphone', no empty camera box."
    const plan = planCallStart("audio", true);
    expect(plan).toEqual({ kind: "dial", media: { audio: true, video: false }, notice: null });
  });

  it("never asks a voice caller a question, camera or no camera", () => {
    for (const cameraAvailable of [true, false]) {
      expect(planCallStart("audio", cameraAvailable).kind).toBe("dial");
    }
  });

  it("shows the camera check before a video call", () => {
    expect(planCallStart("video", true)).toEqual({ kind: "camera-check" });
  });

  it("places a voice call and says so when there is no camera", () => {
    // Section 3.2: "If the device has no camera, the video button starts a
    // voice call and says so in one line."
    const plan = planCallStart("video", false);
    expect(plan.kind).toBe("dial");
    if (plan.kind !== "dial") throw new Error("expected a dial plan");
    expect(plan.media).toEqual({ audio: true, video: false });
    expect(plan.notice).toBe(CAMERA_UNAVAILABLE_NOTICE);
  });

  it("keeps that one line to one sentence with no jargon", () => {
    expect(CAMERA_UNAVAILABLE_NOTICE.split(".").filter((s) => s.trim() !== "")).toHaveLength(1);
    expect(CAMERA_UNAVAILABLE_NOTICE).not.toMatch(/getUserMedia|NotFoundError|track/u);
  });

  it("captures audio on every call it plans, muted or not", () => {
    // `audio: false` would mean no audio transceiver at all, so the call could
    // never be unmuted without renegotiating.
    for (const kind of ["audio", "video"] as const) {
      const plan = planCallStart(kind, false);
      if (plan.kind === "dial") expect(plan.media.audio).toBe(true);
    }
  });
});

describe("cameraCheckResult", () => {
  it("places the call the toggles describe, not the button that opened the screen", () => {
    expect(cameraCheckResult({ micOn: true, camOn: true })).toEqual({
      media: { audio: true, video: true },
      micOn: true,
    });
    expect(cameraCheckResult({ micOn: true, camOn: false })).toEqual({
      media: { audio: true, video: false },
      micOn: true,
    });
  });

  it("makes the callee's word match what was actually placed (section 1)", () => {
    expect(callKindWord(cameraCheckResult({ micOn: true, camOn: false }).media)).toBe(
      "Voice call",
    );
    expect(callKindWord(cameraCheckResult({ micOn: true, camOn: true }).media)).toBe(
      "Video call",
    );
  });

  it("carries a muted start alongside the media rather than inside it", () => {
    const muted = cameraCheckResult({ micOn: false, camOn: true });
    expect(muted.micOn).toBe(false);
    expect(muted.media.audio).toBe(true);
  });
});

describe("cameraCheckControls", () => {
  it("is round buttons in two states, never switches (section 4.1)", () => {
    const on = cameraCheckControls({ micOn: true, camOn: true });
    expect(on.mic.tone).toBe("outline");
    expect(on.camera.tone).toBe("outline");
    const off = cameraCheckControls({ micOn: false, camOn: false });
    expect(off.mic.tone).toBe("solid");
    expect(off.camera.tone).toBe("solid");
  });

  it("slashes the icon and reports pressed in the off state", () => {
    const off = cameraCheckControls({ micOn: false, camOn: false });
    expect(off.mic.icon).toBe("micOff");
    expect(off.camera.icon).toBe("videoOff");
    expect(off.mic.pressed).toBe(true);
    expect(off.camera.pressed).toBe(true);
  });

  it("uses the same words as the in-call row, so one microphone has one name", () => {
    const c = cameraCheckControls({ micOn: true, camOn: true });
    expect(c.mic.label).toBe("Mute");
    expect(c.camera.label).toBe("Camera");
    expect(c.mic.accessibleName).toBe("Mute microphone");
  });

  it("keeps the label fixed while the state moves", () => {
    for (const micOn of [true, false]) {
      for (const camOn of [true, false]) {
        const c = cameraCheckControls({ micOn, camOn });
        expect(c.mic.label).toBe("Mute");
        expect(c.camera.label).toBe("Camera");
      }
    }
  });
});

describe("the camera check's big button", () => {
  it("still reads Call with the camera off (section 3.2)", () => {
    expect(cameraCheckCallLabel()).toBe("Call");
  });

  it("tells a screen reader which kind of call it will place", () => {
    expect(cameraCheckCallAccessibleName("Anna", true)).toBe("Start video call with Anna");
    expect(cameraCheckCallAccessibleName("Anna", false)).toBe("Start voice call with Anna");
  });

  it("stays true when this device cannot name the other person", () => {
    expect(cameraCheckCallAccessibleName(null, true)).toBe("Start video call");
    expect(cameraCheckCallAccessibleName("   ", false)).toBe("Start voice call");
  });
});
