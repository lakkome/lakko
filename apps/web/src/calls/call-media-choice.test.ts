// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { callKindWord, mediaForStart, mediaForToggles } from "./call-media-choice";

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

describe("mediaForToggles", () => {
  it("carries the toggles, not the button that opened the screen", () => {
    // A video call whose camera was turned off in the check IS a voice call,
    // and the callee is told so (docs/ux/calls.md section 1).
    expect(mediaForToggles({ micOn: true, camOn: false })).toEqual({ audio: true, video: false });
    expect(mediaForToggles({ micOn: true, camOn: true })).toEqual({ audio: true, video: true });
  });

  it("keeps audio on for a call placed muted", () => {
    expect(mediaForToggles({ micOn: false, camOn: false })).toEqual({ audio: true, video: false });
  });

  it("agrees with the video button when the user changes nothing", () => {
    // The camera check always opens with the camera on (section 3.2 — it is a
    // camera check), so leaving it alone must place exactly the call the video
    // button asked for. This is the round trip `prejoinDefaults` used to carry
    // before section 3 retired the pre-join screen.
    expect(mediaForToggles({ micOn: true, camOn: true })).toEqual(mediaForStart("video"));
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
