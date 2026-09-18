// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  CONTROLS_IDLE_HIDE_MS,
  ENDED_DISMISS_MS,
  callStatusDetail,
  callSurfaceKind,
  controlsHidden,
  elapsedMs,
  endedSummary,
  formatCallDuration,
  selfViewVisible,
  spokenCallDuration,
  type CallPhaseKind,
} from "./call-screen-model";

const PHASES: readonly CallPhaseKind[] = [
  "idle",
  "calling",
  "connecting",
  "connected",
  "reconnecting",
  "ended",
];

describe("callSurfaceKind", () => {
  it("draws the portrait while nobody has answered", () => {
    expect(callSurfaceKind({ remoteCount: 0, remoteCameraOn: false })).toBe("voice");
    // Even when this device's own camera is live: the caller is looking at the
    // person they called, not at themselves.
    expect(callSurfaceKind({ remoteCount: 0, remoteCameraOn: true })).toBe("voice");
  });

  it("fills the screen only when the sole remote is sending a picture", () => {
    expect(callSurfaceKind({ remoteCount: 1, remoteCameraOn: true })).toBe("video");
  });

  it("draws the portrait, never an empty box, when the remote camera is off", () => {
    // The defect the redesign exists to remove: a video call whose callee has
    // their camera off had nothing to fill the screen with and drew a black
    // rectangle. docs/ux/calls.md section 4.
    expect(callSurfaceKind({ remoteCount: 1, remoteCameraOn: false })).toBe("voice");
  });

  it("keeps the roster grid for a group call whatever the cameras do", () => {
    expect(callSurfaceKind({ remoteCount: 2, remoteCameraOn: false })).toBe("group");
    expect(callSurfaceKind({ remoteCount: 7, remoteCameraOn: true })).toBe("group");
  });
});

describe("callStatusDetail", () => {
  it("says Calling while the other phone is ringing, not Connecting", () => {
    // The store used to collapse `outgoing` into `connecting`, so a ringing
    // call claimed to be negotiating. Section 4's state table separates them.
    expect(callStatusDetail({ phase: "calling", connectedAtMs: null, nowMs: 0 })).toBe(
      "Calling...",
    );
    expect(callStatusDetail({ phase: "connecting", connectedAtMs: null, nowMs: 0 })).toBe(
      "Connecting...",
    );
  });

  it("shows the running time and nothing else once connected", () => {
    expect(callStatusDetail({ phase: "connected", connectedAtMs: 1_000, nowMs: 725_000 })).toBe(
      "12:04",
    );
  });

  it("reads 0:00 rather than a placeholder before the first tick", () => {
    expect(callStatusDetail({ phase: "connected", connectedAtMs: null, nowMs: 5_000 })).toBe(
      "0:00",
    );
  });

  it("keeps the line to one thing in every phase", () => {
    for (const phase of PHASES) {
      const line = callStatusDetail({ phase, connectedAtMs: 0, nowMs: 1_000 });
      expect(line.includes("\n")).toBe(false);
      // Never two states at once (section 4).
      expect(line.split("...").length).toBeLessThanOrEqual(2);
    }
  });

  it("is empty only when there is no call", () => {
    expect(callStatusDetail({ phase: "idle", connectedAtMs: null, nowMs: 0 })).toBe("");
    for (const phase of PHASES.filter((p) => p !== "idle")) {
      expect(callStatusDetail({ phase, connectedAtMs: null, nowMs: 0 })).not.toBe("");
    }
  });
});

describe("formatCallDuration", () => {
  it("matches the shape section 4 prints", () => {
    expect(formatCallDuration(724_000)).toBe("12:04");
  });

  it("drops the leading zero on minutes under an hour, as a phone does", () => {
    expect(formatCallDuration(0)).toBe("0:00");
    expect(formatCallDuration(5_000)).toBe("0:05");
    expect(formatCallDuration(65_000)).toBe("1:05");
  });

  it("pads the minutes once there are hours, so the fields stay aligned", () => {
    expect(formatCallDuration(3_723_000)).toBe("1:02:03");
    expect(formatCallDuration(36_000_000)).toBe("10:00:00");
  });

  it("never prints a negative time when the clock steps backwards", () => {
    expect(formatCallDuration(-5_000)).toBe("0:00");
    expect(elapsedMs(10_000, 5_000)).toBe(0);
  });
});

describe("spokenCallDuration", () => {
  it("is read as a duration, not as a time of day (section 8)", () => {
    expect(spokenCallDuration(724_000)).toBe("12 minutes 4 seconds");
    expect(spokenCallDuration(5_000)).toBe("5 seconds");
    expect(spokenCallDuration(1_000)).toBe("1 second");
    expect(spokenCallDuration(3_723_000)).toBe("1 hour 2 minutes 3 seconds");
  });

  it("always names the unit, so no number is left bare", () => {
    for (const ms of [0, 999, 1_000, 61_000, 3_600_000]) {
      expect(spokenCallDuration(ms)).toMatch(/second|minute|hour/u);
    }
  });
});

describe("endedSummary", () => {
  it("carries the duration of a call that connected", () => {
    expect(endedSummary(1_000, 725_000)).toBe("Call ended · 12:04");
  });

  it("states no duration for a call that never connected", () => {
    // A declined or unanswered call has no duration; printing 0:00 would say
    // something false about it.
    expect(endedSummary(null, 725_000)).toBe("Call ended");
  });
});

describe("controlsHidden", () => {
  it("fades a connected video call after five seconds", () => {
    expect(
      controlsHidden({ surface: "video", phase: "connected", msSinceInteraction: 4_999 }),
    ).toBe(false);
    expect(
      controlsHidden({
        surface: "video",
        phase: "connected",
        msSinceInteraction: CONTROLS_IDLE_HIDE_MS,
      }),
    ).toBe(true);
  });

  it("never hides them on a voice call, however long nobody touches it", () => {
    // Section 4.2: nothing to look at underneath, so hiding only costs a tap.
    expect(
      controlsHidden({ surface: "voice", phase: "connected", msSinceInteraction: 600_000 }),
    ).toBe(false);
  });

  it("never hides them on the group roster either", () => {
    expect(
      controlsHidden({ surface: "group", phase: "connected", msSinceInteraction: 600_000 }),
    ).toBe(false);
  });

  it("never hides them before the call is connected", () => {
    for (const phase of PHASES.filter((p) => p !== "connected")) {
      expect(controlsHidden({ surface: "video", phase, msSinceInteraction: 600_000 })).toBe(false);
    }
  });

  it("gives the user two seconds with the ended screen", () => {
    expect(ENDED_DISMISS_MS).toBe(2_000);
  });
});

describe("selfViewVisible", () => {
  it("follows the local camera on both 1x1 surfaces", () => {
    expect(selfViewVisible("video", true)).toBe(true);
    expect(selfViewVisible("voice", true)).toBe(true);
    expect(selfViewVisible("video", false)).toBe(false);
    expect(selfViewVisible("voice", false)).toBe(false);
  });

  it("is absent on the group grid, where the local tile already exists", () => {
    expect(selfViewVisible("group", true)).toBe(false);
  });
});
