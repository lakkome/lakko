// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { sortConversations, type ConversationRow } from "./conversation-list";
import type { UiRules } from "./ui-rules";

/**
 * The ORDER is not tested here - it is `order_conversations` in
 * `crates/lakko-ui-rules`, shared with iOS and Android and tested there, so a
 * copy of those assertions in TypeScript would only pin the web's guess about
 * a rule it no longer owns.
 *
 * What the web owns, and what these test, is the wiring: which of its rich
 * row's fields reach the rule, how they are converted, and that the indices
 * coming back are applied to the rows going in.
 */

/** Records what the rule was handed, and answers a permutation the test picks. */
function fakeRules(order: number[]): {
  rules: UiRules;
  seen: () => readonly unknown[][];
} {
  const seen: unknown[][] = [];
  class Row {
    constructor(...args: unknown[]) {
      seen.push(args);
    }
  }
  const rules = {
    ConversationKind: { Chat: 0, Group: 1, Channel: 2 },
    ConversationRow: Row,
    orderConversations: () => Uint32Array.from(order),
  } as unknown as UiRules;
  return { rules, seen: () => seen };
}

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
  it("returns the rich rows the shared order picked, in that order", () => {
    const { rules } = fakeRules([2, 0, 1]);
    const ordered = sortConversations(
      [row({ id: "1" }), row({ id: "2" }), row({ id: "3" })],
      rules,
    );
    expect(ordered.map((r) => r.id)).toEqual(["3", "1", "2"]);
  });

  it("hands the rule the six fields it reads, and nothing else", () => {
    // The avatar, the preview and the unread count never cross the boundary:
    // the rule answers with indices so the caller reorders what it holds.
    const { rules, seen } = fakeRules([0]);
    sortConversations(
      [
        row({
          id: "x",
          kind: "channel",
          title: "Lakko news",
          preview: "not read by the rule",
          lastActivityMs: 42,
          unread: 7,
          pinned: true,
          archived: false,
        }),
      ],
      rules,
    );
    expect(seen()).toEqual([["x", 2, "Lakko news", 42n, true, false]]);
  });

  it("maps every kind onto the rule's own, so a tap still routes", () => {
    const { rules, seen } = fakeRules([0, 1, 2]);
    sortConversations(
      [row({ kind: "chat" }), row({ kind: "group" }), row({ kind: "channel" })],
      rules,
    );
    expect(seen().map((args) => args[1])).toEqual([0, 1, 2]);
  });

  it("passes 'nothing has happened yet' as absent rather than as a zero time", () => {
    // Zero is a real instant - the epoch - and the rule distinguishes the two.
    const { rules, seen } = fakeRules([0]);
    sortConversations([row({ lastActivityMs: null })], rules);
    expect(seen()[0]?.[3]).toBeUndefined();
  });

  it("does not mutate what it was given", () => {
    const { rules } = fakeRules([1, 0]);
    const input = [row({ id: "1" }), row({ id: "2" })];
    sortConversations(input, rules);
    expect(input.map((r) => r.id)).toEqual(["1", "2"]);
  });

  it("drops an index the rule could not have meant, rather than a hole in the list", () => {
    const { rules } = fakeRules([0, 9]);
    expect(sortConversations([row({ id: "1" })], rules).map((r) => r.id)).toEqual(["1"]);
  });
});
