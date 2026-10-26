// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { CUSTOM_MAX_USES, customExpiryMs, customMaxUses, termsFormOf } from "./invite";

describe("the terms form of a link", () => {
  const minted = 1_000_000_000_000;
  const day = 86_400_000;
  const of = (expiryMs: number, maxUses: number) =>
    termsFormOf({ label: "Team", expiryMs, mintedAtMs: minted, maxUses });
  it("reads the presets back", () => {
    expect(of(minted + 7 * day + 30_000, 0)).toMatchObject({ label: "Team", expiry: "7d", maxUses: "single" });
    expect(of(minted + day, 10)).toMatchObject({ expiry: "1d", maxUses: "ten" });
    expect(of(minted + 30 * day, 0xff_ff_ff_ff)).toMatchObject({ expiry: "30d", maxUses: "unlimited" });
    expect(of(0, 0)).toMatchObject({ expiry: "never" });
  });
  it("reads anything else as custom", () => {
    expect(of(minted + 12 * day, 25)).toMatchObject({
      expiry: "custom",
      customExpiryAmount: "12",
      customExpiryUnit: "days",
      maxUses: "custom",
      customUses: "25",
    });
    expect(of(minted + 5 * 3_600_000, 0)).toMatchObject({
      expiry: "custom",
      customExpiryAmount: "5",
      customExpiryUnit: "hours",
    });
  });
});

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
