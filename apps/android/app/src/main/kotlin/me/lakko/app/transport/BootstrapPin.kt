// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.transport

import android.util.Base64
import me.lakko.app.BuildConfig

/**
 * ADR-0026 — the build-time-pinned bootstrap-rendezvous parameters for arming
 * the obfuscated tunnel, read from [BuildConfig] (the same pinning pattern as
 * `DeviceAttestKey` / `SfuConfig`).
 *
 * Three values, all set per build via gradle properties:
 *
 *   * [BuildConfig.LAKKO_BOOTSTRAP_TRUST_ANCHOR] — base64 of the 32-byte
 *     Ed25519 verifying key the signed bootstrap config is checked against.
 *   * [BuildConfig.LAKKO_BOOTSTRAP_DOH_DOMAIN] — the rendezvous domain resolved
 *     over JSON-DoH.
 *   * [BuildConfig.LAKKO_BOOTSTRAP_CONFIG_PATH] — the path of the signed config
 *     on that domain (e.g. `/bootstrap.json`).
 *
 * Returns `null` on any unset / blank / malformed value, so a build that did not
 * pin the anchor simply cannot arm — there is no insecure default. The trust
 * anchor is the only security-bearing value; the domain + path are transport
 * hints (acceptance hinges on the Ed25519 signature, not the path).
 */
object BootstrapPin {
    /** The decoded 32-byte Ed25519 trust anchor, or `null` if absent / malformed. */
    val trustAnchor: ByteArray?
        get() {
            val raw = BuildConfig.LAKKO_BOOTSTRAP_TRUST_ANCHOR.trim()
            if (raw.isEmpty()) return null
            return try {
                val decoded = Base64.decode(raw, Base64.DEFAULT)
                if (decoded.size == 32) decoded else null
            } catch (_: IllegalArgumentException) {
                null
            }
        }

    /** The DoH-resolved rendezvous domain, or `null` if unset / blank. */
    val dohDomain: String?
        get() = BuildConfig.LAKKO_BOOTSTRAP_DOH_DOMAIN.trim().ifEmpty { null }

    /** The signed-config path on the rendezvous domain, or `null` if unset. */
    val configPath: String?
        get() = BuildConfig.LAKKO_BOOTSTRAP_CONFIG_PATH.trim().ifEmpty { null }

    /** `true` only when all three are present — the precondition for arming. */
    val isComplete: Boolean
        get() = trustAnchor != null && dohDomain != null && configPath != null
}
