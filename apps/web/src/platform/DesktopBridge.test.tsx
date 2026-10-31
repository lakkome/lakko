// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The page's side of the desktop host: what it posts, how it answers the
 * host's events, and the shortcuts it turns into list commands. The
 * conversation rows, the call host and the router are stubbed; the host is a
 * recorded `fetch`.
 */
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ConversationRow } from "../chat/conversation-list";
import { renderUi, settle } from "../test/ui";

const platform = { desktop: true };
vi.mock("./index", () => ({ isDesktop: (): boolean => platform.desktop }));

const [rows, setRows] = createSignal<readonly ConversationRow[]>([]);
vi.mock("../chat/conversation-rows", () => ({
  createConversationRows: () => ({
    orderedRows: rows,
    groupById: () => new Map(rows().map((r) => [r.id, { groupIdB64: `g-${r.id}` }])),
    kindOf: () => "chat",
    conversationIdOf: () => new Uint8Array([1, 2, 3]),
  }),
}));

const [callLive, setCallLive] = createSignal(false);
const [ringing, setRinging] = createSignal<string | null>(null);
const endCurrentCall = vi.fn(() => {
  setCallLive(false);
  return Promise.resolve();
});
vi.mock("../calls/CallHostContext", () => ({
  useCallHost: () => ({
    anyCallInProgress: () => callLive(),
    endCurrentCall,
    ringingCallIdHex: ringing,
    ringingOfferedVideo: () => true,
    quietIncomingCall: () => null,
    activeGroupIdB64: () => "g-ann",
  }),
}));

const navigate = vi.fn();
vi.mock("@solidjs/router", () => ({
  useNavigate: () => navigate,
  useLocation: () => ({ pathname: "/" }),
}));

const confirmAnswer = { value: true };
const requestConfirm = vi.fn((_r: unknown) => Promise.resolve(confirmAnswer.value));
vi.mock("../ui/confirm-dialog", () => ({ requestConfirm: (r: unknown) => requestConfirm(r) }));

import { DesktopBridge } from "./DesktopBridge";
import { HOST_EVENT } from "./host";
import { clearListCommand, pendingListCommand } from "./shortcuts";

let posts: Array<{ url: string; body: unknown }>;

function row(id: string, unread: number, muted = false): ConversationRow {
  return {
    kind: "chat",
    id,
    title: id,
    preview: "",
    lastActivityMs: 1,
    unread,
    pinned: false,
    muted,
    archived: false,
  };
}

const hostEvent = (kind: string, extra: Record<string, unknown> = {}): boolean =>
  window.dispatchEvent(new CustomEvent(HOST_EVENT, { detail: { kind, ...extra } }));

