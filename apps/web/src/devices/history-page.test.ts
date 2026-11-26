// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0101 section 9 - the rows of one history page, newest first. */
import { describe, expect, it } from "vitest";

import { historyPageRows } from "./history-bundle";

const rows = ["10", "3", "7", "x", "0", "12", "5"].map((messageId) => ({ messageId }));

describe("historyPageRows", () => {
  it("the newest page when `before` is 0, oldest first, unparseable ids left out", () => {
    expect(historyPageRows(rows, 0n, 3).map((r) => r.messageId)).toEqual(["7", "10", "12"]);
  });
  it("only ids below `before`", () => {
    expect(historyPageRows(rows, 7n, 10).map((r) => r.messageId)).toEqual(["3", "5"]);
    expect(historyPageRows(rows, 3n, 10)).toEqual([]);
  });
  it("a page size of 0 gives nothing", () => {
    expect(historyPageRows(rows, 0n, 0)).toEqual([]);
  });
});
