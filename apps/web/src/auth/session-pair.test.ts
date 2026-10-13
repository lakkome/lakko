// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The session pair (ADR-0073 section 3): parsing a landing response and the
 * token provider's timing rule.
 */
import { describe, expect, it } from "vitest";

import {
  REFRESH_AHEAD_MS,
  sessionPairOf,
  tokenAccountSegment,
  tokenDecision,
} from "./session-pair";

const NOW = 1_800_000_000_000;

const WIRE = {
  session_token: "v2.QUNDVA.1.1800000000900000.tag",
  session_expires_at_ms: NOW + 900_000,
  refresh_token: "r1.QUNDVA.1.1802592000000000.tag",
  refresh_expires_at_ms: NOW + 30 * 24 * 60 * 60 * 1000,
  key_generation: 1,
};

describe("sessionPairOf", () => {
  it("reads the five fields and keeps the generation as a bigint", () => {
    expect(sessionPairOf(WIRE)).toEqual({
      sessionToken: WIRE.session_token,
      sessionExpiresAtMs: WIRE.session_expires_at_ms,
      refreshToken: WIRE.refresh_token,
      refreshExpiresAtMs: WIRE.refresh_expires_at_ms,
      keyGeneration: 1n,
    });
  });

  it("accepts the generation as a decimal string or a bigint", () => {
    expect(sessionPairOf({ ...WIRE, key_generation: "7" }).keyGeneration).toBe(7n);
    expect(sessionPairOf({ ...WIRE, key_generation: 9n }).keyGeneration).toBe(9n);
  });

  it("refuses a response missing any field: a half pair could never refresh", () => {
    for (const field of Object.keys(WIRE) as (keyof typeof WIRE)[]) {
      const partial: Partial<typeof WIRE> = { ...WIRE };
      delete partial[field];
      expect(() => sessionPairOf(partial)).toThrow(/session pair/);
    }
    expect(() => sessionPairOf(null)).toThrow(/session pair/);
    expect(() => sessionPairOf({ ...WIRE, session_token: "" })).toThrow(/session_token/);
    expect(() => sessionPairOf({ ...WIRE, key_generation: 0 })).toThrow(/below 1/);
    expect(() => sessionPairOf({ ...WIRE, key_generation: "x" })).toThrow(/key_generation/);
    expect(() => sessionPairOf({ ...WIRE, session_expires_at_ms: Number.NaN })).toThrow(/expires/);
  });
});

describe("tokenDecision", () => {
  const pair = { sessionExpiresAtMs: NOW + 900_000, refreshExpiresAtMs: NOW + 1_000_000 };

  it("hands out the access token while more than the refresh-ahead window remains", () => {
    expect(tokenDecision(pair, NOW)).toBe("use-current");
    expect(tokenDecision(pair, NOW + 900_000 - REFRESH_AHEAD_MS - 1)).toBe("use-current");
  });

  it("refreshes first at exactly the window and inside it, and once expired", () => {
    expect(tokenDecision(pair, NOW + 900_000 - REFRESH_AHEAD_MS)).toBe("refresh-first");
    expect(tokenDecision(pair, NOW + 900_000 - 1)).toBe("refresh-first");
    expect(tokenDecision(pair, NOW + 900_000)).toBe("refresh-first");
    expect(tokenDecision(pair, NOW + 999_999)).toBe("refresh-first");
  });

  it("reports an expired refresh credential, whatever the access token says", () => {
    expect(tokenDecision(pair, NOW + 1_000_000)).toBe("refresh-expired");
    expect(
      tokenDecision({ sessionExpiresAtMs: NOW + 900_000, refreshExpiresAtMs: NOW }, NOW),
    ).toBe("refresh-expired");
  });
});

describe("tokenAccountSegment", () => {
  it("reads the account segment of a v2 access token and an r1 refresh credential alike", () => {
    expect(tokenAccountSegment("v2.QUNDVA.3.123.tag")).toBe("QUNDVA");
    expect(tokenAccountSegment("r1.QUNDVA.3.123.tag")).toBe("QUNDVA");
    // The pre-ADR-0073 four-part shape reads the same.
    expect(tokenAccountSegment("v1.QUNDVA.123.tag")).toBe("QUNDVA");
  });

  it("returns the whole string when it has no such shape", () => {
    expect(tokenAccountSegment("opaque")).toBe("opaque");
    expect(tokenAccountSegment("a.b.c")).toBe("a.b.c");
  });
});
