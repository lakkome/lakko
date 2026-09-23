// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  callHistory,
  callHistoryByDay,
  callHistoryDayLabel,
  type CallHistoryRow,
} from "./call-history";
import type { CallLogEntry } from "./call-log";
import type { CallLogRows } from "./call-log-store";

function entry(over: Partial<CallLogEntry> = {}): CallLogEntry {
  return {
    id: "c1",
    outcome: "outgoing",
    video: false,
    startedAtMs: 1_000,
    durationMs: null,
    ...over,
  } as CallLogEntry;
}

/** A local-time stamp, so a test never depends on the runner's zone. */
function at(y: number, m: number, d: number, h = 12, min = 0): number {
  return new Date(y, m - 1, d, h, min).getTime();
}

describe("callHistory", () => {
  it("interleaves conversations by time rather than listing them one after another", () => {
    // The point of the screen: a person looking for a call remembers roughly
    // when, not which thread it belonged to.
    const rows: CallLogRows = new Map([
      ["anna", [entry({ id: "a1", startedAtMs: 10 }), entry({ id: "a2", startedAtMs: 30 })]],
      ["boris", [entry({ id: "b1", startedAtMs: 20 })]],
    ]);
    expect(callHistory(rows).map((r) => r.entry.id)).toEqual(["a2", "b1", "a1"]);
  });

  it("keeps the conversation each call belongs to", () => {
    const rows: CallLogRows = new Map([["anna", [entry({ id: "a1" })]]]);
    expect(callHistory(rows)[0]?.groupIdB64).toBe("anna");
  });

  it("orders two calls from the same millisecond stably", () => {
    // Without a tiebreak they sit in whatever order the map yields, and the
    // list reshuffles on an unrelated refresh.
    const rows: CallLogRows = new Map([
      ["x", [entry({ id: "zz", startedAtMs: 5 })]],
      ["y", [entry({ id: "aa", startedAtMs: 5 })]],
    ]);
    expect(callHistory(rows).map((r) => r.entry.id)).toEqual(["aa", "zz"]);
  });

  it("caps the list, keeping the newest", () => {
    const many = Array.from({ length: 10 }, (_, i) =>
      entry({ id: `c${i}`, startedAtMs: i }),
    );
    const capped = callHistory(new Map([["g", many]]), 3);
    expect(capped.map((r) => r.entry.startedAtMs)).toEqual([9, 8, 7]);
  });

  it("is empty for an account that has never called", () => {
    expect(callHistory(new Map())).toEqual([]);
  });
});

describe("callHistoryByDay", () => {
  function row(ms: number, id: string): CallHistoryRow {
    return { groupIdB64: "g", entry: entry({ id, startedAtMs: ms }) };
  }

  it("groups a run of calls under one day", () => {
    const days = callHistoryByDay([
      row(at(2026, 9, 23, 18), "c1"),
      row(at(2026, 9, 23, 9), "c2"),
      row(at(2026, 9, 22, 20), "c3"),
    ]);
    expect(days).toHaveLength(2);
    expect(days[0]?.rows.map((r) => r.entry.id)).toEqual(["c1", "c2"]);
    expect(days[1]?.rows.map((r) => r.entry.id)).toEqual(["c3"]);
  });

  it("uses the viewer's own day, not UTC", () => {
    // A call at 00:30 local is today to the person who made it. Filing it
    // under yesterday because UTC says so is wrong in a way nobody would
    // think to report.
    const justAfterMidnight = at(2026, 9, 23, 0, 30);
    const [day] = callHistoryByDay([row(justAfterMidnight, "c1")]);
    expect(day?.day).toBe("2026-09-23");
  });

  it("keeps the order it was given", () => {
    const days = callHistoryByDay([row(at(2026, 9, 23, 9), "c1"), row(at(2026, 9, 23, 18), "c2")]);
    expect(days[0]?.rows.map((r) => r.entry.id)).toEqual(["c1", "c2"]);
  });

  it("is empty for no calls", () => {
    expect(callHistoryByDay([])).toEqual([]);
  });
});

describe("callHistoryDayLabel", () => {
  const now = new Date(2026, 8, 23, 15, 0);

  it("says Today for today", () => {
    expect(callHistoryDayLabel("2026-09-23", now)).toBe("Today");
  });

  it("says Yesterday for yesterday", () => {
    expect(callHistoryDayLabel("2026-09-22", now)).toBe("Yesterday");
  });

  it("crosses a month boundary correctly", () => {
    expect(callHistoryDayLabel("2026-08-31", new Date(2026, 8, 1, 10))).toBe("Yesterday");
  });

  it("names the date for anything older, without the year inside this year", () => {
    const label = callHistoryDayLabel("2026-03-04", now);
    expect(label).not.toBe("Today");
    expect(label).not.toBe("Yesterday");
    expect(label).not.toContain("2026");
  });

  it("names the year for a call from another year", () => {
    expect(callHistoryDayLabel("2025-03-04", now)).toContain("2025");
  });

  it("falls back to the key rather than throwing on something unparseable", () => {
    expect(callHistoryDayLabel("not-a-day", now)).toBe("not-a-day");
  });
});
