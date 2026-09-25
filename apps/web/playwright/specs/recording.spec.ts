// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The composer's one record button, against a real microphone and a real
 * `MediaRecorder`.
 *
 * ## The defects
 *
 * A tap on the button is supposed to switch between voice and video and record
 * NOTHING; a press and hold is supposed to record. And web showed a running
 * recorder that was not recording: the bar ran, the clock counted and the red
 * dot pulsed while the browser was still opening the audio device, so whatever
 * was said in that window was lost — the recording began in the middle of the
 * phrase.
 *
 * ## What this adds over `RecordButton.test.tsx`
 *
 * That file proves the ordering against a stub: the red dot is never drawn
 * before `MediaRecorder.start()` has been called. It cannot prove the
 * consequence, because it has no recorder, no device and no clock a person
 * could disagree with — its `MediaRecorder` is a class that pushes strings into
 * an array. Here the thresholds are met by a real pointer against a real wall
 * clock, and the take that comes out is DECODED: the bytes either carry the
 * window the UI claimed or they do not.
 *
 * ## Chromium only
 *
 * The microphone is Chromium's `--use-fake-device-for-media-stream` fed by
 * `--use-file-for-fake-audio-capture`. Playwright's WebKit has no equivalent
 * flag, so a hold there opens nothing and the machine lands in its denied
 * card — a failure about the harness, not about the product. Skipped by name
 * rather than left to fail.
 */

import { expect, test, type Page } from "@playwright/test";

import { box, openCase } from "./harness";

/** `recording-machine.ts`, quoted so a drift is visible here. */
const HOLD_TO_RECORD_MS = 200;
const TAP_SLOP_PX = 10;
const MIN_TAKE_MS = 500;

const BTN = "button.lakko-record-btn";
const BAR = ".lakko-rec-bar";
const DOT = ".lakko-rec-dot";

test.beforeEach(async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "needs Chromium's fake-audio-capture device");
  await openCase(page, "composer");
});

/** Press the record button's centre with a real mouse and leave it down. */
async function press(page: Page): Promise<{ x: number; y: number }> {
  const b = await box(page.locator(BTN));
  const at = { x: b.x + b.width / 2, y: b.y + b.height / 2 };
  await page.mouse.move(at.x, at.y);
  await page.mouse.down();
  return at;
}

/**
 * How long the page saw the pointer held, so a test about a TAP can say whether
 * it actually performed one.
 *
 * Driving a real mouse costs a round trip per step, and the tap threshold is
 * 200ms — so a `waitForTimeout(100)` between `down` and `up`, which is how these
 * tests were written first, overran the threshold often enough to fail 3 runs in
 * 24 and turn a tap into a hold. Rather than widen a tolerance until the flake
 * hid, the gesture is now as fast as the transport allows and the page reports
 * what it measured: a tap that overran is reported as a HARNESS failure, in
 * those words, instead of as a product defect.
 */
async function watchPressDuration(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as unknown as { __pressAt?: number; __pressMs?: number };
    addEventListener("pointerdown", () => { w.__pressAt = performance.now(); }, true);
    addEventListener("pointerup", () => {
      w.__pressMs = performance.now() - (w.__pressAt ?? performance.now());
    }, true);
  });
}

async function pressDuration(page: Page): Promise<number> {
  return page.evaluate(
    () => (window as unknown as { __pressMs?: number }).__pressMs ?? Number.NaN,
  );
}

/** Assert the gesture really was under the tap threshold, and say whose fault
 *  it is when it was not. */
async function expectWasATap(page: Page): Promise<void> {
  const held = await pressDuration(page);
  expect(
    held,
    `HARNESS: the pointer was held ${Math.round(held)}ms, past the ${HOLD_TO_RECORD_MS}ms tap threshold — this run tested a hold, not a tap`,
  ).toBeLessThan(HOLD_TO_RECORD_MS);
}

function gumCalls(page: Page): Promise<number> {
  return page.evaluate(() => window.__lakkoHarness.gumCalls());
}

function recorderStarts(page: Page): Promise<number> {
  return page.evaluate(() => window.__lakkoHarness.recorderStarts());
}

