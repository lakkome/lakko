// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §6.2.4 — the removal's order.
 *
 * The one property worth pinning is the one the module's own comment states:
 * the registry row is dropped LAST, so a failure before it leaves the chat
 * reachable. Everything it calls is a store with its own tests; here each is
 * replaced by a recorder so the sequence itself is the thing asserted. No
 * IndexedDB in the node test env, which is also why the key store and the
 * message log are stubbed rather than driven.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const calls: string[] = [];
const state = {
  deviceKey: { fake: "key" } as unknown as CryptoKey | null,
  logFails: false,
};

vi.mock("../auth/key-store", () => ({
  getDeviceKey: async (accountIdB64: string) => {
    calls.push(`getDeviceKey ${accountIdB64}`);
    return state.deviceKey;
  },
}));
vi.mock("./message-log-store", () => ({
  deleteGroupFromMessageLog: async (
    accountIdB64: string,
    _key: CryptoKey,
    groupIdHex: string,
  ) => {
    calls.push(`deleteGroupFromMessageLog ${accountIdB64} ${groupIdHex}`);
    if (state.logFails) throw new Error("log write failed");
    return true;
  },
}));
vi.mock("./pending-requests", () => ({
  pruneObservations: (a: string, g: string, live: readonly string[]) => {
    calls.push(`pruneObservations ${a} ${g} [${live.join(",")}]`);
  },
}));
vi.mock("./unapplied-requests", () => ({
  clearUnappliedForGroup: (a: string, g: string) => {
    calls.push(`clearUnappliedForGroup ${a} ${g}`);
  },
}));
vi.mock("./membership-ended", () => ({
  clearLeaveRequested: (a: string, g: string) => {
    calls.push(`clearLeaveRequested ${a} ${g}`);
  },
}));
vi.mock("./membership-retry", () => ({
  retryScopeKey: (a: string, g: string) => `${a} ${g}`,
  clearArmedRetries: (scope: string) => {
    calls.push(`clearArmedRetries ${scope}`);
  },
  clearOwnRequestsForGroup: (a: string, g: string) => {
    calls.push(`clearOwnRequestsForGroup ${a} ${g}`);
  },
}));
vi.mock("../backup/restored-known-groups", () => ({
  dropRestoredKnownGroup: (a: string, g: string) => {
    calls.push(`dropRestoredKnownGroup ${a} ${g}`);
  },
}));

import { deleteChat } from "./delete-chat";

const ACCOUNT = "acct==";
const GROUP_B64 = "grp==";
const GROUP_HEX = "6772";

function registry(): { remove: (g: string) => void } {
  return {
    remove: (g) => {
      calls.push(`chats.remove ${g}`);
    },
  };
}

beforeEach(() => {
  calls.length = 0;
  state.deviceKey = { fake: "key" } as unknown as CryptoKey;
  state.logFails = false;
});

describe("deleteChat", () => {
  it("touches every store in the documented order and drops the registry row last", async () => {
    await deleteChat({
      accountIdB64: ACCOUNT,
      groupIdB64: GROUP_B64,
      groupIdHex: GROUP_HEX,
      chats: registry(),
    });
    expect(calls).toEqual([
      `getDeviceKey ${ACCOUNT}`,
      `deleteGroupFromMessageLog ${ACCOUNT} ${GROUP_HEX}`,
      `pruneObservations ${ACCOUNT} ${GROUP_B64} []`,
      `clearUnappliedForGroup ${ACCOUNT} ${GROUP_B64}`,
      `clearOwnRequestsForGroup ${ACCOUNT} ${GROUP_B64}`,
      `clearLeaveRequested ${ACCOUNT} ${GROUP_B64}`,
      `clearArmedRetries ${ACCOUNT} ${GROUP_B64}`,
      // The account backup stops listing a chat the person deleted.
      `dropRestoredKnownGroup ${ACCOUNT} ${GROUP_B64}`,
      `chats.remove ${GROUP_B64}`,
    ]);
  });

  it("a failed message-log removal propagates and leaves the chat reachable", async () => {
    state.logFails = true;
    await expect(
      deleteChat({
        accountIdB64: ACCOUNT,
        groupIdB64: GROUP_B64,
        groupIdHex: GROUP_HEX,
        chats: registry(),
      }),
    ).rejects.toThrow("log write failed");
    // Nothing after the log ran, and the registry row in particular is
    // still there — the transcript is not orphaned behind a missing row.
    expect(calls).toEqual([
      `getDeviceKey ${ACCOUNT}`,
      `deleteGroupFromMessageLog ${ACCOUNT} ${GROUP_HEX}`,
    ]);
  });

  it("with no device key the message log is left alone and the rest still runs", async () => {
    state.deviceKey = null;
    await deleteChat({
      accountIdB64: ACCOUNT,
      groupIdB64: GROUP_B64,
      groupIdHex: GROUP_HEX,
      chats: registry(),
    });
    expect(calls[0]).toBe(`getDeviceKey ${ACCOUNT}`);
    expect(calls.some((c) => c.startsWith("deleteGroupFromMessageLog"))).toBe(false);
    expect(calls[calls.length - 1]).toBe(`chats.remove ${GROUP_B64}`);
  });
});

describe("deleting a whole conversation (ADR-0073 R.5)", () => {
  const seg = (id: string) => ({ groupIdB64: id, groupIdHex: `hex-${id}` });

  it("orders every held segment before the head, each once", async () => {
    const { conversationDeleteOrder } = await import("./delete-chat");
    expect(
      conversationDeleteOrder([seg("old"), seg("head"), seg("beyond"), seg("old")], "head").map(
        (s) => s.groupIdB64,
      ),
    ).toEqual(["old", "beyond", "head"]);
    // A head the device does not hold: whatever is held still goes.
    expect(conversationDeleteOrder([seg("old")], "head").map((s) => s.groupIdB64)).toEqual(["old"]);
  });

  it("removes every segment's log and registry row, the head's row last", async () => {
    const { deleteConversation } = await import("./delete-chat");
    calls.length = 0;
    state.logFails = false;
    const removed: string[] = [];
    const after: string[] = [];
    await deleteConversation({
      accountIdB64: "acct",
      segments: [seg("old"), seg("head")],
      headGroupIdB64: "head",
      chats: { remove: (g) => removed.push(g) },
      afterSegment: (s) => {
        after.push(s.groupIdB64);
      },
    });
    expect(removed).toEqual(["old", "head"]);
    expect(after).toEqual(["old", "head"]);
    expect(calls.filter((c) => c.startsWith("deleteGroupFromMessageLog"))).toEqual([
      "deleteGroupFromMessageLog acct hex-old",
      "deleteGroupFromMessageLog acct hex-head",
    ]);
  });

  it("a failure leaves the head (the conversation's row) in place", async () => {
    const { deleteConversation } = await import("./delete-chat");
    state.logFails = true;
    const removed: string[] = [];
    await expect(
      deleteConversation({
        accountIdB64: "acct",
        segments: [seg("old"), seg("head")],
        headGroupIdB64: "head",
        chats: { remove: (g) => removed.push(g) },
      }),
    ).rejects.toThrow();
    expect(removed).toEqual([]);
    state.logFails = false;
  });
});
