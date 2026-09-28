// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * G-30 #2 (ADR-0019) — web-client `leaf_auth` wiring tests.
 *
 * The transparency append now REFUSES a new
 * `(account_key -> pubkey)` binding that carries neither a genesis
 * leaf self-signature (`leaf_auth`) nor a rotation supersession
 * proof. These tests pin the CLIENT half of that contract:
 *
 *   1. `DiscoveryClient.uploadIdentityBundle` carries `leaf_auth` on
 *      the wire when (and only when) a `leafAuth` is supplied — the
 *      same omit-on-absent posture as `supersession_proof`.
 *   2. A REAL `signLeafGenesis` self-signature (the exact bytes the
 *      signup + `mintFreshIdentity` publish sites mint) is a 64-byte
 *      blob that round-trips through the request body base64-intact.
 *
 * `signLeafGenesis` runs against the REAL wasm core so the bytes under
 * test are the ones a live publish would emit. `identityUpload` is
 * stubbed to CAPTURE the request body without a network round-trip (no
 * proof of work since ADR-0054 S3).
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { signLeafGenesis } from "../../wasm/lakko_ffi_wasm";
import { DiscoveryClient } from "./client";
import type { IdentityUploadRequest } from "./wire";
import { base64Encode } from "../auth/account-id";
import { accountKey } from "../transparency/account-key";

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await init({ module_or_path: readFileSync(wasmPath) });
});

// 16-byte raw account_id (UUIDv7-shaped placeholder).
const ACCOUNT_ID = new Uint8Array([
  1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
]);
// 32-byte Ed25519 signing seed for a deterministic genesis sig.
const ED25519_SEED = new Uint8Array(32).fill(7);
const SIG_PUBKEY = new Uint8Array(32).fill(1);
const X25519_PUBKEY = new Uint8Array(32).fill(2);

/**
 * Build a `DiscoveryClient` whose `identityUpload` captures the request
 * body instead of hitting the network. Returns the client plus an
 * accessor for the last captured body.
 */
function clientCapturingUpload(): {
  client: DiscoveryClient;
  lastBody: () => IdentityUploadRequest | null;
} {
  const client = new DiscoveryClient("http://localhost:0");
  let captured: IdentityUploadRequest | null = null;

  vi.spyOn(client, "identityUpload").mockImplementation(async (req) => {
    captured = req;
    return { ok: true };
  });

  return { client, lastBody: () => captured };
}

describe("uploadIdentityBundle leaf_auth wiring (G-30 #2)", () => {
  it("omits leaf_auth when no leafAuth is supplied (routine same-key path)", async () => {
    const { client, lastBody } = clientCapturingUpload();
    await client.uploadIdentityBundle(ACCOUNT_ID, SIG_PUBKEY, X25519_PUBKEY);
    const body = lastBody();
    expect(body).not.toBeNull();
    expect(body?.leaf_auth).toBeUndefined();
    expect(body?.supersession_proof).toBeUndefined();
  });

  it("carries base64 leaf_auth when raw bytes are supplied", async () => {
    const { client, lastBody } = clientCapturingUpload();
    const leafAuth = new Uint8Array(64).fill(9);
    await client.uploadIdentityBundle(
      ACCOUNT_ID,
      SIG_PUBKEY,
      X25519_PUBKEY,
      undefined,
      leafAuth,
    );
    expect(lastBody()?.leaf_auth).toBe(base64Encode(leafAuth));
  });

  it("passes a pre-base64'd leaf_auth string through unchanged", async () => {
    const { client, lastBody } = clientCapturingUpload();
    const preEncoded = base64Encode(new Uint8Array(64).fill(3));
    await client.uploadIdentityBundle(
      ACCOUNT_ID,
      SIG_PUBKEY,
      X25519_PUBKEY,
      undefined,
      preEncoded,
    );
    expect(lastBody()?.leaf_auth).toBe(preEncoded);
  });

  it("carries a REAL genesis self-sig minted from the identity seed (publish-site shape)", async () => {
    // Mirror exactly what the signup + mintFreshIdentity sites do:
    //   leafAuth = signLeafGenesis(ed25519Seed, transparencyAccountKey(id))
    const usernameHash = await accountKey(ACCOUNT_ID);
    const genesis = Uint8Array.from(
      signLeafGenesis(ED25519_SEED, usernameHash),
    );
    expect(genesis.length).toBe(64);

    const { client, lastBody } = clientCapturingUpload();
    await client.uploadIdentityBundle(
      ACCOUNT_ID,
      SIG_PUBKEY,
      X25519_PUBKEY,
      undefined,
      genesis,
    );
    expect(lastBody()?.leaf_auth).toBe(base64Encode(genesis));
  });
});
