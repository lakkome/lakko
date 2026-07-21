// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.updates

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.lakko_ffi_uniffi.UpdateStatus

/**
 * Tests for [updateBannerDecision] (ADR-0038 S4) — the pure banner-visibility +
 * dismiss/re-nag cadence policy (UX §5 store-button gate, §6 cadence). Mirror of
 * the iOS `ReleaseBannerDecisionTests`. No IO: the whole cadence is exercised as a
 * total function over [UpdateBannerInputs]; the `UpdateStatus` enum is a bare
 * Kotlin type, so the test needs no native lib.
 */
class UpdateBannerDecisionTest {

    private val day: Long = 24L * 60 * 60 * 1000

    private fun inputs(
        status: UpdateStatus,
        latest: ULong = 42uL,
        storeUrl: Boolean = true,
        devOverride: Boolean = false,
        dismissedThisSession: Boolean = false,
        dismissedRelease: ULong? = null,
        dismissedAt: Long? = null,
        now: Long = 1_000_000_000_000L,
    ): UpdateBannerInputs = UpdateBannerInputs(
        status = status,
        latestRelease = latest,
        storeUrlPresent = storeUrl,
        devOverride = devOverride,
        dismissedThisSession = dismissedThisSession,
        dismissedRelease = dismissedRelease,
        dismissedAtMillis = dismissedAt,
        nowMillis = now,
        reNagAfterMillis = day,
    )

    // ---- Status gate ----

    @Test
    fun upToDateIsAlwaysHidden() {
        assertEquals(
            UpdateBannerDecision.Hidden,
            updateBannerDecision(inputs(status = UpdateStatus.UP_TO_DATE)),
        )
    }

    @Test
    fun updateAvailableWithStoreShowsWithButton() {
        assertEquals(
            UpdateBannerDecision.Shown(showUpdateButton = true),
            updateBannerDecision(inputs(status = UpdateStatus.UPDATE_AVAILABLE)),
        )
    }

    @Test
    fun updateRequiredRendersIdenticallyToAvailable() {
        // v1: UpdateRequired reuses the dismissible banner (ADR-0038 §5 / UX §2).
        assertEquals(
            UpdateBannerDecision.Shown(showUpdateButton = true),
            updateBannerDecision(inputs(status = UpdateStatus.UPDATE_REQUIRED)),
        )
    }

    // ---- Actionability gate (UX §5) ----

    @Test
    fun noStoreNoOverrideIsDormant() {
        assertEquals(
            UpdateBannerDecision.Hidden,
            updateBannerDecision(inputs(status = UpdateStatus.UPDATE_AVAILABLE, storeUrl = false)),
        )
    }

    @Test
    fun noStoreWithDevOverrideShowsTextOnly() {
        assertEquals(
            UpdateBannerDecision.Shown(showUpdateButton = false),
            updateBannerDecision(
                inputs(status = UpdateStatus.UPDATE_AVAILABLE, storeUrl = false, devOverride = true),
            ),
        )
    }

    // ---- Session dismiss ----

    @Test
    fun dismissedThisSessionIsHidden() {
        assertEquals(
            UpdateBannerDecision.Hidden,
            updateBannerDecision(
                inputs(status = UpdateStatus.UPDATE_AVAILABLE, dismissedThisSession = true),
            ),
        )
    }

    // ---- Cold-launch re-nag cadence (UX §6) ----

    @Test
    fun sameTargetWithinWindowIsHidden() {
        // Dismissed release 42; offered 42 again; 1h later → below the 24h window.
        assertEquals(
            UpdateBannerDecision.Hidden,
            updateBannerDecision(
                inputs(
                    status = UpdateStatus.UPDATE_AVAILABLE,
                    latest = 42uL,
                    dismissedRelease = 42uL,
                    dismissedAt = 1_000_000_000_000L,
                    now = 1_000_000_000_000L + 60 * 60 * 1000,
                ),
            ),
        )
    }

    @Test
    fun sameTargetPastWindowReShows() {
        // Dismissed release 42; offered 42; 25h later → past the 24h re-nag window.
        assertEquals(
            UpdateBannerDecision.Shown(showUpdateButton = true),
            updateBannerDecision(
                inputs(
                    status = UpdateStatus.UPDATE_AVAILABLE,
                    latest = 42uL,
                    dismissedRelease = 42uL,
                    dismissedAt = 1_000_000_000_000L,
                    now = 1_000_000_000_000L + 25 * 60 * 60 * 1000,
                ),
            ),
        )
    }

    @Test
    fun newerTargetBypassesWindow() {
        // Dismissed release 42; a strictly-newer 43 offered 1h later — the
        // newer-release rule re-surfaces it at once, ignoring the 24h window.
        assertEquals(
            UpdateBannerDecision.Shown(showUpdateButton = true),
            updateBannerDecision(
                inputs(
                    status = UpdateStatus.UPDATE_AVAILABLE,
                    latest = 43uL,
                    dismissedRelease = 42uL,
                    dismissedAt = 1_000_000_000_000L,
                    now = 1_000_000_000_000L + 60 * 60 * 1000,
                ),
            ),
        )
    }

    // ---- Text-only (dev override) cadence: once per NEW version only ----

    @Test
    fun textOnlySameTargetNeverTimeReNags() {
        // No store URL (dev override) → once-per-new-version: a same target, even
        // long past the 24h window, does NOT re-nag.
        assertEquals(
            UpdateBannerDecision.Hidden,
            updateBannerDecision(
                inputs(
                    status = UpdateStatus.UPDATE_AVAILABLE,
                    latest = 42uL,
                    storeUrl = false,
                    devOverride = true,
                    dismissedRelease = 42uL,
                    dismissedAt = 1_000_000_000_000L,
                    now = 1_000_000_000_000L + 100L * 24 * 60 * 60 * 1000,
                ),
            ),
        )
    }

    @Test
    fun textOnlyNewerTargetReShows() {
        // A strictly-newer version re-surfaces even in text-only mode.
        assertEquals(
            UpdateBannerDecision.Shown(showUpdateButton = false),
            updateBannerDecision(
                inputs(
                    status = UpdateStatus.UPDATE_AVAILABLE,
                    latest = 43uL,
                    storeUrl = false,
                    devOverride = true,
                    dismissedRelease = 42uL,
                    dismissedAt = 1_000_000_000_000L,
                    now = 1_000_000_000_000L + 60 * 60 * 1000,
                ),
            ),
        )
    }

    @Test
    fun neverDismissedShows() {
        assertEquals(
            UpdateBannerDecision.Shown(showUpdateButton = true),
            updateBannerDecision(
                inputs(status = UpdateStatus.UPDATE_AVAILABLE, dismissedRelease = null),
            ),
        )
    }
}
