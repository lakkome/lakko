// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.security.SecureRandom
import java.util.Base64

/**
 * Account-id helpers.
 *
 * Mirror of `apps/web/src/auth/account-id.ts`. The Lakko auth service
 * identifies accounts by an opaque 16-byte UUIDv7 the client picks at
 * signup time. UUIDv7 (RFC 9562 §5.7) embeds a 48-bit
 * Unix-millisecond timestamp followed by 74 bits of randomness, so
 * the resulting bytes:
 *
 *   * sort lexicographically by creation time (useful in DB indices
 *     server-side);
 *   * cannot be guessed (74 bits of entropy is well above what an
 *     attacker can brute-force);
 *   * are leak-free — the timestamp is visible, but reveals only
 *     "user signed up around then", not which device / network /
 *     locale.
 *
 * The auth service treats `account_id` as opaque bytes; this is
 * purely a client-side convention.
 *
 * ## Why `java.util.Base64` and not `android.util.Base64`
 *
 * `java.util.Base64` is part of the JDK since 8 and available on
 * Android since API 26 — our `minSdk`. Using it (instead of the
 * Android-only `android.util.Base64`) means the helpers compile
 * cleanly under `testDebugUnitTest` on the host JVM, which is the
 * surface the `AuthClient` MockEngine tests in `app/src/test/`
 * exercise. The encoder/decoder pair is byte-identical to what
 * `android.util.Base64` (NO_WRAP) produces.
 */

private const val ACCOUNT_ID_LEN = 16

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
 * * `ver` = `0b0111` (UUIDv7) at byte 6 high nibble
 * * `var` = `0b10` (RFC variant) at byte 8 top two bits
 *
 * Uses `SecureRandom` for the 74 random bits — Android's default
 * implementation is backed by `/dev/urandom`, fast, and seeded from
 * the kernel CSPRNG. `SecureRandom` is thread-safe, but the cost of
 * a fresh constructor is one syscall — at signup we do it once per
 * lifetime, not worth a singleton.
 */
public fun generateAccountId(): ByteArray {
    val bytes = ByteArray(ACCOUNT_ID_LEN)
    SecureRandom().nextBytes(bytes)

    val nowMs = System.currentTimeMillis()
    bytes[0] = (nowMs.ushr(40) and 0xFF).toByte()
    bytes[1] = (nowMs.ushr(32) and 0xFF).toByte()
    bytes[2] = (nowMs.ushr(24) and 0xFF).toByte()
    bytes[3] = (nowMs.ushr(16) and 0xFF).toByte()
    bytes[4] = (nowMs.ushr(8) and 0xFF).toByte()
    bytes[5] = (nowMs and 0xFF).toByte()

    // Version 7 in the high nibble of byte 6 (low nibble stays random).
    bytes[6] = ((bytes[6].toInt() and 0x0F) or 0x70).toByte()
    // RFC variant `10xxxxxx` in the top two bits of byte 8.
    bytes[8] = ((bytes[8].toInt() and 0x3F) or 0x80).toByte()

    return bytes
}

/**
 * base64-encode raw bytes (standard alphabet, with padding) — the
 * shape every binary field on the auth wire uses.
 *
 * Single-line output (no `\n` insertion) — `java.util.Base64.getEncoder()`
 * is single-line by default, matching the server's
 * `serde_with::base64::Base64<Standard, Padded>` shape.
 */
public fun base64Encode(bytes: ByteArray): String =
    Base64.getEncoder().encodeToString(bytes)

/**
 * Decode a base64 string back to bytes. Throws `IllegalArgumentException`
 * on invalid input — the caller has already validated wire correctness
 * or trusts the source.
 */
public fun base64Decode(b64: String): ByteArray =
    Base64.getDecoder().decode(b64)

/**
 * base64url-encode raw bytes WITHOUT padding (RFC 4648 §5).
 *
 * Used by URL path segments — axum rejects the standard-alphabet
 * `+` and `/`, so the discovery service's
 * `GET /discovery/identity/{account_id}` path expects this shape.
 */
public fun base64UrlEncodeNoPad(bytes: ByteArray): String =
    Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)

/**
 * Reverse of [base64UrlEncodeNoPad]. The URL decoder accepts both
 * padded and unpadded forms.
 */
public fun base64UrlDecodeNoPad(slug: String): ByteArray =
    Base64.getUrlDecoder().decode(slug)
