// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0073 R.12 - a one-to-one call and a conversation that moved. The cases
/// are those of `apps/web/src/calls/call-continuity.test.ts`.
final class CallContinuityTests: XCTestCase {

    // MARK: - oneToOneCallEndsOnMove

    func testNothingEndsWhenNoOneToOneCallIsLive() {
        XCTAssertFalse(
            CallContinuity.oneToOneCallEndsOnMove(
                callGroupIdB64: nil, unrepairableKeyB64: "old", headGroupIdB64: "S"))
    }

    func testEndsTheMomentTheOtherPersonsResetIsAccepted() {
        XCTAssertTrue(
            CallContinuity.oneToOneCallEndsOnMove(
                callGroupIdB64: "G", unrepairableKeyB64: "old", headGroupIdB64: "G"))
    }

    func testEndsWhenTheConversationsCurrentGroupIsNoLongerTheCalls() {
        XCTAssertTrue(
            CallContinuity.oneToOneCallEndsOnMove(
                callGroupIdB64: "G", unrepairableKeyB64: nil, headGroupIdB64: "S"))
    }

    func testAnOrdinaryCallAndAnUnknownHeadGoOn() {
        XCTAssertFalse(
            CallContinuity.oneToOneCallEndsOnMove(
                callGroupIdB64: "G", unrepairableKeyB64: nil, headGroupIdB64: "G"))
        XCTAssertFalse(
            CallContinuity.oneToOneCallEndsOnMove(
                callGroupIdB64: "G", unrepairableKeyB64: nil, headGroupIdB64: nil))
    }

    // MARK: - oneToOneCallTarget

    private func target(
        unrepairableKeyB64: String? = nil,
        headGroupIdB64: String? = "G",
        headUnrepairableKeyB64: String? = nil
    ) -> CallContinuity.Target {
        CallContinuity.oneToOneCallTarget(
            groupIdB64: "G", unrepairableKeyB64: unrepairableKeyB64,
            headGroupIdB64: headGroupIdB64, headUnrepairableKeyB64: headUnrepairableKeyB64)
    }

    func testALiveOneToOneDialsInItsOwnGroup() {
        XCTAssertEqual(target(), .dial(groupIdB64: "G"))
        XCTAssertEqual(target(headGroupIdB64: nil), .dial(groupIdB64: "G"))
    }

    func testADeadSessionWithASuccessorDialsInTheSuccessor() {
        XCTAssertEqual(target(unrepairableKeyB64: "old", headGroupIdB64: "S"), .dial(groupIdB64: "S"))
    }

    func testADeadSessionWithNoSuccessorDoesNotStart() {
        XCTAssertEqual(target(unrepairableKeyB64: "old"), .unavailable)
        XCTAssertEqual(target(unrepairableKeyB64: "old", headGroupIdB64: nil), .unavailable)
    }

    func testAHeadThatIsItselfDeadIsNotDialled() {
        XCTAssertEqual(
            target(unrepairableKeyB64: "old", headGroupIdB64: "S", headUnrepairableKeyB64: "newer-old"),
            .unavailable)
    }

    func testAViewOnAnOlderSegmentAfterTheMoveDialsWhereTheConversationIs() {
        XCTAssertEqual(target(headGroupIdB64: "S"), .dial(groupIdB64: "S"))
    }

    func testAViewOnAnOlderSegmentDoesNotDialADeadHead() {
        // Row 3 of the table: the asked group is fine, the head is dead.
        XCTAssertEqual(target(headGroupIdB64: "S", headUnrepairableKeyB64: "newer-old"), .unavailable)
    }
}
