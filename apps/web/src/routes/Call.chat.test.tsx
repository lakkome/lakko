// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The chat of a group call on the real call screen (`docs/ux/calls.md`
 * section 4.1.4): the `Chat` circle, its count and preview, the panel, the
 * composer's keys, `Esc`, and a message that was not sent.
 *
 * The screen, the store and the panel are the real ones; the session behind
 * the chat is a control that records what the screen asked of it.
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route } from "@solidjs/router";
import { render } from "solid-js/web";

import { callActions, type Participant } from "../state/call-state";
import { callChatActions, type CallChatMessage } from "../calls/call-chat";
import { all, initWasm, one, present, settle } from "../test/ui";

type CallComponent = typeof import("./Call")["default"];
let Call: CallComponent;

beforeAll(async () => {
  class FakeResizeObserver {
    constructor(private readonly cb: ResizeObserverCallback) {}
    observe(): void {
      this.cb(
        [{ contentRect: { width: 1200, height: 700 } } as ResizeObserverEntry],
        this as unknown as ResizeObserver,
      );
    }
    unobserve(): void {}
    disconnect(): void {}
  }
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(async () => {});
  vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => {});
  await initWasm();
  Call = (await import("./Call")).default;
  await settle();
});

function person(id: string, over: Partial<Participant> = {}): Participant {
  return {
    id,
    displayName: id,
    isLocal: false,
    micOn: true,
    camOn: false,
    quality: "good",
    speaking: false,
    ...over,
  };
}

function message(over: Partial<CallChatMessage>): CallChatMessage {
  return {
    id: "anna:1",
    own: false,
    peerId: "anna",
    seq: 1n,
    text: "hello",
    arrivedAtMs: Date.now(),
    delivery: "sent",
    ...over,
  };
}

let dispose: (() => void) | null = null;
const sent: string[] = [];
const resent: string[] = [];
let collapses = 0;

beforeEach(() => {
  callActions.reset();
  callChatActions.reset();
  sent.length = 0;
  resent.length = 0;
  collapses = 0;
  callActions.setParticipants([
    person("local", { isLocal: true }),
    person("peer-anna", { displayName: "Anna" }),
  ]);
  callActions.setPhase({ kind: "connected" });
  callChatActions.setControl({
    send: (text) => sent.push(text),
    sendAgain: (id) => resent.push(id),
  });
  const host = document.createElement("div");
  document.body.appendChild(host);
  dispose = render(
    () => (
      <MemoryRouter>
        <Route
          path="*"
          component={() => (
            <Call
              embedded={true}
              group={true}
              onExit={() => undefined}
              onCollapse={() => {
                collapses += 1;
              }}
            />
          )}
        />
      </MemoryRouter>
    ),
    host,
  );
});

afterEach(() => {
  dispose?.();
  dispose = null;
  callChatActions.reset();
  document.body.replaceChildren();
});

const chatButton = (): HTMLButtonElement => one('[data-control="chat"] .lk-call-ctl-btn');

function type(text: string): HTMLTextAreaElement {
  const field = one<HTMLTextAreaElement>(".lk-call-chat-field");
  field.value = text;
  field.dispatchEvent(new InputEvent("input", { bubbles: true }));
  return field;
}

function key(el: Element, k: string, init: KeyboardEventInit = {}): void {
  el.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...init }));
}

