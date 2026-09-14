// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  CALL_LINK_DEFAULT_EXPIRY,
  CALL_LINK_DEFAULT_MAX_USES,
  CALL_LINK_EXPIRY_OPTIONS,
  CALL_LINK_MAX_USES_OPTIONS,
  callLinkLabelError,
  callLinkRoutePath,
} from "./create-call-link";
import {
  CALL_INVITE_LABEL_MAX_LEN,
  composePointerCallInviteUrl,
  expiryPresetToMs,
  maxUsesPresetToValue,
} from "./invite";

describe("call-link form presets", () => {
  it("offers 1 hour / 24 hours / 7 days, mapped to the right expiry", () => {
    const now = 1_700_000_000_000;
    expect(CALL_LINK_EXPIRY_OPTIONS.map((o) => o.value)).toEqual(["1h", "1d", "7d"]);
    expect(expiryPresetToMs("1h", now)).toBe(BigInt(now + 3_600_000));
    expect(expiryPresetToMs("1d", now)).toBe(BigInt(now + 86_400_000));
    expect(expiryPresetToMs("7d", now)).toBe(BigInt(now + 7 * 86_400_000));
    expect(CALL_LINK_EXPIRY_OPTIONS.some((o) => o.value === CALL_LINK_DEFAULT_EXPIRY)).toBe(
      true,
    );
  });

  it("offers 1 / 10 / unlimited, mapped to the wire values", () => {
    expect(CALL_LINK_MAX_USES_OPTIONS.map((o) => maxUsesPresetToValue(o.value))).toEqual([
      0, 10, 0xff_ff_ff_ff,
    ]);
    expect(
      CALL_LINK_MAX_USES_OPTIONS.some((o) => o.value === CALL_LINK_DEFAULT_MAX_USES),
    ).toBe(true);
  });
});

describe("callLinkLabelError", () => {
  it("accepts an empty or short label", () => {
    expect(callLinkLabelError("")).toBeNull();
    expect(callLinkLabelError("team standup")).toBeNull();
    expect(callLinkLabelError("a".repeat(CALL_INVITE_LABEL_MAX_LEN))).toBeNull();
  });

  it("measures bytes, not characters", () => {
    // 33 two-byte characters: 33 characters, 66 bytes.
    const label = "é".repeat(33);
    expect(label.length).toBeLessThan(CALL_INVITE_LABEL_MAX_LEN);
    expect(callLinkLabelError(label)).not.toBeNull();
  });
});

describe("callLinkRoutePath", () => {
  it("turns a minted pointer URL into its in-app route", () => {
    const url = composePointerCallInviteUrl(new Uint8Array(32).fill(7), "http://localhost:5173");
    const path = callLinkRoutePath(url);
    expect(path).not.toBeNull();
    expect(path?.startsWith("/c/")).toBe(true);
    expect(url.endsWith(path ?? "unreachable")).toBe(true);
  });

  it("refuses anything that is not a call link", () => {
    expect(callLinkRoutePath("not a url")).toBeNull();
    expect(callLinkRoutePath("https://lakko.me/i/abc")).toBeNull();
    expect(callLinkRoutePath("https://lakko.me/c/")).toBeNull();
    expect(callLinkRoutePath("https://lakko.me/c/AAAA")).toBeNull();
  });
});
