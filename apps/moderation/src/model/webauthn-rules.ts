// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The pure half of the security-key ceremonies: which listed moderator
 * answered, and what a usable PRF result is. The PRF evaluation input is the
 * core's (`moderatorPrfInput()` in the wasm), never restated here: enrolment
 * derives a moderator's listed X25519 key from the PRF output over exactly
 * those bytes, so any other input yields a key that opens no report.
 */

import { b64ToBytes, bytesEqual } from "./bytes";

/** The moderator id (base64) whose credential id equals `rawId`, or `null`. */
export function moderatorForCredential(
  rawId: Uint8Array,
  credentials: readonly { readonly moderator_id_b64: string; readonly credential_id_b64: string }[],
): string | null {
  for (const c of credentials) {
    if (bytesEqual(b64ToBytes(c.credential_id_b64), rawId)) return c.moderator_id_b64;
  }
  return null;
}

/** The 32-byte PRF output from `getClientExtensionResults()`, or `null`. */
export function prfOutput(results: unknown): Uint8Array | null {
  if (results === null || typeof results !== "object") return null;
  const prf = (results as { prf?: unknown }).prf;
  if (prf === null || typeof prf !== "object") return null;
  const first = (prf as { results?: { first?: unknown } }).results?.first;
  let bytes: Uint8Array | null = null;
  if (first instanceof ArrayBuffer) bytes = new Uint8Array(first);
  else if (ArrayBuffer.isView(first)) bytes = new Uint8Array(first.buffer, first.byteOffset, first.byteLength);
  if (bytes === null || bytes.length !== 32) return null;
  return new Uint8Array(bytes);
}
