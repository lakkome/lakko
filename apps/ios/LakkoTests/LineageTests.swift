// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0073 R.5 - one conversation over several groups (`Lineage.swift`).
/// The cases are those of `apps/web/src/chat/lineage.test.ts`, plus the
/// `historyRowOrigin` cases of `chat/pin-older-segment.test.ts`.
final class LineageTests: XCTestCase {

    private typealias Position = LineageRules.TimelinePosition
    private typealias Held = LineageRules.HeldGroup
    private typealias Plan = LineageRules.ConversationPlan

    private let root = "aa"
    private let s1 = "bb"
    private let s2 = "cc"
    private let other = "dd"

    private var lineage: ConversationLineage {
        ConversationLineage.build([
            LineageEntry(successorHex: s2, rootHex: root, hlc: 20),
            LineageEntry(successorHex: s1, rootHex: root, hlc: 10),
        ])
    }

    // MARK: - build

    func testTheConversationIdIsTheRootAndAGroupWithNoRegisterIsItsOwn() {
        XCTAssertEqual(lineage.conversationOf(s1), root)
        XCTAssertEqual(lineage.conversationOf(s2), root)
        XCTAssertEqual(lineage.conversationOf(root), root)
        XCTAssertEqual(lineage.conversationOf(other), other)
        XCTAssertEqual(ConversationLineage.none.conversationOf(s1), s1)
    }

    func testSegmentsAreTheRootThenTheSuccessorsByStamp() {
        XCTAssertEqual(lineage.segmentsOf(root), [root, s1, s2])
        XCTAssertEqual(lineage.segmentsOf(other), [other])
        XCTAssertEqual(lineage.segmentIndexOf(s2), 2)
        XCTAssertEqual(lineage.segmentIndexOf(root), 0)
        XCTAssertTrue(lineage.isSuccessor(s1))
        XCTAssertFalse(lineage.isSuccessor(root))
    }

    func testEqualStampsBreakOnTheIdSoEveryDeviceAgrees() {
        let l = ConversationLineage.build([
            LineageEntry(successorHex: "zz", rootHex: root, hlc: 5),
            LineageEntry(successorHex: "mm", rootHex: root, hlc: 5),
        ])
        XCTAssertEqual(l.segmentsOf(root), [root, "mm", "zz"])
    }

    func testARegisterThatNamesTheParentInsteadOfTheRootStillReadsAsOneConversation() {
        let l = ConversationLineage.build([
            LineageEntry(successorHex: s1, rootHex: root, hlc: 1),
            LineageEntry(successorHex: s2, rootHex: s1, hlc: 2),
        ])
        XCTAssertEqual(l.conversationOf(s2), root)
        XCTAssertEqual(l.segmentsOf(root), [root, s1, s2])
    }

    func testARegisterNamingItselfAndACycleAreIgnored() {
        let l = ConversationLineage.build([
            LineageEntry(successorHex: root, rootHex: root, hlc: 1),
            LineageEntry(successorHex: s1, rootHex: s2, hlc: 2),
            LineageEntry(successorHex: s2, rootHex: s1, hlc: 3),
        ])
        XCTAssertEqual(l.conversationOf(root), root)
        XCTAssertEqual(l.conversationOf(s1), s1)
        XCTAssertEqual(l.conversationOf(s2), s2)
        XCTAssertEqual(l.segmentsOf(s1), [s1])
    }

    // MARK: - headSegment (the conversation's current group)

    private var segments: [String] { [root, s1, s2] }

    func testTheHeadIsTheLastSegmentThisDeviceIsAnActiveMemberOf() {
        XCTAssertEqual(LineageRules.headSegment(segments) { _ in .active }, s2)
        XCTAssertEqual(LineageRules.headSegment(segments) { $0 == self.s2 ? .absent : .active }, s1)
    }

    func testASuccessorTheDeviceHasNotJoinedIsNotItsHead() {
        XCTAssertEqual(LineageRules.headSegment(segments) { $0 == self.root ? .active : .absent }, root)
    }

    func testWithEveryMembershipEndedTheLastHeldSegmentKeepsTheRowReachable() {
        XCTAssertEqual(LineageRules.headSegment(segments) { $0 == self.s2 ? .absent : .ended }, s1)
        XCTAssertNil(LineageRules.headSegment(segments) { _ in .absent })
    }

