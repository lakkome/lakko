// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  callAlert,
  callTag,
  conversationTag,
  messageNotifications,
  notificationImage,
  type NotifyRow,
} from "./desktop-notifications";
import { isAppPath } from "./host";

const row = (over: Partial<NotifyRow> = {}): NotifyRow => ({
  key: "chat:a",
  unread: 0,
  muted: false,
  title: "Ann",
  preview: "hello",
  href: "/chat/a",
  ...over,
});

const hidden = { focused: false, path: "/" };

describe("messageNotifications", () => {
  it("records the first snapshot without announcing anything", () => {
    const r = messageNotifications(null, [row({ unread: 3 })], hidden, "New message");
    expect(r.notify).toEqual([]);
    expect(r.next.get("chat:a")).toBe(3);
  });

  it("announces a rise in a conversation's unread count", () => {
    const prev = new Map([["chat:a", 1]]);
    const r = messageNotifications(prev, [row({ unread: 2 })], hidden, "New message");
    expect(r.notify).toEqual([
      { tag: conversationTag("chat:a"), title: "Ann", body: "hello", target: "/chat/a" },
    ]);
  });

  it("announces a conversation that appears with unread messages", () => {
    const r = messageNotifications(new Map(), [row({ unread: 1 })], hidden, "New message");
    expect(r.notify).toHaveLength(1);
  });

  it("stays quiet when the count stays or falls", () => {
    const prev = new Map([["chat:a", 2]]);
    expect(messageNotifications(prev, [row({ unread: 2 })], hidden, "x").notify).toEqual([]);
    expect(messageNotifications(prev, [row({ unread: 0 })], hidden, "x").notify).toEqual([]);
  });

  it("leaves a channel's posts to the channel rule", () => {
    const prev = new Map([["channel:c", 0]]);
    const channel = row({ key: "channel:c", unread: 1, href: "/channel/c", channel: true });
    expect(messageNotifications(prev, [channel], hidden, "x").notify).toEqual([]);
  });

  it("never announces a muted conversation", () => {
    const prev = new Map([["chat:a", 0]]);
    expect(messageNotifications(prev, [row({ unread: 1, muted: true })], hidden, "x").notify).toEqual([]);
  });

  it("stays quiet for the conversation open in a focused window", () => {
    const prev = new Map([["chat:a", 0]]);
    const open = { focused: true, path: "/chat/a" };
    expect(messageNotifications(prev, [row({ unread: 1 })], open, "x").notify).toEqual([]);
    const other = { focused: true, path: "/chat/b" };
    expect(messageNotifications(prev, [row({ unread: 1 })], other, "x").notify).toHaveLength(1);
    const unfocused = { focused: false, path: "/chat/a" };
    expect(messageNotifications(prev, [row({ unread: 1 })], unfocused, "x").notify).toHaveLength(1);
  });

  it("uses the fallback word when there is no preview", () => {
    const prev = new Map([["chat:a", 0]]);
    const r = messageNotifications(prev, [row({ unread: 1, preview: "  " })], hidden, "New message");
    expect(r.notify[0]?.body).toBe("New message");
  });

  it("skips a conversation it cannot open", () => {
    const prev = new Map([["chat:a", 0]]);
    expect(messageNotifications(prev, [row({ unread: 1, href: null })], hidden, "x").notify).toEqual([]);
  });
});

describe("callAlert", () => {
  const base = {
    ringingCallId: "c1",
    quiet: false,
    alreadyAlerted: null,
    focused: false,
    callerTitle: "Ann",
    incomingWord: "Incoming call",
    acceptWord: "Accept",
    declineWord: "Decline",
    video: true,
  };

  it("notifies once for a call ringing behind a hidden window", () => {
    expect(callAlert(base)).toEqual({
      kind: "notify",
      notification: {
        tag: callTag("c1"),
        title: "Ann",
        body: "Incoming call",
        target: null,
        call: { accept: "Accept", decline: "Decline", callId: "c1", video: true },
      },
    });
    expect(callAlert({ ...base, alreadyAlerted: "c1" })).toEqual({ kind: "none" });
  });

  it("asks for attention when the window is in front", () => {
    expect(callAlert({ ...base, focused: true })).toEqual({ kind: "attention" });
  });

  it("raises nothing for a quiet call or no call", () => {
    expect(callAlert({ ...base, quiet: true })).toEqual({ kind: "none" });
    expect(callAlert({ ...base, ringingCallId: null })).toEqual({ kind: "none" });
  });
});

describe("isAppPath", () => {
  it("accepts app paths only", () => {
    expect(isAppPath("/chat/a")).toBe(true);
    expect(isAppPath("//evil.example")).toBe(false);
    expect(isAppPath("https://evil.example")).toBe(false);
    expect(isAppPath("/a\\b")).toBe(false);
    expect(isAppPath("/a\nb")).toBe(false);
    expect(isAppPath("chat")).toBe(false);
  });
});

describe("message notification picture (ADR-0047 section 6)", () => {
  const jpeg = new Uint8Array([0xff, 0xd8, 0xff, 0xe0]);
  const prev = new Map([["chat:a", 0]]);

  it("carries the micro thumbnail beside the message's words", () => {
    const r = messageNotifications(prev, [row({ unread: 1, preview: "Photo", image: jpeg })], hidden, "New message");
    expect(r.notify[0]?.image).toBe("/9j/4A==");
    expect(r.notify[0]?.body).toBe("Photo");
  });

  it("shows no picture when the notification shows no text", () => {
    const r = messageNotifications(prev, [row({ unread: 1, preview: "  ", image: jpeg })], hidden, "New message");
    expect(r.notify[0]?.body).toBe("New message");
    expect(r.notify[0]?.image).toBeUndefined();
  });

  it("sends nothing that is not a JPEG within the bound", () => {
    const big = new Uint8Array(4097);
    big.set([0xff, 0xd8, 0xff]);
    for (const image of [big, new Uint8Array([1, 2, 3])]) {
      const r = messageNotifications(prev, [row({ unread: 1, image })], hidden, "New message");
      expect(r.notify[0]?.image).toBeUndefined();
    }
    expect(notificationImage(undefined)).toBeNull();
  });
});
