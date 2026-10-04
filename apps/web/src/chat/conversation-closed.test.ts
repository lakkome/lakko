// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 (UX 16.4 / 16.5) — the closed conversation: when a
 * one-to-one closes, the one close state, the composer slot, the terminal
 * row, the list line, and the persisted record.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  clearConversationClosed,
  clearConversationClosedForAccount,
  closedChatListLine,
  closedComposerState,
  closedTerminalRow,
  conversationCloseKind,
  conversationClosed,
  loadConversationClosed,
  oneToOneClosesForReset,
  recordConversationClosed,
  subscribeConversationClosed,
  type ClosedRecord,
} from "./conversation-closed";

class MemoryStorage {
  private map = new Map<string, string>();
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return [...this.map.keys()][i] ?? null;
  }
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, v);
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
  clear(): void {
    this.map.clear();
  }
}

const ACCOUNT = "acct";
const GROUP = "grp";
const RECORD: ClosedRecord = {
  reason: "one-to-one-keys-reset",
  contactAccountIdB64: "anna",
  previousKeyB64: "old",
  closedAtMs: 1000,
};

beforeEach(() => {
  (globalThis as { localStorage?: unknown }).localStorage = new MemoryStorage();
});
afterEach(() => {
  delete (globalThis as { localStorage?: unknown }).localStorage;
});

describe("oneToOneClosesForReset", () => {
  const flagged = new Map([["old", "anna"]]);

  it("closes the one-to-one whose other person holds a flagged contact's OLD key", () => {
    expect(
      oneToOneClosesForReset({
        kind: "chat",
        personKeysB64: ["me", "old"],
        ownPersonKeyB64: "me",
        flaggedPreviousKeys: flagged,
      }),
    ).toEqual({ previousKeyB64: "old", contactAccountIdB64: "anna" });
  });

  it("never a group, never a torn roster, never a stranger, never the NEW key", () => {
    const base = { personKeysB64: ["me", "old"], ownPersonKeyB64: "me", flaggedPreviousKeys: flagged };
    expect(oneToOneClosesForReset({ ...base, kind: "group" })).toBeNull();
    expect(oneToOneClosesForReset({ ...base, kind: "chat", personKeysB64: undefined })).toBeNull();
    expect(oneToOneClosesForReset({ ...base, kind: "chat", personKeysB64: ["me", "x"] })).toBeNull();
    expect(oneToOneClosesForReset({ ...base, kind: "chat", personKeysB64: ["me", "new"] })).toBeNull();
  });

  it("never because of our own key", () => {
    expect(
      oneToOneClosesForReset({
        kind: "chat",
        personKeysB64: ["old", "x"],
        ownPersonKeyB64: "old",
        flaggedPreviousKeys: flagged,
      }),
    ).toBeNull();
  });
});

describe("conversationCloseKind — the one close state", () => {
  it("the membership ending wins over everything", () => {
    const close = conversationCloseKind({
      membership: { kind: "ended", left: false },
      closedRecord: RECORD,
      groupClosed: { successorGroupIdB64: "s", closedAtMs: 5n },
    });
    expect(close.kind).toBe("ended");
    expect(conversationClosed(close)).toBe(true);
  });

  it("then the one-to-one close, then the group close, else open", () => {
    expect(
      conversationCloseKind({ membership: { kind: "active" }, closedRecord: RECORD, groupClosed: undefined })
        .kind,
    ).toBe("one-to-one-keys-reset");
    const group = conversationCloseKind({
      membership: { kind: "active" },
      closedRecord: undefined,
      groupClosed: { successorGroupIdB64: "s", closedAtMs: 5n },
    });
    expect(group).toEqual({ kind: "group-closed", successorGroupIdB64: "s", closedAtMs: 5n });
    const open = conversationCloseKind({
      membership: { kind: "active" },
      closedRecord: undefined,
      groupClosed: undefined,
    });
    expect(open.kind).toBe("open");
    expect(conversationClosed(open)).toBe(false);
  });
});

