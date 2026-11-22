// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const css = readFileSync(fileURLToPath(new URL("../styles.css", import.meta.url)), "utf8");

/** The `@media (pointer: coarse)` block that widens small text actions. */
function coarseBlock(): string {
  const start = css.indexOf("@media (pointer: coarse) {");
  expect(start).toBeGreaterThanOrEqual(0);
  return css.slice(start);
}

describe("touch targets of small text actions on a touch screen", () => {
  it("covers the small buttons and the word actions under a message", () => {
    const block = coarseBlock();
    for (const selector of [
      "button.small",
      "a.ghost.small",
      "button.lk-attachment-retry",
      "button.lk-translate-line-action",
      "button.lk-chip-action",
    ]) {
      expect(block).toContain(selector);
    }
  });

  it("is a 48px area that never shrinks a larger control and never draws", () => {
    const block = coarseBlock();
    for (const side of ["top", "bottom", "left", "right"]) {
      expect(block).toContain(`${side}: min(0px, calc(50% - 24px));`);
    }
    expect(block).toContain('content: "";');
    expect(block).not.toMatch(/::after\s*\{[^}]*(background|border|box-shadow)/);
  });

  it("keeps zero specificity so a control positioned by its own class keeps that position", () => {
    expect(coarseBlock()).toMatch(/:where\(\s*button\.small/);
  });
});
