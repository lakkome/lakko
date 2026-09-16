// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeEach, describe, expect, it } from "vitest";

import {
  belongsToList,
  conversationKindOf,
  decideConversationKind,
  nextPersistedKind,
} from "./conversation-kind";
import {
  observeConversationKind,
  persistedConversationKind,
  recordConversationKind,
  subscribeConversationKindChanged,
} from "./conversation-kind-store";

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

describe("decideConversationKind (the persisted rule)", () => {
  it("derives from the people count at first sight", () => {
    expect(decideConversationKind(null, 2)).toBe("chat");
    expect(decideConversationKind(null, 3)).toBe("group");
    expect(decideConversationKind(null, 1)).toBe("group");
  });

  it("a group created with two people stays a group", () => {
    expect(decideConversationKind("group", 2)).toBe("group");
    expect(decideConversationKind("group", 1)).toBe("group");
  });

  it("a chat becomes a group once a third person is observed", () => {
    expect(decideConversationKind("chat", 3)).toBe("group");
    expect(decideConversationKind("chat", 7)).toBe("group");
  });

  it("a chat never leaves the Chats list for a smaller or torn roster", () => {
    expect(decideConversationKind("chat", 2)).toBe("chat");
    expect(decideConversationKind("chat", 1)).toBe("chat");
    expect(decideConversationKind("chat", 0)).toBe("chat");
  });

  it("a group never becomes a chat", () => {
    expect(nextPersistedKind("group", "chat")).toBe("group");
    expect(nextPersistedKind("chat", "group")).toBe("group");
    expect(nextPersistedKind(null, "chat")).toBe("chat");
    expect(nextPersistedKind("chat", "chat")).toBe("chat");
  });
});

describe("conversation-kind-store", () => {
  const ACCOUNT = "acct-A";
  const OTHER = "acct-B";
  const G = "group-1";

  beforeEach(() => {
    const backing = new Map<string, string>();
    (globalThis as { localStorage?: Storage }).localStorage = {
      getItem: (k: string) => backing.get(k) ?? null,
      setItem: (k: string, v: string) => {
        backing.set(k, v);
      },
      removeItem: (k: string) => {
        backing.delete(k);
      },
      clear: () => backing.clear(),
      key: (i: number) => [...backing.keys()][i] ?? null,
      get length() {
        return backing.size;
      },
    } as Storage;
  });

  it("is empty until something is recorded, and keyed per account", () => {
    expect(persistedConversationKind(ACCOUNT, G)).toBeNull();
    recordConversationKind(ACCOUNT, G, "group");
    expect(persistedConversationKind(ACCOUNT, G)).toBe("group");
    expect(persistedConversationKind(OTHER, G)).toBeNull();
  });

  it("first sight derives from the count and persists it", () => {
    expect(observeConversationKind(ACCOUNT, G, 2)).toBe("chat");
    expect(persistedConversationKind(ACCOUNT, G)).toBe("chat");
    // A later smaller read does not move it.
    expect(observeConversationKind(ACCOUNT, G, 1)).toBe("chat");
    expect(persistedConversationKind(ACCOUNT, G)).toBe("chat");
  });

  it("a torn first read records nothing", () => {
    expect(observeConversationKind(ACCOUNT, G, 0)).toBe("group");
    expect(persistedConversationKind(ACCOUNT, G)).toBeNull();
    expect(observeConversationKind(ACCOUNT, G, 2)).toBe("chat");
  });

  it("flips chat -> group when a third person is observed, and never back", () => {
    recordConversationKind(ACCOUNT, G, "chat");
    expect(observeConversationKind(ACCOUNT, G, 3)).toBe("group");
    expect(persistedConversationKind(ACCOUNT, G)).toBe("group");
    expect(observeConversationKind(ACCOUNT, G, 2)).toBe("group");
    recordConversationKind(ACCOUNT, G, "chat");
    expect(persistedConversationKind(ACCOUNT, G)).toBe("group");
  });

  it("notifies same-page listeners only when a kind actually changes", () => {
    const seen: string[] = [];
    const unsubscribe = subscribeConversationKindChanged((a) => seen.push(a));
    recordConversationKind(ACCOUNT, G, "chat");
    recordConversationKind(ACCOUNT, G, "chat");
    observeConversationKind(ACCOUNT, G, 2);
    expect(seen).toEqual([ACCOUNT]);
    observeConversationKind(ACCOUNT, G, 3);
    expect(seen).toEqual([ACCOUNT, ACCOUNT]);
    unsubscribe();
    recordConversationKind(OTHER, G, "group");
    expect(seen).toEqual([ACCOUNT, ACCOUNT]);
  });
});
