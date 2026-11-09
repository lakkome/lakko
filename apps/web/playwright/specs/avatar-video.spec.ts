// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/profile-screen.md section 7 — where a profile video plays.
 *
 * With a mouse: the list row plays only while the pointer is over it, the chat
 * header only while the pointer is over it. On a touch-only phone: the header
 * plays all the time, the row never does, and a tap does not count as a hover.
 * The video is always muted.
 */

import { existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { devices, expect, test } from "@playwright/test";

import { openCase } from "./harness";

const AVATAR_MP4 = resolve(dirname(fileURLToPath(import.meta.url)), "../fixtures/avatar.mp4");

test.skip(!existsSync(AVATAR_MP4), "avatar.mp4 missing: playwright/fixtures/make-assets.mjs needs ffmpeg");

test.describe("with a mouse", () => {
  test("the row plays only while hovered, and is muted", async ({ page }) => {
    await openCase(page, "avatar-video");
    const row = page.getByTestId("avatar-row");
    await expect(row.locator("video")).toHaveCount(0);
    await expect(row.locator("img")).toHaveCount(1);

    await row.hover();
    const video = row.locator("video");
    await expect(video).toHaveCount(1);
    expect(await video.evaluate((v: HTMLVideoElement) => v.muted)).toBe(true);
    await expect.poll(() => video.evaluate((v: HTMLVideoElement) => !v.paused && v.readyState >= 2)).toBe(true);

    await page.getByTestId("avatar-away").hover();
    await expect(row.locator("video")).toHaveCount(0);
    await expect(row.locator("img")).toHaveCount(1);
  });

  test("the header plays only while hovered", async ({ page }) => {
    await openCase(page, "avatar-video");
    const header = page.getByTestId("avatar-header");
    await expect(header.locator("video")).toHaveCount(0);
    await header.hover();
    await expect(header.locator("video")).toHaveCount(1);
    await page.getByTestId("avatar-away").hover();
    await expect(header.locator("video")).toHaveCount(0);
  });
});

test.describe("on a touch-only phone", () => {
  const { defaultBrowserType: _browser, ...phone } = devices["Pixel 7"];
  test.use(phone);
  // Chromium's mobile emulation is what answers `(hover: none)`.
  test.skip(({ browserName }) => browserName !== "chromium", "mobile emulation is Chromium's");

  test("the header always plays, the row never does, a tap is not a hover", async ({ page }) => {
    await openCase(page, "avatar-video");
    expect(await page.evaluate(() => window.matchMedia("(hover: hover)").matches)).toBe(false);
    const header = page.getByTestId("avatar-header");
    await expect(header.locator("video")).toHaveCount(1);
    expect(await header.locator("video").evaluate((v: HTMLVideoElement) => v.muted)).toBe(true);

    const row = page.getByTestId("avatar-row");
    await row.tap();
    await expect(row.locator("video")).toHaveCount(0);
    await expect(row.locator("img")).toHaveCount(1);
  });
});
