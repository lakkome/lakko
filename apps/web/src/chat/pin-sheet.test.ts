// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/message-actions.md` section 4.1 — the `Pin` / `Unpin` row and the
 * sheet behind it, by authority and by what is already pinned.
 */
import { describe, expect, it } from "vitest";

import {
  PIN_DETAIL_PERSONAL,
  PIN_DETAIL_SHARED,
  PIN_SHEET_TITLE,
  UNPIN_SHEET_TITLE,
  pinRowDecision,
  type PinState,
} from "./pin-sheet";

function decide(over: Partial<PinState>): ReturnType<typeof pinRowDecision> {
  return pinRowDecision({
    pinnedShared: false,
    pinnedPersonal: false,
    maySetShared: true,
    oneToOne: false,
    ...over,
  });
}

function labels(d: ReturnType<typeof pinRowDecision>): string[] {
  return d.action.kind === "sheet" ? d.action.options.map((o) => o.label) : [d.action.option.label];
}

describe("pinRowDecision — nothing pinned", () => {
  it("an owner / admin gets Pin for me and Pin for everyone, each with its line", () => {
    const d = decide({});
    expect(d.row).toBe("Pin");
    expect(d.action.kind).toBe("sheet");
    if (d.action.kind !== "sheet") throw new Error("unreachable");
    expect(d.action.title).toBe(PIN_SHEET_TITLE());
    expect(d.action.options.map((o) => [o.label, o.detail, o.scope, o.pinned])).toEqual([
      ["Pin for me", PIN_DETAIL_PERSONAL(), "personal", true],
      ["Pin for everyone", PIN_DETAIL_SHARED(), "shared", true],
    ]);
  });

  it("a plain member sees the one-row sheet and no explanation", () => {
    const d = decide({ maySetShared: false });
    expect(d.row).toBe("Pin");
    expect(labels(d)).toEqual(["Pin for me"]);
  });

  it("in a one-to-one the shared row reads Pin for both", () => {
    expect(labels(decide({ oneToOne: true }))).toEqual(["Pin for me", "Pin for both"]);
  });
});

describe("pinRowDecision — pinned in one scope", () => {
  it("pinned for me: Unpin acts at once on the personal pin", () => {
    const d = decide({ pinnedPersonal: true });
    expect(d.row).toBe("Unpin");
    expect(d.action).toEqual({
      kind: "immediate",
      option: { scope: "personal", pinned: false, label: "Unpin for me", detail: PIN_DETAIL_PERSONAL() },
    });
  });

  it("pinned for everyone by someone with standing: Unpin acts at once on the shared pin", () => {
    const d = decide({ pinnedShared: true });
    expect(d.row).toBe("Unpin");
    expect(d.action).toEqual({
      kind: "immediate",
      option: { scope: "shared", pinned: false, label: "Unpin for everyone", detail: PIN_DETAIL_SHARED() },
    });
  });

  it("in a one-to-one the immediate unpin reads Unpin for both", () => {
    expect(labels(decide({ pinnedShared: true, oneToOne: true }))).toEqual(["Unpin for both"]);
  });

  it("pinned by an admin, seen by a plain member: the row is Pin and the sheet offers Pin for me alone", () => {
    const d = decide({ pinnedShared: true, maySetShared: false });
    expect(d.row).toBe("Pin");
    expect(d.action.kind).toBe("sheet");
    expect(labels(d)).toEqual(["Pin for me"]);
  });
});

describe("pinRowDecision — pinned in both scopes", () => {
  it("with standing the sheet asks which", () => {
    const d = decide({ pinnedShared: true, pinnedPersonal: true });
    expect(d.row).toBe("Unpin");
    if (d.action.kind !== "sheet") throw new Error("expected a sheet");
    expect(d.action.title).toBe(UNPIN_SHEET_TITLE());
    expect(d.action.options.map((o) => [o.label, o.scope, o.pinned])).toEqual([
      ["Unpin for me", "personal", false],
      ["Unpin for everyone", "shared", false],
    ]);
  });

  it("in a one-to-one the shared row reads Unpin for both", () => {
    expect(labels(decide({ pinnedShared: true, pinnedPersonal: true, oneToOne: true }))).toEqual([
      "Unpin for me",
      "Unpin for both",
    ]);
  });

  it("a plain member holding a message an admin pinned sees Unpin for me only, at once", () => {
    const d = decide({ pinnedShared: true, pinnedPersonal: true, maySetShared: false });
    expect(d.row).toBe("Unpin");
    expect(d.action.kind).toBe("immediate");
    expect(labels(d)).toEqual(["Unpin for me"]);
  });
});