describe("closedComposerState — the composer slot (16.4 / 16.5)", () => {
  const oneToOne = { kind: "one-to-one-keys-reset" as const, record: RECORD };
  const ctx = { contactUnverified: true, contactName: "Anna", joinedSuccessor: false };

  it("an open conversation keeps the composer; an ended one is the ended bar", () => {
    expect(closedComposerState({ kind: "open" }, "", ctx)).toEqual({ kind: "composer" });
    expect(
      closedComposerState({ kind: "ended", state: { kind: "ended", left: true } }, "", ctx).kind,
    ).toBe("withdrawn");
  });

  it("the closed one-to-one: the bar, `Start a new chat` disabled with the helper while unverified", () => {
    const s = closedComposerState(oneToOne, "", ctx);
    expect(s.kind).toBe("closed");
    if (s.kind !== "closed") return;
    expect(s.notice).toBe("You can no longer send messages in this chat.");
    expect(s.action).toEqual({
      label: "Start a new chat",
      enabled: false,
      helper: "Verify Anna's new safety number first.",
    });
  });

  it("enabled once verified, no helper", () => {
    const s = closedComposerState(oneToOne, "", { ...ctx, contactUnverified: false });
    if (s.kind !== "closed") throw new Error("closed expected");
    expect(s.action?.enabled).toBe(true);
    expect(s.action?.helper).toBeNull();
  });

  it("the unsent draft is kept verbatim, a blank one is not (the ended rule)", () => {
    const kept = closedComposerState(oneToOne, "  typed ", ctx);
    if (kept.kind !== "closed") throw new Error("closed expected");
    expect(kept.draft).toBe("  typed ");
    expect(kept.draftLabel).toBe("Unsent draft");
    const blank = closedComposerState(oneToOne, "   ", ctx);
    if (blank.kind !== "closed") throw new Error("closed expected");
    expect(blank.draft).toBeNull();
    expect(blank.draftLabel).toBeNull();
  });

  it("the closed group: the bar alone until this device holds the successor", () => {
    const close = { kind: "group-closed" as const, successorGroupIdB64: "s", closedAtMs: 5n };
    const before = closedComposerState(close, "", ctx);
    if (before.kind !== "closed") throw new Error("closed expected");
    expect(before.notice).toBe("This group was closed.");
    expect(before.action).toBeNull();
    const after = closedComposerState(close, "", { ...ctx, joinedSuccessor: true });
    if (after.kind !== "closed") throw new Error("closed expected");
    expect(after.action).toEqual({ label: "Open the new group", enabled: true, helper: null });
  });

  it("a close without a successor never offers the button", () => {
    const close = { kind: "group-closed" as const, successorGroupIdB64: null, closedAtMs: 5n };
    const s = closedComposerState(close, "", { ...ctx, joinedSuccessor: true });
    if (s.kind !== "closed") throw new Error("closed expected");
    expect(s.action).toBeNull();
  });
});

describe("closedTerminalRow / closedChatListLine — the words", () => {
  const names = { contactName: "Anna", closerName: "Ivan" };

  it("the one-to-one row names the contact; the group row names the closer", () => {
    expect(closedTerminalRow({ kind: "one-to-one-keys-reset", record: RECORD }, names)).toEqual({
      headline: "Anna reset their account keys. This chat is closed.",
      consequence: "The messages you already received are still here. No new ones will arrive.",
    });
    expect(
      closedTerminalRow({ kind: "group-closed", successorGroupIdB64: null, closedAtMs: 1n }, names)
        ?.headline,
    ).toBe("Ivan closed this group after the owner's account keys were reset.");
    expect(closedTerminalRow({ kind: "open" }, names)).toBeNull();
  });

  it("the list lines", () => {
    expect(closedChatListLine({ kind: "one-to-one-keys-reset", record: RECORD }, "Anna")).toBe(
      "Chat closed. Anna reset their account keys.",
    );
    expect(
      closedChatListLine({ kind: "group-closed", successorGroupIdB64: null, closedAtMs: 1n }, ""),
    ).toBe("Group closed");
    expect(closedChatListLine({ kind: "open" }, "")).toBeNull();
  });
});

describe("the persisted record", () => {
  it("round-trips, the first write wins, and the listeners are told", () => {
    let told = 0;
    const unsubscribe = subscribeConversationClosed(() => {
      told += 1;
    });
    expect(loadConversationClosed(ACCOUNT, GROUP)).toBeUndefined();
    expect(recordConversationClosed(ACCOUNT, GROUP, RECORD)).toBe(true);
    expect(recordConversationClosed(ACCOUNT, GROUP, { ...RECORD, closedAtMs: 9999 })).toBe(true);
    expect(loadConversationClosed(ACCOUNT, GROUP)).toEqual(RECORD);
    expect(told).toBe(1);
    unsubscribe();
  });

  it("a corrupt row reads as open: it must not close a chat the person can still use", () => {
    globalThis.localStorage.setItem(`lakko.conversation-closed.v1.${ACCOUNT}.${GROUP}`, "{nope");
    expect(loadConversationClosed(ACCOUNT, GROUP)).toBeUndefined();
    globalThis.localStorage.setItem(
      `lakko.conversation-closed.v1.${ACCOUNT}.${GROUP}`,
      JSON.stringify({ reason: "something-else", contactAccountIdB64: "a", previousKeyB64: "b", closedAtMs: 1 }),
    );
    expect(loadConversationClosed(ACCOUNT, GROUP)).toBeUndefined();
  });

  it("is cleared with the chat and with the account, and only that account's", () => {
    recordConversationClosed(ACCOUNT, GROUP, RECORD);
    recordConversationClosed(ACCOUNT, "other", RECORD);
    recordConversationClosed("someone-else", GROUP, RECORD);
    clearConversationClosed(ACCOUNT, GROUP);
    expect(loadConversationClosed(ACCOUNT, GROUP)).toBeUndefined();
    expect(loadConversationClosed(ACCOUNT, "other")).toEqual(RECORD);
    clearConversationClosedForAccount(ACCOUNT);
    expect(loadConversationClosed(ACCOUNT, "other")).toBeUndefined();
    expect(loadConversationClosed("someone-else", GROUP)).toEqual(RECORD);
  });
});
