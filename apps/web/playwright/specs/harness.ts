// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Opening a fixture, and the two geometry reads every spec here does.
 *
 * The reads are in one place because both are easy to get subtly wrong in a
 * way that makes a test pass: a `boundingBox()` taken before layout has
 * settled, and a "same row" check done on a box that is not rendered.
 */

import { expect, type Locator, type Page } from "@playwright/test";

/** Open a case and wait for the fixture to say it has mounted and laid out.
 *  A fixture that threw fails HERE, naming the error, rather than later as
 *  "the element never appeared". */
export async function openCase(
  page: Page,
  name: string,
  params: Record<string, string | number> = {},
): Promise<void> {
  const q = new URLSearchParams({ case: name });
  for (const [k, v] of Object.entries(params)) q.set(k, String(v));
  await page.goto(`/playwright/harness/index.html?${q.toString()}`);
  // `attached`, not the default `visible`: a fixture that threw leaves the mount
  // point EMPTY, an empty div has no box, and waiting for it to be visible
  // spends the whole test timeout and then reports a missing element instead of
  // the error the fixture recorded. Measured: 30s and an opaque failure, against
  // an immediate "voice fixture is not an MP4".
  await page.waitForSelector("#harness[data-ready], #harness[data-error]", { state: "attached" });
  const err = await page.getAttribute("#harness", "data-error");
  expect(err, "the fixture failed to mount").toBeNull();
}

export interface Box {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** The element's box, asserted to exist and to have been laid out. A zero-area
 *  box is the shape jsdom produces for everything, so measuring one here would
 *  be measuring nothing — which is the whole reason this project exists. */
export async function box(locator: Locator): Promise<Box> {
  const b = await locator.boundingBox();
  expect(b, "no box: the element is absent or not laid out").not.toBeNull();
  const got = b as Box;
  expect(got.height, "a zero-height box cannot be compared").toBeGreaterThan(0);
  return got;
}

/** The vertical centre of a box. What "on the same row" means once a person is
 *  looking at it: two boxes of different heights are level when their middles
 *  are, whatever their parentage in the DOM. */
export function centreY(b: Box): number {
  return b.y + b.height / 2;
}
