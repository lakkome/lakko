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

    /**
     * The build-time pinned WITNESS verifying key (32 bytes), or `null`
     * when the [BuildConfig] field is blank / malformed / wrong-length
     * (ADR-0020 split-view defence).
     *
     * Standard-alphabet base64, read from the build config (never the
     * network — like the operator key). Retained as the BACKWARD-COMPAT
     * source for [pinnedWitnessKeys]: a build pinning a single witness via
     * the pre-ADR-0039 `LAKKO_TRANSPARENCY_WITNESS_KEY` gradle property keeps
     * working (the key-set helper folds it into a one-element set at
     * threshold 1). New builds use the `..._WITNESS_KEYS` set below. Mirror
     * of the web `pinnedWitnessKey()`.
     */
    fun pinnedWitnessKey(): ByteArray? {
        val raw = BuildConfig.LAKKO_TRANSPARENCY_WITNESS_KEY.trim()
        if (raw.isEmpty()) return null
        return try {
            val key = base64Decode(raw)
            if (key.size == OPERATOR_KEY_LEN) key else null
        } catch (_: IllegalArgumentException) {
            null
        }
    }

    /**
     * The build-time pinned WITNESS verifying key SET (each 32 bytes) for the
     * K-of-N witness QUORUM gate (ADR-0039, G-30 finding-1). Read from
     * [BuildConfig.LAKKO_TRANSPARENCY_WITNESS_KEYS] — a comma-separated list
     * of base64 (url-safe or standard alphabet; padding optional) 32-byte
     * Ed25519 verifying keys.
     *
     * Parsing is lenient in the same spirit as [pinnedWitnessKey] (which
     * mapped a bad key to "unconfigured"): a blank entry is skipped, and an
     * entry that does not decode to exactly 32 bytes is DROPPED. Dropping is
     * fail-SAFE — a smaller pinned-key pool can only make the quorum HARDER
     * to satisfy (fewer distinct keys can contribute), never easier; a set
     * that shrinks below the threshold simply makes the gate refuse every
     * root (fail-closed).
     *
     * Returns `[]` when unset / all-blank / all-malformed → the witness gate
     * is OFF (dormant, the operator-only transition posture, exactly the
     * single-key no-key case).
     *
     * BACKWARD-COMPAT: when `..._WITNESS_KEYS` is unset (or contributes no
     * valid key) but the OLD singular `..._WITNESS_KEY` is set, that one key
     * is returned as a one-element set (pair it with the default
     * [witnessThreshold] of 1) — so an existing single-witness build keeps
     * working unchanged. Mirror of the web `pinnedWitnessKeys()`.
     */
    fun pinnedWitnessKeys(): List<ByteArray> {
        val raw = BuildConfig.LAKKO_TRANSPARENCY_WITNESS_KEYS.trim()
        if (raw.isNotEmpty()) {
            val keys = mutableListOf<ByteArray>()
            for (part in raw.split(',')) {
                val entry = part.trim()
                if (entry.isEmpty()) continue // skip blanks
                try {
                    val key = decodeBase64Lenient(entry)
                    if (key.size == OPERATOR_KEY_LEN) keys.add(key) // drop wrong-length
                } catch (_: IllegalArgumentException) {
                    // drop an undecodable entry (mirrors pinnedWitnessKey's leniency)
                }
            }
            if (keys.isNotEmpty()) return keys
        }
        // Backward-compat: fall back to the OLD singular pin as a 1-element set.
        return pinnedWitnessKey()?.let { listOf(it) } ?: emptyList()
    }

    /**
     * The K-of-N witness quorum THRESHOLD (ADR-0039) — the minimum number of
     * DISTINCT pinned witnesses that must each have co-signed the served
     * root. Read from [BuildConfig.LAKKO_TRANSPARENCY_WITNESS_THRESHOLD];
     * defaults to `1` (unset / unparseable / non-positive). Only consulted
     * when [pinnedWitnessKeys] is non-empty. A threshold LARGER than the
     * pinned-key count makes the quorum unsatisfiable → the gate fails closed
     * (refuses every root), the correct posture for that misconfiguration.
     * Mirror of the web `witnessThreshold()`.
     */
    fun witnessThreshold(): Int {
        val n = BuildConfig.LAKKO_TRANSPARENCY_WITNESS_THRESHOLD.trim().toIntOrNull()
        return if (n != null && n >= 1) n else 1
    }

    /**
     * Lenient base64 decode accepting BOTH the url-safe (`-` / `_`) and
     * standard (`+` / `/`) alphabets, with optional padding — the faithful
     * mirror of the web `base64UrlDecodeNoPad` (which maps url-safe →
     * standard, pads, then standard-decodes). Used for the witness key SET
     * so a deployer may paste either alphabet. Throws
     * `IllegalArgumentException` on truly invalid input (caught + dropped by
     * the caller).
     */
    private fun decodeBase64Lenient(entry: String): ByteArray {
        val standard = entry.replace('-', '+').replace('_', '/')
        val padded = standard.padEnd(standard.length + (4 - standard.length % 4) % 4, '=')
        return base64Decode(padded)
    }

    /** Ed25519 verifying-key length. */
    private const val OPERATOR_KEY_LEN: Int = 32
}
