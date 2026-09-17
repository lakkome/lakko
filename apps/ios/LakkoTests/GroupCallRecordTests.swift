// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// The group-call announce contract (`GroupCallStart` / `GroupCallJoin` /
/// `GroupCallLeave` / `CallEnd` on the chat's MLS journal) — the reducer, the
/// liveness rule, the C-11 routing split, the banner decision, and the
/// first-byte sniffers that keep a group-call body off the timeline and out of
/// a notification preview.
///
/// The FFI-facing cases compose their fixtures with the REAL
/// `composeCallSignaling`, so the sniffer is checked against the bytes prost
/// actually emits rather than against a hand-typed tag.
@MainActor
final class GroupCallRecordTests: XCTestCase {

    // MARK: - Fixtures

    private let callA = Data(repeating: 0x0A, count: 16)
    private let callB = Data(repeating: 0x0B, count: 16)
    private let t0: Int64 = 1_800_000_000_000

    private func start(
        _ callId: Data,
        at createdAtMs: Int64,
        leaf: UInt32 = 1,
        video: Bool = false
    ) -> GroupCallSignal {
        .start(
            callId: callId,
            initiatorLeaf: leaf,
            createdAtMs: UInt64(createdAtMs),
            audio: true,
            video: video
        )
    }

    private func record(
        _ signals: [(GroupCallSignal, Int64)],
        from initial: OngoingGroupCall? = nil
    ) -> OngoingGroupCall? {
        signals.reduce(initial) { current, step in
            GroupCallReducer.apply(step.0, to: current, rowCreatedAtMs: step.1)
        }
    }

    // MARK: - Create / join / leave / end

    func testStartCreatesARecordWithNobodyJoined() throws {
        let r = try XCTUnwrap(record([(start(callA, at: t0, leaf: 4, video: true), t0)]))
        XCTAssertEqual(r.callId, callA)
        XCTAssertEqual(r.initiatorLeaf, 4)
        XCTAssertTrue(r.video)
        XCTAssertEqual(r.startedAtMs, t0)
        XCTAssertTrue(r.joinedLeaves.isEmpty)
    }

    func testStartTimeIsBoundedByTheServerRowTime() throws {
        // A sender claiming a start an hour in the future of its DS row does
        // not get to keep its call "fresh" for an extra hour.
        let r = try XCTUnwrap(record([(start(callA, at: t0 + 3_600_000), t0)]))
        XCTAssertEqual(r.startedAtMs, t0)
        // A claimed start earlier than the row is kept as claimed.
        let early = try XCTUnwrap(record([(start(callA, at: t0 - 5_000), t0)]))
        XCTAssertEqual(early.startedAtMs, t0 - 5_000)
    }

