// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX `docs/ux/group-and-channel-privacy.md` 15.2 — `Add publisher` sends a
 * card in the one-to-one, so every client draws "{name} invites you to be a
 * publisher of {channel}." with `Accept` from the message alone. Found live
 * 2026-11-12: the web sent the bare link (no card), an iPhone drew a plain
 * bubble with the link and its secret, and the link carried no channel name.
 *
 * Against the REAL wasm core: the minted link's card opens as a publisher
 * invitation (kind 2) naming the channel and the inviter.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as real from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => real }));

const CHANNEL_HEX = "42".repeat(16);

vi.mock("../state/channels-state", () => ({
  channelsActions: {},
  channelsState: {
    // No metadata record held for the channel: the name comes from the
    // device's own record of it.
    metaByChannel: () => new Map(),
    channels: () => [{ id: "42".repeat(16), localName: "Walk 22 channel", localDescription: "", addedAtMs: 1 }],
  },
}));

import { coPublisherLinkChannelName, installOwnProfileEnvelopeSource, mintOneUseCoPublisherLink } from "./copublisher";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("Add publisher invitation", () => {
  it("carries a card that opens as a publisher invitation naming the channel and the inviter", async () => {
    const owner = real.Identity.fromSeed(new Uint8Array(64).fill(0x61));
    const group = real.PublisherGroup.openOrCreate(owner, new Uint8Array(16).fill(0x42));
    installOwnProfileEnvelopeSource(async () =>
      real.composeProfileEnvelope("Icons QA", undefined, 1n, undefined, undefined, undefined, undefined),
    );
    const ledger = new Uint8Array(6).fill(7);
    const minted = await mintOneUseCoPublisherLink(owner, group, "https://test.lakko.me", {
      uploadGroupInvite: async () => ledger,
    } as never);
    installOwnProfileEnvelopeSource(null);

    const opened = real.openChannelInviteCard(minted.card);
    const preview = opened.preview;
    expect(preview.subscriber).toBeUndefined();
    expect(Array.from(preview.channelId ?? [])).toEqual(Array.from(new Uint8Array(16).fill(0x42)));
    expect(preview.groupName).toBe("Walk 22 channel");
    expect(preview.inviterName).toBe("Icons QA");
    expect(Array.from(opened.ledgerId)).toEqual(Array.from(ledger));
    expect(CHANNEL_HEX).toHaveLength(32);
  });

  it("names the channel by its record, else the device's name, else its @name", () => {
    expect(coPublisherLinkChannelName(" Shared ", "Local", "handle")).toBe("Shared");
    expect(coPublisherLinkChannelName(undefined, " Local ", "handle")).toBe("Local");
    expect(coPublisherLinkChannelName("", "", "@walk22")).toBe("walk22");
    expect(coPublisherLinkChannelName(undefined, "", undefined)).toBeUndefined();
  });
});
