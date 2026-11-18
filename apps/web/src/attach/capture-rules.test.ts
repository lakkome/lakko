// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { pickVideoMime, previewVerdict } from "./capture-rules";

const MiB = 1024 * 1024;

describe("capture rules", () => {
  it("records only MP4", () => {
    expect(pickVideoMime((m) => m === "video/mp4")).toBe("video/mp4");
    expect(pickVideoMime((m) => m.startsWith("video/webm"))).toBeNull();
    expect(
      pickVideoMime(() => {
        throw new Error("no");
      }),
    ).toBeNull();
  });
  it("sends photos and MP4 videos as media", () => {
    expect(previewVerdict({ type: "image/heic", size: 10 }, "media", false)).toBe("photo");
    expect(previewVerdict({ type: "video/mp4", size: 10 }, "media", false)).toBe("video");
  });
  it("refuses another video format or one over the cap, unless sent as a file", () => {
    expect(previewVerdict({ type: "video/quicktime", size: 10 }, "media", false)).toBe("video-refused");
    expect(previewVerdict({ type: "video/mp4", size: 51 * MiB }, "media", false)).toBe("video-refused");
    expect(previewVerdict({ type: "video/quicktime", size: 10 }, "media", true)).toBe("file");
  });
  it("removes a file over the cap", () => {
    expect(previewVerdict({ type: "application/pdf", size: 51 * MiB }, "file", false)).toBe("too-big");
    expect(previewVerdict({ type: "video/mp4", size: 51 * MiB }, "media", true)).toBe("too-big");
    expect(previewVerdict({ type: "audio/mpeg", size: 1 }, "audio", false)).toBe("file");
  });
});
