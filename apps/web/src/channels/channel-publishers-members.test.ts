// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { publishersFromMembers } from "./channel-publishers";

describe("publishersFromMembers", () => {
  it("lists the lowest leaf as the owner, then each other account once", () => {
    const out = publishersFromMembers([
      { accountKeyB64: "B", leafIndex: 3 },
      { accountKeyB64: "A", leafIndex: 0 },
      { accountKeyB64: "B", leafIndex: 5 },
      { accountKeyB64: "C", leafIndex: 1 },
    ]);
    expect(out).toEqual([
      { accountKeyB64: "A", isOwner: true },
      { accountKeyB64: "C", isOwner: false },
      { accountKeyB64: "B", isOwner: false },
    ]);
  });

  it("is empty for an empty group", () => {
    expect(publishersFromMembers([])).toEqual([]);
  });

  it("a removed publisher drops out, so only the owner remains", () => {
    expect(publishersFromMembers([{ accountKeyB64: "A", leafIndex: 0 }])).toEqual([{ accountKeyB64: "A", isOwner: true }]);
  });
});
