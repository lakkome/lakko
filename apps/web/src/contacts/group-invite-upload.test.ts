// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `ContactsClient.createGroupInviteLink` — the short ledger link, or an
 * error.
 *
 * There is no inline form (removed 2026-10-19): any failed upload, a network
 * one included, mints no link and surfaces as an error the page shows with a
 * retry. The log names the status and never the capability.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    composeGroupInviteCapabilityWithPreview: () => ({
      sealed: new Uint8Array(160).fill(0x41),
      linkSecret: new Uint8Array(12).fill(0x07),
      free: () => {},
    }),
    // ADR-0078 section 10: the link's own revocation key.
    linkRevocationPublicKey: () => new Uint8Array(32).fill(0x0a),
    // ADR-0087 section 5: the owner derives the group's knock key.
    dsJournalRootSeed: () => new Uint8Array(32).fill(0x0b),
    groupKnockSeed: () => new Uint8Array(32).fill(0x0c),
    groupKnockPublicKey: () => new Uint8Array(32).fill(0x0d),
    // The widths the real core exports (ADR-0028 short-link amendment).
    inviteBlobIdLen: () => 6,
    inviteBlobIdUrlLen: () => 8,
    inviteLinkSecretLen: () => 12,
    inviteLinkSecretUrlLen: () => 16,
  }),
}));

import { ContactsClient } from "./client";
import {
  DiscoveryHttpError,
  DiscoveryNetworkError,
  type DiscoveryClient,
} from "../discovery/client";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

const ACCOUNT = {
  ed25519Seed: new Uint8Array(32),
  signaturePublicKey: new Uint8Array(32).fill(0x0e),
} as unknown as WasmIdentity;
/** This account owns the group, so the knock key is derived, not held. */
const GROUP = {
  groupId: new Uint8Array(32).fill(3),
  roleTableOwner: () => new Uint8Array(32).fill(0x0e),
} as unknown as WasmGroup;
const PREVIEW = { memberCount: 3 };

let uploaded: unknown[] = [];

function clientWith(upload: () => Promise<Uint8Array>): ContactsClient {
  const discovery = {
    uploadGroupInvite: vi.fn(async (body: unknown) => {
      uploaded.push(body);
      return upload();
    }),
  } as unknown as DiscoveryClient;
  return new ContactsClient(discovery, undefined, { accountIdentityProvider: () => ACCOUNT });
}

afterEach(() => {
  vi.restoreAllMocks();
  uploaded = [];
});

describe("createGroupInviteLink — pointer, or an error", () => {
  it("returns the short pointer link when the upload lands", async () => {
    const client = clientWith(async () => new Uint8Array(6).fill(0xff));
    const link = await client.createGroupInviteLink(null, GROUP, 0n, "https://x.test", PREVIEW, 7);
    expect(link).toBe("https://x.test/g/________#BwcHBwcHBwcHBwcH");
  });

  it("uploads the sealed blob, the link's own revocation key and the clear fields, never the link secret or an issuer key", async () => {
    const client = clientWith(async () => new Uint8Array(6).fill(0xff));
    await client.createGroupInviteLink(null, GROUP, 1234n, "https://x.test", PREVIEW, 7);
    expect(uploaded).toEqual([
      {
        sealed: new Uint8Array(160).fill(0x41),
        revocationPubkey: new Uint8Array(32).fill(0x0a),
        expiryMs: 1234n,
        maxUses: 7,
      },
    ]);
  });

  it("mints no link on a network failure: the error surfaces", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const client = clientWith(async () => {
      throw new DiscoveryNetworkError("/discovery/group-invite", new TypeError("offline"));
    });
    await expect(
      client.createGroupInviteLink(null, GROUP, 0n, "https://x.test", PREVIEW),
    ).rejects.toThrow();
    const logged = warn.mock.calls.map((c) => c.map(String).join(" ")).join("\n");
    expect(logged).toContain("network");
    expect(logged).not.toContain("QUFB");
  });

  it("surfaces a server refusal and logs its status, never the capability", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const client = clientWith(async () => {
      throw new DiscoveryHttpError(401, "/discovery/group-invite", "missing session");
    });
    await expect(
      client.createGroupInviteLink(null, GROUP, 0n, "https://x.test", PREVIEW),
    ).rejects.toThrow();
    const logged = warn.mock.calls.map((c) => c.map(String).join(" ")).join("\n");
    expect(logged).toContain("HTTP 401");
    // 160 bytes of 0x41 would base64 to a run of "QUFB".
    expect(logged).not.toContain("QUFB");
  });
});
