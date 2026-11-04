// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The timeline row, rendered.
 *
 * Two defects a person found by holding a phone, both of them invisible to
 * every rule test in this directory because both are about the TREE a
 * component builds and the EVENT a handler is on, not about a decision:
 *
 *  1. The action strip was revealed on `:hover` and positioned inside the
 *     bubble. On a voice message that is where the play button is, so pressing
 *     play opened the reaction picker and paused the message — it could not be
 *     listened to at all. `message-menu-placement.ts` pins the arithmetic of
 *     the fix and cannot see either half of the defect: that hover opened it,
 *     and that it rendered inside the bubble's own subtree.
 *
 *  2. A lone voice message drew its time on a line BELOW the recording's
 *     duration, where the spec puts them on one. `bubbleMetaSlot` decides
 *     `"voice-line"` and is tested; nothing tested that the component then
 *     actually hands the line to the voice bubble.
 *
 *  3. The reaction row and the action rows lived in ONE card, so the action
 *     rows were stretched to the width of seven emoji and a plus —
 *     `message-actions.md` section 2 makes the action card a separate surface
 *     "sized by its own content". jsdom computes no widths, so what is provable
 *     here is the two things that made the defect: the TREE (one card around
 *     both, rather than two cards) and the CASCADE (a `min-width` floor and an
 *     `align-items` that stretched). The measured widths are in
 *     `playwright/specs/message-menu.spec.ts`. Section 2 then went further -
 *     "Two surfaces that read as two things" - so the row is also pinned here
 *     as a pill placed on its own, with a gap between it and the card.
 *
 * Copy (section 3) is covered here too, because it needs no layout at all: the
 * clipboard is a platform call, and "was it called with the message's text" is
 * exactly what this harness can see.
 *
 * The two context hooks are replaced because they are name resolvers, not part
 * of either defect: reaching them would mean standing up an MLS identity and a
 * profile store to answer a question about a `contextmenu` handler.
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./ChatContext", () => ({ useChats: () => ({ identity: () => null }) }));
vi.mock("../profile/ProfileContext", () => ({
  useProfiles: () => ({
    version: () => 0,
    resolve: (k: Uint8Array | string) => ({ name: typeof k === "string" ? k : "Member" }),
    ownProfile: () => ({ displayName: "Self" }),
    ownUsername: () => "self",
  }),
}));

import { TimelineBubble, type BubbleHost } from "./TimelineBubble";
import { LONG_PRESS_MS, SURFACE_GAP_PX } from "./MessageMenu";
import { createMediaHub, mediaKey, type MediaHub } from "../media/media-hub";
import { shortTime } from "./timeline-layout";
import type { AttachmentRefBytes } from "./attachments";
import type { TimelineRow } from "./timeline-row";
import {
  all,
  initWasm,
  installStylesheet,
  mouse,
  one,
  pointer,
  present,
  renderUi,
  settle,
} from "../test/ui";

beforeAll(async () => {
  await initWasm();
  // jsdom implements no media engine at all: `load`, `play` and `pause` are
  // `notImplemented` stubs that shout at the virtual console. Standing in for
  // the engine is the only way to render a bubble that owns an `<audio>`;
  // nothing in THIS file asserts on them (see `voice-play.test.tsx`).
  HTMLMediaElement.prototype.load = function load(): void {};
  HTMLMediaElement.prototype.play = function play(): Promise<void> {
    return Promise.resolve();
  };
  HTMLMediaElement.prototype.pause = function pause(): void {};
});

const BLOB_ID = new Uint8Array(32).fill(0xab);
const VOICE_KEY = mediaKey(BLOB_ID);
const SENT_AT = "2026-09-24T15:21:00Z";

function voiceRef(): AttachmentRefBytes {
  return {
    blobId: BLOB_ID,
    key: new Uint8Array(32),
    nonce: new Uint8Array(12),
    mime: "audio/mp4",
    size: 4096,
    name: undefined,
    kind: "voice",
    durationMs: 7000,
    waveform: new Uint8Array([10, 90, 200, 140, 60, 180, 30, 210]),
  };
}

let hub: MediaHub;

beforeEach(() => {
  hub = createMediaHub({ voiceQueue: () => [] });
  // Bytes already filed under the blob's own key, so nothing in this file
  // touches the network: the bubble's scroll-into-view warm-up finds a URL and
  // a seek finds a sourced element.
  hub.fileLocal(VOICE_KEY, new Blob([new Uint8Array(16)], { type: "audio/mp4" }), "audio/mp4");
});

