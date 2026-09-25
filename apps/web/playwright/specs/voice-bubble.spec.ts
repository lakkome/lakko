// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The voice bubble, in a browser that has layout and a media engine.
 *
 * Three of the defects a person met on it, each measured rather than inferred.
 * `src/media/VoiceBubble.test.tsx` covers the same ground under jsdom and says
 * where it has to reason by proxy; this file is the other half of those two
 * sentences:
 *
 *   * the bubble growing by nearly 30px the moment playback started is
 *     asserted there as "the cascade resolves `min-height` to
 *     `var(--control-h)`", which a height set any other way would satisfy.
 *     Here it is two `boundingBox()` reads either side of a real press.
 *   * the press that did nothing is asserted there as "`play()` was called
 *     before the handler returned". Here a real engine decodes real AAC and
 *     `currentTime` has to advance. What is NOT covered here is stated in
 *     full at `a press reaches real playback` — read it before trusting this
 *     file with the activation defect, because it does not cover it.
 */

import { expect, test, type Page } from "@playwright/test";

import { box, centreY, openCase } from "./harness";

const PLAY = "button.lakko-voice-play";
const RATE = "button.lakko-voice-rate";
const BUBBLE = ".lk-bubble";
const LINE = ".lakko-voice-line";

test.beforeEach(async ({ page }) => {
  await openCase(page, "voice-only");
});

/** The bubble's element, which the component creates with `new Audio()` and
 *  never appends, so it is reachable only through the harness's constructor
 *  record (`harness/cases.tsx` explains why that is observation, not a stub). */
async function currentTime(page: Page): Promise<number> {
  return page.evaluate(() => {
    const a = window.__lakkoHarness.audios.find((el) => el.src !== "");
    return a === undefined ? -1 : a.currentTime;
  });
}

/** Whether the element that has a source is paused - the product's own claim,
 *  which is a state rather than a rate. */
async function paused(page: Page): Promise<boolean> {
  return page.evaluate(() => {
    const a = window.__lakkoHarness.audios.find((el) => el.src !== "");
    return a === undefined ? false : a.paused;
  });
}

/**
 * The position once the engine has stopped moving it, rather than at the
 * instant of the click.
 *
 * A media element that is told to pause can still finish a buffer it had
 * already decoded, so `currentTime` keeps creeping for a moment afterwards.
 * Reading it immediately and demanding it hold to within 50ms measured that
 * buffer, not whether playback stopped, and failed about one run in six under
 * parallel load - the profile that gets a test deleted rather than fixed.
 */
async function settledTime(page: Page): Promise<number> {
  let last = await currentTime(page);
  for (let i = 0; i < 20; i += 1) {
    await page.waitForTimeout(50);
    const now = await currentTime(page);
    if (now === last) return now;
    last = now;
  }
  return last;
}

// ---------------------------------------------------------------------------
// Defect — the bubble grew by nearly 30px the moment playback started.
// ---------------------------------------------------------------------------

test("the bubble does not change height when playback starts", async ({ page }) => {
  const bubble = page.locator(BUBBLE);
  const before = await box(bubble);

  await page.locator(PLAY).click();
  // Not "after the click": after PLAYBACK. The engine flips this label on its
  // own `play` event, so waiting for it is waiting for the state in which the
  // speed control has entered the quiet line — the exact moment the bubble
  // used to jump, with a person's eye on it.
  await expect(page.locator(PLAY)).toHaveAttribute("aria-label", "Pause voice message");

  const after = await box(bubble);
  // Tight on purpose. There is nothing in this transition that may legitimately
  // change the bubble's height, so a tolerance wide enough to absorb the defect
  // would be a tolerance wide enough to absorb anything. The half pixel is for
  // subpixel rounding in the two reads, not for a layout change.
  expect(Math.abs(after.height - before.height)).toBeLessThan(0.5);
});

