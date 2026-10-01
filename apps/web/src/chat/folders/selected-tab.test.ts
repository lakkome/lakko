// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { ALL_CHATS, resolveTab } from "./selected-tab";

describe("which tab opens (UX 2.5)", () => {
  it("the stored folder, while it exists", () => {
    expect(resolveTab("aa", ["aa", "bb"])).toBe("aa");
  });

  it("All chats when that folder was deleted, or nothing is stored", () => {
    expect(resolveTab("cc", ["aa"])).toBe(ALL_CHATS);
    expect(resolveTab(null, ["aa"])).toBe(ALL_CHATS);
    expect(resolveTab(ALL_CHATS, [])).toBe(ALL_CHATS);
  });
});