describe("the Chat circle", () => {
  it("sits after Participants and opens the panel at its empty state", async () => {
    await settle();
    const row = all(".lk-call-bar .lk-call-ctl").map((el) => el.dataset.control);
    expect(row).toEqual(["mic", "camera", "participants", "chat", "settings", "end"]);
    expect(chatButton().getAttribute("aria-label")).toBe("Open chat");
    expect(present(".lk-call-chat")).toBe(false);

    chatButton().click();
    await settle();
    expect(one(".lk-call-chat").getAttribute("aria-label")).toBe("Call chat");
    expect(one(".lk-call-chat-empty").textContent).toContain(
      "Messages here are seen only by people in the call.",
    );
    expect(one(".lk-call-chat-empty").textContent).toContain("They disappear when the call ends.");
    expect(chatButton().getAttribute("aria-label")).toBe("Close chat");
    expect(document.activeElement).toBe(one(".lk-call-chat-field"));
  });

  it("counts and previews a message from someone else while closed, and opening clears both", async () => {
    await settle();
    const m = message({ text: "are you there?" });
    callChatActions.setMessages([m]);
    callChatActions.incoming(m);
    await settle();
    expect(one(".lk-call-ctl-badge").textContent).toBe("1");
    expect(chatButton().getAttribute("aria-label")).toBe("Open chat, 1 unread");
    expect(one(".lk-call-chat-preview-name").textContent).toBe("Anna");
    expect(one(".lk-call-chat-preview-text").textContent).toBe("are you there?");

    one<HTMLButtonElement>(".lk-call-chat-preview").click();
    await settle();
    expect(present(".lk-call-ctl-badge")).toBe(false);
    expect(present(".lk-call-chat-preview")).toBe(false);
    expect(one(".lk-call-chat-sender").textContent).toBe("Anna");
    expect(one(".lk-call-chat-note").textContent).toContain("seen only by people in the call");
  });
});

describe("the composer", () => {
  it("sends on Enter, starts a line on Shift+Enter, and refuses white space", async () => {
    await settle();
    chatButton().click();
    await settle();
    const send = one<HTMLButtonElement>(".lk-call-chat-send");
    expect(send.disabled).toBe(true);

    let field = type("   ");
    expect(send.disabled).toBe(true);
    key(field, "Enter");
    expect(sent).toEqual([]);

    field = type("hi there");
    expect(send.disabled).toBe(false);
    key(field, "Enter", { shiftKey: true });
    expect(sent).toEqual([]);
    key(field, "Enter");
    expect(sent).toEqual(["hi there"]);
    expect(field.value).toBe("");
  });

  it("takes no more text at the bound and says so", async () => {
    await settle();
    chatButton().click();
    await settle();
    const field = type("я".repeat(401));
    expect(field.value).toBe("я".repeat(400));
    expect(one(".lk-call-chat-too-long").textContent).toBe(
      "A message in a call can't be longer than this.",
    );
  });
});

describe("the emoji button", () => {
  it("opens the emoji picker without a GIF tab and puts an emoji at the caret", async () => {
    await settle();
    chatButton().click();
    await settle();
    const button = one<HTMLButtonElement>(".lk-call-chat-emoji");
    expect(button.getAttribute("aria-label")).toBe("Emoji");
    expect(present(".lk-call-chat .lakko-picker")).toBe(false);
    const field = type("ab");
    field.setSelectionRange(1, 1);
    button.click();
    await settle();
    expect(present(".lk-call-chat .lakko-picker")).toBe(true);
    expect(present(".lk-call-chat .lakko-picker-tabs")).toBe(false);

    const cell = one<HTMLButtonElement>(".lk-call-chat .lakko-emoji-grid button");
    const glyph = cell.textContent ?? "";
    expect(glyph.length).toBeGreaterThan(0);
    cell.click();
    await settle();
    expect(field.value).toBe(`a${glyph}b`);
    // The picker stays open for another one, and nothing was sent.
    expect(present(".lk-call-chat .lakko-picker")).toBe(true);
    expect(sent).toEqual([]);
    expect(one<HTMLButtonElement>(".lk-call-chat-send").disabled).toBe(false);
  });

  it("closes on Esc and leaves the panel open", async () => {
    await settle();
    chatButton().click();
    await settle();
    one<HTMLButtonElement>(".lk-call-chat-emoji").click();
    await settle();
    key(one(".lk-call-chat-field"), "Escape");
    await settle();
    expect(present(".lk-call-chat .lakko-picker")).toBe(false);
    expect(present(".lk-call-chat")).toBe(true);
    expect(collapses).toBe(0);
  });
});

