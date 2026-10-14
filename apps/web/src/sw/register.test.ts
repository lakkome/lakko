// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { shouldCheckForUpdate, UPDATE_CHECK_MIN_GAP_MS } from "./register";

describe("shouldCheckForUpdate", () => {
  it("checks at once when nothing has been checked yet", () => {
    expect(shouldCheckForUpdate(null, 1_000, true)).toBe(true);
  });

  it("never checks from a hidden tab", () => {
    expect(shouldCheckForUpdate(null, 1_000, false)).toBe(false);
    expect(shouldCheckForUpdate(0, 10 * UPDATE_CHECK_MIN_GAP_MS, false)).toBe(false);
  });

  it("does not check again inside the minimum gap", () => {
    expect(shouldCheckForUpdate(1_000, 1_000 + UPDATE_CHECK_MIN_GAP_MS - 1, true)).toBe(false);
  });

  it("checks again once the gap has passed", () => {
    expect(shouldCheckForUpdate(1_000, 1_000 + UPDATE_CHECK_MIN_GAP_MS, true)).toBe(true);
  });
});
