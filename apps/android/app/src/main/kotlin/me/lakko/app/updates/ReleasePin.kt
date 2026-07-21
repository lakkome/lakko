// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.updates

import me.lakko.app.BuildConfig
import me.lakko.app.auth.base64UrlDecodeNoPad

/**
 * ADR-0038 S4 — the build-time-pinned "update available" notice parameters, read
 * from [BuildConfig] (the same pinning pattern as `BootstrapPin` / `DeviceAttestKey`
 * / `SfuConfig`). Mirror of the iOS `ReleasePin`.
 *
 * Five values, all set per build via gradle properties:
 *
 *   * [BuildConfig.LAKKO_RELEASE_KEY] — base64url (no-pad) of the 32-byte Ed25519
 *     verifying key the signed release manifest is checked against (the dedicated,
 *     OFFLINE **publisher** key, ADR-0038 §3). **Empty / blank / malformed →
 *     [verifyingKey] is `null` and the whole feature is DORMANT** (no fetch, no
 *     verify, no banner). There is no serving-tier fallback (unlike
 *     `DeviceAttestKey`'s JWKS): the release key is pin-only, so an unpinned build
 *     simply has the notice off — fail-safe-toward-silence.
 *   * [BuildConfig.LAKKO_RELEASE_MANIFEST_URL] — the manifest URL (a raw 89-byte
 *     signed blob), default [DEFAULT_MANIFEST_URL].
 *   * [BuildConfig.LAKKO_PLAY_STORE_URL] — the outbound Play deep-link. **Default
 *     empty** because no production Play listing exists yet; when empty the Update
 *     button is hidden and (per the S4 gate) the banner stays dormant unless
 *     [devOverride] is set.
 *   * [BuildConfig.LAKKO_CURRENT_RELEASE] — this build's monotone release integer
 *     (default 1), compared as an integer against the manifest's `latest_release` /
 *     `min_supported_release`.
 *   * [BuildConfig.LAKKO_RELEASE_CHECK_DEV_OVERRIDE] — a bool that forces the
 *     banner to show even without a store URL, for pre-launch dogfood testing
 *     (default off).
 *
 * The manifest URL / current release / store URL are transport + display hints;
 * the security boundary is the Ed25519 signature on the manifest, verified in the
 * audited Rust core against [verifyingKey].
 */
object ReleasePin {
    /** The manifest URL used when [BuildConfig.LAKKO_RELEASE_MANIFEST_URL] is unset. */
    const val DEFAULT_MANIFEST_URL: String = "https://lakko.me/.well-known/lakko-release"

    /**
     * The decoded 32-byte Ed25519 publisher verifying key, or `null` when unset /
     * blank / malformed / not 32 bytes. `null` makes the whole feature dormant.
     */
    val verifyingKey: ByteArray?
        get() {
            val raw = BuildConfig.LAKKO_RELEASE_KEY.trim()
            if (raw.isEmpty()) return null
            return try {
                val key = base64UrlDecodeNoPad(raw)
                if (key.size == 32) key else null
            } catch (_: Throwable) {
                null
            }
        }

    /**
     * The signed-manifest URL — the [BuildConfig] value when set, else
     * [DEFAULT_MANIFEST_URL]. Never `null` (the dormancy gate is [verifyingKey]).
     */
    val manifestUrl: String
        get() = BuildConfig.LAKKO_RELEASE_MANIFEST_URL.trim().ifEmpty { DEFAULT_MANIFEST_URL }

    /**
     * The outbound Play deep-link, or `null` when unset / blank. `null` hides the
     * Update button (and, absent [devOverride], the banner).
     */
    val playStoreUrl: String?
        get() = BuildConfig.LAKKO_PLAY_STORE_URL.trim().ifEmpty { null }

    /** This build's monotone release integer ([BuildConfig.LAKKO_CURRENT_RELEASE]), default 1. */
    val currentRelease: ULong
        get() = BuildConfig.LAKKO_CURRENT_RELEASE.trim().toULongOrNull() ?: 1uL

    /**
     * The dev/internal override forcing the banner even without a store URL
     * ([BuildConfig.LAKKO_RELEASE_CHECK_DEV_OVERRIDE]), default `false`.
     */
    val devOverride: Boolean
        get() = BuildConfig.LAKKO_RELEASE_CHECK_DEV_OVERRIDE
}
