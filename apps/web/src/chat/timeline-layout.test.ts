// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  continuesRun,
  dayLabelOf,
  layoutTimeline,
  RUN_WINDOW_MS,
  sameDay,
  shortTime,
  type TimelineItem,
} from "./timeline-layout";

const NOW = new Date(2026, 8, 16, 15, 30);
const at = (y: number, mo: number, d: number, h: number, mi: number, s = 0): string =>
  new Date(y, mo, d, h, mi, s).toISOString();

const row = (
  createdAt: string | undefined,
  senderKey: string | undefined,
  side: TimelineItem["side"] = "incoming",
): TimelineItem => ({ createdAt, senderKey, side });

describe("dayLabelOf", () => {
  it("says Today and Yesterday, then the long date", () => {
    expect(dayLabelOf(at(2026, 8, 16, 1, 0), NOW, "en-GB")).toBe("Today");
    expect(dayLabelOf(at(2026, 8, 15, 23, 59), NOW, "en-GB")).toBe("Yesterday");
    expect(dayLabelOf(at(2026, 8, 3, 9, 0), NOW, "en-GB")).toBe("3 September");
    expect(dayLabelOf(at(2025, 11, 24, 9, 0), NOW, "en-GB")).toBe("24 December 2025");
  });

  it("is empty for an unparseable stamp", () => {
    expect(dayLabelOf("nope", NOW)).toBe("");
  });
});

describe("shortTime / sameDay", () => {
  it("formats the clock time and compares calendar days", () => {
    expect(shortTime(at(2026, 8, 16, 9, 5), "en-GB")).toBe("09:05");
    expect(shortTime(undefined)).toBe("");
    expect(sameDay(at(2026, 8, 16, 0, 1), at(2026, 8, 16, 23, 59))).toBe(true);
    expect(sameDay(at(2026, 8, 15, 23, 59), at(2026, 8, 16, 0, 1))).toBe(false);
    expect(sameDay(undefined, at(2026, 8, 16, 0, 1))).toBe(false);
  });
});

describe("continuesRun", () => {
  const a = row(at(2026, 8, 16, 10, 0), "anna");
  it("joins the same sender within five minutes on the same side", () => {
    expect(continuesRun(a, row(at(2026, 8, 16, 10, 4, 59), "anna"), false)).toBe(true);
    expect(continuesRun(a, row(at(2026, 8, 16, 10, 5, 1), "anna"), false)).toBe(false);
    expect(continuesRun(a, row(at(2026, 8, 16, 10, 1), "ivan"), false)).toBe(false);
    expect(continuesRun(a, row(at(2026, 8, 16, 10, 1), "anna", "outgoing"), false)).toBe(false);
  });

  it("breaks on a system row, a day change and an unknown sender", () => {
    expect(continuesRun(a, row(at(2026, 8, 16, 10, 1), undefined, "system"), false)).toBe(false);
    expect(continuesRun(a, row(at(2026, 8, 16, 10, 1), "anna"), true)).toBe(false);
    expect(continuesRun(row(a.createdAt, undefined), row(at(2026, 8, 16, 10, 1), undefined), false)).toBe(
      false,
    );
  });

  it("a message still sending stays with the one before it", () => {
    expect(continuesRun(row(at(2026, 8, 16, 10, 0), "me", "outgoing"), row(undefined, "me", "outgoing"), false)).toBe(
      true,
    );
  });

  it("the window is five minutes", () => {
    expect(RUN_WINDOW_MS).toBe(300_000);
  });
});

describe("layoutTimeline", () => {
  it("places one separator per day and groups runs", () => {
    const items: TimelineItem[] = [
      row(at(2026, 8, 15, 9, 0), "anna"),
      row(at(2026, 8, 15, 9, 2), "anna"),
      row(at(2026, 8, 15, 9, 3), "ivan"),
      row(at(2026, 8, 16, 8, 0), "ivan"),
      row(at(2026, 8, 16, 8, 1), "me", "outgoing"),
      row(undefined, "me", "outgoing"),
    ];
    const out = layoutTimeline(items, NOW, "en-GB");
    expect(out.map((l) => l.dayLabel)).toEqual(["Yesterday", null, null, "Today", null, null]);
    expect(out.map((l) => l.startsRun)).toEqual([true, false, true, true, true, false]);
    expect(out.map((l) => l.endsRun)).toEqual([false, true, true, true, false, true]);
  });

  it("a system row stands alone and breaks the runs around it", () => {
    const items: TimelineItem[] = [
      row(at(2026, 8, 16, 8, 0), "anna"),
      row(at(2026, 8, 16, 8, 1), undefined, "system"),
      row(at(2026, 8, 16, 8, 2), "anna"),
    ];
    const out = layoutTimeline(items, NOW);
    expect(out.map((l) => [l.startsRun, l.endsRun])).toEqual([
      [true, true],
      [true, true],
      [true, true],
    ]);
  });

  it("a gap over five minutes starts a new run of the same sender", () => {
    const items: TimelineItem[] = [
      row(at(2026, 8, 16, 8, 0), "anna"),
      row(at(2026, 8, 16, 8, 6), "anna"),
    ];
    const out = layoutTimeline(items, NOW);
    expect(out.map((l) => l.startsRun)).toEqual([true, true]);
  });

  it("an empty timeline has no layout", () => {
    expect(layoutTimeline([], NOW)).toEqual([]);
  });
});
