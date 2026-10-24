// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The real-browser UI tests.
 *
 * ## Why this exists beside the jsdom project
 *
 * `vitest.config.ts`'s `ui` project covers the layer above the rules: which
 * gesture reaches what handler, what a component hands its collaborators and
 * in what order, where a node lands in the tree. `src/test/ui.tsx` states what
 * it cannot do, and two of the defects it covers are reachable there only by
 * PROXY — the bubble's ~28px jump is asserted as "the cascade resolves
 * `min-height` to `var(--control-h)`", which a height set any other way
 * satisfies, and the time-below-the-duration as a parentage fact. This project
 * measures both directly, in a browser that has layout and a media engine.
 *
 * ## What it deliberately is not
 *
 * Not end to end. There is no test account and entering a credential is
 * forbidden, so nothing here signs in or talks to a service. A spec opens the
 * fixture page (`playwright/harness/`) at one `?case=` and gets one PRODUCTION
 * component with canned props — see `harness/cases.tsx` for the discipline
 * that makes that worth measuring.
 *
 * And NOT a cover for the autoplay-refusal defect. Neither headless engine
 * enforces the transient user activation that defect depended on; the numbers
 * are in `playwright/README.md` and the spec says so where it is measured.
 *
 * ## Two browsers
 *
 * Chromium runs everything. WebKit runs the layout and menu specs and skips the
 * recording ones by name, because the microphone is a Chromium flag it has no
 * equivalent for.
 */

import { defineConfig, devices } from "@playwright/test";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const MIC_WAV = resolve(HERE, "playwright/fixtures/mic.wav");

export default defineConfig({
  testDir: "./playwright/specs",
  globalSetup: "./playwright/global-setup.ts",
  // Every spec drives one page in one browser; nothing is shared, so the
  // default parallelism is fine. Retries are off ON PURPOSE: these tests
  // measure geometry and timing, and a retry would turn a real flake into a
  // pass nobody looks at. A flaky test here is a bug in the test.
  retries: 0,
  fullyParallel: true,
  reporter: process.env["CI"] === undefined ? [["list"]] : [["list"], ["html", { open: "never" }]],
  timeout: 30_000,
  expect: { timeout: 7_000 },

  use: {
    baseURL: "http://127.0.0.1:5174",
    // A fixed viewport, because a bubble's width decides how its quiet line
    // wraps and every measurement below is a geometry.
    viewport: { width: 900, height: 700 },
    trace: "retain-on-failure",
  },

  projects: [
    {
      name: "chromium",
      use: {
        ...devices["Desktop Chrome"],
        // The recorder asks for the microphone and — for a video note — the
        // camera for real. Without the microphone grant the machine lands in
        // its denied card; without the CAMERA grant it does something quieter
        // and easy to misread, because a permission the browser reports as
        // `prompt` means "this gesture would raise a dialog", and §6 says such
        // a gesture does not record. Measured: no bar, no red dot, no recorder,
        // and nothing on screen to say why.
        permissions: ["microphone", "camera"],
        launchOptions: {
          args: [
            // A fake microphone and camera, so nothing touches real hardware
            // and a headless machine has a device to open at all.
            "--use-fake-device-for-media-stream",
            "--use-fake-ui-for-media-stream",
            // ...fed by a real file, so a capture that works is loud and a
            // capture that did not happen is silent. Chromium loops it.
            `--use-file-for-fake-audio-capture=${MIC_WAV}`,
            // The strictest autoplay policy Chromium offers, named rather than
            // left at the default so a default that moved could not quietly
            // relax what the playback spec runs against. It does NOT make the
            // activation-loss defect reproducible — measured in this session,
            // Chromium allows a deferred `play()` even as the first click of a
            // fresh document, because its activation is sticky per document
            // rather than transient. `playwright/README.md` has the numbers.
            "--autoplay-policy=document-user-activation-required",
          ],
        },
      },
    },
    {
      name: "webkit",
      use: { ...devices["Desktop Safari"] },
    },
    {
      // The one-to-one call only: the other specs measure layout and
      // recording against Chromium and WebKit, and were never run in Firefox.
      name: "firefox",
      testMatch: /one-to-one-call\.spec\.ts$/,
      use: {
        ...devices["Desktop Firefox"],
        launchOptions: {
          // A fake camera and microphone, granted without a prompt.
          firefoxUserPrefs: {
            "media.navigator.streams.fake": true,
            "media.navigator.permission.disabled": true,
          },
        },
      },
    },
  ],

  webServer: {
    command: "npx vite --config playwright/harness/vite.config.ts",
    url: "http://127.0.0.1:5174/playwright/harness/index.html?case=composer",
    reuseExistingServer: process.env["CI"] === undefined,
    timeout: 120_000,
    stdout: "pipe",
    stderr: "pipe",
  },
});