function host(over: Partial<BubbleHost> = {}): BubbleHost {
  return {
    onDownload: () => {},
    onPreview: () => {},
    onHidePreview: () => {},
    getCachedUrl: () => null,
    ownIdentity: "own-key",
    opState: () => undefined,
    readByOthers: () => false,
    readersOf: () => [],
    editingId: null,
    editText: "",
    setEditText: () => {},
    onStartEdit: () => {},
    onCancelEdit: () => {},
    onSaveEdit: () => {},
    onDeleteMessage: () => {},
    onReact: () => {},
    onQuickReact: () => {},
    reactionAllowed: () => ({ mode: "all" }),
    chipConversation: "one-to-one",
    reactionRowFor: () => ["👍", "❤️", "😂", "😮", "😢", "🙏"],
    ownReactorKey: "own-key",
    capNoticeId: null,
    onReply: () => {},
    onJumpTo: () => {},
    onForward: () => {},
    pinLabelOf: () => "Pin",
    onPin: () => {},
    onOpenForwardOrigin: () => {},
    actionsAvailable: true,
    hub,
    mediaUpload: undefined,
    onRetrySend: () => {},
    fullReactPickerId: null,
    onOpenFullReactPicker: () => {},
    onCloseFullReactPicker: () => {},
    selecting: false,
    isSelected: () => false,
    onToggleSelect: () => {},
    onSelect: () => {},
    ...over,
  };
}

function incoming(text: string, attachments: AttachmentRefBytes[] = []): TimelineRow {
  return {
    kind: "incoming",
    event: {
      kind: "application",
      messageId: 42,
      text,
      plaintext: text,
      attachments,
      senderLeaf: 1,
      senderKey: "peer-key",
      createdAt: SENT_AT,
    },
  };
}

/** Our own acked message, which is the only kind that offers Edit and Delete —
 *  and therefore the only one whose action card has more than one row. */
function ownMessage(text: string, attachments: AttachmentRefBytes[] = []): TimelineRow {
  return {
    kind: "outgoing",
    tempId: 7,
    plaintext: text,
    attachments,
    ack: "sent",
    messageId: 42,
    createdAt: SENT_AT,
    authorKey: "own-key",
  };
}

function mount(row: TimelineRow, over: Partial<BubbleHost> = {}): HTMLElement {
  renderUi(() => (
    <TimelineBubble
      row={row}
      isGroup={false}
      startsRun={true}
      endsRun={true}
      {...host(over)}
    />
  ));
  return one(".lk-bubble");
}

// ---------------------------------------------------------------------------
// Defect 1 — the menu opens only on a deliberate act, and outside the bubble.
// ---------------------------------------------------------------------------

describe("the action menu's trigger", () => {
  const MENU = "[role='menu']";

  it("a pointer crossing the bubble opens nothing", () => {
    const bubble = mount(incoming("hello"));
    mouse(bubble, "mouseover");
    mouse(bubble, "mouseenter");
    mouse(bubble, "mousemove");
    expect(present(MENU)).toBe(false);
  });

  it("hovering a voice message's play button opens nothing", () => {
    mount(incoming("", [voiceRef()]));
    const play = one("button.lakko-voice-play");
    mouse(play, "mouseover");
    mouse(play, "mousemove");
    // The defect, stated as the person met it: the strip appeared over this
    // button, so the press that followed hit the strip instead of play.
    expect(present(MENU)).toBe(false);
  });

  it("focus on the waveform slider opens nothing", () => {
    mount(incoming("", [voiceRef()]));
    // `:focus-within` went with hover for this reason: tabbing onto the seek
    // slider is navigation, and revealing the strip there put it over the play
    // button by keyboard instead of by mouse.
    one("[role='slider']").dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    expect(present(MENU)).toBe(false);
  });

  it("a right-click opens it", () => {
    const bubble = mount(incoming("hello"));
    mouse(bubble, "contextmenu", { clientX: 120, clientY: 200 });
    expect(present(MENU)).toBe(true);
    expect(all(MENU + " [role='menuitem']").length).toBeGreaterThan(0);
  });

  it("Shift+F10 on the focused bubble opens it", () => {
    const bubble = mount(incoming("hello"));
    bubble.dispatchEvent(
      new KeyboardEvent("keydown", { key: "F10", shiftKey: true, bubbles: true }),
    );
    expect(present(MENU)).toBe(true);
  });
});

