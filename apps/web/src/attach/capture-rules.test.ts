// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  cameraVideoConstraint,
  isPhoneClassBrowser,
  nextCameraId,
  pickVideoMime,
  previewVerdict,
} from "./capture-rules";

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
  it("opens the chosen camera by id, a phone's front camera by facing mode, otherwise the default device", () => {
    expect(cameraVideoConstraint({ deviceId: "cam-2", phoneClass: false }).deviceId).toEqual({ exact: "cam-2" });
    expect(cameraVideoConstraint({ deviceId: "cam-2", phoneClass: true }).facingMode).toBeUndefined();
    const desktop = cameraVideoConstraint({ deviceId: null, phoneClass: false });
    expect(desktop.facingMode).toBeUndefined();
    expect(desktop.deviceId).toBeUndefined();
    expect(cameraVideoConstraint({ deviceId: "", phoneClass: false }).deviceId).toBeUndefined();
    expect(cameraVideoConstraint({ deviceId: undefined, phoneClass: true }).facingMode).toBe("user");
  });
  it("cycles through cameras by id, skipping ones listed without an id", () => {
    expect(nextCameraId(["a", "b", "c"], "a")).toBe("b");
    expect(nextCameraId(["a", "b", "c"], "c")).toBe("a");
    expect(nextCameraId(["a", "", "c"], "a")).toBe("c");
    expect(nextCameraId(["a", "b"], "unknown")).toBe("a");
    expect(nextCameraId(["a", "b"], null)).toBe("a");
    expect(nextCameraId(["a"], "a")).toBeNull();
    expect(nextCameraId(["", ""], null)).toBeNull();
  });
  it("treats phones and tablets as phone-class, a Mac desktop app as not", () => {
    expect(isPhoneClassBrowser({ userAgent: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)" })).toBe(true);
    expect(isPhoneClassBrowser({ userAgent: "Mozilla/5.0 (Linux; Android 15; Pixel 9)" })).toBe(true);
    expect(isPhoneClassBrowser({ userAgent: "x", userAgentData: { mobile: true } })).toBe(true);
    expect(
      isPhoneClassBrowser({ userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15" }),
    ).toBe(false);
  });
});
