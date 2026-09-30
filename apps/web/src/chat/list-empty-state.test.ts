// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** The empty-state decision of the conversation list (`docs/ux/navigation.md` section 4). */
import { describe, expect, it } from "vitest";

import { showsEmptyState } from "./list-empty-state";

describe("showsEmptyState", () => {
  it("an empty list with nothing typed shows the empty state", () => {
    expect(showsEmptyState(0, "")).toBe(true);
  });

  it("whitespace alone counts as nothing typed", () => {
    expect(showsEmptyState(0, "   ")).toBe(true);
  });

  it("an empty list with something typed gives way to the search", () => {
    expect(showsEmptyState(0, "anna")).toBe(false);
    expect(showsEmptyState(0, " a ")).toBe(false);
  });

  it("a list with conversations never shows the empty state", () => {
    expect(showsEmptyState(1, "")).toBe(false);
    expect(showsEmptyState(3, "anna")).toBe(false);
  });
});
