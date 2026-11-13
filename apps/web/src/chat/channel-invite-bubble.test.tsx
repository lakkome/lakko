// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 14 — the bubble of a channel invitation, rendered.
 *
 * Seen live 2026-11-13 (base scenario 28): after a reload the owner's web drew
 * its used-up publisher invitation as the raw `/g/` link, secret included,
 * because the bubble opened the link through the ledger and the row was gone.
 * The bubble now draws the card the message carried, opened on this device;
 * a link with no card never shows what follows `#` unless it opened as a
 * live group link. The card is composed by the REAL core; the invitation card
 * component is a stand-in that shows what it was handed (its own controls
 * need a router and a session).
 */

// @vitest-environment jsdom

import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const ledgerCalls: string[] = [];

vi.mock("./ChatContext", () => ({ useChats: () => ({ identity: () => null }) }));
vi.mock("../profile/ProfileContext", () => ({
  useProfiles: () => ({
    version: () => 0,
    resolve: (k: Uint8Array | string) => ({ name: typeof k === "string" ? k : "Member" }),
    ownProfile: () => ({ displayName: "Self" }),
    ownUsername: () => "self",
  }),
}));
vi.mock("./ChannelInviteCard", async () => {
  const { groupLinkOf } = await import("./group-link");
  return {
    groupLinkOf,
    ChannelInviteCard: (p: { opened?: { name: string; publisherInvite: boolean }; link?: { url: string } }) => {
      const el = document.createElement("div");
      el.className = "stub-channel-card";
      el.textContent = p.opened !== undefined ? `card:${p.opened.name}:${String(p.opened.publisherInvite)}` : "ledger-card";
      return el;
    },
  };
});
vi.mock("../channels/channel-invites", async (importOriginal) => {
  const real = await importOriginal<typeof import("../channels/channel-invites")>();
  return {
    ...real,
    // The ledger: every link is gone (used up, revoked).
    openChannelLink: async (url: string) => {
      ledgerCalls.push(url);
      throw Object.assign(new Error("gone"), { code: "link-invalid" });
    },
  };
});

import * as realWasm from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { createMediaHub, type MediaHub } from "../media/media-hub";
import { t } from "../l10n";
import { all, initWasm, mouse, one, present, renderUi, settle } from "../test/ui";
import { TimelineBubble, type BubbleHost } from "./TimelineBubble";
import type { TimelineRow } from "./timeline-row";

beforeAll(async () => {
  await initWasm();
});

let hub: MediaHub;
beforeEach(() => {
  ledgerCalls.length = 0;
  hub = createMediaHub({ voiceQueue: () => [] });
});

const SENT_AT = "2026-11-13T09:00:00Z";

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

function mount(row: TimelineRow): HTMLElement {
  renderUi(() => <TimelineBubble row={row} isGroup={false} startsRun={true} endsRun={true} {...host()} />);
  return one(".lk-bubble");
}

function publisherCardB64(channelName: string): string {
  const mint = realWasm.composeCoPublisherInviteCapability(
    new Uint8Array(32).fill(0x11),
    new Uint8Array(32).fill(0x22),
    0n,
    new Uint8Array(16).fill(0x42),
    channelName,
    realWasm.groupKnockPublicKey(new Uint8Array(32).fill(0x33)),
    1,
    undefined,
    "Owner",
  );
  const card = realWasm.composeChannelInviteCard(new Uint8Array(6).fill(7), mint.linkSecret, mint.sealed);
  mint.free();
  return base64Encode(card);
}

describe("a channel invitation's bubble", () => {
  it("draws a reloaded sent invitation from its stored card, ledger unreachable", async () => {
    const link = `${location.origin}/g/BwcHBwcH#c2VjcmV0c2VjcmV0`;
    const row: TimelineRow = {
      kind: "outgoing",
      tempId: -2,
      plaintext: link,
      attachments: [],
      ack: "sent",
      messageId: 42,
      createdAt: SENT_AT,
      authorKey: "own-key",
      channelInviteB64: publisherCardB64("Walk 28 channel"),
    };
    const bubble = mount(row);
    await settle();
    expect(one(".stub-channel-card", bubble).textContent).toBe("card:Walk 28 channel:true");
    expect(bubble.textContent).not.toContain("#");
    expect(bubble.textContent).not.toContain("/g/");
    expect(ledgerCalls).toEqual([]);
  });

  it("never draws the secret of a link with no card that is gone", async () => {
    const link = `${location.origin}/g/DeAdLiNk#c2VjcmV0c2VjcmV0`;
    const row: TimelineRow = {
      kind: "incoming",
      event: {
        kind: "application",
        messageId: 43,
        text: link,
        plaintext: link,
        attachments: [],
        senderLeaf: 1,
        senderKey: "peer-key",
        createdAt: SENT_AT,
      },
    };
    const bubble = mount(row);
    // While it is opened: the link without its secret.
    expect(bubble.textContent).not.toContain("#");
    await settle();
    await settle();
    expect(bubble.textContent).not.toContain("#");
    expect(bubble.textContent).toContain(t("invite.link.invalid"));
    expect(present(".stub-channel-card", bubble)).toBe(true);
  });

  // Follow-up to base scenario 26: the sender's own invitation offers no
  // Edit (it would put the link with its secret in the composer) and no
  // Copy; Delete stays.
  it("offers Delete but neither Edit, Copy nor Forward on an own invitation card", async () => {
    const row: TimelineRow = {
      kind: "outgoing",
      tempId: -3,
      plaintext: `${location.origin}/g/BwcHBwcH#c2VjcmV0c2VjcmV0`,
      attachments: [],
      ack: "sent",
      messageId: 44,
      createdAt: SENT_AT,
      authorKey: "own-key",
      channelInviteB64: publisherCardB64("Walk 26 channel"),
    };
    const bubble = mount(row);
    await settle();
    mouse(bubble, "contextmenu", { clientX: 120, clientY: 200 });
    const items = all("[role='menu'] [role='menuitem']").map((b) => b.textContent ?? "");
    expect(items.some((x) => x.includes(t("common.delete")))).toBe(true);
    expect(items.some((x) => x.includes(t("chat.message.edit")))).toBe(false);
    expect(items.some((x) => x.includes(t("common.copy")))).toBe(false);
    // Maintainer, 2026-11-13: never forwarded.
    expect(items.some((x) => x.includes(t("chat.message.forward")))).toBe(false);
  });
});
