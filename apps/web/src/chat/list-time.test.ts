// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { calendarDaysBetween, formatListTime } from "./list-time";

// A fixed "now": Wednesday 16 September 2026, 15:30 local time.
const NOW = new Date(2026, 8, 16, 15, 30);

describe("formatListTime", () => {
  it("shows the clock time for a message from today, in the locale's form", () => {
    const today = new Date(2026, 8, 16, 14, 5).toISOString();
    expect(formatListTime(today, NOW, "en-GB")).toBe("14:05");
    expect(formatListTime(today, NOW, "en-US")).toBe("2:05 PM");
  });

  it("shows the short weekday for the last six days", () => {
    const yesterday = new Date(2026, 8, 15, 23, 59).toISOString();
    expect(formatListTime(yesterday, NOW, "en-GB")).toBe("Tue");
    const sixDaysAgo = new Date(2026, 8, 10, 9, 0).toISOString();
    expect(formatListTime(sixDaysAgo, NOW, "en-GB")).toBe("Thu");
  });

  it("shows the short date from a week back, with the year once it differs", () => {
    const weekAgo = new Date(2026, 8, 9, 9, 0).toISOString();
    expect(formatListTime(weekAgo, NOW, "en-GB")).toBe("9 Sept");
    const lastYear = new Date(2025, 11, 24, 9, 0).toISOString();
    expect(formatListTime(lastYear, NOW, "en-GB")).toBe("24 Dec 2025");
  });

  it("uses calendar days, not 24-hour windows", () => {
    // 23:59 yesterday is one calendar day back even though it is under
    // sixteen hours before now.
    expect(calendarDaysBetween(new Date(2026, 8, 15, 23, 59), NOW)).toBe(1);
    expect(calendarDaysBetween(new Date(2026, 8, 16, 0, 0), NOW)).toBe(0);
  });

  it("is empty for a missing or unparseable stamp", () => {
    expect(formatListTime(undefined, NOW)).toBe("");
    expect(formatListTime(null, NOW)).toBe("");
    expect(formatListTime("not a date", NOW)).toBe("");
  });
});
