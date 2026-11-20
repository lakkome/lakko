// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  FRAME_INTERVAL_MS,
  FrameSchedule,
  drawPlan,
  frameSize,
  osInsetCorner,
  parseCallWindowView,
  pickShownVideo,
  pressIsDrag,
  sameView,
  type CallWindowView,
  type VideoLike,
} from "./desktop-call-window-model";

const VIEW: CallWindowView = {
  status: "12:04",
  encrypted: true,
  name: "Dana",
  others: 0,
  mute: { icon: "mic-fill", tone: "quiet", pressed: false, label: "Mute microphone", disabled: false },
  picture: { kind: "frame" },
  inset: true,
  spoken: "Return to the call with Dana, 12 minutes",
  announced: "Connected",
  region: "Call",
  lang: "en",
  dir: "ltr",
};

describe("reading a view in the OS window", () => {
  it("reads back what the main page posts", () => {
    const round = parseCallWindowView(JSON.parse(JSON.stringify(VIEW)));
    expect(round).toEqual(VIEW);
    expect(sameView(round, VIEW)).toBe(true);
  });

  it("takes a view with no mute button and an initials picture", () => {
    const v = parseCallWindowView({ ...VIEW, mute: null, picture: { kind: "initials", text: "DA", tone: 3 } });
    expect(v?.mute).toBeNull();
    expect(v?.picture).toEqual({ kind: "initials", text: "DA", tone: 3 });
  });

  it("refuses a malformed view as a whole", () => {
    expect(parseCallWindowView(null)).toBeNull();
    expect(parseCallWindowView("view")).toBeNull();
    expect(parseCallWindowView({ ...VIEW, others: -1 })).toBeNull();
    expect(parseCallWindowView({ ...VIEW, others: 1.5 })).toBeNull();
    expect(parseCallWindowView({ ...VIEW, dir: "up" })).toBeNull();
    expect(parseCallWindowView({ ...VIEW, picture: { kind: "video" } })).toBeNull();
    expect(parseCallWindowView({ ...VIEW, picture: { kind: "initials", text: "D", tone: 7 } })).toBeNull();
    expect(parseCallWindowView({ ...VIEW, mute: undefined })).toBeNull();
    expect(parseCallWindowView({ ...VIEW, mute: { ...VIEW.mute, disabled: "no" } })).toBeNull();
    expect(parseCallWindowView({ ...VIEW, name: "x".repeat(600) })).toBeNull();
    expect(parseCallWindowView({ ...VIEW, encrypted: 1 })).toBeNull();
  });

  it("tells a changed view from the same one", () => {
    expect(sameView(VIEW, { ...VIEW, status: "12:05" })).toBe(false);
    expect(sameView(null, VIEW)).toBe(false);
    expect(sameView(null, null)).toBe(true);
  });
});

describe("drawing a picture into a frame", () => {
  it("crops a wider picture to fill, centred", () => {
    // 4:3 camera into a 16:9 frame: the top and bottom are cropped.
    expect(drawPlan(640, 480, 320, 180, "cover")).toEqual({
      sx: 0,
      sy: 60,
      sw: 640,
      sh: 360,
      dx: 0,
      dy: 0,
      dw: 320,
      dh: 180,
    });
    // An upright phone camera: the sides are cropped.
    const upright = drawPlan(720, 1280, 320, 180, "cover");
    expect(upright?.sw).toBeCloseTo(720);
    expect(upright?.sh).toBeCloseTo(405);
    expect(upright?.sy).toBeCloseTo((1280 - 405) / 2);
  });

  it("letterboxes a shared screen whole", () => {
    const plan = drawPlan(1000, 1000, 320, 180, "contain");
    expect(plan).toEqual({ sx: 0, sy: 0, sw: 1000, sh: 1000, dx: 70, dy: 0, dw: 180, dh: 180 });
    const wide = drawPlan(2000, 500, 320, 180, "contain");
    expect(wide?.dw).toBe(320);
    expect(wide?.dh).toBe(80);
    expect(wide?.dy).toBe(50);
  });

  it("draws nothing from a picture with no size yet", () => {
    expect(drawPlan(0, 0, 320, 180, "cover")).toBeNull();
    expect(drawPlan(640, 480, 0, 180, "contain")).toBeNull();
    expect(drawPlan(Number.NaN, 480, 320, 180, "cover")).toBeNull();
  });

  it("sizes a frame for the screen, at most twice the points", () => {
    expect(frameSize(320, 180, 1)).toEqual({ width: 320, height: 180 });
    expect(frameSize(320, 180, 2)).toEqual({ width: 640, height: 360 });
    expect(frameSize(320, 180, 3)).toEqual({ width: 640, height: 360 });
    expect(frameSize(320, 180, 0.5)).toEqual({ width: 320, height: 180 });
    expect(frameSize(320, 180, Number.NaN)).toEqual({ width: 320, height: 180 });
  });
});

