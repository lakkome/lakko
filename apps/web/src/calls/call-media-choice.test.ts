// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  callKindWord,
  mediaForPrejoin,
  mediaForStart,
  prejoinDefaults,
} from "./call-media-choice";

describe("mediaForStart", () => {
  it("starts an audio call with the camera off", () => {
    expect(mediaForStart("audio")).toEqual({ audio: true, video: false });
  });

  it("starts a video call with the camera on", () => {
    expect(mediaForStart("video")).toEqual({ audio: true, video: true });
  });

  it("always captures the microphone", () => {
    // The mic toggle mutes a captured track; it never declines the capture,
    // because a call placed with no audio transceiver cannot be unmuted
    // mid-call without a renegotiation.
    expect(mediaForStart("audio").audio).toBe(true);
    expect(mediaForStart("video").audio).toBe(true);
  });
});

describe("prejoinDefaults", () => {
  it("opens with the camera off for a voice call", () => {
    // The defect docs/ux/calls.md section 3 names: "an audio call opens the
    // pre-join screen with the camera on".
    expect(prejoinDefaults("audio")).toEqual({ micOn: true, camOn: false });
  });

  it("opens with the camera on for a video call", () => {
    expect(prejoinDefaults("video")).toEqual({ micOn: true, camOn: true });
  });

  it("opens with the microphone on either way", () => {
    expect(prejoinDefaults("audio").micOn).toBe(true);
    expect(prejoinDefaults("video").micOn).toBe(true);
  });
});

describe("mediaForPrejoin", () => {
  it("carries the toggles, not the button that opened the screen", () => {
    // A voice call whose camera was turned on IS a video call, and the callee
    // is told so.
    expect(mediaForPrejoin({ micOn: true, camOn: true })).toEqual({ audio: true, video: true });
    // ...and the other way round.
    expect(mediaForPrejoin({ micOn: true, camOn: false })).toEqual({ audio: true, video: false });
  });

  it("keeps audio on for a call joined muted", () => {
    expect(mediaForPrejoin({ micOn: false, camOn: false })).toEqual({ audio: true, video: false });
  });

  it("agrees with the button when the user changes nothing", () => {
    // The round trip that makes the two rules one rule: open at the default,
    // touch nothing, and the offer is the one the button asked for.
    for (const kind of ["audio", "video"] as const) {
      expect(mediaForPrejoin(prejoinDefaults(kind))).toEqual(mediaForStart(kind));
    }
  });
});

describe("callKindWord", () => {
  it("reads the media flags, so the word cannot disagree with the offer", () => {
    expect(callKindWord({ audio: true, video: true })).toBe("Video call");
    expect(callKindWord({ audio: true, video: false })).toBe("Voice call");
  });

  it("says the same word the callee is shown for the same start", () => {
    expect(callKindWord(mediaForStart("video"))).toBe("Video call");
    expect(callKindWord(mediaForStart("audio"))).toBe("Voice call");
  });
});
