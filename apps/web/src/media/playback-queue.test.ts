// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX spec §2 — auto-continue to the next consecutive unplayed voice message
 * from the same sender, the speed toggle, and the recording format choice.
 */
import { describe, expect, it } from "vitest";

import { nextPlaybackRate, nextVoiceToPlay, rateLabel, type VoiceQueueItem } from "./playback-queue";
import { baseMime, pickRecordingMime, squareCrop } from "./capture-format";

const v = (key: string, senderKey: string | undefined, played = false, isVoice = true): VoiceQueueItem => ({
  key,
  senderKey,
  played,
  isVoice,
});

describe("nextVoiceToPlay", () => {
  it("continues to the next unplayed voice from the same sender", () => {
    expect(nextVoiceToPlay([v("1", "A", true), v("2", "A")], "1")).toBe("2");
  });

  it("skips already-played voice messages inside the same run", () => {
    expect(nextVoiceToPlay([v("1", "A", true), v("2", "A", true), v("3", "A")], "1")).toBe("3");
  });

  it("stops at a different sender, a non-voice row or the end", () => {
    expect(nextVoiceToPlay([v("1", "A"), v("2", "B"), v("3", "A")], "1")).toBeNull();
    expect(nextVoiceToPlay([v("1", "A"), v("t", "A", false, false), v("3", "A")], "1")).toBeNull();
    expect(nextVoiceToPlay([v("1", "A")], "1")).toBeNull();
  });

  it("an unknown sender never continues, and an unknown key finds nothing", () => {
    expect(nextVoiceToPlay([v("1", undefined), v("2", undefined)], "1")).toBeNull();
    expect(nextVoiceToPlay([v("1", "A"), v("2", "A")], "missing")).toBeNull();
  });
});

describe("rates", () => {
  it("cycles 1x, 1.5x, 2x", () => {
    expect(nextPlaybackRate(1)).toBe(1.5);
    expect(nextPlaybackRate(1.5)).toBe(2);
    expect(nextPlaybackRate(2)).toBe(1);
    expect(nextPlaybackRate(3)).toBe(1);
    expect(rateLabel(1.5)).toBe("1.5x");
  });
});

describe("capture format", () => {
  it("prefers the codec-pinned MP4 type and never falls back to WebM", () => {
    const supported = new Set(["audio/webm;codecs=opus", "audio/mp4"]);
    expect(pickRecordingMime("voice", (m) => supported.has(m))).toBe("audio/mp4");
    expect(pickRecordingMime("voice", (m) => m === "audio/mp4;codecs=mp4a.40.2" || m === "audio/mp4")).toBe(
      "audio/mp4;codecs=mp4a.40.2",
    );
    expect(pickRecordingMime("voice", (m) => m.includes("webm"))).toBeNull();
    expect(pickRecordingMime("video_note", (m) => m === "video/mp4")).toBe("video/mp4");
    expect(pickRecordingMime("video_note", () => { throw new Error("x"); })).toBeNull();
    expect(pickRecordingMime("voice", undefined)).toBeNull();
    expect(baseMime("Video/MP4; codecs=avc1")).toBe("video/mp4");
  });

  it("crops the centred square and caps it at 480, even-sided", () => {
    expect(squareCrop(1280, 720)).toEqual({ sx: 280, sy: 0, sourceSide: 720, outputSide: 480 });
    expect(squareCrop(360, 640)).toEqual({ sx: 0, sy: 140, sourceSide: 360, outputSide: 360 });
    expect(squareCrop(321, 400)).toEqual({ sx: 0, sy: 39, sourceSide: 321, outputSide: 320 });
    expect(squareCrop(0, 720)).toBeNull();
  });
});
