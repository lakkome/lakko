// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 section 3.7 - the rotating device's half of the two-way
 * conversation link: signed by the OLDEST own predecessor and the new key,
 * through the real core, and carried on the identity upload.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it, vi } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { DiscoveryClient } from "../discovery/client";
import type { IdentityUploadRequest } from "../discovery/wire";
import { base64Decode, base64Encode } from "./account-id";
import { mintConversationLink } from "./conversation-link-mint";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const seed = (fill: number): Uint8Array => new Uint8Array(64).fill(fill);
const seedB64 = (fill: number): string => base64Encode(seed(fill));
const keyOf = (fill: number): Uint8Array => wasm.Identity.fromSeed(seed(fill)).signaturePublicKey;

describe("mintConversationLink", () => {
  it("names the key just left on a first rotation", () => {
    const link = mintConversationLink(wasm, [seedB64(1)], seed(2), 1_700_000_000);
    expect(link).not.toBeNull();
    expect(wasm.verifyConversationLink(link as Uint8Array, keyOf(2))).toEqual(keyOf(1));
  });

  it("names the OLDEST key on a later rotation: the conversations have not moved", () => {
    const link = mintConversationLink(wasm, [seedB64(1), seedB64(2)], seed(3), 1_700_000_000);
    expect(wasm.verifyConversationLink(link as Uint8Array, keyOf(3))).toEqual(keyOf(1));
  });

  it("does not verify against any key but the new one", () => {
    const link = mintConversationLink(wasm, [seedB64(1)], seed(2), 1_700_000_000);
    expect(() => wasm.verifyConversationLink(link as Uint8Array, keyOf(1))).toThrow();
  });

  it("has nothing to link with no predecessor, or when the predecessor is the current key", () => {
    expect(mintConversationLink(wasm, [], seed(2), 1_700_000_000)).toBeNull();
    expect(mintConversationLink(wasm, [seedB64(2)], seed(2), 1_700_000_000)).toBeNull();
  });
});

describe("the identity upload", () => {
  function capturing(): { client: DiscoveryClient; body: () => IdentityUploadRequest | null } {
    const client = new DiscoveryClient("http://localhost:0");
    let body: IdentityUploadRequest | null = null;
    vi.spyOn(
      client as unknown as { identityUpload(b: IdentityUploadRequest): Promise<{ ok: boolean }> },
      "identityUpload",
    ).mockImplementation(async (b) => {
      body = b;
      return { ok: true };
    });
    return { client, body: () => body };
  }

  it("carries the link in standard base64, beside the proof", async () => {
    const { client, body } = capturing();
    const link = mintConversationLink(wasm, [seedB64(1)], seed(2), 1_700_000_000) as Uint8Array;
    await client.uploadIdentityBundle(
      new Uint8Array(16).fill(1),
      keyOf(2),
      new Uint8Array(32).fill(3),
      new Uint8Array(64).fill(5),
      new Uint8Array(145).fill(4),
      undefined,
      undefined,
      undefined,
      link,
    );
    expect(base64Decode((body() as IdentityUploadRequest).conversation_link as string)).toEqual(link);
  });

  it("omits the field on an upload that carries none", async () => {
    const { client, body } = capturing();
    await client.uploadIdentityBundle(new Uint8Array(16).fill(1), keyOf(2), new Uint8Array(32).fill(3), new Uint8Array(64).fill(5));
    expect(body() as IdentityUploadRequest).not.toHaveProperty("conversation_link");
  });
});
