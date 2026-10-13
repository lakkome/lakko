// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0073 R.5 - the views of a conversation that spans several groups
/// (`LineageViews.swift`). The cases are the lineage ones of
/// `apps/web/src/chat/pin-bar.test.ts`, `chat/pin-older-segment.test.ts`,
/// `chat/forward.test.ts`, `chat/delete-chat.test.ts` and
/// `feed/feed-model.test.ts`.
final class LineageViewsTests: XCTestCase {

    /// A stored shared pin, as this suite needs it: a pin or a stored unpin
    /// with its stamp.
    private struct Pin: Equatable {
        let pinned: Bool
        let updatedAtMs: UInt64
    }

    private func pin(_ updatedAtMs: UInt64, _ pinned: Bool = true) -> Pin {
        Pin(pinned: pinned, updatedAtMs: updatedAtMs)
    }

    private func rowId(_ messageId: Int64, _ distance: Int) -> Int64 {
        LineageRules.historyRowId(messageId, distanceFromHead: distance)
    }

    /// The bar's order over the spanned maps (shared before personal, each
    /// newest first, a stamp tie broken by the later row, a stored unpin and
    /// a message already shown as shared left out), so the web cases that
    /// read the bar after the fold can be read here too.
    private func barOrder(_ spanned: LineageViews.SpannedPins<Pin>) -> [Int64] {
        func newestFirst(_ a: (id: Int64, at: UInt64), _ b: (id: Int64, at: UInt64)) -> Bool {
            a.at == b.at ? a.id > b.id : a.at > b.at
        }
        let shared = spanned.shared
            .filter { $0.value.pinned }
            .map { (id: $0.key, at: $0.value.updatedAtMs) }
            .sorted(by: newestFirst)
        let sharedIds = Set(shared.map(\.id))
        let personal = spanned.personal
            .filter { !sharedIds.contains($0.key) }
            .map { (id: $0.key, at: $0.value) }
            .sorted(by: newestFirst)
        return shared.map(\.id) + personal.map(\.id)
    }

    // MARK: - pinsAcrossSegments

    func testAnOlderSegmentsPinsStayInTheBarUnderTheIdTheirRowIsDrawnWith() {
        let spanned = LineageViews.pinsAcrossSegments([
            LineageViews.SegmentPins(distanceFromHead: 1, shared: [7: pin(10)], personal: [3: 5]),
            LineageViews.SegmentPins(distanceFromHead: 0, shared: [7: pin(20)], personal: [2: 30]),
        ])
        // The same DS message id in two segments is two different messages.
        XCTAssertEqual(spanned.shared.keys.sorted(), [rowId(7, 1), 7])
        XCTAssertEqual(spanned.personal.keys.sorted(), [rowId(3, 1), 2])
        XCTAssertEqual(spanned.shared[7], pin(20))
        XCTAssertEqual(spanned.shared[rowId(7, 1)], pin(10))
        XCTAssertEqual(spanned.personal[2], 30)
        XCTAssertEqual(spanned.personal[rowId(3, 1)], 5)
        let order = barOrder(spanned)
        XCTAssertEqual(order, [7, rowId(7, 1), 2, rowId(3, 1)])
        XCTAssertEqual(order.filter { LineageRules.isHistoryRowId($0) }.count, 2)
    }

    func testAStoredUnpinOfAnOlderSegmentStaysAnUnpin() {
        let spanned = LineageViews.pinsAcrossSegments([
            LineageViews.SegmentPins(distanceFromHead: 2, shared: [4: pin(10, false)], personal: [:])
        ])
        XCTAssertEqual(spanned.shared, [rowId(4, 2): pin(10, false)])
        XCTAssertEqual(barOrder(spanned), [])
    }

    func testAConversationThatNeverMovedIsItsOneSegmentUnchanged() {
        let shared: [Int64: Pin] = [9: pin(1)]
        let personal: [Int64: UInt64] = [4: 2]
        let spanned = LineageViews.pinsAcrossSegments([
            LineageViews.SegmentPins(distanceFromHead: 0, shared: shared, personal: personal)
        ])
        XCTAssertEqual(spanned.shared, shared)
        XCTAssertEqual(spanned.personal, personal)
    }

