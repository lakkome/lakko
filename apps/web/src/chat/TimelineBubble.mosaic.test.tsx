// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/media-messages.md` section 11 — one message with text and several
 * photos, rendered: one bubble, the mosaic first (one tile per photo), the
 * text below it, and the time in the pill only when there is no text.
 */

// @vitest-environment jsdom

import { beforeAll, describe, expect, it, vi } from "vitest";

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
import { createMediaHub } from "../media/media-hub";
import type { AttachmentRefBytes } from "./attachments";
import type { TimelineRow } from "./timeline-row";
import { all, initWasm, one, renderUi } from "../test/ui";

beforeAll(async () => {
  await initWasm();
  // No IntersectionObserver in jsdom: a tile simply never starts loading.
  (globalThis as { IntersectionObserver?: unknown }).IntersectionObserver ??= class {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  };
});

function photo(n: number, w = 1600, h = 1200): AttachmentRefBytes {
  return {
    blobId: new Uint8Array(32).fill(n),
    key: new Uint8Array(32),
    nonce: new Uint8Array(12),
    mime: "image/jpeg",
    size: 1000,
    name: `p${n}.jpg`,
    width: w,
    height: h,
  };
}

function own(text: string, attachments: AttachmentRefBytes[]): TimelineRow {
  return {
    kind: "outgoing",
    tempId: 7,
    plaintext: text,
    attachments,
    ack: "sent",
    messageId: 42,
    createdAt: "2026-11-19T10:00:00Z",
    authorKey: "own-key",
  };
}

function mount(row: TimelineRow): HTMLElement {
  const hub = createMediaHub({ voiceQueue: () => [] });
  const host = {
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
    reactionRowFor: () => [],
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
  } as unknown as BubbleHost;
  renderUi(() => <TimelineBubble row={row} isGroup={false} startsRun={true} endsRun={true} {...host} />);
  return one(".lk-bubble");
}

describe("a message with several photos", () => {
  it("draws one mosaic, one tile per photo, the text below it", () => {
    const bubble = mount(own("Look at these", [photo(1), photo(2), photo(3), photo(4), photo(5)]));
    expect(all(".lk-mosaic")).toHaveLength(1);
    expect(all(".lk-mosaic-tile")).toHaveLength(5);
    expect(bubble.classList.contains("has-mosaic")).toBe(true);
    expect(bubble.classList.contains("media-only")).toBe(false);
    const children = Array.from(bubble.children);
    const mosaicAt = children.findIndex((c) => c.classList.contains("lk-mosaic"));
    const textAt = children.findIndex((c) => c.classList.contains("lk-bubble-text"));
    expect(mosaicAt).toBeGreaterThanOrEqual(0);
    expect(textAt).toBeGreaterThan(mosaicAt);
  });

  it("puts the time in the pill when there is no text", () => {
    const bubble = mount(own("", [photo(1), photo(2), photo(3)]));
    expect(bubble.classList.contains("media-only")).toBe(true);
    expect(all(".lk-mosaic-tile")).toHaveLength(3);
  });

  it("places each tile where the shared rule says, in percentages", () => {
    mount(own("", [photo(1), photo(2)]));
    const tiles = all<HTMLElement>(".lk-mosaic-tile");
    expect(tiles[0]?.style.getPropertyValue("inset-inline-start")).toBe("0%");
    expect(Number.parseFloat(tiles[1]?.style.getPropertyValue("inset-inline-start") ?? "0")).toBeGreaterThan(49);
  });
});
