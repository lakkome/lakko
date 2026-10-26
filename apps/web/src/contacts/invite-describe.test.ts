// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  describeInviteCreated,
  describeInviteExpiry,
  describeInviteLimit,
  describeInviteUsage,
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

  it("rounds to the nearest unit, not down", () => {
    expect(describeInviteExpiry(now + 6.9 * 86_400_000, now)).toBe("Expires in 7d");
    expect(describeInviteExpiry(now + 23.6 * 3_600_000, now)).toBe("Expires in 1d");
    expect(describeInviteExpiry(now + 59.5 * 60_000, now)).toBe("Expires in 1h");
    expect(describeInviteExpiry(now + 30_000, now)).toBe("Expires in 1m");
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

describe("invite usage in the own list", () => {
  it("states the limit until used, then what is left", () => {
    expect(describeInviteUsage(0, 0)).toBe(describeInviteLimit(0));
    expect(describeInviteUsage(0, 10)).toBe(describeInviteLimit(10));
    expect(describeInviteUsage(3, 10)).toBe("7 uses left");
    expect(describeInviteUsage(5, INVITE_USES_UNLIMITED)).toBe(describeInviteLimit(INVITE_USES_UNLIMITED));
  });
});

describe("invite created", () => {
  it("formats a date with the time and swallows garbage", () => {
    // "Sep" or "Sept" depending on the ICU data; the day and hour shift with
    // the runner's time zone.
    expect(describeInviteCreated("2026-09-16T10:00:00Z", "en-GB")).toMatch(
      /^Created \d{1,2} Sept?,? (at )?\d{1,2}:\d{2}$/,
    );
    expect(describeInviteCreated("not a date")).toBe("");
  });
});
