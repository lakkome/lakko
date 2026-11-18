// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The update banner gives the page a strip instead of floating over it (base
 * walk 2026-11-13: on a 1024x768 window it sat on New channel's `Create`, so
 * a click on Create hit the banner). jsdom computes no layout, so what is
 * proven here is the wiring: the banner marks `<html>` while it shows, and
 * the real stylesheet then ends `#root` above the strip.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { installStylesheet, one, renderUi, settle } from "../test/ui";
import { SW_UPDATE_EVENT } from "./register";
import { BANNER_ATTR, BANNER_HEIGHT_VAR, reserveBannerStrip, SwUpdateBanner } from "./SwUpdateBanner";

describe("SwUpdateBanner", () => {
  beforeEach(() => installStylesheet());
  afterEach(() => reserveBannerStrip(document.documentElement, null));

  it("reserves its strip while it shows", async () => {
    const html = document.documentElement;
    renderUi(() => <SwUpdateBanner />);
    await settle();
    expect(html.hasAttribute(BANNER_ATTR)).toBe(false);
    window.dispatchEvent(new CustomEvent(SW_UPDATE_EVENT, { detail: {} as ServiceWorker }));
    await settle();
    expect(one(".lk-update-banner").getAttribute("role")).toBe("status");
    expect(html.hasAttribute(BANNER_ATTR)).toBe(true);
    expect(html.style.getPropertyValue(BANNER_HEIGHT_VAR)).toMatch(/^\d+px$/);
  });

  it("is not a floating card over the page", async () => {
    renderUi(() => <SwUpdateBanner />);
    window.dispatchEvent(new CustomEvent(SW_UPDATE_EVENT, { detail: {} as ServiceWorker }));
    await settle();
    const banner = one(".lk-update-banner");
    // The old banner carried its own inline `left: 50%` card style.
    expect(banner.getAttribute("style") ?? "").toBe("");
    const css = getComputedStyle(banner);
    expect(css.position).toBe("fixed");
    expect(css.bottom).toBe("0px");
  });

  it("ends the page above the strip, and gives the space back", () => {
    const html = document.documentElement;
    const root = document.createElement("div");
    root.id = "root";
    document.body.appendChild(root);
    try {
      reserveBannerStrip(html, 57.2);
      expect(html.style.getPropertyValue(BANNER_HEIGHT_VAR)).toBe("58px");
      expect(getComputedStyle(root).height).toContain(BANNER_HEIGHT_VAR);
      reserveBannerStrip(html, null);
      expect(html.hasAttribute(BANNER_ATTR)).toBe(false);
      expect(getComputedStyle(root).height).not.toContain(BANNER_HEIGHT_VAR);
    } finally {
      root.remove();
    }
  });
});