    func testTheRowIdRuleIsTheOneHandedIn() {
        let spanned = LineageViews.pinsAcrossSegments(
            [LineageViews.SegmentPins(distanceFromHead: 3, shared: [5: pin(1)], personal: [6: 2])],
            rowId: { -($0 * 10 + Int64($1)) })
        XCTAssertEqual(spanned.shared, [-53: pin(1)])
        XCTAssertEqual(spanned.personal, [-63: 2])
    }

    // MARK: - pinTargetOfRow

    private func target(_ rowId: Int64) -> LineageViews.PinTarget? {
        LineageViews.pinTargetOfRow(rowId: rowId, headGroupIdB64: "head", olderGroupIdsB64: ["oldest", "older"])
    }

    func testARowOfTheCurrentGroupIsItsOwnPin() {
        XCTAssertEqual(target(41), LineageViews.PinTarget(groupIdB64: "head", messageId: 41, older: false))
    }

    func testAHistoryRowIsThePinOfItsOwnSegmentUnderItsOriginalId() {
        XCTAssertEqual(target(rowId(41, 1)), LineageViews.PinTarget(groupIdB64: "older", messageId: 41, older: true))
        XCTAssertEqual(target(rowId(7, 2)), LineageViews.PinTarget(groupIdB64: "oldest", messageId: 7, older: true))
    }

    func testAHistoryRowOfASegmentThisDeviceDoesNotHoldIsNothing() {
        XCTAssertNil(target(rowId(7, 3)))
    }

    func testANegativeIdThatIsNoHistoryRowIsNothing() {
        XCTAssertNil(target(-LineageRules.historyIdSpan))
    }

    // MARK: - pinRoute

    func testTheCurrentGroupIsTheOrdinaryPath() {
        XCTAssertEqual(LineageViews.pinRoute(older: false, pinned: true, scope: .shared), .ordinary)
        XCTAssertEqual(LineageViews.pinRoute(older: false, pinned: false, scope: .shared), .ordinary)
    }

    func testAnOlderSegmentsPersonalPinIsUnpinnedInTheStore() {
        XCTAssertEqual(LineageViews.pinRoute(older: true, pinned: false, scope: .personal), .ordinary)
    }

    func testAnOlderSegmentsSharedPinIsUnpinnedOnThisDeviceOnly() {
        XCTAssertEqual(LineageViews.pinRoute(older: true, pinned: false, scope: .shared), .localOnly)
    }

    func testNothingIsNewlyPinnedInAnOlderSegment() {
        XCTAssertEqual(LineageViews.pinRoute(older: true, pinned: true, scope: .shared), .refused)
        XCTAssertEqual(LineageViews.pinRoute(older: true, pinned: true, scope: .personal), .refused)
    }

    // MARK: - historyRowOffersUnpin

    func testAPinnedRowOfAnOlderSegmentOffersUnpinAlone() {
        XCTAssertTrue(LineageViews.historyRowOffersUnpin(isHistoryRow: true, pinRow: .unpin))
    }

    func testAnUnpinnedHistoryRowOffersNothingAndACurrentRowIsNotThisRules() {
        XCTAssertFalse(LineageViews.historyRowOffersUnpin(isHistoryRow: true, pinRow: .pin))
        XCTAssertFalse(LineageViews.historyRowOffersUnpin(isHistoryRow: false, pinRow: .unpin))
    }

    // MARK: - forwardJumpRow

    private func jump(_ originGroupIdB64: String) -> Int64? {
        LineageViews.forwardJumpRow(
            originGroupIdB64: originGroupIdB64, messageId: 12, headGroupIdB64: "S2", heldHistory: ["G", "S1"])
    }

    func testTheCurrentGroupJumpsToTheMessageIdItself() {
        XCTAssertEqual(jump("S2"), 12)
    }