    func testAnEndedNewerSegmentDoesNotHideAnActiveOlderOne() {
        XCTAssertEqual(LineageRules.headSegment(segments) { $0 == self.s2 ? .ended : .active }, s1)
    }

    // MARK: - timeline order and the read position

    func testOrdersBySegmentThenMessageId() {
        XCTAssertLessThan(
            LineageRules.compareTimeline(Position(segment: 0, messageId: 900), Position(segment: 1, messageId: 1)), 0)
        XCTAssertGreaterThan(
            LineageRules.compareTimeline(Position(segment: 1, messageId: 2), Position(segment: 1, messageId: 1)), 0)
        XCTAssertEqual(
            LineageRules.compareTimeline(Position(segment: 2, messageId: 5), Position(segment: 2, messageId: 5)), 0)
    }

    func testAPositionInTheFirstSegmentIsTheEightBytesEveryBuildWrites() {
        let bytes = LineageRules.encodeReadPosition(Position(segment: 0, messageId: 370))
        XCTAssertEqual([UInt8](bytes), [0, 0, 0, 0, 0, 0, 1, 114])
        XCTAssertEqual(LineageRules.decodeReadPosition(bytes), Position(segment: 0, messageId: 370))
    }

    func testALaterSegmentCarriesItsIndexAfterTheIdAndRoundTrips() {
        let bytes = LineageRules.encodeReadPosition(Position(segment: 2, messageId: 7))
        XCTAssertEqual(bytes.count, 12)
        XCTAssertEqual([UInt8](bytes), [0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 2])
        XCTAssertEqual(LineageRules.decodeReadPosition(bytes), Position(segment: 2, messageId: 7))
        // A reader of the 8-byte form sees message 7 of the first segment:
        // less read than was read, never more. The prefix is a slice, whose
        // indices do not start at zero on every platform's `Data`.
        XCTAssertEqual(LineageRules.decodeReadPosition(bytes.prefix(8)), Position(segment: 0, messageId: 7))
        XCTAssertEqual(LineageRules.decodeReadPosition(bytes.suffix(8)), Position(segment: 0, messageId: 7 << 32 | 2))
    }

    func testTooShortToHoldAnIdIsNoPosition() {
        XCTAssertNil(LineageRules.decodeReadPosition(Data(count: 4)))
    }

    func testAnIdPastTheLargestExactIntegerIsNoPosition() {
        // 2^53: one past what the web client can hold exactly.
        XCTAssertNil(LineageRules.decodeReadPosition(Data([0, 0x20, 0, 0, 0, 0, 0, 0])))
        XCTAssertEqual(
            LineageRules.decodeReadPosition(Data([0, 0x1f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff])),
            Position(segment: 0, messageId: (1 << 53) - 1))
    }

    func testMeansPerSegmentNothingBeforeItTheIdInItEverythingAfterIt() {
        let position = Position(segment: 1, messageId: 12)
        XCTAssertEqual(LineageRules.readUpToInSegment(position, segment: 0), .all)
        XCTAssertEqual(LineageRules.readUpToInSegment(position, segment: 1), .upTo(12))
        XCTAssertEqual(LineageRules.readUpToInSegment(position, segment: 2), .nothing)
        XCTAssertEqual(LineageRules.readUpToInSegment(nil, segment: 0), .nothing)
    }

    func testAnOldSegmentsRowsGetUniqueOrderedNegativeOnScreenIds() {
        let a = LineageRules.historyRowId(900, distanceFromHead: 2)
        let b = LineageRules.historyRowId(1, distanceFromHead: 1)
        let c = LineageRules.historyRowId(2, distanceFromHead: 1)
        XCTAssertLessThan(a, b)
        XCTAssertLessThan(b, c)
        XCTAssertLessThan(c, 0)
        XCTAssertTrue(LineageRules.isHistoryRowId(a))
        XCTAssertFalse(LineageRules.isHistoryRowId(1))
        XCTAssertFalse(LineageRules.isHistoryRowId(nil))
    }

    // MARK: - planConversations (one row per conversation)

    func testFoldsAConversationsGroupsIntoOneRowKeyedOnTheRootOpeningTheHead() {
        let plans = LineageRules.planConversations(
            [
                Held(groupHex: root, active: true),
                Held(groupHex: other, active: true),
                Held(groupHex: s1, active: true),
            ],
            lineage: lineage)
        XCTAssertEqual(
            plans,
            [
                Plan(conversationHex: root, headHex: s1, heldSegments: [root, s1]),
                Plan(conversationHex: other, headHex: other, heldSegments: [other]),
            ])
    }

