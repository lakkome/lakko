// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import { conversationWithContact } from "./contact-conversation";
import { chooseStartRoute, messageAction, runMessage, type MessageDeps } from "./message-action";

type Spy = ReturnType<typeof vi.fn>;

function deps(over: Partial<MessageDeps>): MessageDeps & {
  navigate: Spy;
  setBusy: Spy;
  setError: Spy;
  setNote: Spy;
  found: Spy;
  sendInGroup: Spy;
} {
  return {
    chatHref: null,
    accountKnown: true,
    grantReaches: async () => true,
    requestWaiting: () => false,
    sharedGroups: async () => [],
    found: vi.fn(async () => "/chat/new"),
    sendInGroup: vi.fn(async () => undefined),
    navigate: vi.fn(),
    setBusy: vi.fn(),
    setError: vi.fn(),
    setNote: vi.fn(),
    stillOn: () => true,
    ...over,
  } as never;
}

describe("messageAction", () => {
  it("opens the chat when this device holds one, known account or not", () => {
    for (const accountKnown of [true, false]) {
      expect(messageAction({ hasChat: true, accountKnown })).toBe("open");
    }
  });

  it("starts something for a contact with a known account and no chat here", () => {
    expect(messageAction({ hasChat: false, accountKnown: true })).toBe("start");
  });

  it("a contact whose account is not known cannot be messaged from here", () => {
    expect(messageAction({ hasChat: false, accountKnown: false })).toBe("unavailable");
  });
});

describe("chooseStartRoute (who-can-find-me 6.4: one order everywhere)", () => {
  const groups = [
    { groupIdB64: "old", newestMs: 10 },
    { groupIdB64: "new", newestMs: 20 },
  ];

  it("1. a grant that still reaches: the one-to-one is founded", () => {
    expect(chooseStartRoute({ grantReaches: true, requestWaiting: true, sharedGroups: groups })).toEqual({ kind: "found" });
  });

  it("2. a request already waiting: nothing is sent again", () => {
    expect(chooseStartRoute({ grantReaches: false, requestWaiting: true, sharedGroups: groups })).toEqual({ kind: "waiting" });
  });

  it("3. a shared group: the one with the newest message", () => {
    expect(chooseStartRoute({ grantReaches: false, requestWaiting: false, sharedGroups: groups })).toEqual({
      kind: "group",
      groupIdB64: "new",
    });
  });

  it("4. neither: nothing is sent", () => {
    expect(chooseStartRoute({ grantReaches: false, requestWaiting: false, sharedGroups: [] })).toEqual({ kind: "cannot" });
  });
});

describe("runMessage", () => {
  it("opens an existing chat without founding or sending", async () => {
    const d = deps({ chatHref: "/chat/old" });
    await runMessage(d);
    expect(d.navigate).toHaveBeenCalledWith("/chat/old");
    expect(d.found).not.toHaveBeenCalled();
    expect(d.sendInGroup).not.toHaveBeenCalled();
  });

  it("founds the one-to-one when a grant reaches, with progress, then opens it", async () => {
    const d = deps({});
    await runMessage(d);
    expect(d.found).toHaveBeenCalledTimes(1);
    expect(d.setBusy.mock.calls).toEqual([[true], [false]]);
    expect(d.navigate).toHaveBeenCalledWith("/chat/new");
    expect(d.sendInGroup).not.toHaveBeenCalled();
  });

  it("with no grant, sends the request inside the shared group; no chat, the card says sent", async () => {
    const d = deps({ grantReaches: async () => false, sharedGroups: async () => [{ groupIdB64: "walk", newestMs: 1 }] });
    await runMessage(d);
    expect(d.sendInGroup).toHaveBeenCalledWith("walk");
    expect(d.found).not.toHaveBeenCalled();
    expect(d.navigate).not.toHaveBeenCalled();
    expect(d.setNote).toHaveBeenLastCalledWith("request-sent");
  });

  it("Message twice sends one request", async () => {
    let waiting = false;
    const sendInGroup = vi.fn(async () => {
      waiting = true;
    });
    const d = deps({
      grantReaches: async () => false,
      requestWaiting: () => waiting,
      sharedGroups: async () => [{ groupIdB64: "walk", newestMs: 1 }],
      sendInGroup,
    } as never);
    await runMessage(d);
    await runMessage(d);
    expect(sendInGroup).toHaveBeenCalledTimes(1);
    expect(d.setNote).toHaveBeenLastCalledWith("request-sent");
  });

  it("with no grant and no shared group nothing is sent and the card says so", async () => {
    const d = deps({ grantReaches: async () => false });
    await runMessage(d);
    expect(d.found).not.toHaveBeenCalled();
    expect(d.sendInGroup).not.toHaveBeenCalled();
    expect(d.setNote).toHaveBeenLastCalledWith("cannot-message");
  });

  it("a failed request shows the request error and leaves the button usable", async () => {
    const boom = new Error("offline");
    const d = deps({
      grantReaches: async () => false,
      sharedGroups: async () => [{ groupIdB64: "walk", newestMs: 1 }],
      sendInGroup: vi.fn().mockRejectedValue(boom),
    } as never);
    await runMessage(d);
    expect(d.setError).toHaveBeenLastCalledWith(boom, "request");
    expect(d.setBusy).toHaveBeenLastCalledWith(false);
  });

  it("a failed founding shows the start error; pressing again founds again", async () => {
    const boom = new Error("ds down");
    const d = deps({ found: vi.fn().mockRejectedValueOnce(boom).mockResolvedValueOnce("/chat/new") } as never);
    await runMessage(d);
    expect(d.setError).toHaveBeenLastCalledWith(boom, "start");
    expect(d.setBusy).toHaveBeenLastCalledWith(false);
    await runMessage(d);
    expect(d.found).toHaveBeenCalledTimes(2);
    expect(d.navigate).toHaveBeenCalledWith("/chat/new");
  });

  it("logs every failure, also after the screen moved to another contact", async () => {
    const boom = new Error("no grant");
    const logFailure = vi.fn();
    const d = deps({ found: vi.fn().mockRejectedValue(boom), stillOn: () => false, logFailure } as never);
    await runMessage(d);
    expect(logFailure).toHaveBeenCalledWith(boom);
    expect(d.setError).not.toHaveBeenCalledWith(boom, expect.anything());
  });

  it("does nothing for a contact whose account is not known", async () => {
    const d = deps({ accountKnown: false });
    await runMessage(d);
    expect(d.found).not.toHaveBeenCalled();
    expect(d.sendInGroup).not.toHaveBeenCalled();
  });
});

describe("Message on a contact linked to a group (seen live 2026-11-05)", () => {
  it("does not open the group", async () => {
    const group = { groupIdB64: "walk", people: ["me", "qa-ios", "third"], oneToOne: false };
    const theirChat = conversationWithContact({
      contact: { signaturePubkeyB64: "qa-ios", groupIdB64: "walk" },
      conversations: [group],
      ownKeyB64: "me",
      peopleOf: (c) => c.people,
      isOneToOne: (c) => c.oneToOne,
    });
    const d = deps({ chatHref: theirChat === undefined ? null : `/chat/${theirChat.groupIdB64}` });
    await runMessage(d);
    expect(d.navigate).not.toHaveBeenCalledWith("/chat/walk");
    expect(d.found).toHaveBeenCalledTimes(1);
  });
});
