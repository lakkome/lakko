// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0065 R.12 - `call-continuity.ts`. */
import { describe, expect, it } from "vitest";

import { oneToOneCallEndsOnMove, oneToOneCallTarget } from "./call-continuity";

describe("oneToOneCallEndsOnMove", () => {
  it("nothing ends when no one-to-one call is live", () => {
    expect(
      oneToOneCallEndsOnMove({ callGroupIdB64: null, unrepairableKeyB64: "old", headGroupIdB64: "S" }),
    ).toBe(false);
  });
  it("ends the moment the other person's reset is accepted", () => {
    expect(
      oneToOneCallEndsOnMove({ callGroupIdB64: "G", unrepairableKeyB64: "old", headGroupIdB64: "G" }),
    ).toBe(true);
  });
  it("ends when the conversation's current group is no longer the call's", () => {
    expect(
      oneToOneCallEndsOnMove({ callGroupIdB64: "G", unrepairableKeyB64: null, headGroupIdB64: "S" }),
    ).toBe(true);
  });
  it("an ordinary call, and an unknown head, go on", () => {
    expect(
      oneToOneCallEndsOnMove({ callGroupIdB64: "G", unrepairableKeyB64: null, headGroupIdB64: "G" }),
    ).toBe(false);
    expect(
      oneToOneCallEndsOnMove({ callGroupIdB64: "G", unrepairableKeyB64: null, headGroupIdB64: null }),
    ).toBe(false);
  });
});

describe("oneToOneCallTarget", () => {
  const base = {
    groupIdB64: "G",
    unrepairableKeyB64: null,
    headGroupIdB64: "G",
    headUnrepairableKeyB64: null,
  };
  it("a live one-to-one dials in its own group", () => {
    expect(oneToOneCallTarget(base)).toEqual({ kind: "dial", groupIdB64: "G" });
    expect(oneToOneCallTarget({ ...base, headGroupIdB64: null })).toEqual({ kind: "dial", groupIdB64: "G" });
  });
  it("a dead session with a successor dials in the successor", () => {
    expect(
      oneToOneCallTarget({ ...base, unrepairableKeyB64: "old", headGroupIdB64: "S" }),
    ).toEqual({ kind: "dial", groupIdB64: "S" });
  });
  it("a dead session with no successor does not start: nothing is signalled into it", () => {
    expect(oneToOneCallTarget({ ...base, unrepairableKeyB64: "old" })).toEqual({ kind: "unavailable" });
    expect(
      oneToOneCallTarget({ ...base, unrepairableKeyB64: "old", headGroupIdB64: null }),
    ).toEqual({ kind: "unavailable" });
  });
  it("a head that is itself dead is not dialled", () => {
    expect(
      oneToOneCallTarget({
        ...base,
        unrepairableKeyB64: "old",
        headGroupIdB64: "S",
        headUnrepairableKeyB64: "newer-old",
      }),
    ).toEqual({ kind: "unavailable" });
  });
  it("a view on an older segment after the move dials where the conversation is", () => {
    expect(oneToOneCallTarget({ ...base, headGroupIdB64: "S" })).toEqual({ kind: "dial", groupIdB64: "S" });
  });
});
