// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// @vitest-environment jsdom

/**
 * ADR-0087 section 9 — after a join, the old handle is replaced.
 *
 * A Welcome makes a new conversation under the same group id. The chat view is
 * keyed on the id, so a replaced registry entry alone would leave the view
 * holding the old conversation's rows, cursor and frozen roster over the new
 * handle. The join generation is what remounts it; an ADR-0040 reset replaces
 * the entry too and must not.
 */
import { describe, expect, it, vi } from "vitest";

import { renderUi } from "../test/ui";

vi.mock("../ds/access", () => ({
  dsAccess: { registerGroupSource: () => () => undefined },
}));
vi.mock("../ds/session-access", () => ({
  clearDsAccess: () => undefined,
  installMailboxAccess: () => undefined,
  installPredecessorChainAccess: () => undefined,
}));

import { ChatProvider, joinedTimes, useChats, type ChatApi } from "./ChatContext";
import { chatViewKey, groupIdOfViewKey } from "./membership-end";

const handle = (): never => ({ groupId: new Uint8Array(32).fill(7) }) as never;

function chatApi(): ChatApi {
  let api: ChatApi | undefined;
  renderUi(() => (
    <ChatProvider>
      {(() => {
        api = useChats();
        return null;
      })()}
    </ChatProvider>
  ));
  return api as ChatApi;
}

describe("a Welcome replacing a group under the same id", () => {
  it("replaces the handle and moves the view's key", () => {
    const chats = chatApi();
    const first = handle();
    const before = chats.add({ label: "Book club", group: first });
    const second = handle();
    const after = chats.add({ label: "Joined", group: second, rejoined: true });
    expect(chats.groups()).toHaveLength(1);
    expect(chats.get(after.groupIdB64)?.group).toBe(second);
    expect(chatViewKey(after)).not.toBe(chatViewKey(before));
    expect(groupIdOfViewKey(chatViewKey(after))).toBe(after.groupIdB64);
  });

  it("a persist-abort reset replaces the entry and keeps the view mounted", () => {
    const chats = chatApi();
    const joined = chats.add({ label: "Book club", group: handle(), rejoined: true });
    const reset = chats.add({ label: "Book club", group: handle() });
    expect(chatViewKey(reset)).toBe(chatViewKey(joined));
  });
});

describe("when a group was joined", () => {
  it("a reload under the same id keeps the moment, and a Welcome replacing the group is a new one", () => {
    vi.useFakeTimers({ now: 1_000 });
    try {
      const chats = chatApi();
      const joined = chats.add({ label: "Book club", group: handle() });
      expect(joined.createdAtMs).toBe(1_000);
      vi.setSystemTime(2_000);
      expect(chats.add({ label: "Book club", group: handle() }).createdAtMs).toBe(1_000);
      vi.setSystemTime(3_000);
      expect(
        chats.add({ label: "Joined", group: handle(), rejoined: true }).createdAtMs,
      ).toBe(3_000);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a group loaded from the store keeps the stored moment, and has none when none was stored", () => {
    const chats = chatApi();
    const stored = chats.add({ label: "A", group: handle(), restored: true, joinedAtMs: 42 });
    expect(stored.createdAtMs).toBe(42);
    expect(chats.add({ label: "A", group: handle(), restored: true }).createdAtMs).toBe(0);
  });

  it("the store is given only the moments that are known", () => {
    const chats = chatApi();
    const known = chats.add({ label: "A", group: handle(), restored: true, joinedAtMs: 42 });
    expect(joinedTimes(chats.groups())).toEqual({ [known.groupIdB64]: 42 });
    chats.add({ label: "A", group: handle(), restored: true });
    expect(joinedTimes(chats.groups())).toEqual({});
  });
});
