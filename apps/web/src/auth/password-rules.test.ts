// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { MIN_PASSWORD_LENGTH, newPasswordProblem } from "./password-rules";

describe("newPasswordProblem", () => {
  it("accepts a matching pair at the floor", () => {
    const p = "x".repeat(MIN_PASSWORD_LENGTH);
    expect(newPasswordProblem(p, p)).toBeNull();
  });

  it("reports a short password before a mismatch", () => {
    expect(newPasswordProblem("short", "other")).toMatch(/at least/);
  });

  it("reports a mismatch as what to do", () => {
    const p = "x".repeat(MIN_PASSWORD_LENGTH);
    expect(newPasswordProblem(p, `${p}y`)).toBe(
      "The passwords do not match. Type the same password twice.",
    );
  });
});
