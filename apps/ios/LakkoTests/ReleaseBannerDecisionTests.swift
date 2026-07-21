// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// Tests for `updateBannerDecision` (ADR-0038 S4) — the pure banner-visibility
/// + dismiss/re-nag cadence policy (UX §5 store-button gate, §6 cadence). No IO:
/// the whole cadence is exercised as a total function over `UpdateBannerInputs`.
final class ReleaseBannerDecisionTests: XCTestCase {

    private let day: Int64 = 24 * 60 * 60 * 1000

    private func inputs(
        status: UpdateStatus,
        latest: UInt64 = 42,
        storeUrl: Bool = true,
        devOverride: Bool = false,
        dismissedThisSession: Bool = false,
        dismissedRelease: UInt64? = nil,
        dismissedAt: Int64? = nil,
        now: Int64 = 1_000_000_000_000
    ) -> UpdateBannerInputs {
        UpdateBannerInputs(
            status: status,
            latestRelease: latest,
            storeUrlPresent: storeUrl,
            devOverride: devOverride,
            dismissedThisSession: dismissedThisSession,
            dismissedRelease: dismissedRelease,
            dismissedAtMillis: dismissedAt,
            nowMillis: now,
            reNagAfterMillis: day
        )
    }

    // MARK: - Status gate

    func testUpToDateIsAlwaysHidden() {
        XCTAssertEqual(
            updateBannerDecision(inputs(status: .upToDate)),
            .hidden
        )
    }

    func testUpdateAvailableWithStoreShowsWithButton() {
        XCTAssertEqual(
            updateBannerDecision(inputs(status: .updateAvailable)),
            .shown(showUpdateButton: true)
        )
    }

    func testUpdateRequiredRendersIdenticallyToAvailable() {
        // v1: UpdateRequired reuses the dismissible banner (ADR-0038 §5 / UX §2).
        XCTAssertEqual(
            updateBannerDecision(inputs(status: .updateRequired)),
            .shown(showUpdateButton: true)
        )
    }

    // MARK: - Actionability gate (UX §5)

    func testNoStoreNoOverrideIsDormant() {
        XCTAssertEqual(
            updateBannerDecision(inputs(status: .updateAvailable, storeUrl: false)),
            .hidden
        )
    }

    func testNoStoreWithDevOverrideShowsTextOnly() {
        XCTAssertEqual(
            updateBannerDecision(
                inputs(status: .updateAvailable, storeUrl: false, devOverride: true)
            ),
            .shown(showUpdateButton: false)
        )
    }

    // MARK: - Session dismiss

    func testDismissedThisSessionIsHidden() {
        XCTAssertEqual(
            updateBannerDecision(
                inputs(status: .updateAvailable, dismissedThisSession: true)
            ),
            .hidden
        )
    }

    // MARK: - Cold-launch re-nag cadence (UX §6)

    func testSameTargetWithinWindowIsHidden() {
        // Dismissed release 42; offered 42 again; 1h later → below the 24h window.
        XCTAssertEqual(
            updateBannerDecision(
                inputs(
                    status: .updateAvailable,
                    latest: 42,
                    dismissedRelease: 42,
                    dismissedAt: 1_000_000_000_000,
                    now: 1_000_000_000_000 + 60 * 60 * 1000
                )
            ),
            .hidden
        )
    }

    func testSameTargetPastWindowReShows() {
        // Dismissed release 42; offered 42; 25h later → past the 24h re-nag window.
        XCTAssertEqual(
            updateBannerDecision(
                inputs(
                    status: .updateAvailable,
                    latest: 42,
                    dismissedRelease: 42,
                    dismissedAt: 1_000_000_000_000,
                    now: 1_000_000_000_000 + 25 * 60 * 60 * 1000
                )
            ),
            .shown(showUpdateButton: true)
        )
    }

    func testNewerTargetBypassesWindow() {
        // Dismissed release 42; a strictly-newer 43 is offered 1h later — the
        // newer-release rule re-surfaces it at once, ignoring the 24h window.
        XCTAssertEqual(
            updateBannerDecision(
                inputs(
                    status: .updateAvailable,
                    latest: 43,
                    dismissedRelease: 42,
                    dismissedAt: 1_000_000_000_000,
                    now: 1_000_000_000_000 + 60 * 60 * 1000
                )
            ),
            .shown(showUpdateButton: true)
        )
    }

    // MARK: - Text-only (dev override) cadence: once per NEW version only

    func testTextOnlySameTargetNeverTimeReNags() {
        // No store URL (dev override) → once-per-new-version: a same target,
        // even long past the 24h window, does NOT re-nag.
        XCTAssertEqual(
            updateBannerDecision(
                inputs(
                    status: .updateAvailable,
                    latest: 42,
                    storeUrl: false,
                    devOverride: true,
                    dismissedRelease: 42,
                    dismissedAt: 1_000_000_000_000,
                    now: 1_000_000_000_000 + 100 * 24 * 60 * 60 * 1000
                )
            ),
            .hidden
        )
    }

    func testTextOnlyNewerTargetReShows() {
        // A strictly-newer version re-surfaces even in text-only mode.
        XCTAssertEqual(
            updateBannerDecision(
                inputs(
                    status: .updateAvailable,
                    latest: 43,
                    storeUrl: false,
                    devOverride: true,
                    dismissedRelease: 42,
                    dismissedAt: 1_000_000_000_000,
                    now: 1_000_000_000_000 + 60 * 60 * 1000
                )
            ),
            .shown(showUpdateButton: false)
        )
    }

    func testNeverDismissedShows() {
        XCTAssertEqual(
            updateBannerDecision(
                inputs(status: .updateAvailable, dismissedRelease: nil)
            ),
            .shown(showUpdateButton: true)
        )
    }
}