test("nothing on the quiet line grows the line when it appears", async ({ page }) => {
  const line = page.locator(LINE);
  const before = await box(line);

  await page.locator(PLAY).click();
  await expect(page.locator(PLAY)).toHaveAttribute("aria-label", "Pause voice message");

  // The bubble's height is what a person sees; the line is where the growth
  // came from. Measuring both means a regression is reported at its cause
  // rather than as "something in the bubble got taller".
  expect(Math.abs((await box(line)).height - before.height)).toBeLessThan(0.5);
});

// ---------------------------------------------------------------------------
// Defect — the global control floor reached into the quiet line.
// ---------------------------------------------------------------------------

test("the playback-speed chip is not floored at the global control height", async ({ page }) => {
  // Visible first: a `visibility: hidden` box still has a real height, but a
  // measurement of the state a person never sees is not the measurement.
  await page.locator(PLAY).click();
  await expect(page.locator(RATE)).not.toHaveAttribute("data-reserved", "true");

  const chip = await box(page.locator(RATE));
  // `--control-h` is 40px. The chip's padding is 3px a side around a 0.7rem
  // line, so the honest ceiling is well under half of that; 20px separates
  // "the chip sized itself" from "the global button rule sized it" without
  // pinning a font metric that may differ by one pixel between engines.
  expect(chip.height).toBeLessThan(20);

  // The control, and it is not optional: a stylesheet that failed to load
  // would leave every button at its intrinsic height and read as a pass. A
  // plain button appended to this document has to come out AT the floor, which
  // proves both that the sheet is live and that the global rule is the one the
  // chip had to opt out of.
  const floored = await page.evaluate(() => {
    const b = document.createElement("button");
    b.textContent = "control";
    document.body.appendChild(b);
    const h = b.getBoundingClientRect().height;
    const controlH = getComputedStyle(document.documentElement)
      .getPropertyValue("--control-h")
      .trim();
    b.remove();
    return { h, controlH };
  });
  expect(floored.controlH, "the production stylesheet is not loaded").toBe("40px");
  expect(floored.h, "the global button floor is not in force in this document").toBe(40);
});

test("the quiet line is no taller than the clock that is always on it", async ({ page }) => {
  await page.locator(PLAY).click();
  await expect(page.locator(RATE)).not.toHaveAttribute("data-reserved", "true");

  const line = await box(page.locator(LINE));
  const clock = await box(page.locator(`${LINE} .lakko-voice-time`));

  // The rule the stylesheet states in words, measured — and stated about the
  // LINE rather than about each child, which is where a first attempt at this
  // test got it wrong: the speed chip's own box IS taller than the line (its
  // padding is a finger-sized hit area) and its matching negative margins hand
  // the growth back, so a per-child box check fails on a control that is
  // behaving. What must hold is that no child grows the line it sits on, and
  // that is one comparison.
  expect(line.height, "something on the quiet line is growing it")
    .toBeLessThanOrEqual(clock.height + 1);
});

// ---------------------------------------------------------------------------
// Defect — pressing play did nothing, in silence.
// ---------------------------------------------------------------------------

/**
 * WHAT THIS DOES NOT COVER, stated plainly because the gap is easy to miss.
 *
 * The original defect was an engine REFUSAL: `play()` was awaited behind the
 * fetch, landed after the handler's task, and was rejected with
 * `NotAllowedError`. Neither headless engine available here reproduces that.
 * Measured in this session, with a real `locator.click()` and the deferred
 * `play()` as the first click of a fresh document:
 *
 *   * Chromium (153, `--autoplay-policy=document-user-activation-required`)
 *     allows it. Chromium's activation is STICKY per document: one click
 *     licences every later start, so there is no window to lose.
 *   * Playwright's WebKit (26.6) allows it too — the build does not enforce
 *     the transient-activation media policy real Safari does.
 *
 * So this test does NOT discriminate against the activation defect, and
 * restoring that defect leaves it green. What it does cover, and jsdom cannot,
 * is everything between the press and the sound: a real engine, real AAC bytes
 * decoded, the source actually attached, and the clock moving. A press that
 * reaches no element, attaches no source, hands over bytes the engine refuses,
 * or lands on the error path fails here and passes there.
 *
 * The ordering property that the activation rule turns into a requirement is
 * covered where it is decidable: `voice-press.ts` pins the branch, and
 * `VoiceBubble.test.tsx` pins that `play()` is reached on the handler's own
 * stack with nothing awaited in front of it.
 */
