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

    // MARK: - Absent head (F4) — floor-split

    func testAbsentHeadNoFloorFailsOpen() {
        // A true zero-anchor fresh install: no framing (legacy blob / head
        // stripped) AND no persisted floor — restore proceeds, warn.
        let verdict = restoreVerdict(RestoreGateFacts(framed: false, hasFloor: false))
        guard case .failOpen = verdict else {
            return XCTFail("absent head + no floor must fail-open, got \(verdict)")
        }
    }

    func testAbsentHeadFloorHashMatchProceeds() {
        // F4: a returning client whose head was benignly stripped in transit but
        // the envelope byte-matches the last known one — safe to proceed.
        let verdict = restoreVerdict(
            RestoreGateFacts(framed: false, hasFloor: true, absentHashMatchesFloor: true)
        )
        XCTAssertEqual(verdict, .proceed)
    }

    func testAbsentHeadFloorHashMismatchStaleOverride() {
        // F4: a head-stripping downgrade against head-protected history — refuse
        // unless the user explicitly acknowledges (never fail-open for a returning
        // client with a mismatching headless envelope).
        let verdict = restoreVerdict(
            RestoreGateFacts(framed: false, hasFloor: true, absentHashMatchesFloor: false)
        )
        guard case .staleOverride = verdict else {
            return XCTFail("absent head + floor + mismatch must stale-override, got \(verdict)")
        }
    }

    // MARK: - Corrupt floor (F5a) — untrustworthy anchor → stale-override

    func testCorruptFloorStaleOverride() {
        // The local anchor exists but is undecodable — never demote to the weaker
        // fresh path; require an explicit acknowledgement.
        let verdict = restoreVerdict(RestoreGateFacts(floorCorrupt: true))
        guard case .staleOverride = verdict else {
            return XCTFail("corrupt floor must stale-override, got \(verdict)")
        }
    }

    func testCorruptFloorFramedStaleOverride() {
        // A corrupt anchor dominates even a framed blob — F5a short-circuits
        // before the framed hard-fail checks.
        let verdict = restoreVerdict(
            RestoreGateFacts(framed: true, hasFloor: false, floorCorrupt: true)
        )
        guard case .staleOverride = verdict else {
            return XCTFail("corrupt floor + framed must stale-override, got \(verdict)")
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

    func testParseErrorBeatsFloorCorrupt() {
        // A truncated framed blob is a hard tamper regardless of the anchor state
        // — branch 1 (parseError) wins over branch 2 (floorCorrupt).
        let verdict = restoreVerdict(
            RestoreGateFacts(parseError: true, framed: true, floorCorrupt: true)
        )
        guard case .hardFail = verdict else {
            return XCTFail("parseError must beat floorCorrupt, got \(verdict)")
        }
    }

    func testFloorCorruptBeatsRest() {
        // floorCorrupt short-circuits before the framed hard-fail / absent checks;
        // a corrupt anchor is always the user-acknowledged stale path (fail-closed).
        let verdict = restoreVerdict(
            RestoreGateFacts(
                framed: true,
                hasFloor: false,
                floorCorrupt: true,
                bindingInvalid: true,
                versionBelowFloor: true
            )
        )
        guard case .staleOverride = verdict else {
            return XCTFail("floorCorrupt must beat the framed / absent branches, got \(verdict)")
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
