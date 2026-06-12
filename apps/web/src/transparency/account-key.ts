// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Transparency-log account-key derivation (KT-log S3c).
 *
 * The log's opaque leaf field `username_hash` carries, for the
 * discovery integration,
 *
 * ```text
 *   BLAKE3("lakko/transparency/account/v1" || account_id)
 * ```
 *
 * — a 32-byte key derivable by BOTH discovery (on append, see
 * `services/discovery/src/transparency_log.rs::account_key`) AND a
 * client (on verify), because `account_id` is already public. The web
 * client needs it to (a) query `GET /transparency/latest/:account_key`
 * and (b) bind the exact `entry_username_hash` into
 * `verifyLogInclusion`.
 *
 * The derivation is done in the **audited Rust core** via the
 * `transparencyAccountKey` wasm-bindgen export — NOT a bundled JS
 * BLAKE3 — so it cannot drift from the server's `account_key(...)`.
 * The domain string lives once in Rust on each side; this module is a
 * thin async wrapper that loads wasm and copies the result out.
 */

import { loadWasm } from "../wasm";

/**
 * Derive the transparency-log account-key for the raw 16-byte
 * `accountId`. Returns the 32-byte BLAKE3 digest.
 *
 * @throws if `accountId` is not 16 bytes (a programmer error — the
 *   caller always has the canonical UUIDv7).
 */
export async function accountKey(accountId: Uint8Array): Promise<Uint8Array> {
  if (accountId.length !== 16) {
    throw new Error(
      `accountId must be 16 bytes (got ${accountId.length})`,
    );
  }
  const wasm = await loadWasm();
  // Copy out of the wasm-owned buffer — the export already returns a
  // fresh `Vec<u8>`, but keep a defensive copy so callers can retain
  // the bytes past the next wasm call.
  return Uint8Array.from(wasm.transparencyAccountKey(accountId));
}
