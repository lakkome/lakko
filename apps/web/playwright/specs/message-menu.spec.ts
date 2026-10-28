// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which gesture opens a message's action menu, with a real pointer.
 *
 * ## The defect
 *
 * The action strip was revealed on `:hover` and positioned inside the bubble.
 * On a voice message that is exactly where the play button is, so aiming at
 * play landed on the strip: the reaction picker opened and the message paused.
 * A voice message could not be listened to at all.
 *
 * ## What this adds over `TimelineBubble.test.tsx`
 *
 * That file dispatches synthetic `mouseover` / `mousemove` / `focusin` events
 * at the bubble and asserts nothing opens, which catches a handler on the
 * wrong event. It cannot catch the other half of the same defect: a strip that
 * is revealed by a CSS `:hover` rule rather than by a handler, and a strip
 * whose BOX overlaps the play button. Neither is a handler and neither is
 * visible to jsdom, which computes no `:hover` state and no boxes.
 *
 * Here the pointer is really over the element, the cascade really evaluates
 * `:hover`, and the last test measures the overlap directly: a real click at
 * the play button's own centre has to reach play.
 *
 * ## The second defect, and why one test of it is here
 *
 * `message-actions.md` section 2: "The action card is sized by its own content.
 * It is a separate surface from the reaction row and must not inherit its
 * width." It did inherit it — both rows were children of one bordered box whose
 * `align-items` was the default `stretch` — so a card three words long was
 * drawn at the width of seven emoji and a plus.
 *
 * `TimelineBubble.test.tsx` covers the tree and the cascade that caused that,
 * which is most of it. What it cannot cover is the defect as a person saw it,
 * because the defect IS a width and jsdom computes none: every box there is
 * zero. So exactly one test of it lives here, and it does the one thing that
 * needs a browser — compares the two measured widths.
 *
 * The same test also measures the gap section 2 added on 2026-09-25 ("Two
 * surfaces that read as two things"). The jsdom test pins where the component
 * puts each surface given sizes it was handed; only here are the sizes the
 * real ones, so only here is "a visible gap between two real boxes" a
 * measurement rather than arithmetic on a stub.
 */

import { expect, test } from "@playwright/test";

import { box, openCase } from "./harness";
import { SURFACE_GAP_PX } from "../../src/chat/message-menu-placement";

const MENU = "[role='menu']";
const PLAY = "button.lakko-voice-play";
/** A captioned voice message: it has both a surface of the bubble's own to
 *  aim at and the play button the defect was about. */
const CASE = "voice-captioned";

test.beforeEach(async ({ page }) => {
  await openCase(page, CASE);
});

test("a pointer resting on the bubble opens nothing", async ({ page }) => {
  const bubble = page.locator(".lk-bubble");
  await bubble.hover();
  // A real hover: the cascade has evaluated `:hover` for this element and
  // everything inside it. A `:hover`-revealed strip is visible by now.
  await page.waitForTimeout(150);
  expect(await page.locator(MENU).count()).toBe(0);
});

test("a pointer crossing the bubble opens nothing", async ({ page }) => {
  const b = await box(page.locator(".lk-bubble"));
  // Across the whole bubble, in steps, so every element under the path gets a
  // genuine `mouseover` / `mousemove` / `pointerover` and its own `:hover`.
  await page.mouse.move(b.x - 20, b.y + b.height / 2);
  for (let i = 0; i <= 10; i += 1) {
    await page.mouse.move(b.x + (b.width * i) / 10, b.y + b.height / 2);
  }
  expect(await page.locator(MENU).count()).toBe(0);
});

test("a left click on the bubble opens nothing", async ({ page }) => {
  // Deliberately on the caption, not the player: a click on the bubble's own
  // surface activates nothing, and it must not be a way into the menu either —
  // that is what a right click is for.
  await page.locator(".lk-bubble-text").first().click();
  expect(await page.locator(MENU).count()).toBe(0);
});

test("tabbing onto the waveform opens nothing", async ({ page }) => {
  await page.locator("[role='slider']").focus();
  await expect(page.locator("[role='slider']")).toBeFocused();
  // `:focus-within` went with `:hover` for the same reason: revealing the strip
  // on a keyboard arrival put it over the play button without a mouse.
  expect(await page.locator(MENU).count()).toBe(0);
});

test("a right click opens it at the pointer, over the message", async ({ page }) => {
  // docs/ux/message-actions.md section 7 - like Telegram: the card's near
  // corner is where the pointer was, on top of the message, not pushed off to
  // the bubble's side. The outside-the-bubble rule this test used to pin was
  // the cure for a HOVER strip that stole the play button; a menu a person has
  // just right-clicked for intercepts nothing they are about to press.
  const play = page.locator(PLAY);
  const box = (await play.boundingBox())!;
  const px = box.x + box.width / 2;
  const py = box.y + box.height / 2;
  await page.mouse.click(px, py, { button: "right" });
  const menu = page.locator(MENU);
  await expect(menu).toBeVisible();
  expect(await menu.locator("[role='menuitem']").count()).toBeGreaterThan(0);

  const card = await page.locator(".lk-msg-menu-card.lk-msg-menu-items").boundingBox();
  const row = await page.locator(".lk-msg-menu-reactions").boundingBox();
  expect(card, "no action card was drawn").not.toBeNull();
  expect(row, "no reaction row was drawn").not.toBeNull();
  // The card's top edge is at the pointer, and one of its vertical edges is
  // too - the left one ordinarily, the right one when it opened leftward.
  expect(Math.abs(card!.y - py)).toBeLessThanOrEqual(1);
  const atLeft = Math.abs(card!.x - px) <= 1;
  const atRight = Math.abs(card!.x + card!.width - px) <= 1;
  expect(atLeft || atRight, `card x ${card!.x}..${card!.x + card!.width} vs pointer ${px}`).toBe(true);
  // It may sit over the bubble; that is the design, not a defect. What must
  // still hold is that it is one thing the person opened on purpose and the
  // play button is not reachable THROUGH it.
  expect(await page.locator(`.lk-bubble ${MENU}`).count()).toBe(0);
});

test("a left click on the play button plays, and opens nothing", async ({ page }) => {
  // Playwright clicks the element's own centre, so this is the aim the person
  // took. If anything is over it — a hover strip, a stale menu, an overlay —
  // the click lands there instead and this fails on both counts at once.
  await page.locator(PLAY).click();

  await expect(page.locator(PLAY)).toHaveAttribute("aria-label", "Pause voice message");
  expect(await page.locator(MENU).count()).toBe(0);
});

test("a long press on the play button is still play", async ({ page }) => {
  const b = await box(page.locator(PLAY));
  // The touch half of the same defect, with real pointer events: the menu must
  // not take a gesture aimed at a control. `LONG_PRESS_MS` is 500.
  await page.locator(PLAY).dispatchEvent("pointerdown", {
    pointerType: "touch",
    pointerId: 1,
    clientX: b.x + b.width / 2,
    clientY: b.y + b.height / 2,
    bubbles: true,
  });
  await page.waitForTimeout(700);
  expect(await page.locator(MENU).count()).toBe(0);
  await page.locator(PLAY).dispatchEvent("pointerup", {
    pointerType: "touch",
    pointerId: 1,
    clientX: b.x + b.width / 2,
    clientY: b.y + b.height / 2,
    bubbles: true,
  });
});

test("a long press on the bubble's own surface does open it", async ({ page }) => {
  const caption = page.locator(".lk-bubble-text").first();
  const b = await box(caption);
  // The control guard must not have swallowed the gesture everywhere: a long
  // press on the message itself is the touch path to the menu.
  const at = { clientX: b.x + b.width / 2, clientY: b.y + b.height / 2 };
  await caption.dispatchEvent("pointerdown", { pointerType: "touch", pointerId: 1, bubbles: true, ...at });
  await expect(page.locator(MENU)).toBeVisible({ timeout: 2000 });
  await caption.dispatchEvent("pointerup", { pointerType: "touch", pointerId: 1, bubbles: true, ...at });
});

test("the action card is sized by its own content, not the reaction row's", async ({ page }) => {
  await page.locator(".lk-bubble-text").first().click({ button: "right" });
  await expect(page.locator(MENU)).toBeVisible();

  const reactions = await box(page.locator(".lk-msg-menu-reactions"));
  const actions = await box(page.locator(".lk-msg-menu-items"));

  // The defect, as a number: these two were EQUAL, because both were stretched
  // children of one card. A row reading "Copy" is not as wide as seven emoji
  // and a plus, and now it is not drawn as if it were.
  expect(actions.width, "the action card is still as wide as the reaction row").toBeLessThan(
    reactions.width,
  );
  // Two surfaces, so there is a real gap between them rather than a border
  // inside one box. Which one is on top is the layout's call - the row goes
  // below the card when there is no room above, and this case opens near the
  // top of the viewport - so the gap is measured in the order it reports.
  const rowAt = await page.locator(MENU).getAttribute("data-row-at");
  const [upper, lower] = rowAt === "above" ? [reactions, actions] : [actions, reactions];
  expect(rowAt === "above" || rowAt === "below", `row placed ${rowAt}`).toBe(true);
  expect(lower.y - (upper.y + upper.height)).toBeCloseTo(SURFACE_GAP_PX, 0);
});
