// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `calls/video-surface.ts`: which browsers keep call pictures off the
 * compositor's video surfaces, what is written on `<html>`, and that the
 * stylesheet keys its rule on exactly that. The flash itself is a compositor
 * fault no test here can see; it was measured in Firefox (see the module).
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import {
  VIDEO_SURFACE_ATTRIBUTE,
  VIDEO_SURFACES_IN_PAGE,
  bootstrapVideoSurfaces,
  callVideoKeptInPage,
} from "./video-surface";

const MAC_FIREFOX = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:159.0) Gecko/20100101 Firefox/159.0";
const WINDOWS_FIREFOX =
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:159.0) Gecko/20100101 Firefox/159.0";
const LINUX_FIREFOX = "Mozilla/5.0 (X11; Linux x86_64; rv:159.0) Gecko/20100101 Firefox/159.0";
const ANDROID_FIREFOX = "Mozilla/5.0 (Android 15; Mobile; rv:159.0) Gecko/159.0 Firefox/159.0";
const IOS_FIREFOX =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) FxiOS/140.0 Mobile/15E148 Safari/605.1.15";
const CHROME =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const EDGE = `${CHROME} Edg/141.0.0.0`;
const MAC_SAFARI =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
const DESKTOP_WEBKIT = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)";

describe("callVideoKeptInPage", () => {
  it("is true for Firefox on every system Gecko runs on", () => {
    for (const ua of [MAC_FIREFOX, WINDOWS_FIREFOX, LINUX_FIREFOX, ANDROID_FIREFOX]) {
      expect(callVideoKeptInPage({ userAgent: ua }), ua).toBe(true);
    }
  });
  it("is false for Firefox on an iPhone, which is WebKit, and for every other engine", () => {
    for (const ua of [IOS_FIREFOX, CHROME, EDGE, MAC_SAFARI, DESKTOP_WEBKIT, ""]) {
      expect(callVideoKeptInPage({ userAgent: ua }), ua).toBe(false);
    }
  });
});

describe("bootstrapVideoSurfaces", () => {
  function root(): { attrs: Map<string, string>; setAttribute(n: string, v: string): void } {
    const attrs = new Map<string, string>();
    return { attrs, setAttribute: (n, v) => void attrs.set(n, v) };
  }
  it("marks the document in Firefox", () => {
    const r = root();
    bootstrapVideoSurfaces(r, { userAgent: MAC_FIREFOX });
    expect(r.attrs.get(VIDEO_SURFACE_ATTRIBUTE)).toBe(VIDEO_SURFACES_IN_PAGE);
  });
  it("writes nothing elsewhere", () => {
    const r = root();
    bootstrapVideoSurfaces(r, { userAgent: CHROME });
    expect(r.attrs.size).toBe(0);
  });
});

describe("the stylesheet rule", () => {
  const css = readFileSync(resolve(process.cwd(), "src/styles.css"), "utf8");
  const selector = `:root[${VIDEO_SURFACE_ATTRIBUTE}="${VIDEO_SURFACES_IN_PAGE}"] .lk-call video`;
  const at = css.indexOf(selector);
  const rule = at < 0 ? "" : css.slice(at, css.indexOf("}", at));

  it("is keyed on the attribute the bootstrap writes, for every video of the call screen", () => {
    expect(at, "no rule for the call screen's videos").toBeGreaterThan(-1);
    expect(rule).toContain(".lk-incoming-call video");
  });
  it("is a filter that is not the identity (an identity filter was measured to be dropped)", () => {
    const m = /filter:\s*contrast\(([\d.]+)\)/.exec(rule);
    expect(m, "no contrast filter").not.toBeNull();
    const amount = Number(m?.[1]);
    expect(amount).not.toBe(1);
    // Nothing a screen can show: under half a level of 255 at full range.
    expect(Math.abs(amount - 1) * 255).toBeLessThan(0.5);
  });
});
