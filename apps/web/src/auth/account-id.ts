// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Account-id helpers.
 *
 * The Lakko auth service identifies accounts by an opaque 16-byte
 * UUIDv7 the client picks at signup time. UUIDv7 (RFC 9562 §5.7)
 * embeds a 48-bit Unix-millisecond timestamp followed by 74 bits of
 * randomness, so the resulting bytes:
 *
 *   * sort lexicographically by creation time (useful in DB
 *     indices server-side);
 *   * cannot be guessed (74 bits of entropy is well above what an
 *     attacker can brute-force);
 *   * are leak-free — the timestamp is visible, but reveals only
 *     "user signed up around then", not which device / network /
 *     locale.
 *
 * The auth service treats `account_id` as opaque bytes; this is
 * purely a client-side convention.
 */

const ACCOUNT_ID_LEN = 16;

/**
 * Generate a fresh UUIDv7 as raw bytes.
 *
 * Layout (RFC 9562 §5.7):
 *
 * ```text
 *   0               1               2               3
 *   0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
 *  +---------------+---------------+---------------+---------------+
 *  |                           unix_ts_ms                          |
 *  +---------------+---------------+-------+-------+---------------+
 *  |          unix_ts_ms           |  ver  |       rand_a          |
 *  +-------+-----------------------+-------+-----------------------+
 *  |var|                        rand_b                             |
 *  +-------+-------------------------------+-----------------------+
 *  |                            rand_b                             |
 *  +---------------------------------------------------------------+
 * ```
 *
 * - `ver` = `0b0111` (UUIDv7) at byte 6 high nibble
 * - `var` = `0b10` (RFC variant) at byte 8 top two bits
 */
export function generateAccountId(): Uint8Array {
  const bytes = new Uint8Array(ACCOUNT_ID_LEN);
  crypto.getRandomValues(bytes);

  // 48-bit timestamp, big-endian, into bytes 0..6.
  const nowMs = BigInt(Date.now());
  bytes[0] = Number((nowMs >> 40n) & 0xffn);
  bytes[1] = Number((nowMs >> 32n) & 0xffn);
  bytes[2] = Number((nowMs >> 24n) & 0xffn);
  bytes[3] = Number((nowMs >> 16n) & 0xffn);
  bytes[4] = Number((nowMs >> 8n) & 0xffn);
  bytes[5] = Number(nowMs & 0xffn);

  // Version 7 in the high nibble of byte 6 (low nibble stays random).
  // bytes[6] is guaranteed to exist (ACCOUNT_ID_LEN === 16 > 6) but
  // strict TS noUncheckedIndexedAccess wants the explicit narrowing.
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x70;
  // RFC variant `10xxxxxx` in the top two bits of byte 8.
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;

  return bytes;
}

/**
 * base64-encode raw bytes (standard alphabet, with padding) — the
 * shape every binary field on the auth wire uses.
 *
 * Implementation note: we call `btoa` over a binary string. The
 * intermediate string is ASCII-clean by construction (every char
 * code is in `0..256`), so the round-trip is loss-free.
 */
export function base64Encode(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i] ?? 0);
  }
  return btoa(binary);
}

/**
 * Decode a base64 string back to bytes. Throws on invalid input
 * — the caller has already validated wire correctness or trusts
 * the source.
 */
export function base64Decode(b64: string): Uint8Array {
  const binary = atob(b64);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    out[i] = binary.charCodeAt(i);
  }
  return out;
}
