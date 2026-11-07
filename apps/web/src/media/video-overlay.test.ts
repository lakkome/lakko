// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { isStatic, isTransparent, overlayStyle, PICTURES_THROUGH_CANVAS, type VideoBox } from "./video-overlay";

const box: VideoBox = {
  left: 12,
  top: 34,
  width: 240,
  height: 180,
  shown: true,
  objectFit: "cover",
  objectPosition: "50% 50%",
  borderRadius: "50%",
  transform: "matrix(-1, 0, 0, 1, 0, 0)",
  transformOrigin: "120px 90px",
  opacity: "1",
  visibility: "visible",
  zIndex: "auto",
  backgroundColor: "rgb(0, 0, 0)",
};

describe("overlayStyle", () => {
  it("lays the canvas on the video's box with the video's look, taking no clicks", () => {
    expect(overlayStyle(box)).toEqual({
      position: "absolute",
      left: "12px",
      top: "34px",
      width: "240px",
      height: "180px",
      margin: "0",
      "box-sizing": "border-box",
      display: "block",
      "object-fit": "cover",
      "object-position": "50% 50%",
      "border-radius": "50%",
      transform: "matrix(-1, 0, 0, 1, 0, 0)",
      "transform-origin": "120px 90px",
      opacity: "1",
      visibility: "visible",
      "z-index": "auto",
      "background-color": "rgb(0, 0, 0)",
      "pointer-events": "none",
    });
  });

  it("hides the canvas with a video that takes no part in layout, and keeps a hidden one hidden", () => {
    expect(overlayStyle({ ...box, shown: false }).display).toBe("none");
    expect(overlayStyle({ ...box, visibility: "hidden" }).visibility).toBe("hidden");
  });
});

describe("isStatic and isTransparent", () => {
  it("tell an unpositioned element and a colour that paints nothing", () => {
    expect(isStatic("static")).toBe(true);
    expect(isStatic("")).toBe(true);
    expect(isStatic("relative")).toBe(false);
    expect(isStatic("absolute")).toBe(false);
    expect(isTransparent("rgba(0, 0, 0, 0)")).toBe(true);
    expect(isTransparent("transparent")).toBe(true);
    expect(isTransparent("rgb(0, 0, 0)")).toBe(false);
  });
});

describe("PICTURES_THROUGH_CANVAS", () => {
  it("is off in the browser build the tests run as", () => {
    expect(PICTURES_THROUGH_CANVAS).toBe(false);
  });
});
