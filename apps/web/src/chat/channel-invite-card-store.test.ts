// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 14 — a channel invitation's card is kept with the message
 * and read on this device, with no network.
 *
 * Seen live 2026-11-13 (base scenario 28): an owner on web added a publisher;
 * the Chats row read "You: Invitation to publish in Walk 28 channel" and the
 * bubble was a card. The publisher link is one-use and its ledger row goes on
 * approval, so after a reload the web, which opened the link through the
 * ledger, showed the bubble AND the row as the raw `/g/` link, secret
 * included. The card, the envelope and the parse are the REAL core's; the
 * ledger is a stand-in that is never reachable.
 */
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as realWasm from "../../wasm/lakko_ffi_wasm";

const ledgerCalls: string[] = [];
const ledger = new Map<string, unknown>();

vi.mock("../wasm", async () => {
  const ns = await import("../../wasm/lakko_ffi_wasm");
  return { loadWasm: async () => ns, wasmSync: () => ns };
});
vi.mock("../channels/channel-invites", async (importOriginal) => {
  const real = await importOriginal<typeof import("../channels/channel-invites")>();
  return {
    ...real,
    // The ledger: every link the test does not name is gone.
    openChannelLink: async (url: string) => {
      ledgerCalls.push(url);
      const r = ledger.get(url);
      if (r instanceof Error) throw r;
      if (r === undefined) throw Object.assign(new Error("gone"), { code: "link-invalid" });
      return r;
    },
  };
});

import { parseBodyEnvelopeWith } from "./attachments";
import { eventToPersistedMessage, summarizeGroupSlot } from "./background-log";
import { channelInviteRowPreview } from "./channel-invite-preview";
import type { IncomingChatEvent } from "./client";
import type { PersistedMessage } from "./message-log-store";
import { persistedWithOps } from "./message-ops-store";
import { t } from "../l10n";

const ORIGIN = "https://test.lakko.me";
const settle = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

beforeEach(() => {
  ledgerCalls.length = 0;
});

/** A publisher invitation as the owner's web sends it: the `/g/` link as the
 *  text and the card beside it. */
function publisherInvitation(channelName: string): { envelope: Uint8Array; link: string } {
  const knockSeed = new Uint8Array(32).fill(0x33);
  const mint = realWasm.composeCoPublisherInviteCapability(
    new Uint8Array(32).fill(0x11),
    new Uint8Array(32).fill(0x22),
    0n,
    new Uint8Array(16).fill(0x42),
    channelName,
    realWasm.groupKnockPublicKey(knockSeed),
    1,
    undefined,
    "Owner",
  );
  const ledgerId = new Uint8Array(6).fill(7);
  const card = realWasm.composeChannelInviteCard(ledgerId, mint.linkSecret, mint.sealed);
  const link = `${ORIGIN}/g/BwcHBwcH#c2VjcmV0c2VjcmV0`;
  mint.free();
  return { envelope: realWasm.composeChannelInviteEnvelope(link, card), link };
}

/** The message as the receive path files it, then read back off disk. */
function receivedAndReloaded(envelope: Uint8Array): PersistedMessage {
  const body = parseBodyEnvelopeWith(realWasm, envelope);
  if (body.kind !== "chat") throw new Error("not a chat body");
  const event: Extract<IncomingChatEvent, { kind: "application" }> = {
    kind: "application",
    messageId: 42,
    text: body.text,
    attachments: body.attachments,
    plaintext: body.text,
    senderLeaf: 1,
    senderKey: undefined,
    createdAt: new Date(42_000).toISOString(),
    ...(body.channelInvite === undefined ? {} : { channelInvite: body.channelInvite }),
  };
  return JSON.parse(JSON.stringify(eventToPersistedMessage(event))) as PersistedMessage;
}

describe("a stored invitation card", () => {
  it("reads as the invitation after a reload, with the ledger unreachable", () => {
    const { envelope, link } = publisherInvitation("Walk 28 channel");
    const stored = receivedAndReloaded(envelope);
    expect(stored.text).toBe(link);
    expect(typeof stored.channelInviteB64).toBe("string");

    const summary = summarizeGroupSlot({ cursor: "42", messages: [stored] }, null);
    expect(summary.previewChannelInviteB64).toBe(stored.channelInviteB64);
    const row = channelInviteRowPreview(summary.preview, ORIGIN, summary.previewChannelInviteB64);
    expect(row).toBe(t("chats.preview.publisher_invite", { channel: "Walk 28 channel" }));
    expect(row).not.toContain("#");
    // Read from the card alone: the used-up link was never asked about.
    expect(ledgerCalls).toEqual([]);
  });

  it("goes with the message when it is deleted (the card holds the secret)", () => {
    const stored = receivedAndReloaded(publisherInvitation("Walk 28 channel").envelope);
    const deleted = persistedWithOps(stored, { deleted: true, held: [], reactions: [] } as never);
    expect(deleted.deleted).toBe(true);
    expect(deleted.channelInviteB64).toBeUndefined();
    expect(summarizeGroupSlot({ cursor: "42", messages: [deleted] }, null).previewChannelInviteB64).toBeUndefined();
  });
});

describe("a `/g/` link with no card", () => {
  it("never yields text with the secret, while opened and once it is gone", async () => {
    const link = `${ORIGIN}/g/DeAdLiNk#c2VjcmV0c2VjcmV0`;
    const opening = channelInviteRowPreview(link, ORIGIN);
    expect(opening).toBe(`${ORIGIN}/g/DeAdLiNk`);
    await settle();
    const gone = channelInviteRowPreview(link, ORIGIN);
    expect(gone).toBe(t("invite.link.invalid"));
    expect(gone).not.toContain("#");
  });

  it("is the link without its secret when the ledger does not answer", async () => {
    const link = `${ORIGIN}/g/OfFlInE1#c2VjcmV0c2VjcmV0`;
    ledger.set(link, Object.assign(new Error("offline"), { code: "offline" }));
    channelInviteRowPreview(link, ORIGIN);
    await settle();
    expect(channelInviteRowPreview(link, ORIGIN)).toBe(`${ORIGIN}/g/OfFlInE1`);
  });

  it("is the link without its secret when it opens as a group's", async () => {
    const link = `${ORIGIN}/g/GrOuPlNk#c2VjcmV0c2VjcmV0`;
    ledger.set(link, null);
    channelInviteRowPreview(link, ORIGIN);
    await settle();
    expect(channelInviteRowPreview(link, ORIGIN)).toBe(`${ORIGIN}/g/GrOuPlNk`);
  });
});
