// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  mirrorVideoToCanvas,
  videoNeedsCanvas,
  type CanvasSourceVideo,
  type CanvasTarget,
  type FrameScheduler,
} from "./video-canvas";

const SAFARI_LIKE =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)";
const CHROME =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const WEBVIEW2 = `${CHROME} Edg/141.0.0.0`;
const MAC_SAFARI =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
const IPHONE_SAFARI =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1";
const MAC_FIREFOX = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:140.0) Gecko/20100101 Firefox/140.0";

describe("videoNeedsCanvas", () => {
  it("draws through a canvas in the desktop app on WebKit only", () => {
    expect(videoNeedsCanvas(true, { userAgent: SAFARI_LIKE })).toBe(true);
    expect(videoNeedsCanvas(true, { userAgent: CHROME })).toBe(false);
    expect(videoNeedsCanvas(true, { userAgent: WEBVIEW2 })).toBe(false);
  });
  it("in Safari on a Mac, and in no other browser", () => {
    expect(videoNeedsCanvas(false, { userAgent: MAC_SAFARI })).toBe(true);
    expect(videoNeedsCanvas(false, { userAgent: SAFARI_LIKE })).toBe(false);
    expect(videoNeedsCanvas(false, { userAgent: IPHONE_SAFARI })).toBe(false);
    expect(videoNeedsCanvas(false, { userAgent: CHROME })).toBe(false);
    expect(videoNeedsCanvas(false, { userAgent: WEBVIEW2 })).toBe(false);
    expect(videoNeedsCanvas(false, { userAgent: MAC_FIREFOX })).toBe(false);
  });
});

interface FakeVideo extends CanvasSourceVideo {
  videoWidth: number;
  videoHeight: number;
  pending: Array<(now: number) => void>;
  cancelled: number[];
}

function video(withCallback: boolean): FakeVideo {
  const v: FakeVideo = {
    videoWidth: 0,
    videoHeight: 0,
    pending: [],
    cancelled: [],
  };
  if (withCallback) {
    v.requestVideoFrameCallback = (cb): number => {
      v.pending.push(cb);
      return v.pending.length;
    };
    v.cancelVideoFrameCallback = (h): void => {
      v.cancelled.push(h);
    };
  }
  return v;
}

function canvas(): CanvasTarget<FakeVideo> & { draws: Array<[number, number]> } {
  const c = {
    width: 300,
    height: 150,
    draws: [] as Array<[number, number]>,
    getContext: () => ({
      drawImage: (_v: FakeVideo, _x: number, _y: number, w: number, h: number): void => {
        c.draws.push([w, h]);
      },
    }),
  };
  return c;
}

const NO_FRAMES: FrameScheduler = { request: () => 0, cancel: () => {} };

function frame(v: FakeVideo): void {
  const cb = v.pending.shift();
  cb?.(0);
}

describe("mirrorVideoToCanvas", () => {
  it("draws nothing before the video has a size, then draws each frame at the video's size", () => {
    const v = video(true);
    const c = canvas();
    mirrorVideoToCanvas(v, c, NO_FRAMES);
    expect(c.draws).toEqual([]);
    v.videoWidth = 1280;
    v.videoHeight = 720;
    frame(v);
    frame(v);
    expect(c.draws).toEqual([
      [1280, 720],
      [1280, 720],
    ]);
    expect([c.width, c.height]).toEqual([1280, 720]);
  });

  it("stops asking for frames once stopped", () => {
    const v = video(true);
    const c = canvas();
    const stop = mirrorVideoToCanvas(v, c, NO_FRAMES);
    stop();
    expect(v.cancelled.length).toBe(1);
    v.videoWidth = 640;
    v.videoHeight = 360;
    frame(v);
    expect(c.draws).toEqual([]);
  });

  it("paces itself with the fallback where the element has no per-frame callback", () => {
    const v = video(false);
    v.videoWidth = 320;
    v.videoHeight = 180;
    const c = canvas();
    const queued: Array<() => void> = [];
    const sched: FrameScheduler = {
      request: (cb) => queued.push(cb),
      cancel: () => {},
    };
    mirrorVideoToCanvas(v, c, sched);
    queued.shift()?.();
    expect(c.draws).toEqual([
      [320, 180],
      [320, 180],
    ]);
  });
});
