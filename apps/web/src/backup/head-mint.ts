// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Backup-head mint client — `POST /auth/backup/head` (ADR-0037 §2, G-11).
 *
 * The auth service holds the per-account monotone `backup_version` counter and
 * signs a 129-byte `BackupHead` committing the envelope the client is about to
 * store. This is the ONE round-trip the upload path adds: after building the
 * AEAD envelope the client computes its `BLAKE3` (the wasm `backupEnvelopeHash`
 * helper), mints the head here, and frames `TAG ‖ head ‖ envelope` into the
 * single storage `PUT`.
 *
 * Bearer-authenticated: holding a valid session token IS proof the caller
 * OPAQUE-authenticated as `account_id`, which is the security basis — auth binds
 * THAT account to the client-presented envelope hash. No PoW (mirrors the
 * genesis-attest mint).
 *
 * A mint failure is **soft** on the upload side (ADR-0037 §6): the caller falls
 * back to writing the unframed legacy envelope rather than failing the whole
 * backup. So this helper simply throws on any failure and lets the seam decide.
 */

import { DEFAULT_BASE_URL } from "../auth/client";
import { base64UrlDecodeNoPad, base64UrlEncodeNoPad } from "../auth/account-id";
import { sessionFetch } from "../auth/session-fetch";

/** The minted head + its auth-assigned monotone version. */
export interface MintedBackupHead {
  /** The raw 129-byte signed `BackupHead` (decoded from `backup_head_b64`). */
  readonly head: Uint8Array;
  /** The auth-assigned monotone `backup_version` — the floor a future restore
   *  on this device lineage compares against. `bigint` to match the wasm
   *  `VerifiedBackupHead.backupVersion` and never lose precision on a u64. */
  readonly version: bigint;
}

/** `POST /auth/backup/head` response wire shape. */
interface BackupHeadResponse {
  readonly backup_head_b64: string;
  readonly backup_version: number;
}

/**
 * Mint an auth-signed backup head for `envelopeHash`.
 *
 * @param sessionToken bearer token proving the OPAQUE auth (`null` → throws, so
 *                     the soft-fail path writes the legacy envelope).
 * @param envelopeHash the 32-byte `BLAKE3` of the envelope about to be stored.
 * @param baseUrl      auth-service base (defaults to the shared `/api/auth`).
 * @throws on a null token, non-2xx, malformed response, or transport failure.
 */
export async function mintBackupHead(
  sessionToken: string | null,
  envelopeHash: Uint8Array,
  baseUrl: string = DEFAULT_BASE_URL,
): Promise<MintedBackupHead> {
  if (sessionToken === null || sessionToken.length === 0) {
    throw new Error("mintBackupHead: no session token");
  }
  const response = await sessionFetch(`${baseUrl}/auth/backup/head`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${sessionToken}`,
    },
    body: JSON.stringify({
      envelope_hash_b64: base64UrlEncodeNoPad(envelopeHash),
    }),
  });
  if (!response.ok) {
    throw new Error(`mintBackupHead: /auth/backup/head → ${response.status}`);
  }
  const wire = (await response.json()) as BackupHeadResponse;
  if (
    typeof wire.backup_head_b64 !== "string" ||
    typeof wire.backup_version !== "number"
  ) {
    throw new Error("mintBackupHead: malformed response");
  }
  const head = base64UrlDecodeNoPad(wire.backup_head_b64);
  return { head, version: BigInt(wire.backup_version) };
}
