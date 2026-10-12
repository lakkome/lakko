// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/message-actions.md` section 4 — the pinned bar's order, the tap
 * that advances it, and where it stands after the list changes under it.
 */
import { describe, expect, it } from "vitest";

import {
  clampPinIndex,
  nextPinIndex,
  pinBarItems,
  pinBarLabel,
  pinsAcrossSegments,
  type HeldForPin,
} from "./pin-bar";
import { historyRowId, isHistoryRowId } from "./lineage";
import type { StoredPin } from "./pin-store";

const pin = (updatedAtMs: bigint, pinned = true): StoredPin => ({ pinned, updatedAtMs });
const allHeld = (): HeldForPin => ({ deleted: false });

describe("pinBarItems — the order", () => {
  it("shared pins come before personal ones, each newest first", () => {
    const items = pinBarItems({
      shared: new Map([
        [1, pin(100n)],
        [2, pin(300n)],
        [3, pin(200n)],
      ]),
      personal: new Map([
        [4, 50n],
        [5, 900n],
      ]),
      held: allHeld,
    });
    expect(items).toEqual([
      { messageId: 2, scope: "shared" },
      { messageId: 3, scope: "shared" },
      { messageId: 1, scope: "shared" },
      { messageId: 5, scope: "personal" },
      { messageId: 4, scope: "personal" },
    ]);
  });

  it("a personal pin never outranks a shared one however new it is", () => {
    const items = pinBarItems({
      shared: new Map([[1, pin(1n)]]),
      personal: new Map([[2, 999_999n]]),
      held: allHeld,
    });
    expect(items.map((i) => i.scope)).toEqual(["shared", "personal"]);
  });

  it("a message pinned in both scopes appears once, as the shared one", () => {
    const items = pinBarItems({
      shared: new Map([[7, pin(10n)]]),
      personal: new Map([[7, 20n]]),
      held: allHeld,
    });
    expect(items).toEqual([{ messageId: 7, scope: "shared" }]);
  });

  it("a stored UNPIN is not in the bar, and the personal pin of that message still is", () => {
    const items = pinBarItems({
      shared: new Map([[7, pin(10n, false)]]),
      personal: new Map([[7, 20n]]),
      held: allHeld,
    });
    expect(items).toEqual([{ messageId: 7, scope: "personal" }]);
  });

  it("a pinned message this device does not hold is skipped, the pin kept elsewhere", () => {
    const items = pinBarItems({
      shared: new Map([
        [1, pin(100n)],
        [2, pin(200n)],
      ]),
      personal: new Map([[3, 5n]]),
      held: (id) => (id === 2 ? null : allHeld()),
    });
    expect(items.map((i) => i.messageId)).toEqual([1, 3]);
  });

  it("a deleted message leaves the bar", () => {
    const items = pinBarItems({
      shared: new Map([[1, pin(100n)]]),
      personal: new Map([[2, 5n]]),
      held: (id) => ({ deleted: id === 1 }),
    });
    expect(items).toEqual([{ messageId: 2, scope: "personal" }]);
  });

  it("an equal stamp is broken by the later message first", () => {
    const items = pinBarItems({
      shared: new Map([
        [3, pin(100n)],
        [9, pin(100n)],
        [5, pin(100n)],
      ]),
      personal: new Map(),
      held: allHeld,
    });
    expect(items.map((i) => i.messageId)).toEqual([9, 5, 3]);
  });

  it("no pins, no bar", () => {
    expect(pinBarItems({ shared: new Map(), personal: new Map(), held: allHeld })).toEqual([]);
  });
});

describe("nextPinIndex — the tap", () => {
  it("advances to the next older pin", () => {
    expect(nextPinIndex(0, 3)).toBe(1);
    expect(nextPinIndex(1, 3)).toBe(2);
  });

  it("wraps past the oldest to the newest", () => {
    expect(nextPinIndex(2, 3)).toBe(0);
  });

  it("with one pin it stays", () => {
    expect(nextPinIndex(0, 1)).toBe(0);
  });

  it("with none it is zero", () => {
    expect(nextPinIndex(0, 0)).toBe(0);
  });
});

describe("clampPinIndex — after the list changed", () => {
  it("keeps a position that still exists", () => {
    expect(clampPinIndex(2, 3)).toBe(2);
  });

  it("returns to the newest when the position is gone", () => {
    expect(clampPinIndex(2, 2)).toBe(0);
    expect(clampPinIndex(5, 1)).toBe(0);
    expect(clampPinIndex(0, 0)).toBe(0);
    expect(clampPinIndex(-1, 3)).toBe(0);
  });
});

describe("pinBarLabel", () => {
  it("names the position when there are several", () => {
    expect(pinBarLabel(1, 3)).toBe("Pinned message 2 of 3");
  });

  it("is just the bar with one", () => {
    expect(pinBarLabel(0, 1)).toBe("Pinned message");
  });
});

describe("a conversation's pins span its segments (ADR-0065 R.5)", () => {
  it("an older segment's pins stay in the bar under the id their row is drawn with", () => {
    const spanned = pinsAcrossSegments(
      [
        { distanceFromHead: 1, shared: new Map([[7, pin(10n)]]), personal: new Map([[3, 5n]]) },
        { distanceFromHead: 0, shared: new Map([[7, pin(20n)]]), personal: new Map([[2, 30n]]) },
      ],
      historyRowId,
    );
    // The same DS message id in two segments is two different messages.
    expect([...spanned.shared.keys()].sort((a, b) => a - b)).toEqual([historyRowId(7, 1), 7]);
    expect([...spanned.personal.keys()].sort((a, b) => a - b)).toEqual([historyRowId(3, 1), 2]);
    const items = pinBarItems({ ...spanned, held: allHeld });
    expect(items).toEqual([
      { messageId: 7, scope: "shared" },
      { messageId: historyRowId(7, 1), scope: "shared" },
      { messageId: 2, scope: "personal" },
      { messageId: historyRowId(3, 1), scope: "personal" },
    ]);
    expect(items.filter((i) => isHistoryRowId(i.messageId))).toHaveLength(2);
  });

  it("a stored unpin of an older segment stays an unpin", () => {
    const spanned = pinsAcrossSegments(
      [{ distanceFromHead: 2, shared: new Map([[4, pin(10n, false)]]), personal: new Map() }],
      historyRowId,
    );
    expect(pinBarItems({ ...spanned, held: allHeld })).toEqual([]);
  });

  it("a conversation that never moved is its one segment, unchanged", () => {
    const shared = new Map([[9, pin(1n)]]);
    const personal = new Map([[4, 2n]]);
    const spanned = pinsAcrossSegments([{ distanceFromHead: 0, shared, personal }], historyRowId);
    expect([...spanned.shared]).toEqual([...shared]);
    expect([...spanned.personal]).toEqual([...personal]);
  });
});
