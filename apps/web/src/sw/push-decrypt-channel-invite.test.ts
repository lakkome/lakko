// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 14, UX `docs/ux/group-and-channel-privacy.md` 15.2 — a
 * message carrying a channel invitation card is notified like its card
 * ("Invitation to publish in {channel}"), never as its text, which is the
 * `/g/` link WITH ITS SECRET. Seen live 2026-11-12 on an iPhone's Chats row;
 * the background notification read the same text.
 *
 * The card and the envelope are composed and parsed by the REAL core; the MLS
 * group, the IDB reads and the DS are stand-ins.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as realWasm from "../../wasm/lakko_ffi_wasm";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { decryptPushPreview } from "../../public/sw-push-decrypt.js";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const GROUP_HEX = "ac".repeat(32);
const GROUP_BYTES = new Uint8Array(32).fill(0xac);
const SENDER_KEY = new Uint8Array(32).fill(0x5e);
const LINK = "https://test.lakko.me/g/BwcHBwcH";

function publisherCardMessage(channelName: string | undefined): Uint8Array {
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
    "Icons QA",
  );
  const card = realWasm.composeChannelInviteCard(new Uint8Array(6).fill(7), mint.linkSecret, mint.sealed);
  mint.free();
  return realWasm.composeChannelInviteEnvelope(`${LINK}#secretsecret1234`, card);
}

function deps(payload: Uint8Array) {
  return {
    readWakeBinding: async () => ({ accountId: "acct" }),
    lookupSenderName: async () => "Icons QA",
    lookupRoute: async () => ({
      accountId: "acct",
      groupIdB64: Buffer.from(GROUP_BYTES).toString("base64"),
      subIdHex: "dd".repeat(16),
    }),
    getDeviceKey: async () => ({}),
    getIdentitySeed: async () => new Uint8Array(64),
    loadSnapshot: async () => ({ snapshotBytes: new Uint8Array([1]), knownGroupIds: [] }),
    wasm: {
      Identity: { fromSeed: () => ({ restoreStorage: () => undefined, free: () => undefined }) },
      Group: {
        load: () => ({
          processIncomingVerifyingRoles: (_i: unknown, p: Uint8Array) => ({
            kind: "Application",
            applicationData: p,
            senderLeaf: 2,
          }),
          members: () => [{ leafIndex: 2, signaturePublicKey: SENDER_KEY }],
          free: () => undefined,
        }),
      },
      parseBodyEnvelope: (bytes: Uint8Array) => realWasm.parseBodyEnvelope(bytes),
      openChannelInviteCard: (card: Uint8Array) => realWasm.openChannelInviteCard(card),
    },
    dsFetchGroup: async () => ({ messages: [{ id: 42, payload, createdAt: new Date(42_000).toISOString() }] }),
    saveSnapshot: () => undefined,
    saveMessageLog: () => undefined,
    advanceCursor: () => undefined,
  };
}

const wake = { subHex: "dd".repeat(16), groupHex: GROUP_HEX, pointer: 42 };

describe("a channel invitation's notification", () => {
  it("reads like its card and never carries the link or its secret", async () => {
    const preview = await decryptPushPreview(wake, deps(publisherCardMessage("Walk 22 channel")));
    expect(preview?.body).toContain("Invitation to publish in Walk 22 channel");
    expect(preview?.body).not.toContain("/g/");
    expect(preview?.body).not.toContain("secret");
  });

  it("names a channel with no name by the generic word", async () => {
    const preview = await decryptPushPreview(wake, deps(publisherCardMessage(undefined)));
    expect(preview?.body).toContain("Invitation to publish in Channel");
  });
});