describe("pictures (section 4.1.4.1)", () => {
  it("says only pictures can be sent when a pasted file is not one", async () => {
    await settle();
    chatButton().click();
    callChatActions.setControl({
      send: (text) => sent.push(text),
      sendAgain: (id) => resent.push(id),
      sendPicture: () => undefined,
    });
    await settle();
    const field = one<HTMLTextAreaElement>(".lk-call-chat-field");
    // jsdom has no DataTransfer: the panel reads only `files`.
    const data = { files: [new File([new Uint8Array(4)], "notes.pdf", { type: "application/pdf" })] };
    const paste = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(paste, "clipboardData", { value: data });
    field.dispatchEvent(paste);
    await settle();
    expect(paste.defaultPrevented).toBe(true);
    expect(field.value).toBe("");
    expect(all(".lk-call-chat-too-long").map((el) => el.textContent)).toContain(
      "Only pictures can be sent in a call.",
    );
    expect(present(".lk-call-chat-tray")).toBe(false);
  });

  it("draws a picture, its loading and its missing state, and offers Save picture", async () => {
    await settle();
    chatButton().click();
    callChatActions.setMessages([
      message({ id: "anna:1", text: "", picture: { width: 640, height: 480, status: "ready", url: "blob:x" } }),
      message({ id: "anna:2", seq: 2n, text: "", picture: { width: 10, height: 10, status: "loading", url: null } }),
      message({ id: "anna:3", seq: 3n, text: "", picture: { width: 10, height: 10, status: "unavailable", url: null } }),
    ]);
    await settle();
    const pictures = all(".lk-call-chat-picture");
    expect(pictures).toHaveLength(3);
    expect(pictures[0]?.style.width).toBe("240px");
    expect(pictures[0]?.style.height).toBe("180px");
    expect(pictures[0]?.querySelector("img")?.getAttribute("src")).toBe("blob:x");
    expect(present(".lk-call-chat-picture-spinner")).toBe(true);
    expect(one(".lk-call-chat-picture-gone").textContent).toBe("Picture not available.");

    one(".lk-call-chat-bubble").dispatchEvent(
      new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }),
    );
    await settle();
    expect(all(".lk-call-chat-menu [role=menuitem]").map((el) => el.textContent)).toEqual(["Save picture"]);
  });

  it("opens a picture over the call and closes it on Esc without closing the chat", async () => {
    await settle();
    chatButton().click();
    callChatActions.setMessages([
      message({ id: "anna:1", text: "", picture: { width: 640, height: 480, status: "ready", url: "blob:x" } }),
    ]);
    await settle();
    one<HTMLButtonElement>(".lk-call-chat-picture-open").click();
    await settle();
    const viewer = one(".lk-call-chat-viewer");
    key(viewer, "Escape");
    await settle();
    expect(present(".lk-call-chat-viewer")).toBe(false);
    expect(present(".lk-call-chat")).toBe(true);
    expect(collapses).toBe(0);
  });
});

describe("a message that was not sent", () => {
  it("says so and sends it again", async () => {
    await settle();
    chatButton().click();
    callChatActions.setMessages([
      message({ id: "own:1", own: true, peerId: null, text: "lost", delivery: "not-sent" }),
    ]);
    await settle();
    expect(one(".lk-call-chat-bubble").classList.contains("not-sent")).toBe(true);
    expect(one(".lk-call-chat-failed").textContent).toContain("Not sent.");
    one<HTMLButtonElement>(".lk-call-chat-retry").click();
    expect(resent).toEqual(["own:1"]);
  });
});

describe("Esc", () => {
  it("closes the panel first, and only then puts the call away", async () => {
    await settle();
    chatButton().click();
    await settle();
    key(document.body, "Escape");
    await settle();
    expect(present(".lk-call-chat")).toBe(false);
    expect(collapses).toBe(0);
    key(document.body, "Escape");
    expect(collapses).toBe(1);
  });

  it("closes the Copy menu before the panel", async () => {
    await settle();
    chatButton().click();
    callChatActions.setMessages([message({})]);
    await settle();
    one(".lk-call-chat-bubble").dispatchEvent(
      new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }),
    );
    await settle();
    expect(all(".lk-call-chat-menu [role=menuitem]").map((el) => el.textContent)).toEqual(["Copy"]);
    key(one(".lk-call-chat-field"), "Escape");
    await settle();
    expect(present(".lk-call-chat-menu")).toBe(false);
    expect(present(".lk-call-chat")).toBe(true);
  });
});