    func testJoinAndLeaveUpdateJoinedLeavesForTheMatchingCall() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
            (.join(callId: callA, leafIndex: 2), t0 + 2_000),
            (.leave(callId: callA, leafIndex: 1), t0 + 3_000),
        ]))
        XCTAssertEqual(r.joinedLeaves, [2])
        XCTAssertEqual(r.lastActivityMs, t0 + 3_000)
    }

    func testJoinLeaveAndEndForAnotherCallIdAreDropped() throws {
        // C-12 — a body naming a call id the record does not track changes
        // nothing, in particular a stray CallEnd does not clear the banner.
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
            (.join(callId: callB, leafIndex: 7), t0 + 2_000),
            (.leave(callId: callB, leafIndex: 1), t0 + 3_000),
            (.end(callId: callB), t0 + 4_000),
        ]))
        XCTAssertEqual(r.callId, callA)
        XCTAssertEqual(r.joinedLeaves, [1])
    }

    func testJoinWithNoRecordCreatesNothing() {
        XCTAssertNil(record([(.join(callId: callA, leafIndex: 1), t0)]))
    }

    func testEndClearsTheMatchingRecord() {
        XCTAssertNil(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
            (.end(callId: callA), t0 + 2_000),
        ]))
    }

    func testReannouncingTheTrackedCallKeepsItsJoins() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 3), t0 + 1_000),
            (start(callA, at: t0 + 5_000), t0 + 5_000),
        ]))
        XCTAssertEqual(r.joinedLeaves, [3])
        XCTAssertEqual(r.startedAtMs, t0)
    }

    // MARK: - Liveness

    func testAnUnjoinedCallIsLiveForTheGraceWindowOnly() throws {
        let r = try XCTUnwrap(record([(start(callA, at: t0), t0)]))
        XCTAssertTrue(GroupCallReducer.isLive(r, nowMs: t0))
        XCTAssertTrue(GroupCallReducer.isLive(r, nowMs: t0 + GroupCallReducer.unjoinedGraceMs))
        XCTAssertFalse(GroupCallReducer.isLive(r, nowMs: t0 + GroupCallReducer.unjoinedGraceMs + 1))
    }

    func testAJoinedCallStaysLiveUntilTheMaxAge() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
        ]))
        XCTAssertTrue(GroupCallReducer.isLive(r, nowMs: t0 + 2 * 60 * 60 * 1000))
        XCTAssertTrue(GroupCallReducer.isLive(r, nowMs: t0 + GroupCallReducer.maxLiveAgeMs))
        XCTAssertFalse(GroupCallReducer.isLive(r, nowMs: t0 + GroupCallReducer.maxLiveAgeMs + 1))
    }

    func testTheLastLeaveEndsLivenessAfterTheGraceWindow() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
            (.leave(callId: callA, leafIndex: 1), t0 + 120_000),
        ]))
        XCTAssertFalse(GroupCallReducer.isLive(r, nowMs: t0 + 120_000))
    }

    func testTheWindowsAreTheContractValues() {
        XCTAssertEqual(GroupCallReducer.maxLiveAgeMs, 4 * 60 * 60 * 1000)
        XCTAssertEqual(GroupCallReducer.unjoinedGraceMs, 60 * 1000)
    }

    // MARK: - Two call ids

    func testOfTwoLiveCallsTheEarlierStartWinsInEitherArrivalOrder() throws {
        let earlyA = start(callB, at: t0)  // callB starts first
        let lateB = start(callA, at: t0 + 10_000)

        let oneWay = try XCTUnwrap(record([(earlyA, t0), (lateB, t0 + 10_000)]))
        let otherWay = try XCTUnwrap(record([(lateB, t0 + 10_000), (earlyA, t0 + 10_500)]))
        XCTAssertEqual(oneWay.callId, callB)
        XCTAssertEqual(otherWay.callId, callB)
    }

    func testOfTwoLiveCallsWithTheSameStartTheLowerCallIdHexWins() throws {
        let r1 = try XCTUnwrap(record([(start(callB, at: t0), t0), (start(callA, at: t0), t0 + 1)]))
        let r2 = try XCTUnwrap(record([(start(callA, at: t0), t0), (start(callB, at: t0), t0 + 1)]))
        XCTAssertEqual(r1.callId, callA)
        XCTAssertEqual(r2.callId, callA)
    }

    func testAStaleRecordIsReplacedByANewStart() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0), t0),
            // callA never got a join; two minutes later it is not live.
            (start(callB, at: t0 + 120_000), t0 + 120_000),
        ]))
        XCTAssertEqual(r.callId, callB)
    }

    func testAStaleStartDoesNotDisplaceALiveCall() throws {
        let r = try XCTUnwrap(record([
            (start(callB, at: t0), t0),
            (.join(callId: callB, leafIndex: 1), t0 + 1_000),
            // An old announce (claimed an hour back, so never live) for a lower
            // call id must not steal the record from the live call.
            (start(callA, at: t0 - 3_600_000), t0 + 2_000),
        ]))
        XCTAssertEqual(r.callId, callB)
        XCTAssertEqual(r.joinedLeaves, [1])
    }

    // MARK: - C-11 routing

    func testGroupControlBodiesNeverReachTheOneToOneSession() {
        let media = FfiCallMediaFlags(audio: true, video: false)
        let bodies: [FfiCallSignalingBody] = [
            .groupStart(callId: callA, initiatorLeaf: 1, createdAtMs: 1, media: media),
            .groupJoin(callId: callA, leafIndex: 1, joinedAtMs: 1),
            .groupLeave(callId: callA, leafIndex: 1, reason: .hungUp),
        ]
        for body in bodies {
            let route = CallSignalingRoute.of(body)
            XCTAssertFalse(route.forwardToOneToOne, "\(body) must not reach the 1x1 session")
            XCTAssertNotNil(route.groupSignal, "\(body) must update the group-call record")
        }
    }

    func testOneToOneBodiesAreForwardedAndCarryNoGroupSignal() {
        let media = FfiCallMediaFlags(audio: true, video: false)
        let bodies: [FfiCallSignalingBody] = [
            .offer(callId: callA, sdp: "v=0", createdAtMs: 1, media: media),
            .answer(callId: callA, sdp: "v=0"),
            .iceCandidate(callId: callA, candidate: "c", sdpMid: "0", sdpMLineIndex: 0),
        ]
        for body in bodies {
            let route = CallSignalingRoute.of(body)
            XCTAssertTrue(route.forwardToOneToOne)
            XCTAssertNil(route.groupSignal)
        }
    }

    func testCallEndIsBothTheOneToOneAndTheGroupTerminal() {
        let route = CallSignalingRoute.of(.end(callId: callA, reason: .hungUp))
        XCTAssertTrue(route.forwardToOneToOne)
        XCTAssertEqual(route.groupSignal, .end(callId: callA))
    }

    func testTheRepositoryFoldsGroupBodiesAndForwardsOnlyOneToOneBodies() throws {
        let repo = ChatRepository()
        let group = "ab"
        var forwarded: [FfiCallSignalingBody] = []
        repo.callSignalingSink = { row in forwarded.append(row.body) }

        let startBytes = try composeCallSignaling(
            body: .groupStart(
                callId: callA, initiatorLeaf: 2, createdAtMs: UInt64(t0),
                media: FfiCallMediaFlags(audio: true, video: true)))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 1, payload: startBytes, createdAt: "2027-01-15T08:00:00Z", senderLeaf: 2)
        let joinBytes = try composeCallSignaling(
            body: .groupJoin(callId: callA, leafIndex: 2, joinedAtMs: UInt64(t0)))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 2, payload: joinBytes, createdAt: "2027-01-15T08:00:01Z", senderLeaf: 2)

        XCTAssertTrue(forwarded.isEmpty, "group bodies reached the 1x1 sink: \(forwarded)")
        let r = try XCTUnwrap(repo.groupCalls[group])
        XCTAssertEqual(r.callId, callA)
        XCTAssertEqual(r.joinedLeaves, [2])

        let offerBytes = try composeCallSignaling(
            body: .offer(
                callId: callB, sdp: "v=0", createdAtMs: 1,
                media: FfiCallMediaFlags(audio: true, video: false)))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 3, payload: offerBytes, createdAt: "2027-01-15T08:00:02Z", senderLeaf: 3)
        XCTAssertEqual(forwarded.count, 1)
        XCTAssertEqual(repo.groupCalls[group]?.joinedLeaves, [2])

        let endBytes = try composeCallSignaling(body: .end(callId: callA, reason: .hungUp))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 4, payload: endBytes, createdAt: "2027-01-15T08:00:03Z", senderLeaf: 3)
        XCTAssertEqual(forwarded.count, 2)
        XCTAssertNil(repo.groupCalls[group])
    }

    func testAStartWithAnUnreadableRowTimeIsDropped() throws {
        let repo = ChatRepository()
        let startBytes = try composeCallSignaling(
            body: .groupStart(
                callId: callA, initiatorLeaf: 2, createdAtMs: UInt64(t0),
                media: FfiCallMediaFlags(audio: true, video: false)))
        repo.routeCallSignaling(
            groupIdHex: "ab", messageId: 5, payload: startBytes, createdAt: "not a time", senderLeaf: 2)
        XCTAssertNil(repo.groupCalls["ab"])
    }

    // MARK: - A body speaks only for its MLS sender

    /// The claimed leaf of each group body is what the check compares.
    func testEachGroupBodyClaimsTheLeafItNames() {
        XCTAssertEqual(start(callA, at: t0, leaf: 4).claimedLeaf, 4)
        XCTAssertEqual(GroupCallSignal.join(callId: callA, leafIndex: 5).claimedLeaf, 5)
        XCTAssertEqual(GroupCallSignal.leave(callId: callA, leafIndex: 6).claimedLeaf, 6)
        XCTAssertNil(GroupCallSignal.end(callId: callA).claimedLeaf)
    }

    /// The pure rule: equal leaves pass, any other leaf or no sender refuses,
    /// `CallEnd` names no leaf and passes.
    func testTheLeafCheckTable() {
        XCTAssertTrue(start(callA, at: t0, leaf: 4).speaksFor(senderLeaf: 4))
        XCTAssertFalse(start(callA, at: t0, leaf: 4).speaksFor(senderLeaf: 5))
        XCTAssertFalse(start(callA, at: t0, leaf: 4).speaksFor(senderLeaf: nil))
        XCTAssertFalse(GroupCallSignal.join(callId: callA, leafIndex: 1).speaksFor(senderLeaf: 2))
        XCTAssertFalse(GroupCallSignal.leave(callId: callA, leafIndex: 1).speaksFor(senderLeaf: 2))
        XCTAssertTrue(GroupCallSignal.end(callId: callA).speaksFor(senderLeaf: nil))
    }

    /// Through the repository: a member cannot announce a call as someone
    /// else, join someone else into the roster, or leave them out of it. Each
    /// forged row is dropped; the honest rows around them still land.
    func testTheRepositoryDropsGroupBodiesWhoseLeafIsNotTheSender() throws {
        let repo = ChatRepository()
        let group = "cd"
        let forgedStart = try composeCallSignaling(
            body: .groupStart(
                callId: callA, initiatorLeaf: 2, createdAtMs: UInt64(t0),
                media: FfiCallMediaFlags(audio: true, video: false)))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 6, payload: forgedStart, createdAt: "2027-01-15T08:00:00Z", senderLeaf: 7)
        XCTAssertNil(repo.groupCalls[group], "a start naming leaf 2, sent by leaf 7, must not create a call")
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 7, payload: forgedStart, createdAt: "2027-01-15T08:00:00Z", senderLeaf: nil)
        XCTAssertNil(repo.groupCalls[group], "a start with no member sender must not create a call")

        repo.routeCallSignaling(
            groupIdHex: group, messageId: 8, payload: forgedStart, createdAt: "2027-01-15T08:00:00Z", senderLeaf: 2)
        XCTAssertEqual(repo.groupCalls[group]?.callId, callA)

        let forgedJoin = try composeCallSignaling(
            body: .groupJoin(callId: callA, leafIndex: 3, joinedAtMs: UInt64(t0)))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 9, payload: forgedJoin, createdAt: "2027-01-15T08:00:01Z", senderLeaf: 2)
        XCTAssertEqual(repo.groupCalls[group]?.joinedLeaves, [], "leaf 2 cannot join leaf 3 into the call")
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 10, payload: forgedJoin, createdAt: "2027-01-15T08:00:01Z", senderLeaf: 3)
        XCTAssertEqual(repo.groupCalls[group]?.joinedLeaves, [3])

        let forgedLeave = try composeCallSignaling(
            body: .groupLeave(callId: callA, leafIndex: 3, reason: .hungUp))
        repo.routeCallSignaling(
            groupIdHex: group, messageId: 11, payload: forgedLeave, createdAt: "2027-01-15T08:00:02Z", senderLeaf: 2)
        XCTAssertEqual(repo.groupCalls[group]?.joinedLeaves, [3], "leaf 2 cannot leave leaf 3 out of the call")
    }

    // MARK: - Banner decision

    func testALiveCallThisDeviceIsNotInIsJoinable() throws {
        let r = try XCTUnwrap(record([
            (start(callA, at: t0, video: true), t0),
            (.join(callId: callA, leafIndex: 1), t0 + 1_000),
            (.join(callId: callA, leafIndex: 2), t0 + 2_000),
        ]))
        XCTAssertEqual(
            GroupCallAffordance.of(record: r, activeGroupCallId: nil, nowMs: t0 + 10_000),
            .joinable(callId: callA, joinedCount: 2, video: true)
        )
        XCTAssertEqual(
            GroupCallAffordance.of(record: r, activeGroupCallId: callA, nowMs: t0 + 10_000),
            .inCall
        )
    }

    func testADeadRecordOffersNothing() throws {
        let r = try XCTUnwrap(record([(start(callA, at: t0), t0)]))
        XCTAssertEqual(
            GroupCallAffordance.of(
                record: r, activeGroupCallId: nil,
                nowMs: t0 + GroupCallReducer.unjoinedGraceMs + 1),
            .none
        )
        XCTAssertEqual(GroupCallAffordance.of(record: nil, activeGroupCallId: nil, nowMs: t0), .none)
    }

    // MARK: - Sniffers (timeline + notification)

    private func groupBodies() throws -> [(String, Data)] {
        [
            ("groupStart", try composeCallSignaling(
                body: .groupStart(
                    callId: callA, initiatorLeaf: 1, createdAtMs: UInt64(t0),
                    media: FfiCallMediaFlags(audio: true, video: false)))),
            ("groupJoin", try composeCallSignaling(
                body: .groupJoin(callId: callA, leafIndex: 1, joinedAtMs: UInt64(t0)))),
            ("groupLeave", try composeCallSignaling(
                body: .groupLeave(callId: callA, leafIndex: 1, reason: .hungUp))),
        ]
    }

    func testTheComposedGroupBodiesOpenWithTheExpectedTags() throws {
        let bodies = try groupBodies()
        XCTAssertEqual(bodies[0].1.first, 0x72)
        XCTAssertEqual(bodies[1].1.first, 0x7A)
        XCTAssertEqual(Array(bodies[2].1.prefix(2)), [0x82, 0x01])
    }

    func testGroupBodiesAreCallSignalingNeverAChatBubble() throws {
        for (name, bytes) in try groupBodies() {
            XCTAssertTrue(ChatClient.looksLikeCallSignaling(bytes), name)
            let event = ChatClient.toEvent(
                messageId: 1,
                createdAt: "2027-01-15T08:00:00Z",
                processed: .application(data: bytes, senderLeaf: 1, senderAccount: nil),
                resolveSender: { "member-\($0)" }
            )
            guard case .callSignaling = event else {
                return XCTFail("\(name) classified as \(event), not .callSignaling")
            }
        }
    }

    func testGroupBodiesAreNeverANotificationPreview() throws {
        for (name, bytes) in try groupBodies() {
            XCTAssertTrue(nseLooksLikeCallSignaling(bytes), name)
            XCTAssertTrue(
                nseIsServiceEnvelope(
                    bytes,
                    decodesAsPushRoutingSecret: { _ in false },
                    decodesAsCommitterBeacon: { _ in false },
                    committerBeaconEnvelopeLen: 27,
                    decodesAsProfile: { _ in false },
                    profileEnvelopeLens: 28...315,
                    decodesAsGroupName: { _ in false },
                    groupNameEnvelopeLens: 30...285
                ),
                name
            )
        }
    }

    func testTheLeaveTagNeedsItsContinuationByte() {
        // 0x82 alone is not field 16; neither is 0x82 followed by another byte.
        for bytes in [Data([0x82]), Data([0x82, 0x02, 0x0A]), Data([0x80, 0x01])] {
            XCTAssertFalse(ChatClient.looksLikeCallSignaling(bytes), "\(Array(bytes))")
            XCTAssertFalse(nseLooksLikeCallSignaling(bytes), "\(Array(bytes))")
        }
    }

    func testLegacyPlainTextStartingWithRorZIsNotCallSignaling() {
        // 0x72 / 0x7A are ASCII `r` / `z`; the group tags must not swallow a
        // plain-text body that merely starts with one.
        for text in ["real message", "zebra crossing", "r", "z"] {
            let bytes = Data(text.utf8)
            XCTAssertFalse(ChatClient.looksLikeCallSignaling(bytes), text)
            XCTAssertFalse(nseLooksLikeCallSignaling(bytes), text)
        }
    }

    func testAGroupBodyWithWrongFramingIsNotCallSignaling() throws {
        for (name, bytes) in try groupBodies() {
            let truncated = bytes.dropLast()
            var extended = bytes
            extended.append(0x00)
            for candidate in [Data(truncated), extended] {
                XCTAssertFalse(ChatClient.looksLikeCallSignaling(candidate), name)
                XCTAssertFalse(nseLooksLikeCallSignaling(candidate), name)
            }
        }
    }

    func testAChatEnvelopeIsStillNotCallSignaling() throws {
        let chat = try composeEnvelope(text: "hello", attachments: [])
        XCTAssertFalse(ChatClient.looksLikeCallSignaling(chat))
        XCTAssertFalse(nseLooksLikeCallSignaling(chat))
    }
}

