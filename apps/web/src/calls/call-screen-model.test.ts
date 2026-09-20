// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  CONTROLS_IDLE_HIDE_MS,
  ENDED_DISMISS_MS,
  callFillSource,
  callStatusDetail,
  callSurfaceKind,
  controlsHidden,
  elapsedMs,
  endedSummary,
  featuredParticipant,
  formatCallDuration,
  galleryParticipants,
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
    expect(
      callSurfaceKind({ remoteCount: 0, remoteCameraOn: false, localCameraOn: false }),
    ).toBe("voice");
  });

  it("keeps the portrait while a video call is still ringing out", () => {
    // The caller's own camera is live from the camera check, but filling the
    // screen with it would take away the only thing on the screen that says
    // who is being called.
    expect(
      callSurfaceKind({ remoteCount: 0, remoteCameraOn: true, localCameraOn: true }),
    ).toBe("voice");
  });

  it("fills the screen when the other person is sending a picture", () => {
    expect(
      callSurfaceKind({ remoteCount: 1, remoteCameraOn: true, localCameraOn: false }),
    ).toBe("video");
  });

  it("fills the screen when THIS device turns its camera on mid-call", () => {
    // Section 4.1: "Pressing it starts the camera, tells the other side, and
    // the screen becomes the video layout for both."
    expect(
      callSurfaceKind({ remoteCount: 1, remoteCameraOn: false, localCameraOn: true }),
    ).toBe("video");
  });

  it("returns to the portrait when the camera goes off again", () => {
    // Section 4.1's last sentence, and the reason the surface cannot be
    // decided once at the start of the call.
    expect(
      callSurfaceKind({ remoteCount: 1, remoteCameraOn: false, localCameraOn: false }),
    ).toBe("voice");
  });

  it("draws the portrait, never an empty box, when no camera is on anywhere", () => {
    expect(
      callSurfaceKind({ remoteCount: 1, remoteCameraOn: false, localCameraOn: false }),
    ).toBe("voice");
  });

  it("keeps the roster grid for a group call whatever the cameras do", () => {
    expect(
      callSurfaceKind({ remoteCount: 2, remoteCameraOn: false, localCameraOn: false }),
    ).toBe("group");
    expect(
      callSurfaceKind({ remoteCount: 7, remoteCameraOn: true, localCameraOn: true }),
    ).toBe("group");
  });
});

