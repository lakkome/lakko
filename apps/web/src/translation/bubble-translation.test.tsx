// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The translated bubble and the transcript button, rendered (UX 3.1, 3.2,
 * 4.1), with fake on-device engines and the real shared rules.
 *
 * Reachable here: the menu row's place and words, that the text inside the
 * bubble is replaced and the quiet line marks it, that Copy takes what is
 * shown and Reply the original, and that the `Aa` button's space is in the
 * row whether or not it is offered. Not reachable: any real engine, and
 * layout (jsdom computes none).
 */

// @vitest-environment jsdom

import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../chat/ChatContext", () => ({ useChats: () => ({ identity: () => null }) }));
vi.mock("../profile/ProfileContext", () => ({
  useProfiles: () => ({
    version: () => 0,
    resolve: (k: Uint8Array | string) => ({ name: typeof k === "string" ? k : "Member" }),
  }),
}));
const copied: string[] = [];
vi.mock("../chat/copy-message", () => ({
  copyMessageText: (text: string) => {
    copied.push(text);
    return Promise.resolve();
  },
}));

import * as wasm from "../../wasm/lakko_ffi_wasm";
import { TimelineBubble, type BubbleHost } from "../chat/TimelineBubble";
import type { AttachmentRefBytes } from "../chat/attachments";
import type { TimelineRow } from "../chat/timeline-row";
import { createMediaHub, mediaKey, type MediaHub } from "../media/media-hub";
import { all, initWasm, mouse, one, present, renderUi, settle } from "../test/ui";
import { TranslationContext } from "./context";
import { TranslationController } from "./controller";
import type { Engines } from "./engines";
import { MemoryResultBackend, ResultStore } from "./result-store";
import { DEFAULT_SETTINGS } from "./settings";

beforeAll(async () => {
  await initWasm();
  HTMLMediaElement.prototype.load = function load(): void {};
  HTMLMediaElement.prototype.play = function play(): Promise<void> {
    return Promise.resolve();
  };
  HTMLMediaElement.prototype.pause = function pause(): void {};
});

let hub: MediaHub;
let transcriptionReady: boolean;

function engines(): Engines {
  return {
    translation: {
      present: true,
      state: (s, t) => Promise.resolve(s === "es" && t === "en" ? "ready" : "unavailable"),
      translate: () => Promise.resolve("hello friends"),
    },
    detection: {
      present: true,
      detect: (text) =>
        Promise.resolve(
          text === "hola amigos" ? { tag: "es", permille: 950 } : text === "hello" ? { tag: "en", permille: 950 } : null,
        ),
    },
    transcription: {
      present: true,
      state: () => Promise.resolve(transcriptionReady ? "ready" : "unavailable"),
      install: () => Promise.resolve(true),
      transcribe: () => Promise.resolve({ text: "hola amigos", complete: true }),
    },
  };
}

function controller(): TranslationController {
  return new TranslationController({
    engines: engines(),
    store: new ResultStore(new MemoryResultBackend()),
    rules: () => wasm,
    accountId: () => "YWNjb3VudA==",
    interfaceLanguage: () => "en",
    settings: () => DEFAULT_SETTINGS,
    recordPack: () => undefined,
  });
}

const replies: number[] = [];

function host(): BubbleHost {
  return {
    conversationId: "Z3JvdXA=",
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
    onReply: (id) => replies.push(id),
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
      createdAt: "2026-10-25T10:00:00Z",
    },
  };
}

const BLOB_ID = new Uint8Array(32).fill(0xcd);
function voice(): AttachmentRefBytes {
  return {
    blobId: BLOB_ID,
    key: new Uint8Array(32),
    nonce: new Uint8Array(12),
    mime: "audio/mp4",
    size: 4096,
    name: undefined,
    kind: "voice",
    durationMs: 3000,
    waveform: new Uint8Array([10, 90, 200]),
  };
}

