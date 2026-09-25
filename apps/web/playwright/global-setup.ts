// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Generate the two audio assets before anything opens a browser.
 *
 * Here rather than in a shell step so `npm run test:e2e` works from a fresh
 * checkout: the assets are binary and generated, so they are gitignored, and a
 * spec that opened a fixture page with no bytes behind it would fail as "the
 * bubble never reached playing" instead of "the asset is missing".
 */

// @ts-expect-error -- a plain .mjs helper with no types; Playwright runs it.
import { makeAssets } from "./fixtures/make-assets.mjs";

export default function globalSetup(): void {
  const out = makeAssets() as { voice: string; kind: string };
  if (out.kind !== "m4a") {
    // Said here AND enforced in the fixture, which throws by name when the
    // asset is absent. Not softened to a skip: a measurement that quietly
    // stopped running is the failure mode this whole project exists to stop, so
    // the voice specs fail loudly and say what is missing.
    process.stdout.write(
      "[harness] ffmpeg not found, so playwright/fixtures/voice.m4a was not built.\n" +
        "[harness] The voice-bubble and message-menu specs will fail naming the missing fixture.\n",
    );
  }
}
