// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { conversationTitle, type ConversationTitleInput } from "./conversation-title";

const ME = "me";
const ANNA = "anna";
const BORIS = "boris";

const NAMES: Record<string, string> = { me: "Me", anna: "Anna", boris: "Boris" };

function input(over: Partial<ConversationTitleInput> = {}): ConversationTitleInput {
  return {
    kind: "chat",
    memberKeysB64: [ME, ANNA],
    ownKeyB64: ME,
    fallbackLabel: "fallback",
    sharedName: undefined,
    contactNameById: undefined,
    contactNameByMember: undefined,
    nameOf: (k) => NAMES[k] ?? k,
    groupTitle: (name, members) => (name.length > 0 ? name : members.join(", ")),
    ...over,
  };
}

describe("a one-to-one chat", () => {
  it("is called after the other person, read from the roster", () => {
    // Live from the roster rather than from how the chat was created, so it
    // is right however it came to exist.
    expect(conversationTitle(input())).toBe("Anna");
  });

  it("does not name itself after us", () => {
    expect(conversationTitle(input())).not.toBe("Me");
  });

  it("falls back to a name the user typed when the peer cannot be resolved", () => {
    // A chat whose roster we cannot read yet: the contact list still knows
    // who this is.
    expect(
      conversationTitle(
        input({ memberKeysB64: [ME], contactNameById: "Anna at work" }),
      ),
    ).toBe("Anna at work");
  });

  it("ignores an empty contact name rather than showing a blank row", () => {
    expect(
      conversationTitle(input({ memberKeysB64: [ME], contactNameById: "" })),
    ).toBe("fallback");
  });

  it("finds the contact by its members when the id does not match", () => {
    expect(
      conversationTitle(
        input({ memberKeysB64: [ME], contactNameByMember: "Anna by key" }),
      ),
    ).toBe("Anna by key");
  });
});

describe("a group", () => {
  it("uses the shared name once one has arrived", () => {
    expect(
      conversationTitle(
        input({ kind: "group", memberKeysB64: [ME, ANNA, BORIS], sharedName: "Climbing" }),
      ),
    ).toBe("Climbing");
  });

  it("is described by its members when it has no name, never by an id", () => {
    expect(
      conversationTitle(
        input({
          kind: "group",
          memberKeysB64: [ME, ANNA, BORIS],
          fallbackLabel: "",
        }),
      ),
    ).toBe("Anna, Boris");
  });

  it("does not take a contact's name, even with two members", () => {
    // A two-person GROUP is not a one-to-one chat: naming it after the other
    // person would hide that it is a group someone could be added to.
    expect(
      conversationTitle(
        input({
          kind: "group",
          memberKeysB64: [ME, ANNA],
          contactNameById: "Anna at work",
          sharedName: "Just us",
        }),
      ),
    ).toBe("Just us");
  });

  it("prefers the shared name over the store's own label", () => {
    expect(
      conversationTitle(
        input({ kind: "group", sharedName: "Shared", fallbackLabel: "local" }),
      ),
    ).toBe("Shared");
  });

  it("treats an absent shared name as absent whether it is null or undefined", () => {
    for (const sharedName of [null, undefined]) {
      expect(
        conversationTitle(input({ kind: "group", sharedName, fallbackLabel: "local" })),
      ).toBe("local");
    }
  });
});
