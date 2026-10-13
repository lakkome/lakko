// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0073 R.7 case 1 - the held send. The cases are those of
/// `apps/web/src/chat/held-send.test.ts`, the pure half of
/// `held-reply-and-flush.test.ts`, and the publish guard's table of
/// `dead-session-guard.test.ts`.
final class HeldSendTests: XCTestCase {

    // MARK: - sendRoute

    func testHoldsAOneToOneWhoseOtherPersonWasSuperseded() {
        XCTAssertEqual(HeldSend.sendRoute(kind: .chat, unrepairableKeyB64: "old-anna"), .hold)
    }

    func testSendsInAOneToOneThatIsFineAndOnTheSuccessor() {
        XCTAssertEqual(HeldSend.sendRoute(kind: .chat, unrepairableKeyB64: nil), .send)
    }

    func testNeverHoldsAGroup() {
        XCTAssertEqual(HeldSend.sendRoute(kind: .group, unrepairableKeyB64: "old-owner"), .send)
        XCTAssertEqual(HeldSend.sendRoute(kind: .group, unrepairableKeyB64: nil), .send)
    }

    // MARK: - heldVerdict

    func testGoesOutAsSoonAsTheRouteAllows() {
        XCTAssertEqual(HeldSend.heldVerdict(route: .send, heldAtMs: 0, nowMs: 10 * HeldSend.giveUpMs), .send)
    }

    func testWaitsThenGivesUpToTheOrdinaryFailedState() {
        XCTAssertEqual(
            HeldSend.heldVerdict(route: .hold, heldAtMs: 1000, nowMs: 1000 + HeldSend.giveUpMs - 1), .wait)
        XCTAssertEqual(
            HeldSend.heldVerdict(route: .hold, heldAtMs: 1000, nowMs: 1000 + HeldSend.giveUpMs), .giveUp)
    }

    func testTheGiveUpIsTenMinutes() {
        XCTAssertEqual(HeldSend.giveUpMs, 600_000)
    }

    func testANonsenseClockPairNeverTraps() {
        XCTAssertEqual(HeldSend.heldVerdict(route: .hold, heldAtMs: Int64.min, nowMs: Int64.max), .giveUp)
        XCTAssertEqual(HeldSend.heldVerdict(route: .hold, heldAtMs: Int64.max, nowMs: Int64.min), .wait)
    }

    // MARK: - Store

    func testKeepsMessagesPerConversationInSendOrderAndHandsThemOverOnce() {
        let store = HeldSend.Store<String>()
        store.hold(conversationId: "c1", payload: "first", heldAtMs: 1)
        store.hold(conversationId: "c2", payload: "other", heldAtMs: 2)
        store.hold(conversationId: "c1", payload: "second", heldAtMs: 3)
        XCTAssertEqual(store.count(conversationId: "c1"), 2)
        XCTAssertEqual(store.take(conversationId: "c1").map(\.payload), ["first", "second"])
        XCTAssertTrue(store.take(conversationId: "c1").isEmpty)
        XCTAssertEqual(store.count(conversationId: "c2"), 1)
        store.clear()
        XCTAssertTrue(store.take(conversationId: "c2").isEmpty)
    }

    func testPutsUnsentMessagesBackAtTheFrontInOrder() {
        let store = HeldSend.Store<String>()
        store.hold(conversationId: "c", payload: "a", heldAtMs: 1)
        store.hold(conversationId: "c", payload: "b", heldAtMs: 2)
        let taken = store.take(conversationId: "c")
        store.hold(conversationId: "c", payload: "typed-meanwhile", heldAtMs: 3)
        store.putBack(conversationId: "c", items: Array(taken.dropFirst()))
        XCTAssertEqual(store.take(conversationId: "c").map(\.payload), ["b", "typed-meanwhile"])
    }

    func testPuttingNothingBackHoldsNothing() {
        let store = HeldSend.Store<String>()
        store.putBack(conversationId: "c", items: [])
        XCTAssertTrue(store.all().isEmpty)
        XCTAssertEqual(store.count(conversationId: "c"), 0)
    }

    func testListsAndReplacesEverything() {
        let store = HeldSend.Store<String>()
        store.hold(conversationId: "gone", payload: "old", heldAtMs: 0)
        store.replaceAll([
            (conversationId: "c1", item: HeldSend.Item(heldAtMs: 1, payload: "x")),
            (conversationId: "c2", item: HeldSend.Item(heldAtMs: 2, payload: "y")),
            (conversationId: "c1", item: HeldSend.Item(heldAtMs: 3, payload: "z")),
        ])
        let all = store.all()
        XCTAssertEqual(all.map(\.conversationId), ["c1", "c2"])
        XCTAssertEqual(all[0].items.map(\.payload), ["x", "z"])
        XCTAssertEqual(all[0].items.map(\.heldAtMs), [1, 3])
        XCTAssertEqual(all[1].items.map(\.payload), ["y"])
        XCTAssertEqual(all[1].items.map(\.heldAtMs), [2])
    }

