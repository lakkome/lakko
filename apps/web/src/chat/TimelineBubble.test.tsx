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
 * The two context hooks are replaced because they are name resolvers, not part
 * of either defect: reaching them would mean standing up an MLS identity and a
 * profile store to answer a question about a `contextmenu` handler.
 */

// @vitest-environment jsdom

import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./ChatContext", () => ({ useChats: () => ({ identity: () => null }) }));
vi.mock("../profile/ProfileContext", () => ({
  useProfiles: () => ({
    version: () => 0,
    resolve: (k: Uint8Array | string) => ({ name: typeof k === "string" ? k : "Member" }),
  }),
}));

import { TimelineBubble, type BubbleHost } from "./TimelineBubble";
import { LONG_PRESS_MS } from "./MessageMenu";
import { createMediaHub, mediaKey, type MediaHub } from "../media/media-hub";
import { shortTime } from "./timeline-layout";
import type { AttachmentRefBytes } from "./attachments";
import type { TimelineRow } from "./timeline-row";
import { all, initWasm, mouse, one, pointer, present, renderUi } from "../test/ui";

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
