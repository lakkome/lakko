// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import type { CaseRow } from "../api/types";
import { NO_FILTERS, daySpan, defaultOrder, evidenceSummary, hasFilters, queueQuery, reasonsSummary } from "./queue";

function row(over: Partial<CaseRow>): CaseRow {
  return {
    kind: "account",
    target_b64: "",
    target_hex: "00",
    status: "new",
    lock: null,
    decided_at_ms: null,
    reasons: {},
    reports: 1,
    open_reports: 1,
    with_messages: 0,
    checked_messages_reports: 0,
    first_day: "2026-11-01",
    last_day: "2026-11-01",
    ...over,
  };
}

describe("queueQuery", () => {
  it("is empty with no filters", () => {
    expect(queueQuery(NO_FILTERS)).toBe("");
    expect(hasFilters(NO_FILTERS)).toBe(false);
  });

  it("sends the set filters by name and the checked filter as true / false", () => {
    expect(queueQuery({ status: "new", reason: "child-safety", kind: "group-handle", checked: "yes" })).toBe(
      "?status=new&reason=child-safety&kind=group-handle&checked=true",
    );
    expect(queueQuery({ ...NO_FILTERS, checked: "no" })).toBe("?checked=false");
    expect(hasFilters({ ...NO_FILTERS, reason: "spam" })).toBe(true);
  });
});

describe("reasonsSummary", () => {
  it("lists the most reported first, then sheet order, and drops zeros", () => {
    expect(reasonsSummary({ harassment: 2, spam: 14, hate: 0 })).toBe("Spam 14 · Harassment 2");
    expect(reasonsSummary({ other: 1, spam: 1 })).toBe("Spam 1 · Other 1");
    expect(reasonsSummary({})).toBe("");
  });
});

describe("evidenceSummary", () => {
  it("counts reports with checked messages and reports with nothing included", () => {
    expect(evidenceSummary({ reports: 16, with_messages: 12, checked_messages_reports: 3 })).toBe(
      "3 reports have checked messages · 4 with nothing included",
    );
    expect(evidenceSummary({ reports: 1, with_messages: 1, checked_messages_reports: 1 })).toBe("1 report has checked messages");
    expect(evidenceSummary({ reports: 2, with_messages: 0, checked_messages_reports: 0 })).toBe(
      "No checked messages · 2 with nothing included",
    );
  });
});

describe("defaultOrder", () => {
  it("puts cases with checked messages first, then more reports, then the latest day", () => {
    const a = row({ target_hex: "a", reports: 20 });
    const b = row({ target_hex: "b", reports: 2, checked_messages_reports: 1 });
    const c = row({ target_hex: "c", reports: 5, last_day: "2026-11-02" });
    const d = row({ target_hex: "d", reports: 5, last_day: "2026-11-09" });
    expect(defaultOrder([a, c, b, d]).map((r) => r.target_hex)).toEqual(["b", "a", "d", "c"]);
  });

  it("does not reorder the input array", () => {
    const input = [row({ target_hex: "x", reports: 1 }), row({ target_hex: "y", reports: 2 })];
    defaultOrder(input);
    expect(input.map((r) => r.target_hex)).toEqual(["x", "y"]);
  });
});

describe("daySpan", () => {
  it("shows one day once", () => {
    expect(daySpan("2026-11-01", "2026-11-01")).toBe("2026-11-01");
    expect(daySpan("2026-11-01", "2026-11-19")).toBe("2026-11-01 – 2026-11-19");
  });
});