beforeEach(() => {
  platform.desktop = true;
  posts = [];
  setRows([]);
  setCallLive(false);
  setRinging(null);
  confirmAnswer.value = true;
  navigate.mockClear();
  requestConfirm.mockClear();
  endCurrentCall.mockClear();
  clearListCommand();
  vi.stubGlobal("fetch", (url: string, init?: RequestInit) => {
    posts.push({
      url,
      body: typeof init?.body === "string" ? JSON.parse(init.body) : undefined,
    });
    return Promise.resolve(new Response(null, { status: 204 }));
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

const states = (): unknown[] =>
  posts.filter((p) => p.url === "/__host/app/state").map((p) => p.body);

describe("DesktopBridge", () => {
  it("posts the state at start and on each change, not on every read", async () => {
    renderUi(() => <DesktopBridge />);
    await settle();
    setRows([row("a", 2), row("b", 1, true), row("c", 0)]);
    await settle();
    setRows([row("a", 3), row("b", 1, true), row("c", 0)]);
    await settle();
    setCallLive(true);
    await settle();
    expect(states()).toEqual([
      { callLive: false, unread: 0 },
      { callLive: false, unread: 1 },
      { callLive: true, unread: 1 },
    ]);
  });

  it("asks before quitting during a call, ends the call, then quits", async () => {
    setCallLive(true);
    renderUi(() => <DesktopBridge />);
    await settle();
    hostEvent("quit-requested");
    await settle();
    await settle();
    expect(requestConfirm).toHaveBeenCalledOnce();
    expect(requestConfirm.mock.calls[0]?.[0]).toMatchObject({
      title: "End the call and quit?",
      confirmLabel: "End call and quit",
      tone: "danger",
    });
    expect(endCurrentCall).toHaveBeenCalledOnce();
    expect(posts.at(-1)?.url).toBe("/__host/app/quit");
  });

  it("does nothing when the question is cancelled", async () => {
    setCallLive(true);
    confirmAnswer.value = false;
    renderUi(() => <DesktopBridge />);
    await settle();
    hostEvent("quit-requested");
    await settle();
    expect(endCurrentCall).not.toHaveBeenCalled();
    expect(posts.some((p) => p.url === "/__host/app/quit")).toBe(false);
  });

  it("notifies a rise in unread, never for a muted chat, and opens it on click", async () => {
    setRows([row("a", 1), row("b", 0, true)]);
    renderUi(() => <DesktopBridge />);
    await settle();
    // The first snapshot announces nothing.
    expect(posts.filter((p) => p.url === "/__host/app/notify")).toEqual([]);
    setRows([row("a", 2), row("b", 3, true)]);
    await settle();
    const notified = posts.filter((p) => p.url === "/__host/app/notify").map((p) => p.body);
    expect(notified).toEqual([
      { tag: "conversation:chat:a", title: "a", body: "New message", target: "/chat/AQID" },
    ]);
    hostEvent("open-target", { target: "/chat/AQID" });
    expect(navigate).toHaveBeenCalledWith("/chat/AQID");
    hostEvent("open-target", { target: "https://evil.example" });
    expect(navigate).toHaveBeenCalledTimes(1);
  });

  it("raises one notification for a call ringing behind the window", async () => {
    setRows([row("ann", 0)]);
    renderUi(() => <DesktopBridge />);
    await settle();
    setRinging("c1");
    await settle();
    setRinging("c1");
    await settle();
    const notified = posts.filter((p) => p.url === "/__host/app/notify").map((p) => p.body);
    expect(notified).toHaveLength(1);
    expect(notified[0]).toMatchObject({
      tag: "call:c1",
      title: "ann",
      target: null,
      call: { callId: "c1", video: true },
    });
  });

  it("takes the call notification back when the ring ends", async () => {
    setRows([row("ann", 0)]);
    renderUi(() => <DesktopBridge />);
    await settle();
    setRinging("c2");
    await settle();
    setRinging(null);
    await settle();
    const withdrawn = posts.filter((p) => p.url === "/__host/app/withdraw").map((p) => p.body);
    expect(withdrawn).toEqual([{ tag: "call:c2" }]);
  });

  it("says once that Lakko is still running", async () => {
    renderUi(() => <DesktopBridge />);
    await settle();
    hostEvent("still-running");
    await settle();
    const notified = posts.filter((p) => p.url === "/__host/app/notify").map((p) => p.body);
    expect(notified).toMatchObject([{ tag: "still-running", target: null }]);
  });

  it("opens Settings for the menu item", async () => {
    renderUi(() => <DesktopBridge />);
    hostEvent("open-settings");
    expect(navigate).toHaveBeenCalledWith("/settings");
  });

  it("turns Mod+K into a search for the list, going to Chats first", async () => {
    renderUi(() => <DesktopBridge />);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", ctrlKey: true, metaKey: true }));
    // Both modifiers is no shortcut on either platform.
    expect(pendingListCommand()).toBeNull();
    const mac = /mac/i.test(navigator.platform);
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "k", ctrlKey: !mac, metaKey: mac, cancelable: true }),
    );
    expect(navigate).toHaveBeenCalledWith("/");
    expect(pendingListCommand()).toBe("search");
  });

  it("goes to a destination on Mod+Shift+digit, from a field too", () => {
    renderUi(() => <DesktopBridge />);
    const mac = /mac/i.test(navigator.platform);
    const input = document.createElement("input");
    document.body.appendChild(input);
    input.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "#",
        code: "Digit3",
        shiftKey: true,
        ctrlKey: !mac,
        metaKey: mac,
        bubbles: true,
        cancelable: true,
      }),
    );
    expect(navigate).toHaveBeenCalledWith("/contacts");
    expect(pendingListCommand()).toBeNull();
  });

  it("in the browser build sends nothing and answers no host event", async () => {
    platform.desktop = false;
    setCallLive(true);
    renderUi(() => <DesktopBridge />);
    await settle();
    hostEvent("quit-requested");
    hostEvent("open-settings");
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", ctrlKey: true }));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", metaKey: true }));
    await settle();
    expect(pendingListCommand()).toBeNull();
    expect(posts).toEqual([]);
    expect(requestConfirm).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });
});
