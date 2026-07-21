// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// Tests for `restoreVerdict` (ADR-0037 §6, G-11) — the pure rollback-detection
/// classifier. Swift mirror of the web `head-gate.test.ts`. Verifies the
/// load-bearing branch order: parse error → hard; absent head → fail-open;
/// returning (has floor) → hard on invalid/mismatch, stale-override on
/// below-floor; fresh install → hard on invalid, stale-override on over-window.
final class BackupHeadGateTests: XCTestCase {

    // MARK: - Absent head → fail-open (availability exception)

    func testAbsentHeadFailsOpen() {
        // No framing (legacy blob / head stripped) — restore proceeds, warn.
        let verdict = restoreVerdict(RestoreGateFacts(framed: false))
        guard case .failOpen = verdict else {
            return XCTFail("absent head must fail-open, got \(verdict)")
        }
    }

    // MARK: - Parse error → hard-fail (truncated framed blob)

    func testTruncatedFramedBlobHardFails() {
        // A parse error always wins, regardless of other facts.
        let verdict = restoreVerdict(RestoreGateFacts(parseError: true, framed: true, hasFloor: true))
        guard case .hardFail = verdict else {
            return XCTFail("truncated framed blob must hard-fail, got \(verdict)")
        }
    }

    // MARK: - Returning client (has floor) — hard monotonic floor

    func testReturningBindingInvalidHardFails() {
        let verdict = restoreVerdict(
            RestoreGateFacts(framed: true, hasFloor: true, bindingInvalid: true)
        )
        guard case .hardFail = verdict else {
            return XCTFail("returning bad binding must hard-fail, got \(verdict)")
        }
    }

    func testReturningHashMismatchHardFails() {
        let verdict = restoreVerdict(
            RestoreGateFacts(framed: true, hasFloor: true, hashMismatch: true)
        )
        guard case .hardFail = verdict else {
            return XCTFail("returning hash mismatch must hard-fail, got \(verdict)")
        }
    }

    func testReturningVersionBelowFloorStaleOverride() {
        let verdict = restoreVerdict(
            RestoreGateFacts(framed: true, hasFloor: true, versionBelowFloor: true)
        )
        guard case .staleOverride = verdict else {
            return XCTFail("returning below-floor must stale-override, got \(verdict)")
        }
    }

    func testReturningCleanProceeds() {
        let verdict = restoreVerdict(RestoreGateFacts(framed: true, hasFloor: true))
        XCTAssertEqual(verdict, .proceed)
    }

    // MARK: - Fresh install (no floor) — freshness window

    func testFreshInvalidHardFails() {
        let verdict = restoreVerdict(
            RestoreGateFacts(framed: true, hasFloor: false, freshInvalid: true)
        )
        guard case .hardFail = verdict else {
            return XCTFail("fresh invalid must hard-fail, got \(verdict)")
        }
    }

    func testFreshStaleStaleOverride() {
        let verdict = restoreVerdict(
            RestoreGateFacts(framed: true, hasFloor: false, freshStale: true)
        )
        guard case .staleOverride = verdict else {
            return XCTFail("fresh over-window must stale-override, got \(verdict)")
        }
    }

    func testFreshCleanProceeds() {
        let verdict = restoreVerdict(RestoreGateFacts(framed: true, hasFloor: false))
        XCTAssertEqual(verdict, .proceed)
    }

    // MARK: - Branch-order precedence

    func testParseErrorBeatsEverything() {
        // Even with framed + fresh-stale set, a parse error is the terminal
        // verdict (branch 1 wins).
        let verdict = restoreVerdict(
            RestoreGateFacts(
                parseError: true, framed: true, hasFloor: false, freshStale: true
            )
        )
        guard case .hardFail = verdict else {
            return XCTFail("parseError must win, got \(verdict)")
        }
    }

    func testReturningBindingInvalidBeatsBelowFloor() {
        // A returning client's bad signature (hard) wins over a below-floor
        // version (stale) — the binding check comes first.
        let verdict = restoreVerdict(
            RestoreGateFacts(
                framed: true, hasFloor: true, bindingInvalid: true, versionBelowFloor: true
            )
        )
        guard case .hardFail = verdict else {
            return XCTFail("bad binding must beat below-floor, got \(verdict)")
        }
    }

    // MARK: - Staleness config knob (default fallback)

    func testStalenessDefaults() {
        // With no Info.plist override in the test host, the default 30-day
        // window applies (seconds + millis).
        XCTAssertEqual(backupHeadStalenessSecsDefault, 2_592_000)
        XCTAssertEqual(backupHeadStalenessSecs(), 2_592_000)
        XCTAssertEqual(backupHeadStalenessMillis(), 2_592_000_000)
    }
}