describe("the action menu's place in the tree", () => {
  it("is rendered outside the bubble's own subtree", () => {
    const bubble = mount(incoming("hello"));
    mouse(bubble, "contextmenu", { clientX: 10, clientY: 10 });
    const menu = one("[role='menu']");
    // Not "at different coordinates": OUTSIDE. Anything inside the bubble can
    // overlap the bubble's own controls, and a coordinate rule cannot be
    // checked by looking at one bubble kind.
    expect(bubble.contains(menu)).toBe(false);
    expect(present("[role='menu']", bubble)).toBe(false);
  });

  it("carries both halves the one gesture opens", () => {
    const bubble = mount(incoming("hello"), { actionsAvailable: true });
    mouse(bubble, "contextmenu", { clientX: 10, clientY: 10 });
    // The quick reactions used to be a second step behind a React button, and
    // that button was positioned inside the bubble too.
    expect(all("[role='group'][aria-label='React'] [role='menuitem']").length).toBeGreaterThan(1);
  });
});

describe("a long press aimed at a control", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  // Restored, because vitest does not: without this the fake clock leaks into
  // every describe below and a `setTimeout(0)` there never fires, so a test
  // awaiting a settled promise times out twenty seconds later with a message
  // about its own `await` rather than about the clock.
  afterEach(() => {
    vi.useRealTimers();
  });

  it("on the play button belongs to the button, not the message", () => {
    mount(incoming("", [voiceRef()]));
    const play = one("button.lakko-voice-play");
    pointer(play, "pointerdown", { pointerType: "touch", clientX: 50, clientY: 50 });
    vi.advanceTimersByTime(LONG_PRESS_MS + 50);
    // Holding play is still play. This is the touch half of the same defect:
    // the menu must not take a gesture the person aimed at something else.
    expect(present("[role='menu']")).toBe(false);
    pointer(play, "pointerup", { pointerType: "touch", clientX: 50, clientY: 50 });
  });

  it("on the waveform belongs to the slider", () => {
    mount(incoming("", [voiceRef()]));
    const wave = one("[role='slider']");
    pointer(wave, "pointerdown", { pointerType: "touch", clientX: 50, clientY: 50 });
    vi.advanceTimersByTime(LONG_PRESS_MS + 50);
    expect(present("[role='menu']")).toBe(false);
    pointer(wave, "pointerup", { pointerType: "touch", clientX: 50, clientY: 50 });
  });

  it("on the bubble's own surface does open the menu", () => {
    const bubble = mount(incoming("hello"));
    pointer(bubble, "pointerdown", { pointerType: "touch", clientX: 50, clientY: 50 });
    vi.advanceTimersByTime(LONG_PRESS_MS + 50);
    // The control guard must not have swallowed the gesture everywhere: a long
    // press on the message itself is the touch path to the menu.
    expect(present("[role='menu']")).toBe(true);
  });

  it("is abandoned once the finger starts travelling", () => {
    const bubble = mount(incoming("hello"));
    pointer(bubble, "pointerdown", { pointerType: "touch", clientX: 50, clientY: 50 });
    pointer(bubble, "pointermove", { pointerType: "touch", clientX: 50, clientY: 140 });
    vi.advanceTimersByTime(LONG_PRESS_MS + 50);
    expect(present("[role='menu']")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Defect 5 — a lone voice message's time is on the recording's own line.
// ---------------------------------------------------------------------------

describe("where a message's time is drawn", () => {
  it("a lone voice message carries it on the voice row's quiet line", () => {
    mount(incoming("", [voiceRef()]));
    const metas = all(".lk-bubble-meta");
    // Exactly one: the defect drew the bubble's own row UNDER the player as
    // well, which is what put the length of the recording and the time it was
    // sent on two different levels.
    expect(metas.length).toBe(1);
    const line = one(".lakko-voice-line");
    expect(line.contains(metas[0] as Node)).toBe(true);
    // Not a literal clock: `shortTime` is locale-dependent, and asserting
    // "15:21" would pin the test runner's timezone rather than the tree.
    expect(metas[0]?.textContent).toContain(shortTime(SENT_AT));
  });

  it("the duration and the time end up on the same line", () => {
    mount(incoming("", [voiceRef()]));
    const line = one(".lakko-voice-line");
    expect(present(".lakko-voice-time", line)).toBe(true);
    expect(present(".lk-bubble-meta", line)).toBe(true);
  });

  it("a voice message WITH a caption keeps the bubble's own row", () => {
    const bubble = mount(incoming("listen to this", [voiceRef()]));
    // "Nothing but media" is the load-bearing half of the rule: a captioned
    // voice message has a paragraph to sit under, so the row below is right.
    expect(present(".lakko-voice-line .lk-bubble-meta")).toBe(false);
    expect(present(":scope > .lk-bubble-meta", bubble)).toBe(true);
  });

  it("a plain text message keeps the bubble's own row", () => {
    const bubble = mount(incoming("just words"));
    expect(present(":scope > .lk-bubble-meta", bubble)).toBe(true);
    expect(all(".lk-bubble-meta").length).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// Defect 3 — the action card is a separate surface from the reaction row.
// `message-actions.md` section 2.
// ---------------------------------------------------------------------------

/** Open the menu on `row` and hand back the three elements section 2 names. */
function openMenuOn(row: TimelineRow, over: Partial<BubbleHost> = {}): {
  wrapper: HTMLElement;
  reactions: HTMLElement;
  items: HTMLElement | null;
} {
  const bubble = mount(row, over);
  mouse(bubble, "contextmenu", { clientX: 40, clientY: 60 });
  return {
    wrapper: one("[role='menu']"),
    reactions: one(".lk-msg-menu-reactions"),
    items: document.querySelector<HTMLElement>(".lk-msg-menu-items"),
  };
}

describe("the action card's width", () => {
  beforeEach(() => {
    installStylesheet();
  });

  it("is its own, not the reaction row's", () => {
    const { wrapper, reactions, items } = openMenuOn(ownMessage("hello"));

    // The two rows were ALREADY siblings under this element, so the tree alone
    // does not tell the defect from the fix. It is pinned anyway, because
    // nesting one inside the other would make every assertion below vacuous.
    expect(items).not.toBeNull();
    expect(reactions.contains(items as Node)).toBe(false);
    expect((items as HTMLElement).contains(reactions)).toBe(false);
    expect((items as HTMLElement).parentElement).toBe(wrapper);
    expect(reactions.parentElement).toBe(wrapper);

    // What WAS the defect is which element is the card, and that is entirely a
    // matter of the cascade. `display: flex` on the reaction row is the
    // control: if jsdom dropped the sheet, every read below is a default and
    // this fails rather than passing vacuously.
    expect(getComputedStyle(reactions).display).toBe("flex");
    // The surface is on each card. It used to be on the element AROUND both —
    // one bordered, padded box, which is what made the second row's width the
    // first row's business.
    expect(getComputedStyle(reactions).paddingTop).toBe("4px");
    expect(getComputedStyle(items as HTMLElement).paddingTop).toBe("4px");
    // Not `"0px"`: jsdom serialises an unset `padding-top` as `"0"`, and
    // pinning that would be pinning the harness. What matters is that the
    // element around both is not itself padded like a card.
    expect(getComputedStyle(wrapper).paddingTop).not.toBe("4px");
    // The floor that stopped either card being narrower than 180px. The other
    // half of the stretching, the wrapper's flex `stretch`, is gone with the
    // wrapper's layout: it no longer lays its children out at all (below).
    expect(getComputedStyle(wrapper).minWidth).not.toBe("180px");
    // And where the width comes from instead: the card's own content.
    expect(getComputedStyle(items as HTMLElement).width).toBe("max-content");
  });

  it("draws the row and the card as two floating surfaces with a gap", () => {
    // `message-actions.md` section 2, "Two surfaces that read as two things".
    // jsdom lays nothing out, so every box is zero and the layout would put
    // the two surfaces on top of each other by default. Each surface is given
    // a size here - the only thing this stub decides - and what is asserted is
    // what the component then DID with the sizes: where it put each surface.
    const ROW = { width: 234, height: 34 };
    const CARD = { width: 118, height: 104 };
    const real = HTMLElement.prototype.getBoundingClientRect;
    HTMLElement.prototype.getBoundingClientRect = function rect(this: HTMLElement): DOMRect {
      const size = this.classList.contains("lk-msg-menu-reactions")
        ? ROW
        : this.classList.contains("lk-msg-menu-items")
          ? CARD
          : { width: 0, height: 0 };
      return new DOMRect(0, 0, size.width, size.height);
    };
    try {
      const { wrapper, reactions, items } = openMenuOn(ownMessage("hello"));
      const card = items as HTMLElement;

      // Two elements, each a surface in its own right; neither holds the other.
      expect(reactions).not.toBe(card);
      expect(reactions.classList.contains("lk-msg-menu-surface")).toBe(true);
      expect(card.classList.contains("lk-msg-menu-surface")).toBe(true);
      expect(reactions.contains(card)).toBe(false);
      expect(card.contains(reactions)).toBe(false);
      // Each placed on its own, and the element around them is a positioner:
      // not a flex column stacking two cards, and not something a press in the
      // gap lands on instead of the backdrop.
      expect(getComputedStyle(reactions).position).toBe("absolute");
      expect(getComputedStyle(card).position).toBe("absolute");
      expect(getComputedStyle(wrapper).display).not.toBe("flex");
      expect(getComputedStyle(wrapper).pointerEvents).toBe("none");
      expect(getComputedStyle(reactions).pointerEvents).toBe("auto");
      // The row is a pill with its own outline, not a second copy of the card.
      expect(getComputedStyle(reactions).borderRadius).toBe("999px");
      expect(card.classList.contains("lk-msg-menu-card")).toBe(true);
      expect(reactions.classList.contains("lk-msg-menu-card")).toBe(false);

      // The gap. The pointer (y 60) leaves room above for the row, so the row
      // floats above the card with `SURFACE_GAP_PX` of nothing between them.
      expect(wrapper.dataset.rowAt).toBe("above");
      const rowTop = Number.parseFloat(reactions.style.top);
      const cardTop = Number.parseFloat(card.style.top);
      expect(cardTop - (rowTop + ROW.height)).toBe(SURFACE_GAP_PX);
      expect(SURFACE_GAP_PX).toBeGreaterThan(0);
    } finally {
      HTMLElement.prototype.getBoundingClientRect = real;
    }
  });

  it("carries the section 2 order, with Delete separated", () => {
    const { items } = openMenuOn(ownMessage("hello"));
    const labels = all("[role='menuitem'] span", items as HTMLElement).map((s) => s.textContent);
    // Section 2: the ones that produce a new message (Reply, then Forward -
    // ADR-0052 decisions 1 and 2), then the ones that only read (Copy, then
    // Select - section 8), then the ones that change this message (Pin,
    // decision 3, then Edit), then the destructive one.
    expect(labels).toEqual(["Reply", "Forward", "Copy", "Select", "Pin", "Edit", "Delete"]);
    // "separated", and structurally so: the divider is a wrapper of its own,
    // not a border hung off the colour class.
    const del = one(".lk-msg-menu-destructive [role='menuitem']");
    expect(del.classList.contains("danger")).toBe(true);
    expect(present(".lk-msg-menu-destructive", items as HTMLElement)).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Copy — `message-actions.md` section 3. "Nothing leaves the device."
// ---------------------------------------------------------------------------

describe("Copy", () => {
  /** Every text handed to the clipboard, in order. jsdom has no clipboard at
   *  all, so this stands in for the platform and asserts only what our own
   *  code did to it. */
  let written: string[];

  beforeEach(() => {
    written = [];
    Object.defineProperty(globalThis.navigator, "clipboard", {
      value: {
        writeText: (t: string): Promise<void> => {
          written.push(t);
          return Promise.resolve();
        },
      },
      configurable: true,
    });
  });

  it("puts the message's text on the clipboard", async () => {
    const { items } = openMenuOn(incoming("the exact words"));
    const copy = itemNamed(items as HTMLElement, "Copy");
    mouse(copy, "click");
    await settle();
    expect(written).toEqual(["the exact words"]);
  });

  it("copies what the person is reading, not the original, once edited", async () => {
    const { items } = openMenuOn(incoming("before"), {
      opState: () => ({ editedText: "after", held: [], reactions: new Map() }),
    });
    mouse(itemNamed(items as HTMLElement, "Copy"), "click");
    await settle();
    expect(written).toEqual(["after"]);
  });

  it("is offered on somebody else's message too", () => {
    // Not the Edit / Delete predicate: section 3 offers Copy on "any message
    // with text", and an incoming message has no Edit row at all.
    const { items } = openMenuOn(incoming("hello"));
    expect(labelsOf(items as HTMLElement)).toEqual(["Reply", "Forward", "Copy", "Select", "Pin"]);
  });

  it("is absent on a message with no text", () => {
    // A lone voice message: there is nothing to put on the clipboard, and an
    // action that copies an empty string is worse than one that is not there.
    const { items } = openMenuOn(ownMessage("", [voiceRef()]));
    expect(present(".lk-msg-menu-items")).toBe(true);
    expect((items as HTMLElement).textContent).not.toContain("Copy");
    expect(labelsOf(items as HTMLElement)).toEqual([
      "Reply",
      "Forward",
      "Select",
      "Pin",
      "Edit",
      "Delete",
    ]);
  });

  it("leaves an incoming media message with Reply, Forward, Select and Pin in the card", () => {
    // Nothing to copy and nothing to edit; Reply, Forward, Select and Pin are
    // offered on any complete message with an id (ADR-0052, section 8), so
    // the card is drawn with those four rows.
    const { items } = openMenuOn(incoming("", [voiceRef()]));
    expect(labelsOf(items as HTMLElement)).toEqual(["Reply", "Forward", "Select", "Pin"]);
    expect(present("[role='group'][aria-label='React']")).toBe(true);
  });
});

/** The action card's rows, by label, in order. */
function labelsOf(items: HTMLElement): Array<string | null> {
  return all("[role='menuitem'] span", items).map((s) => s.textContent);
}

/** The action-card row whose label is `label`. */
function itemNamed(items: HTMLElement, label: string): HTMLElement {
  const row = all<HTMLElement>("[role='menuitem']", items).find((b) => b.textContent?.trim() === label);
  expect(row, `no ${label} row`).toBeDefined();
  return row as HTMLElement;
}

// ---------------------------------------------------------------------------
// Reply — ADR-0052 decision 1, `message-actions.md` section 3.1.
// ---------------------------------------------------------------------------

describe("Reply", () => {
  it("is the FIRST row of the card and hands the view this message's id", () => {
    const chosen: number[] = [];
    const { items } = openMenuOn(incoming("hello"), { onReply: (id) => chosen.push(id) });
    const first = one<HTMLElement>("[role='menuitem']", items as HTMLElement);
    expect(first.textContent).toContain("Reply");
    mouse(first, "click");
    expect(chosen).toEqual([42]);
    // The menu closed with the action, as every row's does.
    expect(present("[role='menu']")).toBe(false);
  });

  function withReply(row: TimelineRow, reply: { target: number; author?: string; excerpt?: string }): TimelineRow {
    if (row.kind === "outgoing") return { ...row, reply };
    if (row.event.kind !== "application") return row;
    return { ...row, event: { ...row.event, reply } };
  }

  it("draws the carried snapshot as a named button above the text, on both sides", () => {
    for (const row of [incoming("yes"), ownMessage("yes")]) {
      document.body.innerHTML = "";
      const bubble = mount(withReply(row, { target: 41, author: "Anna", excerpt: "at five?" }));
      const quote = one<HTMLButtonElement>("button.lk-reply-quote", bubble);
      expect(quote.getAttribute("aria-label")).toBe("Reply to Anna");
      expect(one(".lk-reply-quote-name", quote).textContent).toBe("Anna");
      expect(one(".lk-reply-quote-excerpt", quote).textContent).toBe("at five?");
      // Above the text: the quote precedes the text node in the bubble.
      const text = one(".lk-bubble-text", bubble);
      expect(quote.compareDocumentPosition(text) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    }
  });

  it("a reply with no snapshot draws an empty name line and the word Message", () => {
    const bubble = mount(withReply(incoming("ok"), { target: 41 }));
    const quote = one<HTMLButtonElement>("button.lk-reply-quote", bubble);
    expect(quote.getAttribute("aria-label")).toBe("Reply");
    expect(one(".lk-reply-quote-name", quote).textContent).toBe("");
    expect(one(".lk-reply-quote-excerpt", quote).textContent).toBe("Message");
  });

  it("pressing the quote asks the view to jump to the quoted id", () => {
    const jumps: number[] = [];
    const bubble = mount(withReply(incoming("ok"), { target: 41, author: "A", excerpt: "x" }), {
      onJumpTo: (id) => jumps.push(id),
    });
    mouse(one("button.lk-reply-quote", bubble), "click");
    expect(jumps).toEqual([41]);
  });

  it("a plain message draws no quote block", () => {
    const bubble = mount(incoming("ok"));
    expect(present(".lk-reply-quote", bubble)).toBe(false);
  });

  it("the row carries its DS id, which is what a jump looks up", () => {
    mount(incoming("ok"));
    expect(one("li.lk-bubble-row").getAttribute("data-message-id")).toBe("42");
  });

  it("a media message sent as a reply keeps the ordinary bubble, so the quote has a surface", () => {
    const bubble = mount(withReply(incoming("", [voiceRef()]), { target: 41, author: "A", excerpt: "x" }));
    expect(present(".lk-reply-quote", bubble)).toBe(true);
    expect(bubble.classList.contains("chromeless")).toBe(false);
    expect(bubble.classList.contains("media-only")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Forward — ADR-0052 decision 2, `message-actions.md` sections 5 and 5.1.
// ---------------------------------------------------------------------------

describe("Forward", () => {
  function withForward(
    row: TimelineRow,
    forward: { author?: string; origin?: { groupIdB64: string; messageId: number } },
  ): TimelineRow {
    if (row.kind === "outgoing") return { ...row, forward };
    if (row.event.kind !== "application") return row;
    return { ...row, event: { ...row.event, forward } };
  }

  it("is the SECOND row of the card, after Reply, and hands the view this message's id", () => {
    const chosen: number[] = [];
    const { items } = openMenuOn(incoming("hello"), { onForward: (id) => chosen.push(id) });
    const rows = all<HTMLElement>("[role='menuitem']", items as HTMLElement);
    expect(rows[0]?.textContent).toContain("Reply");
    expect(rows[1]?.textContent).toContain("Forward");
    mouse(rows[1] as HTMLElement, "click");
    expect(chosen).toEqual([42]);
    expect(present("[role='menu']")).toBe(false);
  });

  it("draws one named line above the content, on both sides, with the arrow", () => {
    for (const row of [incoming("the words"), ownMessage("the words")]) {
      document.body.innerHTML = "";
      const bubble = mount(withForward(row, { author: "Anna" }));
      const line = one<HTMLButtonElement>("button.lk-forward-line", bubble);
      expect(line.getAttribute("aria-label")).toBe("Forwarded from Anna");
      expect(one(".lk-forward-line-text", line).textContent).toBe("Forwarded from Anna");
      expect(present("svg", line)).toBe(true);
      // Above the content: the line precedes the text node in the bubble.
      const text = one(".lk-bubble-text", bubble);
      expect(line.compareDocumentPosition(text) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
      // A label, not a person: plain text, no link.
      expect(present("a", line)).toBe(false);
    }
  });

  it("reads just Forwarded when no name was carried", () => {
    const bubble = mount(withForward(incoming("ok"), { origin: { groupIdB64: "Zw==", messageId: 3 } }));
    const line = one<HTMLButtonElement>("button.lk-forward-line", bubble);
    expect(line.getAttribute("aria-label")).toBe("Forwarded");
    expect(line.textContent).toBe("Forwarded");
  });

  it("pressing the line hands the view the carried origin, or undefined when there is none", () => {
    const seen: unknown[] = [];
    const origin = { groupIdB64: "Zw==", messageId: 3 };
    let bubble = mount(withForward(incoming("ok"), { author: "A", origin }), {
      onOpenForwardOrigin: (o) => seen.push(o),
    });
    mouse(one("button.lk-forward-line", bubble), "click");
    document.body.innerHTML = "";
    bubble = mount(withForward(incoming("ok"), { author: "A" }), {
      onOpenForwardOrigin: (o) => seen.push(o),
    });
    mouse(one("button.lk-forward-line", bubble), "click");
    expect(seen).toEqual([origin, undefined]);
  });

  it("a plain message draws no line", () => {
    const bubble = mount(incoming("ok"));
    expect(present(".lk-forward-line", bubble)).toBe(false);
  });

  it("a forwarded media message keeps the ordinary bubble, so the line has a surface", () => {
    const bubble = mount(withForward(incoming("", [voiceRef()]), { author: "A" }));
    expect(present(".lk-forward-line", bubble)).toBe(true);
    expect(bubble.classList.contains("chromeless")).toBe(false);
    expect(bubble.classList.contains("media-only")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Pin — ADR-0052 decision 3, `message-actions.md` sections 4 and 4.1.
// ---------------------------------------------------------------------------

describe("Pin", () => {
  it("follows the read-only rows (Copy, Select) and hands the view this message's id", () => {
    const chosen: number[] = [];
    const { items } = openMenuOn(incoming("hello"), { onPin: (id) => chosen.push(id) });
    const rows = all<HTMLElement>("[role='menuitem']", items as HTMLElement);
    expect(rows[1]?.textContent).toContain("Forward");
    expect(rows[3]?.textContent).toContain("Select");
    expect(rows[4]?.textContent).toContain("Pin");
    mouse(rows[4] as HTMLElement, "click");
    expect(chosen).toEqual([42]);
    expect(present("[role='menu']")).toBe(false);
  });

  it("reads Unpin when the view says the message is pinned, asked about THIS message", () => {
    const asked: number[] = [];
    const { items } = openMenuOn(ownMessage("hello"), {
      pinLabelOf: (id) => {
        asked.push(id);
        return "Unpin";
      },
    });
    expect(labelsOf(items as HTMLElement)).toContain("Unpin");
    expect(labelsOf(items as HTMLElement)).not.toContain("Pin");
    expect(asked).toContain(42);
  });
});

// ---------------------------------------------------------------------------
// Select — `message-actions.md` section 8. The rules are `selection.ts`; what
// is pinned here is the row: the mark, the whole-row toggle, the right-click
// toggle, and the menu staying shut while selecting.
// ---------------------------------------------------------------------------

describe("Select", () => {
  const MARK = ".lk-select-mark";

  it("is the FOURTH row of the card, after Copy, and hands the view this message's id", () => {
    const entered: number[] = [];
    const { items } = openMenuOn(incoming("hello"), { onSelect: (id) => entered.push(id) });
    const rows = all<HTMLElement>("[role='menuitem']", items as HTMLElement);
    expect(rows[2]?.textContent).toContain("Copy");
    expect(rows[3]?.textContent).toContain("Select");
    mouse(rows[3] as HTMLElement, "click");
    expect(entered).toEqual([42]);
    expect(present("[role='menu']")).toBe(false);
  });

  it("outside the mode there is no mark", () => {
    mount(incoming("hello"));
    expect(present(MARK)).toBe(false);
    expect(present(".lk-select-mark-slot")).toBe(false);
  });

  it("in the mode a selectable row shows the mark: a checkbox named by the text, unchecked", () => {
    mount(incoming("hello  there"), { selecting: true });
    const mark = one<HTMLElement>(MARK);
    expect(mark.getAttribute("role")).toBe("checkbox");
    expect(mark.getAttribute("aria-checked")).toBe("false");
    expect(mark.getAttribute("aria-label")).toBe("hello there");
    expect(one(".lk-bubble-row").classList.contains("selectable")).toBe(true);
  });

  it("a message with no text is named by its kind", () => {
    mount(incoming("", [voiceRef()]), { selecting: true });
    expect(one<HTMLElement>(MARK).getAttribute("aria-label")).toBe("Voice message");
  });

  it("the mark is checked, and the row tinted, when the view says this id is selected", () => {
    mount(incoming("hello"), { selecting: true, isSelected: (id) => id === 42 });
    expect(one<HTMLElement>(MARK).getAttribute("aria-checked")).toBe("true");
    expect(one(".lk-bubble-row").classList.contains("selected")).toBe(true);
  });

  it("the whole row is the tap target, and the mark toggles exactly once", () => {
    const toggled: number[] = [];
    mount(incoming("hello"), { selecting: true, onToggleSelect: (id) => toggled.push(id) });
    mouse(one(".lk-bubble-row"), "click");
    expect(toggled).toEqual([42]);
    // The mark's own click must not reach the row and toggle it back.
    mouse(one(MARK), "click");
    expect(toggled).toEqual([42, 42]);
  });

  it("a right-click toggles like a tap and opens no menu", () => {
    const toggled: number[] = [];
    const bubble = mount(incoming("hello"), {
      selecting: true,
      onToggleSelect: (id) => toggled.push(id),
    });
    mouse(bubble, "contextmenu", { clientX: 40, clientY: 60 });
    expect(present("[role='menu']")).toBe(false);
    expect(toggled).toEqual([42]);
    // And the bubble is no longer a tab stop: there is no menu to reach.
    expect(bubble.hasAttribute("tabindex")).toBe(false);
  });

  it("a deleted message is not selectable: no mark, an empty slot, no toggle", () => {
    const toggled: number[] = [];
    mount(incoming("hello"), {
      selecting: true,
      opState: () => ({ deleted: true, held: [], reactions: new Map() }),
      onToggleSelect: (id) => toggled.push(id),
    });
    expect(present(MARK)).toBe(false);
    expect(present(".lk-select-mark-slot")).toBe(true);
    mouse(one(".lk-bubble-row"), "click");
    expect(toggled).toEqual([]);
  });

  it("a message whose send failed is not selectable (the Forward gate)", () => {
    const toggled: number[] = [];
    const failed: TimelineRow = { ...ownMessage("hello"), ack: "failed" } as TimelineRow;
    mount(failed, { selecting: true, onToggleSelect: (id) => toggled.push(id) });
    expect(present(MARK)).toBe(false);
    expect(present(".lk-select-mark-slot")).toBe(true);
    mouse(one(".lk-bubble-row"), "click");
    expect(toggled).toEqual([]);
  });
});
