// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0052 decision 2 / `docs/ux/message-actions.md` section 5.1 — which
 * conversations the "Forward to" picker offers, and the search field.
 *
 * The ORDER is the shared rule in `crates/lakko-ui-rules`, tested there; what
 * is asserted here is the web's own wiring - which candidates reach the rule,
 * that the rule's answer is applied, and the two filters (live memberships,
 * no channels) the picker adds over the Chats list.
 */
import { describe, expect, it } from "vitest";

import { filterForwardRows, forwardPickerRows, type ForwardCandidate } from "./forward-picker";
import type { UiRules } from "./ui-rules";

/** The shared rule stood in for: records the rows it was handed, answers
 *  the identity order (or one the test picks). */
function fakeRules(order?: number[]): { rules: UiRules; seen: () => readonly unknown[][] } {
  const seen: unknown[][] = [];
  class Row {
    constructor(...args: unknown[]) {
      seen.push(args);
    }
  }
  const rules = {
    ConversationKind: { Chat: 0, Group: 1, Channel: 2 },
    ConversationRow: Row,
    orderConversations: (rows: unknown[]) =>
      Uint32Array.from(order ?? rows.map((_, i) => i)),
  } as unknown as UiRules;
  return { rules, seen: () => seen };
}

function candidate(over: Partial<ForwardCandidate> = {}): ForwardCandidate {
  return {
    groupIdB64: "a",
    kind: "chat",
    title: "Anna",
    sub: "see you then",
    lastActivityMs: 1_000,
    pinned: false,
    muted: false,
    archived: false,
    live: true,
    ...over,
  };
}

describe("forwardPickerRows", () => {
  it("offers live memberships only", () => {
    const { rules } = fakeRules();
    const rows = forwardPickerRows(
      [
        candidate({ groupIdB64: "live" }),
        candidate({ groupIdB64: "ended", live: false }),
        candidate({ groupIdB64: "unknown", live: undefined }),
      ],
      rules,
    );
    expect(rows.map((r) => r.id)).toEqual(["live"]);
  });

  it("hands the rule the same six fields the Chats list does, and applies its order", () => {
    const { rules, seen } = fakeRules([1, 0]);
    const rows = forwardPickerRows(
      [
        candidate({ groupIdB64: "x", kind: "group", title: "Team", pinned: true, lastActivityMs: 5 }),
        candidate({ groupIdB64: "y", title: "Anna", archived: true, lastActivityMs: null }),
      ],
      rules,
    );
    expect(seen()).toEqual([
      ["x", 1, "Team", 5n, true, false],
      ["y", 0, "Anna", undefined, false, true],
    ]);
    expect(rows.map((r) => r.id)).toEqual(["y", "x"]);
  });

  it("carries the title, the second line and the flags onto the row", () => {
    const { rules } = fakeRules();
    const [row] = forwardPickerRows(
      [candidate({ title: "Anna", sub: "You: ok", pinned: true, muted: true })],
      rules,
    );
    expect(row).toEqual({
      id: "a",
      kind: "chat",
      title: "Anna",
      preview: "You: ok",
      lastActivityMs: 1_000,
      unread: 0,
      pinned: true,
      muted: true,
      archived: false,
    });
  });

  it("hands the rule the Chats list's pin order, as the picker's own row ids", () => {
    let pinOrder: unknown = null;
    const rules = {
      ConversationKind: { Chat: 0, Group: 1, Channel: 2 },
      ConversationRow: class {},
      orderConversations: (rows: unknown[], order: unknown) => {
        pinOrder = order;
        return Uint32Array.from(rows.map((_, i) => i));
      },
    } as unknown as UiRules;
    forwardPickerRows(
      [
        candidate({ groupIdB64: "head-a", conversationHex: "aa", pinned: true }),
        candidate({ groupIdB64: "head-b", conversationHex: "BB", pinned: true }),
      ],
      rules,
      ["bb", "cc", "AA"],
    );
    // "cc" names no conversation this device offers; the rest map to rows.
    expect(pinOrder).toEqual(["head-b", "head-a"]);
  });

  it("is empty when nothing is live", () => {
    expect(forwardPickerRows([candidate({ live: false })], fakeRules().rules)).toEqual([]);
  });
});

describe("filterForwardRows", () => {
  const { rules } = fakeRules();
  const rows = forwardPickerRows(
    [
      candidate({ groupIdB64: "1", title: "Anna", sub: "see you then" }),
      candidate({ groupIdB64: "2", title: "Team", sub: "Boris: shipped" }),
    ],
    rules,
  );

  it("keeps every row for an empty or blank query", () => {
    expect(filterForwardRows(rows, "")).toBe(rows);
    expect(filterForwardRows(rows, "   ").map((r) => r.id)).toEqual(["1", "2"]);
  });

  it("matches the title or the second line, loosely", () => {
    expect(filterForwardRows(rows, "ann").map((r) => r.id)).toEqual(["1"]);
    expect(filterForwardRows(rows, "SHIPPED").map((r) => r.id)).toEqual(["2"]);
    expect(filterForwardRows(rows, "nobody")).toEqual([]);
  });
});
