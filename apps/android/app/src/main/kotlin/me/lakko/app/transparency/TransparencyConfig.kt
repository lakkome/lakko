// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transparency

import me.lakko.app.BuildConfig
import me.lakko.app.auth.base64Decode

/**
 * Build-time configuration for the transparency-log integration
 * (KT-log S3e — the Android mirror of the web `transparency/config.ts`).
 *
 * Two gradle-property-sourced [BuildConfig] fields, both inlined at build:
 *
 *   * [BuildConfig.LAKKO_TRANSPARENCY_URL] (gradle prop
 *     `lakko.transparency.url`) — the transparency service base URL
 *     (e.g. `https://transparency.lakko.me`, no trailing path).
 *     **Blank → the whole log check is OFF.**
 *
 *   * [BuildConfig.LAKKO_TRANSPARENCY_OPERATOR_KEY] (gradle prop
 *     `lakko.transparency.operator.key`) — base64 (standard alphabet) of
 *     the operator's 32-byte Ed25519 verifying key. This is the trust
 *     anchor for every signed root and is **pinned out-of-band at build
 *     time**. The client NEVER trusts the `/transparency/operator-key`
 *     endpoint for verification — a malicious serving tier cannot swap
 *     the operator key. **Blank / malformed → the log check is OFF** (the
 *     same skip path as a missing URL).
 *
 * The skip path is deliberately fail-OPEN for availability: the
 * transparency log is an equivocation-detection / gossip layer ON TOP of
 * the existing supersession/pin gate (ADR-0016), which remains the
 * primary, fail-closed trust mechanism. A build that has not provisioned
 * the operator key (or points at no transparency service) simply runs
 * without the extra check rather than refusing to start.
 */
object TransparencyConfig {

    /**
     * The configured transparency-service base URL, or `null` if unset.
     * Trailing slashes are trimmed so callers can append
     * `/transparency/...` cleanly. Mirror of the web `transparencyBaseUrl()`.
     */
    fun baseUrl(): String? {
        val trimmed = BuildConfig.LAKKO_TRANSPARENCY_URL.trim().trimEnd('/')
        return trimmed.ifEmpty { null }
    }

    /**
     * The build-time pinned operator verifying key (32 bytes), or `null`
     * when the [BuildConfig] field is blank / malformed / wrong-length.
     *
     * Standard-alphabet base64 (the wire shape every binary auth field
     * uses, and what `GET /transparency/operator-key` itself returns) —
     * but here it is read from the build config, never from the network.
     * Mirror of the web `pinnedOperatorKey()`.
     */
    fun pinnedOperatorKey(): ByteArray? {
        val raw = BuildConfig.LAKKO_TRANSPARENCY_OPERATOR_KEY.trim()
        if (raw.isEmpty()) return null
        return try {
            val key = base64Decode(raw)
            if (key.size == OPERATOR_KEY_LEN) key else null
        } catch (_: IllegalArgumentException) {
            null
        }
    }

    /** Ed25519 verifying-key length. */
    private const val OPERATOR_KEY_LEN: Int = 32
}
