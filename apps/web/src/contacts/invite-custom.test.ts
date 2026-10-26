// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { CUSTOM_MAX_USES, customExpiryMs, customMaxUses } from "./invite";

describe("custom invite expiry", () => {
  const now = 1_000_000_000_000;
  it("accepts 1 hour to 365 days", () => {
    expect(customExpiryMs("1", "hours", now)).toBe(BigInt(now + 3_600_000));
    expect(customExpiryMs("12", "days", now)).toBe(BigInt(now + 12 * 86_400_000));
    expect(customExpiryMs("365", "days", now)).toBe(BigInt(now + 365 * 86_400_000));
    expect(customExpiryMs(" 8760 ", "hours", now)).toBe(BigInt(now + 8760 * 3_600_000));
  });
  it("refuses anything else", () => {
    for (const bad of ["", "0", "-1", "1.5", "abc", "1e3", "366"]) {
      expect(customExpiryMs(bad, "days", now)).toBeNull();
    }
    expect(customExpiryMs("8761", "hours", now)).toBeNull();
  });
});

describe("custom invite uses", () => {
  it("maps one person to the single-use sentinel", () => {
    expect(customMaxUses("1")).toBe(0);
    expect(customMaxUses("25")).toBe(25);
    expect(customMaxUses(String(CUSTOM_MAX_USES))).toBe(CUSTOM_MAX_USES);
  });
  it("refuses anything else", () => {
    for (const bad of ["", "0", "-3", "2.5", "x", String(CUSTOM_MAX_USES + 1)]) {
      expect(customMaxUses(bad)).toBeNull();
    }
  });
});
