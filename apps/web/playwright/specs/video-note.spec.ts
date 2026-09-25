// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A video note, recorded through the production pipeline with a real camera,
 * a real canvas and a real H.264 encoder.
 *
 * ## The defect
 *
 * A video note is not recorded from the camera: the frame is drawn onto a
 * canvas (centre-cropped to a square), and the RECORDER records the canvas
 * (`media-capture.ts` says why — the square, and the mid-take camera flip).
 * The pipeline attached the camera to a `<video>`, asked it to play without
 * waiting, armed a draw loop that returns early while the source has no frame
 * yet, and then handed `canvas.captureStream()` to the encoder in the same
 * synchronous breath. So at the instant the recorder was started the canvas had
 * never been drawn on — the camera had not produced its first frame, and could
 * not have, since nothing had yielded to the event loop.
 *
 * ## Where the measurement is taken, and why there
 *
 * `harness.canvasAtRecorderStart()` reads the canvas the recorder was handed at
 * the instant `MediaRecorder.start()` was called. That is where the property
 * lives: one frame later the draw loop has filled the canvas in, and the
 * question is no longer answerable from anything.
 *
 * The file the take produces is measured too, but NOT for its opening pixels,
 * and that is a finding rather than a choice — see
 * `the take's picture covers the window the bar claimed`, which states what
 * Chromium actually does with a canvas nobody has drawn on.
 *
 * Measured here with the defect in place: the canvas handed to the recorder read
 * `max: 0` — an untouched canvas, every pixel of it — and with a camera made to
 * take 400ms the file came out holding 1279ms while its picture stopped at
 * 875ms. After the fix both hold.
 *
 * ## Chromium only
 *
 * The camera is Chromium's `--use-fake-device-for-media-stream`, which draws a
 * moving colour pattern — loud on every pixel, so a canvas that was drawn on
 * and a canvas that was not are not close. The camera comes from the same flag
 * the microphone does and Playwright's WebKit has neither
 * (`playwright/README.md`), so these three skip there by name rather than
 * failing about the harness.
 */

import { expect, test, type Page } from "@playwright/test";

import { box, openCase } from "./harness";

/** `recording-machine.ts`, quoted so a drift is visible here. */
const MIN_TAKE_MS = 500;

const BTN = "button.lakko-record-btn";
const BAR = ".lakko-rec-bar";
const DOT = ".lakko-rec-dot";

/**
 * A camera that takes as long to produce its first frame as a real one does
 * (`harness/cases.tsx` states why that is a change to the environment and not
 * to the product). Chromium's fake camera crosses that line in a few tens of
 * milliseconds, which is too narrow for two round trips per sample to see
 * into — the same trap `playwright/README.md` records for the microphone.
 */
const CAM_WARMUP_MS = 400;

test.beforeEach(async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "needs Chromium's fake camera device");
  await openCase(page, "composer-video");
});

async function press(page: Page): Promise<void> {
  const b = await box(page.locator(BTN));
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
  await page.mouse.down();
}

function recorderStarts(page: Page): Promise<number> {
  return page.evaluate(() => window.__lakkoHarness.recorderStarts());
}

// ---------------------------------------------------------------------------
// The recorder is never handed a canvas with nothing on it.
// ---------------------------------------------------------------------------

test("the recorder is not started on a canvas the camera has not drawn on", async ({ page }) => {
  await expect(page.locator(BTN)).toHaveAttribute("aria-label", "Record video message");
  await press(page);

  // The take is running: the device was opened and the encoder started.
  await expect.poll(() => recorderStarts(page), { message: "no recorder was started" }).toBe(1);
  const canvas = await page.evaluate(() => window.__lakkoHarness.canvasAtRecorderStart());
  await page.waitForTimeout(MIN_TAKE_MS + 200);
  await page.mouse.up();

  expect(canvas, "the recorder was not recording a canvas at all").not.toBeNull();
  const facts = canvas as { width: number; height: number; mean: number; max: number };
  // The square the pipeline crops to, so this is the production canvas and not
  // some other surface.
  expect(facts.width).toBe(facts.height);
  expect(facts.width).toBeGreaterThan(0);
  // THE DEFECT, as the encoder met it: an untouched canvas is transparent
  // black, so every pixel of it reads zero. The fake camera's pattern has a
  // mean around 85 and a peak above 180, so there is no tolerance to argue
  // about — this is "a frame, or no frame".
  expect(facts.max, "the canvas handed to the recorder was blank").toBeGreaterThan(20);
  expect(facts.mean).toBeGreaterThan(5);
});

// ---------------------------------------------------------------------------
// And the bar does not claim to be recording while it waits for that frame.
// ---------------------------------------------------------------------------