// ---------------------------------------------------------------------------
// A tap switches the mode, and records nothing.
// ---------------------------------------------------------------------------

test("a tap switches the mode and opens no device", async ({ page }) => {
  await expect(page.locator(BTN)).toHaveAttribute("aria-label", "Record voice message");
  await watchPressDuration(page);

  await press(page);
  // Straight back up, with no movement at all, so the press is a tap on both of
  // the machine's counts and the gesture is as short as a real mouse can be
  // driven.
  await page.mouse.up();

  // Soft, so that a run in which the transport overran the threshold still
  // reports what the product did as well as the guard below. Blaming the
  // product for the harness and hiding a defect behind the harness are both
  // failures of a test suite; reporting both is neither.
  await expect.soft(page.locator(BTN)).toHaveAttribute("aria-label", "Record video message");
  // The whole point of the threshold: not "the take was discarded" but "the
  // microphone was never opened". Counted through the platform call itself.
  expect.soft(await gumCalls(page), "a tap opened a capture device").toBe(0);
  expect.soft(await page.locator(BAR).count(), "a tap swapped the field for a bar").toBe(0);
  expect.soft(await page.evaluate(() => window.__lakkoHarness.sent.length)).toBe(0);

  await expectWasATap(page);
});

test("a press that drifts past the slop is neither a tap nor a take", async ({ page }) => {
  await watchPressDuration(page);
  const at = await press(page);
  await page.mouse.move(at.x - (TAP_SLOP_PX + 8), at.y);
  await page.mouse.up();

  // Still voice: a drifting finger is not a request to switch the camera on.
  await expect.soft(page.locator(BTN)).toHaveAttribute("aria-label", "Record voice message");
  expect.soft(await gumCalls(page), "a drifting press opened a capture device").toBe(0);

  await expectWasATap(page);
});

// ---------------------------------------------------------------------------
// A hold records, and says so only once it is true.
// ---------------------------------------------------------------------------

test("a hold opens the device and then records", async ({ page }) => {
  await press(page);

  // The bar takes over once the press has become a hold, and it says the device
  // is opening rather than that anything is being recorded.
  await expect(page.locator(BAR)).toBeVisible();
  await expect(page.locator(DOT)).toBeVisible();
  // The device really was asked for, once.
  expect(await gumCalls(page)).toBe(1);
  await expect(page.locator(BAR)).toContainText("Slide to cancel");

  await page.waitForTimeout(MIN_TAKE_MS + 400);
  await page.mouse.up();

  // Releasing a held take SENDS it (UX spec §2.3); the preview belongs to the
  // locked path, which is reached by sliding up instead of lifting.
  await expect
    .poll(() => page.evaluate(() => window.__lakkoHarness.sent.length), {
      message: "the hold produced no take",
    })
    .toBe(1);
  const take = await page.evaluate(() => {
    const t = window.__lakkoHarness.sent[0];
    return t === undefined ? null : { mime: t.mime, size: t.blob.size, durationMs: t.durationMs };
  });
  expect(take?.mime, "a take must be in the one format every client can play").toBe("audio/mp4");
  expect(take?.size ?? 0).toBeGreaterThan(0);
  // The composer is back to a field with no bar over it.
  await expect(page.locator(BAR)).toHaveCount(0);
});

/**
 * The device is made to take `GUM_DELAY_MS`, which is what a real microphone
 * takes and what Chromium's fake one does not (`harness/cases.tsx` says why
 * that is a change to the environment and not to the product). Without it the
 * window this test watches is a few tens of milliseconds wide and two
 * round-trips per sample cannot see into it.
 */
const GUM_DELAY_MS = 400;

test("the bar says the device is opening, and claims nothing more", async ({ page }) => {
  await openCase(page, "composer", { gumDelay: GUM_DELAY_MS });
  await press(page);

  await expect(page.locator(BAR)).toBeVisible();
  // The microphone has been asked for and has not answered.
  await expect(page.locator(BAR)).toContainText("Starting");
  expect(await gumCalls(page)).toBe(1);
  // THE DEFECT, as a person saw it: the red dot pulsing over a recorder that
  // had not been constructed, let alone started.
  expect(await page.locator(DOT).count(), "the bar claims to be recording").toBe(0);
  expect(await recorderStarts(page)).toBe(0);

  await page.mouse.up();
});

