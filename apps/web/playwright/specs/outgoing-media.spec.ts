// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * An outgoing voice message across the moment its send lands.
 *
 * ## The defect
 *
 * A person plays back their own voice message and the sound stops mid-word,
 * because the send finished. The take's bytes are filed under a local key
 * until the upload comes back with a blob id; when it does, the timeline's row
 * is REPLACED — and a replaced row object is, to the list that draws it, a
 * different row. The bubble is torn down and rebuilt around bytes that were
 * never in doubt, and the element that was making sound goes with it.
 *
 * The reachable route is the failed one. A row whose ack is `pending` has an
 * upload fraction, and the voice bubble's play button is disabled while it does
 * (`mediaUploadFor` in `routes/Chat.tsx`, `uploading()` in `MediaBubbles.tsx`),
 * so a person cannot start a take that is still uploading. What they CAN play
 * is a take that did not send — it sits in the timeline as `Not sent · Retry` —
 * and the retry then replaces the row twice: once to mark it pending, once to
 * make it sent.
 *
 * ## What is measured
 *
 * The element, through the harness's constructor record: how many were built,
 * and whether the one that was playing is still playing. `currentTime` is the
 * only direct evidence that samples are still being decoded, and the COUNT is
 * what distinguishes "playback continued" from "a second element started over".
 *
 * Measured here with the defect in place: one message produced THREE `Audio`
 * elements — one per row replacement — the first left frozen at 0.564s with its
 * source removed, and the sound never came back (the clock still read 0 eight
 * hundred milliseconds later).
 *
 * Both engines: the fixture's bytes are AAC in MP4 and nothing here records.
 */

import { expect, test, type Page } from "@playwright/test";

import { openCase } from "./harness";

const PLAY = "button.lakko-voice-play";
const RATE = "button.lakko-voice-rate";

interface Elements {
  /** Every `Audio` the page has constructed. */
  readonly count: number;
  /** The one that has a source: its position, and whether it is running. */
  readonly time: number;
  readonly paused: boolean;
}

async function elements(page: Page): Promise<Elements> {
  return page.evaluate(() => {
    const all = window.__lakkoHarness.audios;
    const live = all.find((el) => el.src !== "");
    return {
      count: all.length,
      time: live === undefined ? -1 : live.currentTime,
      paused: live === undefined ? true : live.paused,
    };
  });
}

test.beforeEach(async ({ page }) => {
  await openCase(page, "outgoing-voice-send");
});

/** Press play and wait until the engine is really decoding. */
async function startPlayback(page: Page): Promise<void> {
  await expect(page.locator(PLAY)).toBeEnabled();
  await page.locator(PLAY).click();
  await expect(page.locator(PLAY)).toHaveAttribute("aria-label", "Pause voice message");
  await expect
    .poll(async () => (await elements(page)).time, { message: "currentTime never advanced" })
    .toBeGreaterThan(0.2);
}

test("playback survives the send landing", async ({ page }) => {
  await startPlayback(page);
  const before = await elements(page);

  await page.evaluate(() => {
    // The two row replacements the retry performs, in the order `runMediaJob`
    // performs them.
    window.__lakkoHarness.retryUpload?.();
    window.__lakkoHarness.completeUpload?.();
  });

  const after = await elements(page);
  // The element, not merely "some element": a rebuilt bubble constructs its own
  // and starts from nothing, which is exactly what a person heard.
  expect(after.count, "the bubble was rebuilt: a second element was constructed").toBe(before.count);
  expect(after.paused, "the element was paused by the send landing").toBe(false);

  // And still decoding afterwards, which is the person's complaint in one line.
  await expect
    .poll(async () => (await elements(page)).time, { message: "the clock stopped when the send landed" })
    .toBeGreaterThan(before.time + 0.2);
  expect((await elements(page)).count).toBe(before.count);

  // This message is still THE one playing, as the bubble says it is. The hub
  // keys that on the media key, and the send landing changes the key — so
  // without moving the claim across with the bytes the sound would go on while
  // the bubble showed the total duration in place of the elapsed time and hid
  // the speed control that was just used.
  await expect(page.locator(RATE)).not.toHaveAttribute("data-reserved", "true");
  await expect(page.locator(".lakko-voice-time")).not.toHaveText("0:07");
});

test("the sound does not stop when Retry disables the button", async ({ page }) => {
  // The first of the two replacements on its own. It is worth its own test
  // because it happens the moment the person presses Retry, before any upload
  // has finished — so a fix that only addressed the completion would leave the
  // defect where a person meets it first.
  await startPlayback(page);
  const before = await elements(page);

  await page.evaluate(() => window.__lakkoHarness.retryUpload?.());

  // The button goes away for the duration of the upload (it has a progress
  // ring on it) — the sound does not.
  await expect(page.locator(PLAY)).toBeDisabled();
  expect((await elements(page)).count).toBe(before.count);
  await expect
    .poll(async () => (await elements(page)).time, { message: "the clock stopped when Retry was pressed" })
    .toBeGreaterThan(before.time + 0.2);
});

test("the row still becomes a sent message", async ({ page }) => {
  // The control, and it is not a formality: keeping a bubble alive across a row
  // replacement is only correct if the bubble then shows the NEW row. A bubble
  // that survived by ignoring the replacement would pass every assertion above
  // and leave the message reading `Not sent` forever.
  await expect(page.locator(".lk-bubble-meta")).toContainText("Not sent");

  await page.evaluate(() => {
    window.__lakkoHarness.retryUpload?.();
    window.__lakkoHarness.completeUpload?.();
  });

  await expect(page.locator(".lk-bubble-meta")).not.toContainText("Not sent");
  // A sent row has no upload state, so the button is usable again.
  await expect(page.locator(PLAY)).toBeEnabled();
  // One bubble, one voice row: the replacement must not leave two behind.
  expect(await page.locator(".lakko-voice").count()).toBe(1);
});