describe("which video a tile is showing", () => {
  const video = (over: Partial<VideoLike> & { shown?: string }): VideoLike => ({
    readyState: 4,
    videoWidth: 640,
    videoHeight: 360,
    dataset: over.shown === undefined ? {} : { shown: over.shown },
    ...over,
  });

  it("skips the layer underneath", () => {
    const under = video({ shown: "false" });
    const top = video({ shown: "true" });
    expect(pickShownVideo([under, top])).toBe(top);
  });

  it("takes an unlayered video, and none that has no frame yet", () => {
    const plain = video({});
    expect(pickShownVideo([plain])).toBe(plain);
    expect(pickShownVideo([video({ readyState: 1 })])).toBeNull();
    expect(pickShownVideo([video({ videoWidth: 0 })])).toBeNull();
    expect(pickShownVideo([])).toBeNull();
  });
});

describe("when a frame is sent", () => {
  it("sends a moving picture at most once an interval", () => {
    const s = new FrameSchedule();
    expect(s.dueForVideo(0)).toBe(true);
    s.started(0, null);
    s.finished();
    expect(s.dueForVideo(FRAME_INTERVAL_MS - 1)).toBe(false);
    expect(s.dueForVideo(FRAME_INTERVAL_MS)).toBe(true);
  });

  it("drops frames while one is still on its way rather than queueing them", () => {
    const s = new FrameSchedule();
    s.started(0, null);
    expect(s.dueForVideo(10 * FRAME_INTERVAL_MS)).toBe(false);
    s.finished();
    expect(s.dueForVideo(10 * FRAME_INTERVAL_MS)).toBe(true);
  });

  it("sends a photo once, again only when it changes or comes back", () => {
    const s = new FrameSchedule();
    expect(s.dueForStill("blob:a")).toBe(true);
    s.started(0, "blob:a");
    s.finished();
    expect(s.dueForStill("blob:a")).toBe(false);
    expect(s.dueForStill("blob:b")).toBe(true);
    s.reset();
    expect(s.dueForStill("blob:a")).toBe(true);
  });
});

describe("a press on the OS window", () => {
  it("is a click until it moves six points", () => {
    expect(pressIsDrag(0, 0)).toBe(false);
    expect(pressIsDrag(3, 4)).toBe(false);
    expect(pressIsDrag(6, 0)).toBe(true);
    expect(pressIsDrag(-4, -5)).toBe(true);
  });
});

describe("where the inset sits in the OS window", () => {
  it("in the bottom corner away from the nearer screen edge", () => {
    // Right half of a 1920-wide screen: inset on the left.
    expect(osInsetCorner(1584, 320, 0, 1920)).toBe("bottom-left");
    // Left half: inset on the right.
    expect(osInsetCorner(16, 320, 0, 1920)).toBe("bottom-right");
    // A screen to the left of the main one.
    expect(osInsetCorner(-336, 320, -1920, 1920)).toBe("bottom-left");
  });
});
