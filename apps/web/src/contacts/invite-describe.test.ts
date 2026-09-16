// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  describeInviteCreated,
  describeInviteExpiry,
  describeInviteUses,
  INVITE_USES_UNLIMITED,
  inviteUsesLeft,
} from "./invite-describe";

describe("invite expiry", () => {
  const now = 1_000_000_000_000;
  it("reads the sentinels and the remaining time", () => {
    expect(describeInviteExpiry(0, now)).toBe("Never expires");
    expect(describeInviteExpiry(now - 1, now)).toBe("Expired");
    expect(describeInviteExpiry(now + 3 * 86_400_000, now)).toBe("Expires in 3d");
    expect(describeInviteExpiry(now + 2 * 3_600_000, now)).toBe("Expires in 2h");
    expect(describeInviteExpiry(now + 10_000, now)).toBe("Expires in 1m");
  });
});

describe("invite uses", () => {
  it("treats 0 as single-use and the sentinel as unlimited", () => {
    expect(inviteUsesLeft(0, 0)).toBe(1);
    expect(inviteUsesLeft(1, 0)).toBe(0);
    expect(inviteUsesLeft(4, 10)).toBe(6);
    expect(inviteUsesLeft(99, INVITE_USES_UNLIMITED)).toBeNull();
    expect(inviteUsesLeft(12, 10)).toBe(0);
  });

  it("words the count", () => {
    expect(describeInviteUses(0, 0)).toBe("1 use left");
    expect(describeInviteUses(1, 0)).toBe("Used up");
    expect(describeInviteUses(4, 10)).toBe("6 uses left");
    expect(describeInviteUses(0, INVITE_USES_UNLIMITED)).toBe("Unlimited uses");
  });
});

describe("invite created", () => {
  it("formats a date and swallows garbage", () => {
    // "Sep" or "Sept" depending on the ICU data; the day may shift by one
    // with the runner's time zone.
    expect(describeInviteCreated("2026-09-16T10:00:00Z", "en-GB")).toMatch(/^Created \d{1,2} Sept?$/);
    expect(describeInviteCreated("not a date")).toBe("");
  });
});