function mount(row: TimelineRow, c: TranslationController): HTMLElement {
  renderUi(() => (
    <TranslationContext.Provider value={c}>
      <TimelineBubble row={row} isGroup={false} startsRun={true} endsRun={true} {...host()} />
    </TranslationContext.Provider>
  ));
  return one(".lk-bubble");
}

async function flush(): Promise<void> {
  // The store digests with WebCrypto, which settles on a later task.
  for (let i = 0; i < 10; i++) {
    await settle();
    await new Promise((r) => setTimeout(r, 5));
  }
}

function menuLabels(): string[] {
  return all("[role='menu'] .lk-msg-menu-card [role='menuitem']").map((b) => b.textContent?.trim() ?? "");
}

beforeEach(() => {
  hub = createMediaHub({ voiceQueue: () => [] });
  hub.fileLocal(mediaKey(BLOB_ID), new Blob([new Uint8Array(16)], { type: "audio/mp4" }), "audio/mp4");
  transcriptionReady = false;
  copied.length = 0;
  replies.length = 0;
});

describe("a translated bubble", () => {
  it("offers Translate after Copy, then shows the translation marked, with the original one tap away", async () => {
    const c = controller();
    const bubble = mount(incoming("hola amigos"), c);
    await flush();
    mouse(bubble, "contextmenu", { clientX: 100, clientY: 100 });
    const labels = menuLabels();
    expect(labels).toContain("Translate");
    expect(labels.indexOf("Translate")).toBe(labels.indexOf("Copy") + 1);

    const translate = all<HTMLButtonElement>("[role='menu'] [role='menuitem']").find(
      (b) => b.textContent?.trim() === "Translate",
    );
    translate?.click();
    await flush();
    expect(one(".lk-bubble-text").textContent).toContain("hello friends");
    const line = one(".lk-translate-line").textContent ?? "";
    expect(line).toContain("Translated from Spanish");
    expect(line).toContain("Show original");

    // Copy takes what is shown; Reply names the original message.
    mouse(bubble, "contextmenu", { clientX: 100, clientY: 100 });
    expect(menuLabels()).toContain("Show original");
    all<HTMLButtonElement>("[role='menu'] [role='menuitem']").find((b) => b.textContent?.trim() === "Copy")?.click();
    expect(copied).toEqual(["hello friends"]);
    mouse(bubble, "contextmenu", { clientX: 100, clientY: 100 });
    all<HTMLButtonElement>("[role='menu'] [role='menuitem']").find((b) => b.textContent?.trim() === "Reply")?.click();
    expect(replies).toEqual([42]);

    // Show original on the line puts the original back.
    all<HTMLButtonElement>(".lk-translate-line-action").find((b) => b.textContent === "Show original")?.click();
    await flush();
    expect(one(".lk-bubble-text").textContent).toContain("hola amigos");
    expect(one(".lk-translate-line").textContent).toContain("Spanish");
  });

  it("offers no Translate row for a message in a reading language", async () => {
    const c = controller();
    const bubble = mount(incoming("hello"), c);
    await flush();
    mouse(bubble, "contextmenu", { clientX: 100, clientY: 100 });
    expect(menuLabels()).not.toContain("Translate");
  });
});

describe("the transcript button", () => {
  it("holds its space in the row when it is not offered", async () => {
    mount(incoming("", [voice()]), controller());
    await flush();
    const button = one(".lakko-voice-line .lakko-transcript-button");
    expect(button.getAttribute("data-reserved")).toBe("true");
    expect(button.getAttribute("tabindex")).toBe("-1");
  });

  it("transcribes below the row when offered", async () => {
    transcriptionReady = true;
    mount(incoming("", [voice()]), controller());
    await flush();
    const button = one<HTMLButtonElement>(".lakko-voice-line .lakko-transcript-button");
    expect(button.hasAttribute("data-reserved")).toBe(false);
    button.click();
    await flush();
    expect(present(".lakko-voice-row .lakko-transcript")).toBe(false);
    expect(one(".lakko-transcript-text").textContent).toBe("hola amigos");
    expect(one(".lakko-transcript").textContent).toContain("Transcript");
  });
});