describe("callFillSource", () => {
  it("shows the other person whenever they are sending — that is the call", () => {
    expect(callFillSource("video", true, false)).toBe("remote");
    expect(callFillSource("video", true, true)).toBe("remote");
  });

  it("falls back to this device's own camera when there is no incoming picture", () => {
    // The person who just turned their camera on gets to see the thing they
    // started; there is no other picture in existence.
    expect(callFillSource("video", false, true)).toBe("local");
  });

  it("has nothing to draw on the portrait or the roster", () => {
    for (const surface of ["voice", "group"] as const) {
      expect(callFillSource(surface, true, true)).toBe("none");
    }
  });

  it("never claims a picture that does not exist", () => {
    expect(callFillSource("video", false, false)).toBe("none");
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
  it("is drawn in the corner while the other person fills the screen", () => {
    expect(selfViewVisible("video", true, "remote")).toBe(true);
  });

  it("is drawn over the portrait while a video call rings out", () => {
    expect(selfViewVisible("voice", true, "none")).toBe(true);
  });

  it("is absent when this device's own camera IS the screen", () => {
    // A corner copy of the picture behind it is the same view twice.
    expect(selfViewVisible("video", true, "local")).toBe(false);
  });

  it("is absent for the person receiving a picture they are not returning", () => {
    // Section 4.1: "their screen shows the incoming picture with their
    // self-view absent" — which is what a camera that is off already means.
    expect(selfViewVisible("video", false, "remote")).toBe(false);
  });

  it("follows the local camera off", () => {
    expect(selfViewVisible("voice", false, "none")).toBe(false);
  });

  it("is absent on the group grid, where the local tile already exists", () => {
    expect(selfViewVisible("group", true, "none")).toBe(false);
  });
});

describe("featuredParticipant (§4.4)", () => {
  const tile = (
    id: string,
    over: Partial<{ isLocal: boolean; camOn: boolean; speaking: boolean }> = {},
  ) => ({ id, isLocal: false, camOn: false, speaking: false, ...over });
  const me = tile("me", { isLocal: true, camOn: true });

  it("features nobody when no one else is sending video", () => {
    // The defect: the first remote held the big slot with an avatar in it
    // while other cameras ran in small tiles. With no remote camera at all the
    // honest layout is an equal grid, the local camera notwithstanding.
    const roster = [me, tile("a"), tile("b"), tile("c")];
    expect(featuredParticipant(roster, null)).toBeNull();
    expect(galleryParticipants(roster, null).map((p) => p.id)).toEqual(["me", "a", "b", "c"]);
  });

  it("features the first remote with a camera, in roster order", () => {
    const roster = [me, tile("a"), tile("b", { camOn: true }), tile("c", { camOn: true })];
    expect(featuredParticipant(roster, null)?.id).toBe("b");
    expect(galleryParticipants(roster, roster[2] ?? null).map((p) => p.id)).toEqual([
      "me",
      "a",
      "c",
    ]);
  });

  it("prefers a speaking remote with a camera over an earlier silent one", () => {
    const roster = [me, tile("a", { camOn: true }), tile("b", { camOn: true, speaking: true })];
    expect(featuredParticipant(roster, null)?.id).toBe("b");
  });

  it("never features a speaker who is not sending video", () => {
    const roster = [me, tile("a", { speaking: true }), tile("b", { camOn: true })];
    expect(featuredParticipant(roster, null)?.id).toBe("b");
    expect(featuredParticipant([me, tile("a", { speaking: true })], null)).toBeNull();
  });

  it("never features the local tile while anyone else is present", () => {
    expect(featuredParticipant([me, tile("a")], null)).toBeNull();
  });

  it("features the local tile when alone", () => {
    expect(featuredParticipant([me], null)?.id).toBe("me");
    expect(featuredParticipant([tile("me-off", { isLocal: true })], null)?.id).toBe("me-off");
  });

  it("lets a pin win over the camera rule", () => {
    const roster = [me, tile("a"), tile("b", { camOn: true })];
    expect(featuredParticipant(roster, "a")?.id).toBe("a");
  });

  it("ignores a pin that names nobody in the roster", () => {
    const roster = [me, tile("a"), tile("b", { camOn: true })];
    expect(featuredParticipant(roster, "gone")?.id).toBe("b");
  });

  it("hands the slot to the next camera when the featured one stops", () => {
    const before = [me, tile("a", { camOn: true }), tile("b", { camOn: true })];
    const after = [me, tile("a"), tile("b", { camOn: true })];
    expect(featuredParticipant(before, null)?.id).toBe("a");
    expect(featuredParticipant(after, null)?.id).toBe("b");
  });

  it("returns null on an empty roster", () => {
    expect(featuredParticipant([], null)).toBeNull();
    expect(galleryParticipants([], null)).toEqual([]);
  });
});

describe("callSurfaceKind on a group call (§4.4)", () => {
  it("is the group surface before anyone else has joined", () => {
    // The person who started the call is its first participant; a portrait
    // of a person who is not there was what the web drew instead.
    expect(
      callSurfaceKind({
        groupCall: true,
        remoteCount: 0,
        remoteCameraOn: false,
        localCameraOn: true,
      }),
    ).toBe("group");
  });

  it("stays the group surface with one other person, camera or no camera", () => {
    expect(
      callSurfaceKind({ groupCall: true, remoteCount: 1, remoteCameraOn: true, localCameraOn: false }),
    ).toBe("group");
    expect(
      callSurfaceKind({ groupCall: true, remoteCount: 1, remoteCameraOn: false, localCameraOn: false }),
    ).toBe("group");
  });

  it("leaves the 1x1 paths alone when the flag is absent", () => {
    expect(
      callSurfaceKind({ remoteCount: 1, remoteCameraOn: false, localCameraOn: false }),
    ).toBe("voice");
  });
});
