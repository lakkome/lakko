// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { jpegName, scaledSize } from "./photo-encode";

describe("photo encode", () => {
  it("scales the longest side down to the cap", () => {
    expect(scaledSize(5120, 2880, 2560)).toEqual({ width: 2560, height: 1440 });
    expect(scaledSize(3000, 4000, 2560)).toEqual({ width: 1920, height: 2560 });
  });
  it("never enlarges", () => {
    expect(scaledSize(800, 600, 2560)).toEqual({ width: 800, height: 600 });
  });
  it("keeps at least one pixel", () => {
    expect(scaledSize(10000, 1, 320)).toEqual({ width: 320, height: 1 });
  });
  it("renames to jpg", () => {
    expect(jpegName("IMG_1.HEIC")).toBe("IMG_1.jpg");
    expect(jpegName("noext")).toBe("noext.jpg");
    expect(jpegName("")).toBe("photo.jpg");
  });
});
