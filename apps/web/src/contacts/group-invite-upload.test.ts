// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `ContactsClient.createGroupInviteLink` — when the short ledger link is
 * given up for the inline one.
 *
 * The inline form is for a TRANSPORT failure only (a censored or offline
 * network). A server answer — a 401 from a client with no session token, a
 * 429, a 5xx — surfaces as an error instead: silently falling back is how a
 * missing bearer token went unnoticed, every link coming out as the huge
 * inline URL. The log names the status and never the capability.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    composeGroupInviteCapabilityWithPreview: () => ({
      sealed: new Uint8Array(160).fill(0x41),
      linkSecret: new Uint8Array(12).fill(0x07),
      issuerPubkey: new Uint8Array(32).fill(0x05),
    }),
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

const ACCOUNT = { ed25519Seed: new Uint8Array(32) } as unknown as WasmIdentity;
const GROUP = { groupId: new Uint8Array(32).fill(3) } as unknown as WasmGroup;
const PREVIEW = { memberCount: 3 };

let lastUpload: unknown = null;

function clientWith(upload: () => Promise<Uint8Array>): ContactsClient {
  const discovery = {
    uploadGroupInvite: vi.fn(async (arg: unknown) => {
      lastUpload = arg;
      return upload();
    }),
  } as unknown as DiscoveryClient;
  return new ContactsClient(discovery, undefined, { accountIdentityProvider: () => ACCOUNT });
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("createGroupInviteLink — pointer, inline, or an error", () => {
  it("returns the short pointer link when the upload lands", async () => {
    const client = clientWith(async () => new Uint8Array(6).fill(0xff));
    const link = await client.createGroupInviteLink(
      null,
      GROUP,
      123n,
      "https://x.test",
      PREVIEW,
      5,
    );
    expect(link).toBe("https://x.test/g/________#BwcHBwcHBwcHBwcH");
  });

  it("uploads the sealed blob with the clear issuer key, expiry and use ceiling, never the secret", async () => {
    const client = clientWith(async () => new Uint8Array(6).fill(0xff));
    await client.createGroupInviteLink(null, GROUP, 123n, "https://x.test", PREVIEW, 5);
    expect(lastUpload).toEqual({
      sealed: new Uint8Array(160).fill(0x41),
      issuerPubkey: new Uint8Array(32).fill(0x05),
      expiryMs: 123n,
      maxUses: 5,
    });
  });

  it("falls back to the inline link on a transport failure", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const client = clientWith(async () => {
      throw new DiscoveryNetworkError("/discovery/group-invite", new TypeError("offline"));
    });
    const link = await client.createGroupInviteLink(null, GROUP, 0n, "https://x.test", PREVIEW);
    const slug = link.split("/g/")[1]?.split("#")[0] ?? "";
    expect(slug.length).toBeGreaterThan(43);
    expect(link.endsWith("#BwcHBwcHBwcHBwcH")).toBe(true);
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
