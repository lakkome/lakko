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
    actionsAvailable: true,
    hub,
    mediaUpload: undefined,
    onRetrySend: () => {},
    fullReactPickerId: null,
    onOpenFullReactPicker: () => {},
    onCloseFullReactPicker: () => {},
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
    // Section 2: the ones that only read (Copy), then the ones that change this
    // message (Edit), then the destructive one.
    expect(labels).toEqual(["Copy", "Edit", "Delete"]);
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
    // An incoming message's action card has exactly the one row.
    const copy = one("[role='menuitem']", items as HTMLElement);
    expect(copy.textContent).toContain("Copy");
    mouse(copy, "click");
    await settle();
    expect(written).toEqual(["the exact words"]);
  });

  it("copies what the person is reading, not the original, once edited", async () => {
    const { items } = openMenuOn(incoming("before"), {
      opState: () => ({ editedText: "after", reactions: new Map() }),
    });
    mouse(one("[role='menuitem']", items as HTMLElement), "click");
    await settle();
    expect(written).toEqual(["after"]);
  });

  it("is offered on somebody else's message too", () => {
    // Not the Edit / Delete predicate: section 3 offers Copy on "any message
    // with text", and an incoming message has no Edit row at all.
    const { items } = openMenuOn(incoming("hello"));
    expect(all("[role='menuitem']", items as HTMLElement).length).toBe(1);
    expect((items as HTMLElement).textContent).toContain("Copy");
  });

  it("is absent on a message with no text", () => {
    // A lone voice message: there is nothing to put on the clipboard, and an
    // action that copies an empty string is worse than one that is not there.
    const { items } = openMenuOn(ownMessage("", [voiceRef()]));
    expect(present(".lk-msg-menu-items")).toBe(true);
    expect((items as HTMLElement).textContent).not.toContain("Copy");
    expect(all("[role='menuitem'] span", items as HTMLElement).map((s) => s.textContent)).toEqual([
      "Edit",
      "Delete",
    ]);
  });

  it("leaves an incoming media message no action card at all", () => {
    // Nothing to copy and nothing to edit, so the second surface is not drawn:
    // an empty bordered card under the reactions reads as a menu that failed.
    const { items } = openMenuOn(incoming("", [voiceRef()]));
    expect(items).toBeNull();
    expect(present("[role='group'][aria-label='React']")).toBe(true);
  });
});
