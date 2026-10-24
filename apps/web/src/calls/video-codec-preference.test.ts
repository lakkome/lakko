// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { preferVp8, vp8Preferences, type CodecCapabilityLike, type CodecPreferenceTransceiver } from "./video-codec-preference";

/** WebKit's list order: H264 first. */
const WEBKIT: CodecCapabilityLike[] = [
  { mimeType: "video/H264", clockRate: 90000, sdpFmtpLine: "packetization-mode=1" },
  { mimeType: "video/rtx", clockRate: 90000 },
  { mimeType: "video/VP8", clockRate: 90000 },
  { mimeType: "video/H265", clockRate: 90000 },
  { mimeType: "video/red", clockRate: 90000 },
  { mimeType: "video/ulpfec", clockRate: 90000 },
];

function transceiver(kind: "audio" | "video"): CodecPreferenceTransceiver & { set: CodecCapabilityLike[] | null } {
  const t = {
    receiver: { track: { kind } },
    set: null as CodecCapabilityLike[] | null,
    setCodecPreferences(codecs: CodecCapabilityLike[]): void {
      t.set = codecs;
    },
  };
  return t;
}

describe("vp8Preferences", () => {
  it("keeps VP8 and the formats that wrap it, and drops every other media codec", () => {
    expect(vp8Preferences(WEBKIT)?.map((c) => c.mimeType)).toEqual([
      "video/VP8",
      "video/rtx",
      "video/red",
      "video/ulpfec",
    ]);
  });

  it("gives nothing to set when the browser has no VP8", () => {
    expect(vp8Preferences(WEBKIT.filter((c) => c.mimeType !== "video/VP8"))).toBeNull();
  });
});

describe("preferVp8", () => {
  it("sets VP8 on every video transceiver and leaves the audio ones alone", () => {
    const audio = transceiver("audio");
    const video = transceiver("video");
    expect(preferVp8([audio, video], WEBKIT)).toEqual({ applied: 1, unsupported: 0 });
    expect(audio.set).toBeNull();
    expect(video.set?.[0]?.mimeType).toBe("video/VP8");
  });

  it("counts a transceiver it could not set instead of hiding it", () => {
    const noApi = { receiver: { track: { kind: "video" } } };
    const refuses = {
      receiver: { track: { kind: "video" } },
      setCodecPreferences(): void {
        throw new Error("InvalidModificationError");
      },
    };
    expect(preferVp8([noApi, refuses], WEBKIT)).toEqual({ applied: 0, unsupported: 2 });
    expect(preferVp8([transceiver("video")], null)).toEqual({ applied: 0, unsupported: 1 });
  });
});