/**
 * Sampled in the window between the DEVICE opening and the CAMERA producing a
 * frame, which is the window this test is about and not one a clock can find.
 *
 * A first attempt waited a fixed 220ms after the press. It passed on the run
 * where six tests shared the fake device and failed on the run where three did:
 * `getUserMedia`'s own latency moves with worker contention (measured here from
 * 23ms to 479ms), so the sample landed on either side of the resolution
 * depending on the worker count. A test whose discrimination depends on that is
 * not a test, so the harness reports the resolution instead
 * (`harness.gumAnswers()`) and the sample is taken FROM it: the device is open,
 * the camera has been given 400ms to warm up, and the question is what the bar
 * says in between.
 */
test("the bar says the camera is opening, and claims nothing more", async ({ page }) => {
  await openCase(page, "composer-video", { camWarmup: CAM_WARMUP_MS });
  await press(page);

  await expect(page.locator(BAR)).toBeVisible();
  await expect
    .poll(() => page.evaluate(() => window.__lakkoHarness.gumAnswers()), {
      message: "the device never opened",
    })
    .toBe(1);

  // Waiting for the first frame has to be part of "Starting…", not part of a
  // clock that is already running.
  await expect(page.locator(BAR)).toContainText("Starting");
  expect(await page.locator(DOT).count(), "the bar claims to be recording").toBe(0);
  expect(await recorderStarts(page), "the encoder was started before the camera drew").toBe(0);
  expect(
    await page.evaluate(() => window.__lakkoHarness.recorder?.elapsedMs() ?? -1),
    "the clock is counting a recording that has not started",
  ).toBe(0);

  await page.mouse.up();
});

// ---------------------------------------------------------------------------
// The file, and what Chromium does with a canvas nobody drew on.
// ---------------------------------------------------------------------------

/**
 * The consequence in the bytes, and it is NOT "the take opens with blank
 * frames" — measured in this session, Chromium never encodes one.
 *
 * A canvas capture track emits NOTHING while the canvas is untouched (measured
 * directly: zero frames over 500ms from an undrawn canvas, and the first frame
 * arriving 415ms in when the canvas was first drawn at 400ms), and the first
 * frame it does emit is stamped ZERO. So the audio covers the whole take, the
 * picture covers the take MINUS the camera's opening window, and the two are
 * silently slid against each other by exactly that much: the sound at any
 * moment belongs to a frame that has not been shown yet.
 *
 * "Which sound goes with which frame" is not decidable here — the fake camera
 * and the fake microphone are uncorrelated — so what is asserted is the half
 * that is: the picture has to reach the end of the take it is the picture of.
 * With the defect and a 400ms camera it falls short by about that much.
 */
test("the take's picture covers the window the bar claimed", async ({ page }) => {
  await openCase(page, "composer-video", { camWarmup: CAM_WARMUP_MS });
  await press(page);
  await expect(page.locator(DOT)).toBeVisible();

  await page.waitForTimeout(1200);
  await page.mouse.up();

  await expect
    .poll(() => page.evaluate(() => window.__lakkoHarness.sent.length), {
      message: "the hold produced no take",
    })
    .toBe(1);
  const take = await page.evaluate(() => {
    const t = window.__lakkoHarness.sent[0];
    return t === undefined ? null : { mime: t.mime, size: t.blob.size };
  });
  expect(take?.mime, "a take must be in the one format every client can play").toBe("video/mp4");
  expect(take?.size ?? 0).toBeGreaterThan(0);

  const facts = await page.evaluate(async () => {
    const t = window.__lakkoHarness.sent[0];
    if (t === undefined) return null;
    return window.__lakkoHarness.decodeVideo(t.blob);
  });
  expect(facts, "no take to decode").not.toBeNull();
  const { durationMs, firstFrameMs, lastFrameMs, firstFrame } = facts as {
    durationMs: number;
    firstFrameMs: number;
    lastFrameMs: number;
    firstFrame: { mean: number; max: number };
  };

  // The opening frame is a camera frame. This holds under the defect too (see
  // above) and is asserted anyway: a pipeline that DID encode the blank canvas
  // would be a second, worse failure, and this is the only place it would show.
  expect(firstFrame.max, "the take opens on a blank frame").toBeGreaterThan(20);
  expect(firstFrameMs, "the picture does not start at the start of the file").toBeLessThan(100);

  // 150ms is two frame intervals at the pipeline's 30fps plus a margin, well
  // inside the 400ms the camera was made to take. Anything beyond it is a take
  // whose sound outlives its picture.
  expect(
    durationMs - lastFrameMs,
    `the file holds ${Math.round(durationMs)}ms but the picture stops at ${Math.round(lastFrameMs)}ms`,
  ).toBeLessThan(150);
});
