// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  conversationMatches,
  mergeConversations,
  sectionConversations,
  sortConversations,
  type ConversationRow,
} from "./conversation-list";

function row(over: Partial<ConversationRow> = {}): ConversationRow {
  return {
    id: "a",
    kind: "chat",
    title: "Anna",
    preview: "see you then",
    lastActivityMs: 1_000,
    unread: 0,
    pinned: false,
    muted: false,
    archived: false,
    ...over,
  };
}

describe("sortConversations", () => {
  it("puts the newest conversation on top, whatever kind it is", () => {
    // The whole point of merging: a list sorted by WHEN cannot be three
    // lists, and the newest thing that happened does not care which
    // container it arrived in.
    const ordered = sortConversations([
      row({ id: "1", kind: "chat", title: "Anna", lastActivityMs: 10 }),
      row({ id: "2", kind: "channel", title: "Lakko news", lastActivityMs: 30 }),
      row({ id: "3", kind: "group", title: "Climbing", lastActivityMs: 20 }),
    ]);
    expect(ordered.map((r) => r.id)).toEqual(["2", "3", "1"]);
  });

  it("pins above everything, including something newer", () => {
    const ordered = sortConversations([
      row({ id: "new", lastActivityMs: 999 }),
      row({ id: "pinned", lastActivityMs: 1, pinned: true }),
    ]);
    expect(ordered[0]?.id).toBe("pinned");
  });

  it("sinks archived below everything, including something newer", () => {
    const ordered = sortConversations([
      row({ id: "archived", lastActivityMs: 999, archived: true }),
      row({ id: "live", lastActivityMs: 1 }),
    ]);
    expect(ordered.map((r) => r.id)).toEqual(["live", "archived"]);
  });

  it("keeps a pinned archived conversation with the pinned ones", () => {
    // Pinning is an explicit act and archiving is a tidy-up; the explicit
    // one wins, or pinning something then archiving it would look like the
    // pin was ignored.
    const ordered = sortConversations([
      row({ id: "live", lastActivityMs: 500 }),
      row({ id: "both", lastActivityMs: 1, pinned: true, archived: true }),
    ]);
    expect(ordered[0]?.id).toBe("both");
  });

  it("treats a conversation nothing has happened in as the oldest", () => {
    const ordered = sortConversations([
      row({ id: "empty", lastActivityMs: null, title: "Zoe" }),
      row({ id: "old", lastActivityMs: 1, title: "Anna" }),
    ]);
    expect(ordered.map((r) => r.id)).toEqual(["old", "empty"]);
  });

  it("orders two empty conversations by title, so the list does not reshuffle", () => {
    // Both are `null`, so without a tiebreak they would sit in whatever
    // order the stores happened to yield and a refresh would look like a bug.
    const a = row({ id: "1", title: "Zoe", lastActivityMs: null });
    const b = row({ id: "2", title: "Anna", lastActivityMs: null });
    expect(sortConversations([a, b]).map((r) => r.id)).toEqual(["2", "1"]);
    expect(sortConversations([b, a]).map((r) => r.id)).toEqual(["2", "1"]);
  });

  it("does not mutate what it was given", () => {
    const input = [row({ id: "1", lastActivityMs: 1 }), row({ id: "2", lastActivityMs: 2 })];
    sortConversations(input);
    expect(input.map((r) => r.id)).toEqual(["1", "2"]);
  });
});

describe("mergeConversations", () => {
  it("interleaves the sources by time rather than concatenating them", () => {
    const chats = [row({ id: "c", kind: "chat", lastActivityMs: 5 })];
    const groups = [row({ id: "g", kind: "group", lastActivityMs: 15 })];
    const channels = [row({ id: "n", kind: "channel", lastActivityMs: 10 })];
    expect(mergeConversations(chats, groups, channels).map((r) => r.id)).toEqual([
      "g",
      "n",
      "c",
    ]);
  });

  it("copes with a source that is empty", () => {
    expect(mergeConversations([], [row({ id: "only" })], []).map((r) => r.id)).toEqual([
      "only",
    ]);
  });
});

describe("sectionConversations", () => {
  const ordered = sortConversations([
    row({ id: "live1", lastActivityMs: 9 }),
    row({ id: "live2", lastActivityMs: 8 }),
    ...Array.from({ length: 6 }, (_, i) =>
      row({ id: `arch${i}`, lastActivityMs: i, archived: true }),
    ),
  ]);

  it("splits live from archived without reordering either", () => {
    const s = sectionConversations(ordered, false);
    expect(s.live.map((r) => r.id)).toEqual(["live1", "live2"]);
    expect(s.archived).toHaveLength(6);
    expect(s.archived[0]?.id).toBe("arch5");
  });

  it("keeps archived inline on a wide layout however many there are", () => {
    expect(sectionConversations(ordered, false).collapseArchived).toBe(false);
  });

  it("collapses archived on a narrow layout once there are more than a handful", () => {
    expect(sectionConversations(ordered, true).collapseArchived).toBe(true);
  });

  it("leaves a handful of archived inline even when narrow", () => {
    const few = sortConversations([
      row({ id: "live", lastActivityMs: 9 }),
      row({ id: "arch", lastActivityMs: 1, archived: true }),
    ]);
    expect(sectionConversations(few, true).collapseArchived).toBe(false);
  });
});

describe("conversationMatches", () => {
  it("matches part of a title, because this runs on data we already hold", () => {
    expect(conversationMatches(row({ title: "Climbing club" }), "climb")).toBe(true);
  });

  it("matches the preview too", () => {
    expect(conversationMatches(row({ preview: "see you then" }), "you the")).toBe(true);
  });

  it("ignores case and accents", () => {
    expect(conversationMatches(row({ title: "Renée" }), "renee")).toBe(true);
    expect(conversationMatches(row({ title: "Renee" }), "RENÉE")).toBe(true);
  });

  it("matches everything on an empty query", () => {
    expect(conversationMatches(row(), "   ")).toBe(true);
  });

  it("does not match what is not there", () => {
    expect(conversationMatches(row({ title: "Anna", preview: "hi" }), "boris")).toBe(false);
  });
});
