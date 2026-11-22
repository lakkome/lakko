// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  DELETED_COLLAPSE_MS,
  DELETED_HOLD_MS,
  DeletedHold,
  idsEnteringHold,
  isRowDrawn,
  newestNotDeleted,
  newlyDeletedIds,
  withoutDeleted,
  type DeletedPhase,
  type HoldClock,
} from "./deleted-rows";

/** A clock a test advances by hand. */
function manualClock(): HoldClock & { advance: (ms: number) => void } {
  let now = 0;
  let seq = 0;
  const timers = new Map<number, { at: number; fn: () => void }>();
  return {
    setTimeout: (fn, ms) => {
      seq += 1;
      timers.set(seq, { at: now + ms, fn });
      return seq;
    },
    clearTimeout: (h) => {
      timers.delete(h as number);
    },
    advance: (ms) => {
      const until = now + ms;
      for (;;) {
        let next: [number, { at: number; fn: () => void }] | undefined;
        for (const e of timers) if (e[1].at <= until && (next === undefined || e[1].at < next[1].at)) next = e;
        if (next === undefined) break;
        timers.delete(next[0]);
        now = next[1].at;
        next[1].fn();
      }
      now = until;
    },
  };
}

describe("newlyDeletedIds", () => {
  it("names only the ids the batch deleted", () => {
    const prev = new Map([
      [1, { deleted: true }],
      [2, {}],
    ]);
    const next = new Map([
      [1, { deleted: true }],
      [2, { deleted: true }],
      [3, { deleted: true }],
      [4, {}],
    ]);
    expect(newlyDeletedIds(prev, next)).toEqual([2, 3]);
  });
});

describe("idsEnteringHold", () => {
  it("holds only rows drawn before the batch, and only in front", () => {
    const drawnBefore = new Set([10, 11]);
    expect(idsEnteringHold({ drawnBefore, newlyDeleted: [10, 12], inFront: true })).toEqual([10]);
    expect(idsEnteringHold({ drawnBefore, newlyDeleted: [10, 11], inFront: false })).toEqual([]);
  });

  it("a message and its deletion in one batch are never held", () => {
    expect(idsEnteringHold({ drawnBefore: new Set([1]), newlyDeleted: [2], inFront: true })).toEqual([]);
  });
});

describe("withoutDeleted / isRowDrawn", () => {
  const deleted = (id: number): boolean => id === 2 || id === 3;
  it("drops deleted rows except those in their hold, keeping order", () => {
    const held = new Map<number, DeletedPhase>([[3, "hold"]]);
    const rows = [{ id: 1 }, { id: 2 }, { id: 3 }, { id: undefined }];
    expect(withoutDeleted(rows, (r) => r.id, deleted, held)).toEqual([{ id: 1 }, { id: 3 }, { id: undefined }]);
  });
  it("a row with no id is drawn", () => {
    expect(isRowDrawn(undefined, () => true, new Map())).toBe(true);
  });
});

describe("newestNotDeleted", () => {
  it("falls back past deleted items", () => {
    const items: { n: number; deleted?: boolean }[] = [{ n: 1 }, { n: 2, deleted: true }, { n: 3, deleted: true }];
    expect(newestNotDeleted(items)?.n).toBe(1);
  });
  it("is undefined when nothing is left", () => {
    expect(newestNotDeleted([{ deleted: true }])).toBeUndefined();
  });
  it("honours the eligibility filter", () => {
    const items: { n: number; system?: boolean; deleted?: boolean }[] = [{ n: 1 }, { n: 2, system: true }];
    expect(newestNotDeleted(items, (i) => i.system !== true)?.n).toBe(1);
  });
});

describe("DeletedHold", () => {
  it("holds for 2 s, collapses for 250 ms, then removes", () => {
    const clock = manualClock();
    const seen: ReadonlyMap<number, DeletedPhase>[] = [];
    const hold = new DeletedHold((p) => seen.push(p), () => false, clock);
    hold.start([5]);
    expect(hold.current().get(5)).toBe("hold");
    clock.advance(DELETED_HOLD_MS - 1);
    expect(hold.current().get(5)).toBe("hold");
    clock.advance(1);
    expect(hold.current().get(5)).toBe("collapsing");
    clock.advance(DELETED_COLLAPSE_MS);
    expect(hold.current().has(5)).toBe(false);
    expect(seen.map((m) => m.get(5) ?? "gone")).toEqual(["hold", "collapsing", "gone"]);
  });

  it("with reduced motion removes at the end of the hold", () => {
    const clock = manualClock();
    const hold = new DeletedHold(() => undefined, () => true, clock);
    hold.start([5]);
    clock.advance(DELETED_HOLD_MS);
    expect(hold.current().has(5)).toBe(false);
  });

  it("each row runs on its own timer", () => {
    const clock = manualClock();
    const hold = new DeletedHold(() => undefined, () => false, clock);
    hold.start([1]);
    clock.advance(1000);
    hold.start([2]);
    clock.advance(1000);
    expect(hold.current().get(1)).toBe("collapsing");
    expect(hold.current().get(2)).toBe("hold");
  });

  it("clear ends every hold and no timer fires after it", () => {
    const clock = manualClock();
    const seen: number[] = [];
    const hold = new DeletedHold((p) => seen.push(p.size), () => false, clock);
    hold.start([1, 2]);
    hold.clear();
    expect(hold.current().size).toBe(0);
    clock.advance(10_000);
    expect(seen).toEqual([2, 0]);
  });

  it("a second start of a held id does not restart its timer", () => {
    const clock = manualClock();
    const hold = new DeletedHold(() => undefined, () => false, clock);
    hold.start([1]);
    clock.advance(1500);
    hold.start([1]);
    clock.advance(500);
    expect(hold.current().get(1)).toBe("collapsing");
  });
});
