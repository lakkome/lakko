// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.updates

import uniffi.lakko_ffi_uniffi.UpdateStatus

/**
 * The pure "should the update banner show, and with what controls" policy
 * (ADR-0038 S4; UX spec §5 store-button gate + §6 dismiss/re-nag cadence). Mirror
 * of the iOS `UpdateBannerDecision`.
 *
 * This holds ONLY the decision — the IO that resolves the facts (fetch + verify +
 * status + persisted dismiss state) lives in [ReleaseUpdateChecker]. Keeping it a
 * total function over a plain value makes the whole cadence unit-testable without
 * a network, `SharedPreferences`, or the UniFFI verify.
 */

/** What the banner should render, post-signature-verify. */
public sealed interface UpdateBannerDecision {
    /** No banner. */
    public data object Hidden : UpdateBannerDecision

    /**
     * Show the banner. [showUpdateButton] gates the primary Update control — it is
     * hidden when no store URL is configured, leaving a purely informational text
     * notice (never a dead / greyed-out button, UX §5).
     */
    public data class Shown(val showUpdateButton: Boolean) : UpdateBannerDecision
}

/**
 * The facts a banner decision is computed from. [status], [latestRelease],
 * [storeUrlPresent], [devOverride] come from the verified manifest + build config;
 * [dismissedRelease] / [dismissedAtMillis] come from the per-platform prefs store;
 * [nowMillis] + [reNagAfterMillis] drive the cold-launch cadence.
 */
public data class UpdateBannerInputs(
    /**
     * The signature-verified update status (fail-safe: an unverified / unfetched
     * manifest resolves to [UpdateStatus.UP_TO_DATE] upstream, so this is only ever
     * the authenticated verdict).
     */
    val status: UpdateStatus,
    /**
     * The offered `latest_release` — the key the dismissal is stored against.
     * Meaningful only when [status] is not [UpdateStatus.UP_TO_DATE].
     */
    val latestRelease: ULong,
    /** A store deep-link URL is configured (`LAKKO_PLAY_STORE_URL` non-empty). */
    val storeUrlPresent: Boolean,
    /** The dev/internal override forcing the banner without a store URL. */
    val devOverride: Boolean,
    /** The banner was already dismissed earlier in THIS process run. */
    val dismissedThisSession: Boolean = false,
    /** The `latest_release` the user last dismissed (persisted); `null` if never. */
    val dismissedRelease: ULong? = null,
    /** When that dismiss happened, Unix millis (persisted); `null` if never. */
    val dismissedAtMillis: Long? = null,
    /** Now, Unix millis. */
    val nowMillis: Long,
    /** The cold-launch re-nag window in millis (24h) for the actionable banner. */
    val reNagAfterMillis: Long,
)

/**
 * The pure banner-visibility classifier. Total over [UpdateBannerInputs]; no IO.
 * Branch order is load-bearing:
 *
 *   1. [UpdateStatus.UP_TO_DATE] → never a banner (UX §2 — no positive-confirmation
 *      surface).
 *   2. Actionability gate — dormant unless a store URL exists to act on OR a dev
 *      override is set (UX §5 optional hardening; the S4 gate). Avoids a nag with no
 *      payoff and a dead Update control.
 *   3. Dismissed this run → gone for the rest of the session (UX §6).
 *   4. Cold-launch re-nag cadence keyed to the dismissed target (UX §6):
 *      * a strictly-newer `latest_release` re-surfaces at once, bypassing both the
 *        time window and the prior dismissal (the newer-release rule);
 *      * else an actionable banner re-nags at most once per [UpdateBannerInputs.reNagAfterMillis]
 *        (≈ once/day); a text-only (dev-override) banner does NOT time-re-nag — it
 *        shows once per NEW version only, since the user cannot act on it.
 *
 * [UpdateStatus.UPDATE_AVAILABLE] and [UpdateStatus.UPDATE_REQUIRED] render
 * identically in v1 (UX §2 / ADR-0038 §5 — no blocking screen yet; the
 * `min_supported` field is carried so a future gate needs no wire change).
 */
public fun updateBannerDecision(i: UpdateBannerInputs): UpdateBannerDecision {
    when (i.status) {
        UpdateStatus.UP_TO_DATE -> return UpdateBannerDecision.Hidden
        UpdateStatus.UPDATE_AVAILABLE, UpdateStatus.UPDATE_REQUIRED -> Unit
    }

    // Actionability gate — no store URL and no override → dormant.
    if (!(i.storeUrlPresent || i.devOverride)) {
        return UpdateBannerDecision.Hidden
    }

    // Dismissed this run → gone until a later cold launch.
    if (i.dismissedThisSession) {
        return UpdateBannerDecision.Hidden
    }

    val dismissed = i.dismissedRelease
    if (dismissed != null) {
        // A strictly-newer release is a fresh target — re-nag immediately.
        if (i.latestRelease > dismissed) {
            return UpdateBannerDecision.Shown(i.storeUrlPresent)
        }
        // Same-or-older target already dismissed.
        if (i.storeUrlPresent) {
            // Actionable: re-nag at most once per window.
            val elapsed = i.nowMillis - (i.dismissedAtMillis ?: 0L)
            if (elapsed < i.reNagAfterMillis) {
                return UpdateBannerDecision.Hidden
            }
        } else {
            // Text-only (dev override): once per new version only, no time re-nag.
            return UpdateBannerDecision.Hidden
        }
    }

    return UpdateBannerDecision.Shown(i.storeUrlPresent)
}