    // MARK: - identityCheckDue

    func testRunsWhenNeverCheckedOrOlderThanTheInterval() {
        XCTAssertTrue(HeldSend.identityCheckDue(lastCheckedAtMs: nil, nowMs: 5, intervalMs: 100))
        XCTAssertTrue(HeldSend.identityCheckDue(lastCheckedAtMs: 0, nowMs: 100, intervalMs: 100))
        XCTAssertFalse(HeldSend.identityCheckDue(lastCheckedAtMs: 1, nowMs: 100, intervalMs: 100))
    }

    // MARK: - opRoute

    func testAnOpIsAppliedLocallyAndNeverSentWhereAMessageWouldBeHeld() {
        XCTAssertEqual(HeldSend.opRoute(kind: .chat, unrepairableKeyB64: "old-key"), .localOnly)
    }

    func testAnOpIsSentEverywhereElse() {
        XCTAssertEqual(HeldSend.opRoute(kind: .chat, unrepairableKeyB64: nil), .send)
        XCTAssertEqual(HeldSend.opRoute(kind: .group, unrepairableKeyB64: "owner-key"), .send)
        XCTAssertEqual(HeldSend.opRoute(kind: .group, unrepairableKeyB64: nil), .send)
    }

    // MARK: - isDeadSession

    func testAOneToOneWhoseOtherPersonsResetWasAcceptedIsDead() {
        XCTAssertTrue(HeldSend.isDeadSession(kind: .chat, unrepairableKeyB64: "old-key"))
    }

    func testALiveOneToOneAndEveryGroupAreNotDead() {
        XCTAssertFalse(HeldSend.isDeadSession(kind: .chat, unrepairableKeyB64: nil))
        XCTAssertFalse(HeldSend.isDeadSession(kind: .group, unrepairableKeyB64: "owner-key"))
    }

    // MARK: - flushVerdict

    private func flush(
        held: Int = 2, viewMounted: Bool = false, headActive: Bool = true,
        headRoute: HeldSend.SendRoute = .send
    ) -> HeldSend.FlushVerdict {
        HeldSend.flushVerdict(held: held, viewMounted: viewMounted, headActive: headActive, headRoute: headRoute)
    }

    func testFlushesOnceTheConversationsCurrentGroupCanTakeAMessage() {
        XCTAssertEqual(flush(), .flush)
    }

    func testWaitsWhileTheCurrentGroupIsStillTheSupersededSession() {
        XCTAssertEqual(flush(headRoute: .hold), .wait)
    }

    func testLeavesThemToAMountedViewOfTheConversation() {
        XCTAssertEqual(flush(viewMounted: true), .wait)
    }

    func testWaitsWhenThisDeviceIsNotLiveInTheCurrentGroupAndWithNothingHeld() {
        XCTAssertEqual(flush(headActive: false), .wait)
        XCTAssertEqual(flush(held: 0), .wait)
    }

    // MARK: - replyForSend (held-reply-and-flush.test.ts)

    private let reply = HeldSend.Reply(target: 7, author: "Anna", excerpt: "see you")

    func testNoReplyStaysNoReply() {
        XCTAssertNil(HeldSend.replyForSend(nil, composedInGroupIdB64: "old", sendIntoGroupIdB64: "new"))
    }

    func testAReplySentIntoTheGroupItWasComposedInIsUnchanged() {
        XCTAssertEqual(
            HeldSend.replyForSend(reply, composedInGroupIdB64: "old", sendIntoGroupIdB64: "old"), reply)
    }

    func testAReplyFlushedIntoTheSuccessorKeepsItsQuoteAndDropsItsJumpTarget() {
        XCTAssertEqual(
            HeldSend.replyForSend(reply, composedInGroupIdB64: "old", sendIntoGroupIdB64: "new"),
            HeldSend.Reply(target: HeldSend.replyNoJumpTarget, author: "Anna", excerpt: "see you"))
    }

    func testAReplyWhoseComposeGroupWasNotRecordedIsNeverSentWithATarget() {
        XCTAssertEqual(
            HeldSend.replyForSend(reply, composedInGroupIdB64: nil, sendIntoGroupIdB64: "new")?.target,
            HeldSend.replyNoJumpTarget)
    }