/// The record-permission gate every call entry point runs first.
final class MicrophonePermissionTests: XCTestCase {

    func testGrantedProceedsWithoutPrompting() async {
        var prompted = false
        let ok = await MicrophonePermission.ensureGranted(
            status: { .granted },
            request: { prompted = true; return false }
        )
        XCTAssertTrue(ok)
        XCTAssertFalse(prompted)
    }

    func testDeniedRefusesWithoutPrompting() async {
        var prompted = false
        let ok = await MicrophonePermission.ensureGranted(
            status: { .denied },
            request: { prompted = true; return true }
        )
        XCTAssertFalse(ok)
        XCTAssertFalse(prompted, "the system never re-prompts after a denial")
    }

    func testUndeterminedFollowsThePrompt() async {
        let granted = await MicrophonePermission.ensureGranted(
            status: { .undetermined }, request: { true })
        let refused = await MicrophonePermission.ensureGranted(
            status: { .undetermined }, request: { false })
        XCTAssertTrue(granted)
        XCTAssertFalse(refused)
    }
}

/// The "Create call link" form's offered choices and label rule.
final class CallLinkFormTests: XCTestCase {

    func testExpiryChoicesAreOneHourOneDayAndSevenDays() {
        XCTAssertEqual(CallLinkForm.expiryChoices, [.oneHour, .oneDay, .sevenDays])
        let now: UInt64 = 1_000_000
        XCTAssertEqual(CallInviteExpiryPreset.oneHour.toMs(nowMs: now), now + 3_600_000)
        XCTAssertEqual(CallInviteExpiryPreset.oneDay.toMs(nowMs: now), now + 86_400_000)
        XCTAssertEqual(CallInviteExpiryPreset.sevenDays.toMs(nowMs: now), now + 7 * 86_400_000)
        XCTAssertFalse(CallLinkForm.expiryChoices.contains(.never))
    }

    func testMaxUsesChoicesAreOneTenAndUnlimited() {
        XCTAssertEqual(CallLinkForm.maxUsesChoices.map { $0.toValue() }, [0, 10, UInt32.max])
    }

    func testTheLabelBudgetIsCountedInBytes() {
        XCTAssertNil(CallLinkForm.labelError(""))
        XCTAssertNil(CallLinkForm.labelError(String(repeating: "a", count: CallInviteUrl.labelMaxLen)))
        XCTAssertNotNil(CallLinkForm.labelError(String(repeating: "a", count: CallInviteUrl.labelMaxLen + 1)))
        // 33 two-byte characters = 66 bytes: over the budget at 33 characters.
        XCTAssertNotNil(CallLinkForm.labelError(String(repeating: "\u{00E9}", count: 33)))
    }
}
