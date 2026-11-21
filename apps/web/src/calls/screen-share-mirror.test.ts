// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { ownShareView, sameSize, sameView, type MirrorFacts } from "./screen-share-mirror";

const base: MirrorFacts = {
  displaySurface: undefined,
  captured: {},
  captureHandleIsOwn: undefined,
  ownWindow: { width: 1440, height: 900 },
  ownScreen: { width: 1512, height: 982 },
  devicePixelRatio: 2,
  screenExtended: undefined,
};

const f = (over: Partial<MirrorFacts>): MirrorFacts => ({ ...base, ...over });

describe("ownShareView", () => {
  it("calls a tab a mirror only when the Capture Handle names this page", () => {
    expect(ownShareView(f({ displaySurface: "browser", captureHandleIsOwn: true }))).toEqual({
      kind: "mirror",
      surface: "tab",
    });
    expect(ownShareView(f({ displaySurface: "browser", captureHandleIsOwn: false }))).toEqual({ kind: "picture" });
    // A tab of this window's size is still another tab.
    expect(
      ownShareView(f({ displaySurface: "browser", captured: { width: 2880, height: 1800 } })),
    ).toEqual({ kind: "picture" });
  });

  it("calls a window a mirror when it has this window's size, in device or CSS pixels", () => {
    expect(ownShareView(f({ displaySurface: "window", captured: { width: 2880, height: 1800 } }))).toEqual({
      kind: "mirror",
      surface: "window",
    });
    expect(ownShareView(f({ displaySurface: "window", captured: { width: 1441, height: 899 } }))).toEqual({
      kind: "mirror",
      surface: "window",
    });
    expect(ownShareView(f({ displaySurface: "window", captured: { width: 1920, height: 1080 } }))).toEqual({
      kind: "picture",
    });
    expect(ownShareView(f({ displaySurface: "window" }))).toEqual({ kind: "picture" });
  });

  it("calls a screen a mirror on a single screen or at this screen's size", () => {
    expect(ownShareView(f({ displaySurface: "monitor", screenExtended: false }))).toEqual({
      kind: "mirror",
      surface: "screen",
    });
    expect(ownShareView(f({ displaySurface: "monitor", captured: { width: 3024, height: 1964 } }))).toEqual({
      kind: "mirror",
      surface: "screen",
    });
    expect(
      ownShareView(f({ displaySurface: "monitor", screenExtended: true, captured: { width: 3840, height: 2160 } })),
    ).toEqual({ kind: "picture" });
  });

  it("falls back on the sizes when no surface is reported", () => {
    expect(ownShareView(f({ captured: { width: 1440, height: 900 } }))).toEqual({ kind: "mirror", surface: "window" });
    expect(ownShareView(f({ captured: { width: 1512, height: 982 } }))).toEqual({ kind: "mirror", surface: "screen" });
    expect(ownShareView(f({ captured: { width: 800, height: 600 } }))).toEqual({ kind: "picture" });
    expect(ownShareView(f({}))).toEqual({ kind: "picture" });
  });
});

describe("sameSize", () => {
  it("allows two pixels and refuses a missing or zero size", () => {
    expect(sameSize({ width: 102, height: 98 }, { width: 100, height: 100 }, 1)).toBe(true);
    expect(sameSize({ width: 103, height: 100 }, { width: 100, height: 100 }, 1)).toBe(false);
    expect(sameSize({ width: 0, height: 0 }, { width: 0, height: 0 }, 1)).toBe(false);
    expect(sameSize({ width: 100 }, { width: 100, height: 100 }, 1)).toBe(false);
    expect(sameSize({ width: 100, height: 100 }, undefined, 1)).toBe(false);
  });
});

describe("sameView", () => {
  it("compares kind and surface", () => {
    expect(sameView({ kind: "picture" }, { kind: "picture" })).toBe(true);
    expect(sameView({ kind: "mirror", surface: "tab" }, { kind: "mirror", surface: "window" })).toBe(false);
    expect(sameView(null, null)).toBe(true);
    expect(sameView(null, { kind: "picture" })).toBe(false);
  });
});

describe("parseHostSurfaces", () => {
  it("reads counts and sizes and treats anything else as unknown", async () => {
    const { parseHostSurfaces } = await import("./screen-share-host");
    expect(parseHostSurfaces({ screens: 1, window: { width: 2200, height: 1520 }, screen: { width: 3024, height: 1964 } })).toEqual({
      screens: 1,
      window: { width: 2200, height: 1520 },
      screen: { width: 3024, height: 1964 },
    });
    expect(parseHostSurfaces({ screens: 0, window: null, screen: { width: "x" } })).toEqual({
      screens: undefined,
      window: undefined,
      screen: undefined,
    });
    expect(parseHostSurfaces(null).screens).toBeUndefined();
  });
});
