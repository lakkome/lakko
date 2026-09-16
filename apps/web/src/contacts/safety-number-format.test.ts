// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { safetyNumberGroups, safetyNumberRows } from "./safety-number-format";

const SIXTY = "12345 67890 12345 67890 12345 67890 12345 67890 12345 67890 12345 67890";

describe("safety number formatting", () => {
  it("splits the core's string into twelve groups of five", () => {
    const groups = safetyNumberGroups(SIXTY);
    expect(groups).toHaveLength(12);
    expect(groups.every((g) => g.length === 5)).toBe(true);
    expect(groups.join(" ")).toBe(SIXTY);
  });

  it("gives the same groups for the bare digits", () => {
    expect(safetyNumberGroups(SIXTY.replace(/ /g, ""))).toEqual(safetyNumberGroups(SIXTY));
  });

  it("lays the groups out in rows of four", () => {
    const rows = safetyNumberRows(SIXTY);
    expect(rows).toHaveLength(3);
    expect(rows.map((r) => r.length)).toEqual([4, 4, 4]);
    expect(rows.flat().join(" ")).toBe(SIXTY);
  });

  it("keeps a short tail as a partial row and returns nothing for nothing", () => {
    expect(safetyNumberRows("12345 67890 11111 22222 33333", 4)).toEqual([
      ["12345", "67890", "11111", "22222"],
      ["33333"],
    ]);
    expect(safetyNumberGroups("")).toEqual([]);
    expect(safetyNumberRows("")).toEqual([]);
  });
});