test("the red dot is never drawn before the recorder has started", async ({ page }) => {
  await openCase(page, "composer", { gumDelay: GUM_DELAY_MS });
  await press(page);

  // Sampled across the whole opening window, in a real browser this time: at no
  // point may the bar say it is recording while `MediaRecorder.start()` has not
  // been called. `RecordButton.test.tsx` asserts the same thing against a stub
  // recorder; here both the recorder and the device are the platform's.
  const violations: string[] = [];
  for (let i = 0; i < 60; i += 1) {
    const [dot, started] = await Promise.all([page.locator(DOT).count(), recorderStarts(page)]);
    if (dot > 0 && started === 0) violations.push(`sample ${i}: dot with no recorder started`);
    if (started > 0 && dot > 0) break;
    await page.waitForTimeout(10);
  }
  await page.mouse.up();
  expect(violations).toEqual([]);
});

// ---------------------------------------------------------------------------
// The recording does not lose the start of the phrase.
// ---------------------------------------------------------------------------

/**
 * Measured as a DEFICIT, which is the only form of this a browser can settle.
 *
 * The fake microphone is a continuous tone that Chromium loops, so "which part
 * of the phrase was captured" is not a question the bytes can answer — every
 * window of the source sounds the same, and no alignment between the source and
 * the take is available. What the bytes CAN answer is how much was captured:
 * the bar starts its clock when the machine enters a capturing phase, and the
 * defect was that capture began later, so the file was short by however long
 * the device took to open. Lost audio is missing duration, and the duration is
 * in the file.
 *
 * So: hold from the moment the bar first claims to be recording, release,
 * decode, and compare. The claim is the wall clock from the red dot — the
 * strongest "this is recording" signal the bar has — to the release.
 *
 * The 400ms device is load-bearing, not a convenience. Measured in this
 * session with the defect restored and the fake device left at its own speed,
 * the deficit was 11-54ms: inside the encoder's frame quantisation, so the test
 * passed on a recorder that was demonstrably losing the start. The delay is
 * what gives the property something to be true or false about.
 */
test("the take carries the whole window the bar claimed to be recording", async ({ page }) => {
  await openCase(page, "composer", { gumDelay: GUM_DELAY_MS });
  await press(page);
  await expect(page.locator(DOT)).toBeVisible();
  const claimStart = Date.now();

  await page.waitForTimeout(1500);
  const claimEnd = Date.now();
  await page.mouse.up();

  await expect
    .poll(() => page.evaluate(() => window.__lakkoHarness.sent.length))
    .toBe(1);
  const decoded = await page.evaluate(async () => {
    const take = window.__lakkoHarness.sent[0];
    if (take === undefined) return null;
    return window.__lakkoHarness.decode(take.blob);
  });
  expect(decoded, "no take to decode").not.toBeNull();
  const { durationMs, peakInFirstMs } = decoded as { durationMs: number; peakInFirstMs: number };
  const claimed = claimEnd - claimStart;

  // Not silence. The source is loud on every sample, so a near-zero peak in the
  // first 150ms means the recorder was not capturing at the start of its own
  // file — a different failure from a short file, and worth its own assertion.
  expect(peakInFirstMs, "the take opens with silence").toBeGreaterThan(0.05);

  // And not short. The tolerance sits between two measured populations rather
  // than being chosen for comfort: with the recorder starting on the grant the
  // file comes out LONGER than the claim (the stop is asynchronous), by 245ms
  // to 415ms across the runs measured here; with the defect restored and a 400ms
  // device it came out 330ms to 463ms SHORT. 150ms is inside that gap on both
  // sides. Anything beyond it is audio spoken into a bar that claimed to be
  // recording and never arrived.
  expect(
    claimed - durationMs,
    `claimed ${claimed}ms, the file holds ${durationMs}ms`,
  ).toBeLessThan(150);
});