    func testADeviceThatHoldsOnlyTheSuccessorShowsItUnderTheConversationId() {
        XCTAssertEqual(
            LineageRules.planConversations([Held(groupHex: s2, active: true)], lineage: lineage),
            [Plan(conversationHex: root, headHex: s2, heldSegments: [s2])])
    }

    func testWithoutARegisterTheSuccessorIsItsOwnConversation() {
        XCTAssertEqual(
            LineageRules.planConversations([Held(groupHex: s2, active: true)], lineage: .none),
            [Plan(conversationHex: s2, headHex: s2, heldSegments: [s2])])
    }

    // MARK: - oldSegmentPolled

    private func polled(
        isHead: Bool = false, oneToOne: Bool = false, movedAtMs: Int64? = 1000, nowMs: Int64 = 2000
    ) -> Bool {
        LineageRules.oldSegmentPolled(isHead: isHead, oneToOne: oneToOne, movedAtMs: movedAtMs, nowMs: nowMs)
    }

    func testTheHeadIsAlwaysPolled() {
        XCTAssertTrue(polled(isHead: true, oneToOne: true))
    }

    func testAOneToOnesOldJournalIsNotPolledAtAllAfterTheMove() {
        XCTAssertFalse(polled(oneToOne: true))
    }

    func testAGroupsOldJournalIsPolledForTheRetentionWindow() {
        XCTAssertTrue(polled())
        XCTAssertFalse(polled(nowMs: 1000 + LineageRules.oldSegmentPollWindowMs))
        XCTAssertTrue(polled(movedAtMs: nil))
    }

    func testTheRetentionWindowIsThirtyDays() {
        XCTAssertEqual(LineageRules.oldSegmentPollWindowMs, 2_592_000_000)
    }

    // MARK: - retiresOldSegmentPush

    private func retires(
        headHex: String? = "new", headActive: Bool = true, alreadyRetired: Bool = false
    ) -> Bool {
        LineageRules.retiresOldSegmentPush(
            groupHex: "old", headHex: headHex, headActive: headActive, alreadyRetired: alreadyRetired)
    }

    func testAnOlderSegmentIsRetiredOnceThisDeviceIsLiveInTheCurrentGroup() {
        XCTAssertTrue(retires())
    }

    func testTheCurrentGroupAndAGroupThatIsItsOwnConversationKeepTheirs() {
        XCTAssertFalse(retires(headHex: "old"))
        XCTAssertFalse(retires(headHex: nil))
    }

    func testADeviceNotLiveInTheSuccessorKeepsTheOldSubscription() {
        XCTAssertFalse(retires(headActive: false))
    }

    func testRetiringIsDoneOnce() {
        XCTAssertFalse(retires(alreadyRetired: true))
    }

    // MARK: - historyRowOrigin (the inverse of historyRowId)

    func testHistoryRowOriginRoundTripsAMessageIdAndADistance() {
        XCTAssertEqual(LineageRules.historyIdSpan, 1_099_511_627_776)
        let cases: [(Int64, Int)] = [(1, 1), (41, 1), (41, 3), (LineageRules.historyIdSpan - 1, 2)]
        for (id, d) in cases {
            XCTAssertEqual(
                LineageRules.historyRowOrigin(LineageRules.historyRowId(id, distanceFromHead: d)),
                LineageRules.HistoryRowOrigin(messageId: id, distanceFromHead: d))
        }
    }

    func testHistoryRowOriginIsNilForARowOfTheCurrentGroupAndForANumberNoHistoryRowHas() {
        XCTAssertNil(LineageRules.historyRowOrigin(41))
        XCTAssertNil(LineageRules.historyRowOrigin(0))
        XCTAssertNil(LineageRules.historyRowOrigin(-LineageRules.historyIdSpan))
        // The web case is `NaN`; an `Int64` has none, so the numbers no
        // history row can have here are those past the exact-integer bound.
        XCTAssertNil(LineageRules.historyRowOrigin(-(1 << 53)))
        XCTAssertNil(LineageRules.historyRowOrigin(Int64.min))
    }
}
