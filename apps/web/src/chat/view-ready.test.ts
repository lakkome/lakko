// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { chatListReady, conversationReady, namesKnown } from "./view-ready";

const read = {
  account: "a",
  profilesLoadedFor: "a",
  groupNamesLoadedFor: "a",
} as const;

describe("namesKnown (navigation.md 2.4)", () => {
  it("is true once both name stores are read for the signed-in account", () => {
    expect(namesKnown(read)).toBe(true);
  });

  it("is false with no session", () => {
    expect(namesKnown({ ...read, account: null })).toBe(false);
  });

  it("is false while either store is unread", () => {
    expect(namesKnown({ ...read, profilesLoadedFor: null })).toBe(false);
    expect(namesKnown({ ...read, groupNamesLoadedFor: null })).toBe(false);
  });

  it("is false when a store was read for another account", () => {
    expect(namesKnown({ ...read, profilesLoadedFor: "b" })).toBe(false);
    expect(namesKnown({ ...read, groupNamesLoadedFor: "b" })).toBe(false);
  });
});

describe("chatListReady", () => {
  it("waits for the names, the message log and the channels", () => {
    expect(chatListReady({ ...read, logRead: true, channelsRead: true })).toBe(true);
    expect(chatListReady({ ...read, logRead: false, channelsRead: true })).toBe(false);
    expect(chatListReady({ ...read, logRead: true, channelsRead: false })).toBe(false);
    expect(chatListReady({ ...read, profilesLoadedFor: null, logRead: true, channelsRead: true })).toBe(false);
  });
});

describe("conversationReady", () => {
  it("waits for the names and the conversation's stored messages", () => {
    expect(conversationReady({ ...read, logRead: true })).toBe(true);
    expect(conversationReady({ ...read, logRead: false })).toBe(false);
    expect(conversationReady({ ...read, groupNamesLoadedFor: null, logRead: true })).toBe(false);
  });
});