    func testTheNoTargetValueNamesNoMessage() {
        // DS ids start at 1 and history rows are negative.
        XCTAssertEqual(HeldSend.replyNoJumpTarget, 0)
    }

    func testAReplyAlreadyWithoutATargetIsHandedBackAsItIs() {
        let noJump = HeldSend.Reply(target: 0, author: "Anna", excerpt: "see you")
        XCTAssertEqual(
            HeldSend.replyForSend(noJump, composedInGroupIdB64: "old", sendIntoGroupIdB64: "new"), noJump)
    }

    func testTheGenericFormKeepsEverythingButTheTarget() {
        struct OwnReply: Equatable {
            let target: Int64
            let quote: String
        }
        let own = OwnReply(target: 9, quote: "q")
        let sent = HeldSend.replyForSend(
            own, composedInGroupIdB64: "old", sendIntoGroupIdB64: "new",
            target: { $0.target }, withTarget: { OwnReply(target: $1, quote: $0.quote) })
        XCTAssertEqual(sent, OwnReply(target: 0, quote: "q"))
        let same = HeldSend.replyForSend(
            own, composedInGroupIdB64: "g", sendIntoGroupIdB64: "g",
            target: { $0.target }, withTarget: { OwnReply(target: $1, quote: $0.quote) })
        XCTAssertEqual(same, own)
    }

    // MARK: - flushedRowsToShow (held-reply-and-flush.test.ts)

    private func shown(_ held: Set<Int64>, _ flushed: [String]) -> [String] {
        HeldSend.flushedRowsToShow(shownMessageIds: held, flushed: flushed, messageId: { Int64($0) })
    }

    func testShowsWhatTheViewDoesNotHoldInSendOrder() {
        XCTAssertEqual(shown([5], ["6", "7"]), ["6", "7"])
    }

    func testSkipsWhatTheLogReadAlreadyBroughtIn() {
        XCTAssertEqual(shown([6], ["6", "7"]), ["7"])
    }

    func testShowsARepeatedReportOnceAndDropsAnUnreadableId() {
        XCTAssertEqual(shown([], ["6", "6", "x"]), ["6"])
    }

    // MARK: - what a dead session does with each outgoing thing

    private func action(_ outgoing: HeldSend.Outgoing, dead: Bool = true) -> HeldSend.OutgoingAction {
        HeldSend.outgoingAction(kind: .chat, unrepairableKeyB64: dead ? "old-key" : nil, outgoing: outgoing)
    }

    func testTextMediaAndAForwardAreHeld() {
        XCTAssertEqual(action(.text), .hold)
        XCTAssertEqual(action(.media), .hold)
        XCTAssertEqual(action(.forward), .hold)
    }

    func testAnEditADeleteAndAReactionAreAppliedLocallyOnly() {
        XCTAssertEqual(action(.edit), .localOnly)
        XCTAssertEqual(action(.delete), .localOnly)
        XCTAssertEqual(action(.reaction), .localOnly)
    }

    func testTypingAndAReadReceiptAreNotSent() {
        XCTAssertEqual(action(.typing), .notSent)
        XCTAssertEqual(action(.readReceipt), .notSent)
    }

    func testTheClaimIsTheOnlyThingPublishedIntoADeadSession() {
        XCTAssertEqual(action(.successorClaim), .publish)
        let published = HeldSend.Outgoing.allCases.filter { action($0) == .publish }
        XCTAssertEqual(published, [.successorClaim])
    }

    func testEverythingIsPublishedInALiveOneToOneAndInEveryGroup() {
        for outgoing in HeldSend.Outgoing.allCases {
            XCTAssertEqual(action(outgoing, dead: false), .publish)
            XCTAssertEqual(
                HeldSend.outgoingAction(kind: .group, unrepairableKeyB64: "owner-key", outgoing: outgoing),
                .publish)
        }
    }

    // MARK: - the publish guard (dead-session-guard.test.ts)

    func testThePublishGuardRefusesAMessageAndAControlFrame() {
        XCTAssertTrue(HeldSend.publishRefused(isDeadSession: true, isSuccessorClaim: false))
    }

    func testThePublishGuardLetsTheSuccessorClaimThrough() {
        XCTAssertFalse(HeldSend.publishRefused(isDeadSession: true, isSuccessorClaim: true))
    }

    func testThePublishGuardBlocksNothingInALiveSession() {
        XCTAssertFalse(HeldSend.publishRefused(isDeadSession: false, isSuccessorClaim: false))
        XCTAssertFalse(HeldSend.publishRefused(isDeadSession: false, isSuccessorClaim: true))
    }
}
