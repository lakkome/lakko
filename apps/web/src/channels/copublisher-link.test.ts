// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The co-publisher `/g/` link is uploaded to the ledger like a group link
 * (the ADR-0028 sealed-link amendment), as iOS and Android do. Only a
 * transport failure falls back to the inline link; a refusal from the server
 * is surfaced, never hidden behind an inline link. Runs against the real wasm
 * core, so the minted blob is opened under the minted secret.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", async () => {
  const mod = await import("../../wasm/lakko_ffi_wasm");
  return { loadWasm: async () => mod };
});

import { createCoPublisherLink } from "./copublisher";
import { DiscoveryNetworkError } from "../discovery/client";
import {
  inviteLinkWidths,
  splitInviteLink,
  type InviteLinkWidths,
} from "../contacts/invite-link";
import { base64UrlDecodeNoPad } from "../auth/account-id";
import type { Identity, PublisherGroup } from "../../wasm/lakko_ffi_wasm";

const ORIGIN = "https://lakko.me";
let W: InviteLinkWidths;

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
  W = inviteLinkWidths(wasm);
});

const GROUP_ID = new Uint8Array(32).fill(0x5c);
const identity = { ed25519Seed: new Uint8Array(32).fill(0x11) } as unknown as Identity;
const publisherGroup = { groupId: () => GROUP_ID } as unknown as PublisherGroup;

interface Upload {
  readonly sealed: Uint8Array;
  readonly issuerPubkey: Uint8Array;
  readonly expiryMs: bigint;
  readonly maxUses: number;
}

function secretOf(link: string): Uint8Array {
  const split = splitInviteLink(link, "g", W);
  if (split === null || split.fragment.kind !== "secret") throw new Error(`no secret in ${link}`);
  return split.fragment.secret;
}

describe("the co-publisher link", () => {
  it("is uploaded and comes back as a pointer link that opens to the channel's group", async () => {
    const uploads: Upload[] = [];
    const blobId = new Uint8Array(W.blobIdLen).fill(0x21);
    const link = await createCoPublisherLink(identity, publisherGroup, ORIGIN, {
      uploadGroupInvite: async (u: Upload) => {
        uploads.push(u);
        return blobId;
      },
    } as never);

    expect(uploads).toHaveLength(1);
    const split = splitInviteLink(link, "g", W);
    expect(split?.slug.length).toBe(W.blobIdUrlLen);
    const secret = secretOf(link);
    expect(secret.length).toBe(W.linkSecretLen);
    // The secret travels in the fragment only, never in the upload.
    const uploaded = uploads[0]!;
    expect(Buffer.from(uploaded.sealed).includes(Buffer.from(secret))).toBe(false);
    expect(uploaded.issuerPubkey.length).toBe(32);
    // The uploaded blob opens under the link's secret to this channel's group.
    const cap = wasm.GroupInviteCapability.openLink(uploaded.sealed, secret);
    expect(Array.from(cap.groupId)).toEqual(Array.from(GROUP_ID));
  });

  it("falls back to the inline link only when the ledger cannot be reached", async () => {
    const link = await createCoPublisherLink(identity, publisherGroup, ORIGIN, {
      uploadGroupInvite: async () => {
        throw new DiscoveryNetworkError("group-invite", new Error("offline"));
      },
    } as never);
    const split = splitInviteLink(link, "g", W);
    expect(split).not.toBeNull();
    const sealed = base64UrlDecodeNoPad(split!.slug);
    const cap = wasm.GroupInviteCapability.openLink(sealed, secretOf(link));
    expect(Array.from(cap.groupId)).toEqual(Array.from(GROUP_ID));
  });

  it("surfaces a refusal from the server instead of minting an inline link", async () => {
    await expect(
      createCoPublisherLink(identity, publisherGroup, ORIGIN, {
        uploadGroupInvite: async () => {
          throw new Error("discovery 401");
        },
      } as never),
    ).rejects.toThrow("discovery 401");
  });
});
