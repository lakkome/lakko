// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { postTimeLabel, relativeTimeLabel } from "./relative-time";

const NOW = 1_800_000_000_000;

describe("relativeTimeLabel", () => {
  it("reads 'just now' under a minute", () => {
    expect(relativeTimeLabel(NOW, NOW)).toBe("just now");
    expect(relativeTimeLabel(NOW - 59_000, NOW)).toBe("just now");
  });

  it("counts minutes, hours and days in whole words", () => {
    expect(relativeTimeLabel(NOW - 60_000, NOW)).toBe("1 minute ago");
    expect(relativeTimeLabel(NOW - 5 * 60_000, NOW)).toBe("5 minutes ago");
    expect(relativeTimeLabel(NOW - 3_600_000, NOW)).toBe("1 hour ago");
    expect(relativeTimeLabel(NOW - 7 * 3_600_000, NOW)).toBe("7 hours ago");
    expect(relativeTimeLabel(NOW - 86_400_000, NOW)).toBe("1 day ago");
    expect(relativeTimeLabel(NOW - 12 * 86_400_000, NOW)).toBe("12 days ago");
  });

  it("accepts a bigint stamp (the rotation state stores one)", () => {
    expect(relativeTimeLabel(BigInt(NOW - 2 * 3_600_000), NOW)).toBe("2 hours ago");
  });

  it("never reads a future stamp as a negative interval", () => {
    expect(relativeTimeLabel(NOW + 90_000, NOW)).toBe("just now");
  });
});

describe("postTimeLabel", () => {
  // Wednesday 16 September 2026, 15:30 local time.
  const now = new Date(2026, 8, 16, 15, 30);

  it("shows only the clock time for a post from today", () => {
    expect(postTimeLabel(new Date(2026, 8, 16, 14, 5).getTime(), now, "en-GB")).toBe("14:05");
  });

  it("adds the short date for an older post, and the year once it differs", () => {
    expect(postTimeLabel(new Date(2026, 8, 2, 18, 40).getTime(), now, "en-GB")).toBe(
      "2 Sept, 18:40",
    );
    expect(postTimeLabel(new Date(2025, 0, 3, 20, 0).getTime(), now, "en-GB")).toBe(
      "3 Jan 2025, 20:00",
    );
  });

  it("is empty for an unparseable stamp", () => {
    expect(postTimeLabel(Number.NaN, now)).toBe("");
  });
});
