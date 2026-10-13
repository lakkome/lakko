// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0073 R.5 - a pin of an older segment can be unpinned. */
import { describe, expect, it } from "vitest";

import { historyRowId, historyRowOrigin, HISTORY_ID_SPAN } from "./lineage";
import { historyRowOffersUnpin, pinRoute, pinTargetOfRow } from "./pin-bar";

describe("historyRowOrigin (the inverse of historyRowId)", () => {
  it("round-trips a message id and a distance", () => {
    for (const [id, d] of [
      [1, 1],
      [41, 1],
      [41, 3],
      [HISTORY_ID_SPAN - 1, 2],
    ] as const) {
      expect(historyRowOrigin(historyRowId(id, d))).toEqual({ messageId: id, distanceFromHead: d });
    }
  });
  it("is null for a row of the current group and for a number no history row has", () => {
    expect(historyRowOrigin(41)).toBeNull();
    expect(historyRowOrigin(0)).toBeNull();
    expect(historyRowOrigin(-HISTORY_ID_SPAN)).toBeNull();
    expect(historyRowOrigin(Number.NaN)).toBeNull();
  });
});

describe("pinTargetOfRow", () => {
  const base = { headGroupIdB64: "head", olderGroupIdsB64: ["oldest", "older"], origin: historyRowOrigin };
  it("a row of the current group is its own pin", () => {
    expect(pinTargetOfRow({ ...base, rowId: 41 })).toEqual({ groupIdB64: "head", messageId: 41, older: false });
  });
  it("a history row is the pin of its own segment under its original id", () => {
    expect(pinTargetOfRow({ ...base, rowId: historyRowId(41, 1) })).toEqual({
      groupIdB64: "older",
      messageId: 41,
      older: true,
    });
    expect(pinTargetOfRow({ ...base, rowId: historyRowId(7, 2) })).toEqual({
      groupIdB64: "oldest",
      messageId: 7,
      older: true,
    });
  });
  it("a history row of a segment this device does not hold is nothing", () => {
    expect(pinTargetOfRow({ ...base, rowId: historyRowId(7, 3) })).toBeNull();
  });
});

describe("pinRoute", () => {
  it("the current group is the ordinary path", () => {
    expect(pinRoute({ older: false, pinned: true, scope: "shared" })).toBe("ordinary");
    expect(pinRoute({ older: false, pinned: false, scope: "shared" })).toBe("ordinary");
  });
  it("an older segment's personal pin is unpinned in the store", () => {
    expect(pinRoute({ older: true, pinned: false, scope: "personal" })).toBe("ordinary");
  });
  it("an older segment's shared pin is unpinned on this device only: nothing is published", () => {
    expect(pinRoute({ older: true, pinned: false, scope: "shared" })).toBe("local-only");
  });
  it("nothing is newly pinned in an older segment", () => {
    expect(pinRoute({ older: true, pinned: true, scope: "shared" })).toBe("refused");
    expect(pinRoute({ older: true, pinned: true, scope: "personal" })).toBe("refused");
  });
});

describe("historyRowOffersUnpin", () => {
  it("a pinned row of an older segment offers Unpin alone", () => {
    expect(historyRowOffersUnpin({ isHistoryRow: true, pinRow: "Unpin" })).toBe(true);
  });
  it("an unpinned history row offers nothing, and a current row is not this rule's", () => {
    expect(historyRowOffersUnpin({ isHistoryRow: true, pinRow: "Pin" })).toBe(false);
    expect(historyRowOffersUnpin({ isHistoryRow: false, pinRow: "Unpin" })).toBe(false);
  });
});
