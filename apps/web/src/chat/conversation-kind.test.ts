// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { belongsToList, conversationKindOf } from "./conversation-kind";

describe("conversationKindOf", () => {
  it("two people is a chat", () => {
    expect(conversationKindOf(2)).toBe("chat");
  });

  it("three or more people is a group", () => {
    expect(conversationKindOf(3)).toBe("group");
    expect(conversationKindOf(12)).toBe("group");
  });

  it("a conversation with only the reader in it is a group, not a chat", () => {
    // A 1x1 admits the peer in the same commit that creates it, so one
    // person means a group whose members have not landed yet.
    expect(conversationKindOf(1)).toBe("group");
  });

  it("a torn roster (zero people) is not shown as a chat", () => {
    expect(conversationKindOf(0)).toBe("group");
  });
});

describe("belongsToList", () => {
  it("splits the store into two disjoint lists", () => {
    for (const n of [0, 1, 2, 3, 4, 50]) {
      expect(belongsToList(n, "chat") !== belongsToList(n, "group")).toBe(true);
    }
  });

  it("a chat never appears in the Groups list", () => {
    expect(belongsToList(2, "group")).toBe(false);
    expect(belongsToList(2, "chat")).toBe(true);
  });
});