test("a press reaches real playback, and the clock advances", async ({ page }) => {
  await page.locator(PLAY).click();

  // The engine's own `play` event drives this label; a stub cannot fake it and
  // a refused start never fires it.
  await expect(page.locator(PLAY)).toHaveAttribute("aria-label", "Pause voice message");

  // Decoding, not merely started: `currentTime` only moves while samples are
  // being consumed.
  await expect
    .poll(() => currentTime(page), { message: "currentTime never advanced" })
    .toBeGreaterThan(0.15);

  // And the production DOM says so too, through the waveform's played bars —
  // the signal a person actually watches.
  await expect
    .poll(() => page.locator('.lakko-wave > i[data-on="true"]').count())
    .toBeGreaterThan(0);
});

test("the bytes are already on an element before anything is pressed", async ({ page }) => {
  // The other half of the same defect: the first play used to spend a storage
  // round trip and an AEAD open inside the press, heard as about a second of
  // nothing. The scroll-into-view warm-up is what removed it, and what it
  // leaves behind is an element that already has its source — which is also
  // the state `voice-press.ts` calls `ready`, and therefore what decides that
  // a press may call `play()` on its own stack at all.
  await expect
    .poll(() => currentTime(page), { message: "no element has a source: the warm-up did not run" })
    .toBe(0);
});

test("a second press pauses", async ({ page }) => {
  await page.locator(PLAY).click();
  await expect(page.locator(PLAY)).toHaveAttribute("aria-label", "Pause voice message");
  await expect.poll(() => currentTime(page)).toBeGreaterThan(0.15);

  await page.locator(PLAY).click();

  await expect(page.locator(PLAY)).toHaveAttribute("aria-label", /^Play voice message/);
  // Paused, not merely relabelled. The element's own state is the claim; the
  // position is then checked from where the engine actually left it, so this
  // asserts that playback STOPPED rather than how fast it stopped.
  await expect.poll(() => paused(page)).toBe(true);
  const stopped = await settledTime(page);
  await page.waitForTimeout(250);
  expect(Math.abs((await currentTime(page)) - stopped)).toBeLessThan(0.02);
});

// ---------------------------------------------------------------------------
// Defect — the message's time sat below the recording's duration.
// ---------------------------------------------------------------------------

test("the message time sits on the same visual row as the duration", async ({ page }) => {
  const duration = await box(page.locator(".lakko-voice-time"));
  const sentAt = await box(page.locator(".lk-bubble-meta"));

  // Centres, not parentage. The jsdom test asserts the tree — that the meta is
  // a child of the quiet line — which is the CAUSE; this is the thing the
  // person reported ("the time is noticeably below the duration") and it holds
  // however the two boxes are nested. One pixel of slack for two different
  // font metrics on one line.
  expect(Math.abs(centreY(sentAt) - centreY(duration)))
    .toBeLessThan(1);

  // Trailing end, and after the duration: level but out of order would satisfy
  // the centres alone.
  expect(sentAt.x).toBeGreaterThan(duration.x);
});

test("a captioned voice message keeps its time on the row below", async ({ page }) => {
  // The control for the measurement above: the rule is "a voice message ALONE
  // in its bubble", and a captioned one has a paragraph to sit under. Without
  // this, a component that put the meta on the voice line unconditionally
  // would pass the test above and still be wrong.
  await openCase(page, "voice-captioned");
  const duration = await box(page.locator(".lakko-voice-time"));
  const sentAt = await box(page.locator(".lk-bubble-meta"));
  expect(centreY(sentAt)).toBeGreaterThan(centreY(duration) + 4);
  expect(await page.locator(`${LINE} .lk-bubble-meta`).count()).toBe(0);
});
