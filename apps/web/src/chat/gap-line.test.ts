// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0091 / UX conversation-rejoin 2.4 - placing and narrowing the line. */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { bindContactsAccount } from "../contacts/local-store";
import { gapLineTimes, insertByMessageId, placeGapLines } from "./gap-line";
import { addGapLine, gapLines, narrowGapLine } from "./reentry-store";

describe("placement", () => {
  it("sits before the first entry at or after the time the device came back", () => {
    const p = placeGapLines([100, 200, Number.NaN, 400], [{ fromMs: 50, toMs: 300 }]);
    expect([...p.before.keys()]).toEqual([3]);
    expect(p.trailing).toEqual([]);
  });

  it("at the bottom when nothing happened since", () => {
    const p = placeGapLines([100, 200], [{ fromMs: 50, toMs: 300 }]);
    expect(p.trailing).toEqual([{ fromMs: 50, toMs: 300 }]);
  });

  it("the date is shown once on the same day", () => {
    const day = Date.UTC(2026, 10, 18, 10, 0);
    const same = gapLineTimes({ fromMs: day, toMs: day + 3_600_000 }, "en-GB");
    const other = gapLineTimes({ fromMs: day, toMs: day + 3 * 86_400_000 }, "en-GB");
    expect(same.to.length).toBeLessThan(same.from.length);
    expect(other.to.length).toBeGreaterThanOrEqual(other.from.length - 2);
  });
});

describe("narrowing (missed messages arrived)", () => {
  let data: Map<string, string>;
  beforeEach(() => {
    data = new Map();
    (globalThis as unknown as { localStorage: Storage }).localStorage = {
      get length() {
        return data.size;
      },
      clear: () => data.clear(),
      getItem: (k: string) => data.get(k) ?? null,
      key: (i: number) => [...data.keys()][i] ?? null,
      removeItem: (k: string) => void data.delete(k),
      setItem: (k: string, v: string) => void data.set(k, String(v)),
    };
    bindContactsAccount("Z2FwLWFjY291bnQ=");
  });
  afterEach(() => bindContactsAccount(null));

  it("a complete bundle removes the line; a partial one ends it at its earliest message", () => {
    addGapLine("g", { fromMs: 100, toMs: 900 });
    narrowGapLine("g", 900, { complete: false, earliestMs: 400 });
    expect(gapLines("g")).toEqual([{ fromMs: 100, toMs: 400 }]);
    addGapLine("h", { fromMs: 100, toMs: 900 });
    narrowGapLine("h", 900, { complete: true, earliestMs: 400 });
    expect(gapLines("h")).toEqual([]);
  });
});

describe("the gap's messages in an open chat", () => {
  it("go in by DS id before newer rows, never twice, sends still pending stay last", () => {
    type R = { id?: number; t: string };
    const existing: R[] = [{ id: 1, t: "a" }, { id: 9, t: "new" }, { t: "pending" }];
    const out = insertByMessageId(existing, [{ id: 5, t: "gap" }, { id: 3, t: "gap" }, { id: 9, t: "dup" }], (r) => r.id);
    expect(out.map((r) => r.id ?? "p")).toEqual([1, 3, 5, 9, "p"]);
    expect(out.find((r) => r.id === 9)?.t).toBe("new");
  });
});