    func testAnOlderSegmentJumpsToTheIdItsRowIsDrawnUnderByDistanceFromTheHead() {
        XCTAssertEqual(jump("S1"), rowId(12, 1))
        XCTAssertEqual(jump("G"), rowId(12, 2))
    }

    func testASegmentThisDeviceDoesNotHoldJumpsNowhere() {
        XCTAssertNil(jump("other"))
    }

    func testAConversationThatNeverMovedJumpsUnchanged() {
        XCTAssertEqual(
            LineageViews.forwardJumpRow(originGroupIdB64: "G", messageId: 5, headGroupIdB64: "G", heldHistory: []),
            5)
    }

    // MARK: - conversationDeleteOrder

    private func seg(_ id: String) -> LineageViews.ConversationSegment {
        LineageViews.ConversationSegment(groupIdB64: id, groupIdHex: "hex-\(id)")
    }

    func testOrdersEveryHeldSegmentBeforeTheHeadEachOnce() {
        let order = LineageViews.conversationDeleteOrder(
            [seg("old"), seg("head"), seg("beyond"), seg("old")], headGroupIdB64: "head")
        XCTAssertEqual(order.map(\.groupIdB64), ["old", "beyond", "head"])
        XCTAssertEqual(order.map(\.groupIdHex), ["hex-old", "hex-beyond", "hex-head"])
        // A head the device does not hold: whatever is held still goes.
        XCTAssertEqual(
            LineageViews.conversationDeleteOrder([seg("old")], headGroupIdB64: "head").map(\.groupIdB64), ["old"])
    }

    // MARK: - the feed's sources are conversations, not groups

    private func conv(_ conversationHex: String, _ headHex: String) -> LineageViews.GroupConversationForFeed {
        LineageViews.GroupConversationForFeed(
            conversationHex: conversationHex, headHex: headHex, slug: "slug-\(headHex)",
            title: "title-\(conversationHex)", audience: 3)
    }

    func testAConversationThatMovedIsOneSourceDrawnFromItsCurrentGroupKeyedOnTheConversation() {
        let muted: Set<String> = ["root"]
        let sources = LineageViews.groupFeedSources([conv("root", "successor"), conv("plain", "plain")]) {
            muted.contains($0)
        }
        XCTAssertEqual(
            sources,
            [
                LineageViews.FeedSource(
                    kind: .group, idHex: "successor", conversationHex: "root", slug: "slug-successor",
                    title: "title-root", muted: true, audience: 3),
                LineageViews.FeedSource(
                    kind: .group, idHex: "plain", conversationHex: nil, slug: "slug-plain", title: "title-plain",
                    muted: false, audience: 3),
            ])
        XCTAssertEqual(sources.map { LineageViews.feedSourceKey($0) }, ["root", "plain"])
        XCTAssertEqual(LineageViews.feedSourceKey(idHex: "successor", conversationHex: "root"), "root")
        XCTAssertEqual(LineageViews.feedSourceKey(idHex: "plain", conversationHex: nil), "plain")
    }

    func testNeverListsOneConversationTwice() {
        let sources = LineageViews.groupFeedSources([conv("root", "a"), conv("root", "b")]) { _ in false }
        XCTAssertEqual(sources.count, 1)
        XCTAssertEqual(sources.first?.idHex, "a")
    }

    func testHiddenBeforeTheMoveStaysHiddenAfterIt() {
        let sources = LineageViews.groupFeedSources([conv("root", "successor"), conv("plain", "plain")]) { _ in false }
        // Hiding is by the source key (the conversation id), as the feed's
        // hidden list applies it.
        func withoutHidden(_ hidden: Set<String>) -> [LineageViews.FeedSource] {
            sources.filter { !hidden.contains(LineageViews.feedSourceKey($0)) }
        }
        XCTAssertEqual(withoutHidden(["root"]).map(\.idHex), ["plain"])
        // The successor's own id hides nothing: it is not what a person hid.
        XCTAssertEqual(withoutHidden(["successor"]).count, 2)
    }
}
