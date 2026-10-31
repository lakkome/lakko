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

import { ChatProvider, useChats, type ChatApi } from "./ChatContext";
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
