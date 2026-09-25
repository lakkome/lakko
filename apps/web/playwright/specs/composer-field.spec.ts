// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The composer's message field: it grows to five lines and then stops
 * (`docs/ux` section 5.2).
 *
 * ## Why a real browser
 *
 * Because the claim is a HEIGHT IN PIXELS, and every one of the three rules
 * that produce it is a cascade interaction jsdom cannot resolve: a `max-height`
 * clamping an inline `height`, `box-sizing: border-box` folding the padding
 * into both, and `overflow-y: auto` turning the clamp into a scroll rather than
 * a clipped draft. Under jsdom every box is zero-area, so the strongest thing
 * available there is "the rule resolves to `130px`" — which a field that grew
 * past it anyway would satisfy, and which says nothing about whether the text
 * above the cap is still reachable.
 *
 * It is worth measuring because the failure is loud and one property wide.
 * Drop the `max-height` and a long draft grows the composer until it pushes the
 * timeline off the screen; change `box-sizing` and the cap lands 20px out; drop
 * `overflow-y` and the draft past line five cannot be read back at all.
 *
 * ## What is driven, and what is not reproduced
 *
 * The route grows the field in `growComposer` by writing `scrollHeight` onto
 * `style.height` — two lines this file deliberately does NOT copy, because a
 * fixture that computed the height would be measuring its own arithmetic. What
 * a spec here does instead is vary the one input the route varies, the inline
 * height, across the range a draft takes it through, and measure what the
 * shipped stylesheet does with it. So what is covered is the CAP and the
 * geometry either side of it; that the route asks for the right height in the
 * first place is named at the end of this file as uncovered.
 *
 * Both browsers: this is layout only, so there is nothing for WebKit to skip.
 */

import { expect, test, type Page } from "@playwright/test";

import { box, openCase } from "./harness";

const FIELD = "textarea.lk-composer-input";

/** Five lines of 22px plus the 20px of vertical padding, per the stylesheet's
 *  own comment on `.lk-composer-input`. */
const CAP_PX = 130;
/** One line, in the same arithmetic. */
const ONE_LINE_PX = 42;

test.beforeEach(async ({ page }) => {
  await openCase(page, "composer-field");
});

/** The height the route would ask for, as it asks for it: the content's own
 *  `scrollHeight` with the inline height released first. Returned rather than
 *  applied, so a spec can compare "what the draft wants" with "what it gets". */
async function wantedHeight(page: Page, lines: number): Promise<number> {
  return page.evaluate((n) => {
    const el = document.querySelector<HTMLTextAreaElement>("textarea.lk-composer-input");
    if (el === null) return -1;
    el.value = Array.from({ length: n }, (_, i) => `line ${i + 1}`).join("\n");
    const had = el.style.height;
    el.style.height = "auto";
    const want = el.scrollHeight;
    el.style.height = had;
    return want;
  }, lines);
}

/** Put the draft in and give the field the height a grown field would have. */
async function grownTo(page: Page, lines: number): Promise<void> {
  await page.evaluate((n) => {
    const el = document.querySelector<HTMLTextAreaElement>("textarea.lk-composer-input");
    if (el === null) return;
    el.value = n === 0 ? "" : Array.from({ length: n }, (_, i) => `line ${i + 1}`).join("\n");
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, lines);
}

test("an empty field is one line tall", async ({ page }) => {
  const b = await box(page.locator(FIELD));
  expect(b.height).toBeCloseTo(ONE_LINE_PX, 0);
});

test("a five-line draft is exactly the cap, and a fifty-line one is no taller", async ({ page }) => {
  await grownTo(page, 5);
  const five = await box(page.locator(FIELD));
  expect(five.height).toBeCloseTo(CAP_PX, 0);

  await grownTo(page, 50);
  const fifty = await box(page.locator(FIELD));
  // The whole of "and stops". Without the cap this is ten times the height and
  // the timeline above it is gone.
  expect(fifty.height).toBeCloseTo(CAP_PX, 0);
});

test("the draft above the cap is scrolled to, not cut off", async ({ page }) => {
  await grownTo(page, 20);

  const { clientHeight, scrollHeight, scrollable } = await page.evaluate(() => {
    const el = document.querySelector<HTMLTextAreaElement>("textarea.lk-composer-input");
    if (el === null) return { clientHeight: -1, scrollHeight: -1, scrollable: false };
    el.scrollTop = 10_000;
    return {
      clientHeight: el.clientHeight,
      scrollHeight: el.scrollHeight,
      // The clamp is only acceptable because the rest of the draft is still
      // reachable. `scrollTop` moving is the proof; an `overflow: hidden` field
      // of the same height would refuse to move and lose the draft silently.
      scrollable: el.scrollTop > 0,
    };
  });

  expect(scrollHeight).toBeGreaterThan(clientHeight);
  expect(scrollable).toBe(true);
});

test("the field the draft asks for is the field it gets, until the cap", async ({ page }) => {
  // Between one line and the cap nothing clamps, so the route's own arithmetic
  // and the rendered box must agree — this is what makes `box-sizing:
  // border-box` load-bearing rather than incidental. A `content-box` field
  // renders 20px taller than the height asked for at every step.
  for (const lines of [1, 2, 3, 4, 5]) {
    const want = await wantedHeight(page, lines);
    await grownTo(page, lines);
    const got = await box(page.locator(FIELD));
    expect(Math.min(want, CAP_PX), `${lines} line(s)`).toBeCloseTo(got.height, 0);
  }
});

test("clearing the draft puts the field back to one line", async ({ page }) => {
  await grownTo(page, 12);
  expect((await box(page.locator(FIELD))).height).toBeCloseTo(CAP_PX, 0);

  await grownTo(page, 0);

  // `min-height` is what makes this true rather than the field collapsing to
  // the height of no text at all.
  expect((await box(page.locator(FIELD))).height).toBeCloseTo(ONE_LINE_PX, 0);
});

/**
 * ## Not covered here
 *
 * **That the route grows the field at all.** `growComposer` is called from an
 * effect on the draft signal inside `ChatView` (`routes/Chat.tsx`), which is
 * unreachable in isolation: it is inline JSX in a ~7,700-line component reading
 * six providers whose `Context` objects are not exported. Delete the effect and
 * every test in this file stays green while the field never leaves one line.
 * Covering it needs the composer row extracted into a component of its own.
 *
 * **The rest of the row.** The attach control, the emoji button and the
 * trailing microphone-or-send switch are in the same place and unreachable for
 * the same reason. `src/media/ComposerPicker.test.tsx` covers the picker the
 * emoji button opens; what opens it is not covered anywhere.
 *
 * **The field coming back after a take.** That the recording bar replaces the
 * field is covered from both sides — `src/media/RecordButton.test.tsx` proves a
 * tap does NOT swap it and a hold does, and `recording.spec.ts` proves what the
 * bar then says. That the FIELD returns after a take is cancelled or sent is
 * covered by neither, and it is a swap of two production classes of possibly
 * different heights, so the row can jump. It needs a hold and a slide-cancel
 * against a real device — the shape `recording.spec.ts` already has, and a test
 * there rather than a fixture here.
 */
