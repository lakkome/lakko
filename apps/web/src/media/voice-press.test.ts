// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { voicePress, type VoicePressState } from "./voice-press";

function state(over: Partial<VoicePressState> = {}): VoicePressState {
  return { uploading: false, playing: false, ready: false, ...over };
}

describe("voicePress", () => {
  it("does nothing while the take is still uploading", () => {
    expect(voicePress(state({ uploading: true }))).toBe("ignore");
    // Even if it somehow got as far as playing: the button is disabled, and a
    // press on it is not a pause request.
    expect(voicePress(state({ uploading: true, playing: true, ready: true }))).toBe("ignore");
  });

  it("pauses what is playing", () => {
    expect(voicePress(state({ playing: true, ready: true }))).toBe("pause");
  });

  // The regression this module exists for: when the bytes are here the press
  // must resolve to "play-now", which the caller is required to act on
  // synchronously. If this ever becomes "prepare-then-play" again, the press
  // goes back to awaiting before `play()` — and a voice message that is fully
  // warm silently refuses to play on any engine that drops the user
  // activation at the end of the handler's task.
  it("plays immediately when the element already has its bytes", () => {
    expect(voicePress(state({ ready: true }))).toBe("play-now");
  });

  it("prepares first only when there are no bytes yet", () => {
    expect(voicePress(state())).toBe("prepare-then-play");
  });

  it("never asks to prepare something that is already playing", () => {
    // A playing element is sourced by definition, but the order of the rules
    // is what guarantees it: pause is decided before readiness is consulted.
    expect(voicePress(state({ playing: true, ready: false }))).toBe("pause");
  });
});
