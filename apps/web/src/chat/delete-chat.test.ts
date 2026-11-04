// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `Delete this chat`, the order.
 *
 * The one property worth pinning here is the one the module's own comment
 * states: the MLS group and then the registry row go LAST, so a failure before
 * them leaves the chat reachable. What the list holds is pinned by
 * `membership-wipe.test.ts`; here it is replaced by a recorder so the sequence
 * itself is the thing asserted.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const calls: string[] = [];
const state = { failedSteps: [] as string[] };

vi.mock("./membership-wipe", () => ({
  groupWipeSteps: (target: { groupIdB64: string; groupIdHex: string; conversationHex: string }) => [
    { name: `list ${target.groupIdB64} ${target.groupIdHex} ${target.conversationHex}`, run: () => undefined },
  ],
  runWipeSteps: async (steps: readonly { name: string }[]) => {
    for (const s of steps) calls.push(s.name);
    return state.failedSteps;
  },
}));

import { conversationDeleteOrder, deleteChat, deleteConversation } from "./delete-chat";
import type { GroupWipeContexts } from "./membership-wipe";

const CONTEXTS = {} as GroupWipeContexts;

function input(): Parameters<typeof deleteChat>[0] {
  return {
    accountIdB64: "acct",
    groupIdB64: "grp",
    groupIdHex: "67",
    conversationHex: "root",
    contexts: CONTEXTS,
    chats: {
      remove: (g) => {
        calls.push(`chats.remove ${g}`);
      },
    },
    deleteGroup: async (g) => {
      calls.push(`deleteGroup ${g}`);
    },
  };
}

beforeEach(() => {
  calls.length = 0;
  state.failedSteps = [];
});

describe("deleteChat", () => {
  it("runs the list, then deletes the MLS group, and drops the registry row last", async () => {
    await deleteChat(input());
    expect(calls).toEqual(["list grp 67 root", "deleteGroup grp", "chats.remove grp"]);
  });

  it("a store that could not be cleared leaves the chat reachable", async () => {
    state.failedSteps = ["message-log"];
    await expect(deleteChat(input())).rejects.toThrow("message-log");
    expect(calls).toEqual(["list grp 67 root"]);
  });

  it("an MLS delete that failed leaves the row", async () => {
    await expect(
      deleteChat({
        ...input(),
        deleteGroup: async () => {
          throw new Error("refused");
        },
      }),
    ).rejects.toThrow("refused");
    expect(calls.some((c) => c.startsWith("chats.remove"))).toBe(false);
  });
});

describe("deleting a whole conversation (ADR-0073 R.5)", () => {
  const seg = (id: string) => ({ groupIdB64: id, groupIdHex: `hex-${id}` });

  it("orders every held segment before the head, each once", () => {
    expect(
      conversationDeleteOrder([seg("old"), seg("head"), seg("beyond"), seg("old")], "head").map(
        (s) => s.groupIdB64,
      ),
    ).toEqual(["old", "beyond", "head"]);
    // A head the device does not hold: whatever is held still goes.
    expect(conversationDeleteOrder([seg("old")], "head").map((s) => s.groupIdB64)).toEqual(["old"]);
  });

  it("removes every segment, the head's row last, keyed on the one conversation", async () => {
    const removed: string[] = [];
    await deleteConversation({
      accountIdB64: "acct",
      segments: [seg("head"), seg("old")],
      headGroupIdB64: "head",
      conversationHex: "root",
      contexts: CONTEXTS,
      deleteGroup: async () => undefined,
      chats: { remove: (g) => removed.push(g) },
    });
    expect(removed).toEqual(["old", "head"]);
    expect(calls).toEqual(["list old hex-old root", "list head hex-head root"]);
  });

  it("a failure leaves the head (the conversation's row) in place", async () => {
    state.failedSteps = ["message-log"];
    const removed: string[] = [];
    await expect(
      deleteConversation({
        accountIdB64: "acct",
        segments: [seg("old"), seg("head")],
        headGroupIdB64: "head",
        conversationHex: "root",
        contexts: CONTEXTS,
        deleteGroup: async () => undefined,
        chats: { remove: (g) => removed.push(g) },
      }),
    ).rejects.toThrow();
    expect(removed).toEqual([]);
  });
});
