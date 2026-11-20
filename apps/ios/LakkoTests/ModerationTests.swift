// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0098 step 2 on iOS: attested messages, the block routing, the
/// moderation client's answers, the moderator-list gate and the report
/// sheet's states.
@MainActor
final class ModerationTests: XCTestCase {

    // MARK: - Attested messages (send and receive through real groups)

    private struct Pair {
        let owner: Identity
        let bob: Identity
        let ownerGroup: Group
        let bobGroup: Group
    }

    private func pair() throws -> Pair {
        let owner = try Identity.fromSeed(seed: Data(repeating: 0x21, count: 32))
        let bob = try Identity.fromSeed(seed: Data(repeating: 0x42, count: 32))
        let ownerGroup = try Group.create(identity: owner, kind: .group)
        let add = try ownerGroup.addMemberFromWire(identity: owner, keyPackage: try bob.createKeyPackage())
        let bobGroup = try Group.joinFromWelcome(welcome: add.welcome, identity: bob)
        return Pair(owner: owner, bob: bob, ownerGroup: ownerGroup, bobGroup: bobGroup)
    }

    func testAnAttestedChatMessageReadsAsItsTextAndKeepsVerifiedEvidence() throws {
        let p = try pair()
        let envelope = try AttachmentBridge.composeEnvelopeBytes(text: "hello from Bob", attachments: [])
        let attested = try ChatRepository.attested(identity: p.bob, group: p.bobGroup, envelope: envelope)
        XCTAssertNotEqual(attested, envelope, "a chat envelope is wrapped before the encrypt")
        let wire = try p.bobGroup.createApplicationMessage(identity: p.bob, plaintext: attested)
        let processed = try p.ownerGroup.processIncomingVerifyingRoles(identity: p.owner, bytes: wire)

        let event = ChatClient.toEvent(messageId: 7, createdAt: "2026-11-08T00:00:00Z", processed: processed, group: p.ownerGroup)
        guard case .application(_, let text, _, _, _, _, _) = event else {
            return XCTFail("an attested chat message is a timeline row, got \(event)")
        }
        XCTAssertEqual(text, "hello from Bob")

        let evidence = try XCTUnwrap(ChatClient.messageEvidence(of: processed, group: p.ownerGroup))
        XCTAssertEqual(evidence.attestation, .verified)
        XCTAssertEqual(evidence.plaintext, attested, "the stored plaintext is the wrapper as received")
        XCTAssertEqual(evidence.leafKey, try p.bob.signaturePublicKey(), "the sender LEAF key, read at receipt")
    }

    func testAnAttestationForAnotherGroupFailsAndTheMessageStillShows() throws {
        let p = try pair()
        let other = try Group.create(identity: p.bob, kind: .group)
        let envelope = try AttachmentBridge.composeEnvelopeBytes(text: "moved here", attachments: [])
        let foreign = try attestMessage(identity: p.bob, groupId: try other.groupId(), envelope: envelope)
        let wire = try p.bobGroup.createApplicationMessage(identity: p.bob, plaintext: foreign)
        let processed = try p.ownerGroup.processIncomingVerifyingRoles(identity: p.owner, bytes: wire)

        guard case .application(_, let text, _, _, _, _, _) = ChatClient.toEvent(
            messageId: 1, createdAt: "", processed: processed, group: p.ownerGroup) else {
            return XCTFail("a failed attestation drops nothing")
        }
        XCTAssertEqual(text, "moved here")
        XCTAssertEqual(ChatClient.messageEvidence(of: processed, group: p.ownerGroup)?.attestation, .failed)
    }

    func testAnUnwrappedChatMessageIsAbsentAndTypingKeepsNoEvidence() throws {
        let p = try pair()
        let plain = try p.bobGroup.createApplicationMessage(
            identity: p.bob, plaintext: try AttachmentBridge.composeEnvelopeBytes(text: "old client", attachments: []))
        let processedPlain = try p.ownerGroup.processIncomingVerifyingRoles(identity: p.owner, bytes: plain)
        XCTAssertEqual(ChatClient.messageEvidence(of: processedPlain, group: p.ownerGroup)?.attestation, .absent)

        let typing = try p.bobGroup.createApplicationMessage(
            identity: p.bob, plaintext: try composeTypingEnvelope(isTyping: true))
        let processedTyping = try p.ownerGroup.processIncomingVerifyingRoles(identity: p.owner, bytes: typing)
        XCTAssertNil(ChatClient.messageEvidence(of: processedTyping, group: p.ownerGroup),
                     "only a chat message is kept for a report")
    }

    func testAServiceEnvelopeIsNeverAttested() throws {
        let p = try pair()
        XCTAssertThrowsError(try ChatRepository.attested(
            identity: p.bob, group: p.bobGroup, envelope: composeCommitterBeacon()))
    }

    // MARK: - Block routing

    private let blocked = base64Encode(Data(repeating: 0xBB, count: 32))
    private let friend = base64Encode(Data(repeating: 0xCC, count: 32))

    func testOneToOneRowsOfABlockedPersonAreDroppedAndOthersKept() {
        let events: [IncomingChatEvent] = [
            .application(messageId: 1, text: "hi", attachments: [], senderKey: blocked, createdAt: "", reply: nil, forward: nil),
            .op(messageId: 2, op: .typing(isTyping: true), target: 0, text: "", emoji: nil, senderKey: blocked, createdAt: ""),
            .application(messageId: 3, text: "yo", attachments: [], senderKey: friend, createdAt: "", reply: nil, forward: nil),
            .serviceEnvelope(messageId: 4, createdAt: ""),
        ]
        let kept = BlockRouting.keptInOneToOne(
            events, senders: [blocked, blocked, friend, nil], isBlocked: { $0 == self.blocked })
        XCTAssertEqual(kept.events.count, 2)
        XCTAssertEqual(kept.senders, [friend, nil], "senders stay aligned; a sender-less row is always kept")
    }

    func testMisalignedSendersDropNothing() {
        let events: [IncomingChatEvent] = [
            .application(messageId: 1, text: "hi", attachments: [], senderKey: blocked, createdAt: "", reply: nil, forward: nil),
        ]
        XCTAssertEqual(BlockRouting.keptInOneToOne(events, senders: [], isBlocked: { _ in true }).events.count, 1)
    }

    func testTheOneToOnePathOfEachRow() {
        XCTAssertEqual(BlockRouting.oneToOnePath(of: .op(
            messageId: 1, op: .readReceipt, target: 1, text: "", emoji: nil, senderKey: nil, createdAt: "")),
            .incomingSignal)
        XCTAssertEqual(BlockRouting.oneToOnePath(of: .op(
            messageId: 1, op: .edit, target: 1, text: "x", emoji: nil, senderKey: nil, createdAt: "")),
            .oneToOneMessage)
        XCTAssertEqual(BlockRouting.oneToOnePath(of: .callSignaling(
            messageId: 1, payloadBytes: Data(), createdAt: "", senderLeaf: nil, senderDeviceKey: nil, senderKey: nil)),
            .incomingCall)
    }

    func testABlockedRequestIsDroppedFromTheInboxMerge() {
        let key = Data(repeating: 0xBB, count: 32)
        let request = IncomingContactRequest(
            messageId: 9, fromPubkey: key, fromAccountId: nil, fromPseudonym: Data(count: 32),
            validAfterMs: 0, intro: nil, createdAt: "")
        let merged = ContactsInboxMerge.merge(
            fetched: [request], pending: [], accepted: [], isBlocked: { $0 == key })
        XCTAssertTrue(merged.kept.isEmpty)
        XCTAssertEqual(merged.dropped.first?.reason, .blocked)
    }

    // MARK: - Report menu row

    func testReportIsOfferedOnSomeoneElsesMessageOnly() {
        XCTAssertEqual(
            MessageMenuAction.offered(canReply: true, canForward: true, canEditOrDelete: false, hasText: true, canReport: true).last,
            .report)
        XCTAssertFalse(
            MessageMenuAction.offered(canReply: true, canForward: true, canEditOrDelete: true, hasText: true, canReport: true)
                .contains(.report))
    }

    // MARK: - Moderation client answers

    func testTheSendAnswers() {
        let ok = Data(#"{"receipt_id":"AAAAAAAAAAAAAAAAAAAAAA"}"#.utf8)
        XCTAssertEqual(ModerationClient.sendResult(status: 201, body: ok), .sent(receiptId: "AAAAAAAAAAAAAAAAAAAAAA"))
        XCTAssertEqual(ModerationClient.sendResult(status: 429, body: Data()), .limited)
        XCTAssertEqual(ModerationClient.sendResult(
            status: 503, body: Data(#"{"error":"reporting-unavailable"}"#.utf8)), .unavailable)
        XCTAssertEqual(ModerationClient.sendResult(
            status: 503, body: Data(#"{"error":"anon-tokens-disabled"}"#.utf8)), .unavailable)
        XCTAssertEqual(ModerationClient.sendResult(status: 503, body: Data("<html>".utf8)), .failed,
                       "an outage in front of the service is not 'unavailable'")
        XCTAssertEqual(ModerationClient.sendResult(status: 400, body: Data()), .failed)
        XCTAssertEqual(ModerationClient.sendResult(tokenOutcome: .quotaExhausted(retryAfterSecs: nil)), .limited)
        XCTAssertEqual(ModerationClient.sendResult(tokenOutcome: .disabled), .unavailable)
        XCTAssertEqual(ModerationClient.sendResult(tokenOutcome: .timedOut), .failed)
    }

    func testTheListAnswersAndTheRootKeyAndReceiptShapes() {
        XCTAssertEqual(ModerationClient.listResult(status: 200, body: Data([1])), .list(Data([1])))
        XCTAssertEqual(ModerationClient.listResult(
            status: 503, body: Data(#"{"error":"reporting-unavailable"}"#.utf8)), .unavailable)
        XCTAssertEqual(ModerationClient.listResult(status: 500, body: Data()), .failed)
        XCTAssertNil(ModerationClient.rootKey(fromInfoValue: ""))
        XCTAssertNil(ModerationClient.rootKey(fromInfoValue: "AAAA"))
        XCTAssertEqual(ModerationClient.rootKey(fromInfoValue: base64UrlEncodeNoPad(Data(repeating: 7, count: 32)))?.count, 32)
        XCTAssertTrue(ModerationClient.isReceiptId("abcdefghijklmnopqrstu-"))
        XCTAssertFalse(ModerationClient.isReceiptId("../etc/passwd/xxxxxxxx"))
        XCTAssertFalse(ModerationClient.isReceiptId("short"))
    }

    func testAnOlderModeratorListIsRefused() {
        XCTAssertTrue(ModeratorListGate.accepts(version: 3, highestSeen: nil))
        XCTAssertTrue(ModeratorListGate.accepts(version: 3, highestSeen: 3))
        XCTAssertTrue(ModeratorListGate.accepts(version: 4, highestSeen: 3))
        XCTAssertFalse(ModeratorListGate.accepts(version: 2, highestSeen: 3))
    }

    func testAListThatDoesNotVerifyIsNotAdmitted() {
        let defaults = UserDefaults(suiteName: "moderation-tests")!
        defaults.removePersistentDomain(forName: "moderation-tests")
        XCTAssertNil(ModeratorListGate.admit(Data([0, 1, 2]), rootKey: Data(repeating: 1, count: 32), defaults: defaults))
        XCTAssertNil(defaults.object(forKey: ModeratorListGate.highestKey), "nothing is recorded for a refused list")
    }

    // MARK: - Evidence store bound

    func testTheEvidenceStoreKeepsTheNewestMessages() {
        let e = MessageEvidence(attestation: .absent, plaintext: Data(), leafKey: Data(count: 32), binding: Data())
        var rows: [String: MessageEvidence] = [:]
        for i in 0..<(MessageEvidenceStore.maxPerConversation + 10) { rows[String(i)] = e }
        let bounded = MessageEvidenceStore.bounded(rows)
        XCTAssertEqual(bounded.count, MessageEvidenceStore.maxPerConversation)
        XCTAssertNil(bounded["0"])
        XCTAssertNotNil(bounded[String(MessageEvidenceStore.maxPerConversation + 9)])
    }

    // MARK: - Report sheet states

    private final class Spy {
        var followUps: [ReportFollowUp] = []
        var recorded: [String] = []
        var sealedEvidence: [FfiEvidenceItem] = []
    }

    private func model(
        target: ReportTarget = .person(accountKey: Data(repeating: 1, count: 32)),
        followUp: ReportFollowUp? = .block,
        rootKey: Data? = Data(repeating: 9, count: 32),
        list: ModerationClient.ListResult = .list(Data([1])),
        send: ModerationClient.SendResult = .sent(receiptId: "AAAAAAAAAAAAAAAAAAAAAA"),
        picked: [Int64] = [],
        held: Set<Int64> = [],
        fixed: [FfiEvidenceItem] = [],
        spy: Spy
    ) -> ReportSheetModel {
        let request = ReportRequest(
            target: target, targetName: "Bob", conversationKind: .oneToOne, groupIdHex: "aa",
            initialMessageIds: picked, fixedEvidence: fixed, followUp: followUp, personName: "Bob")
        var ports = ReportSheetModel.Ports(
            moderatorList: { list },
            rootKey: rootKey,
            groupLookupKey: { _ in Data(repeating: 5, count: 64) },
            evidence: { held.contains($0) ? .listing(wire: Data([UInt8($0)])) : nil },
            send: { _ in send },
            record: { id, _ in spy.recorded.append(id) },
            applyFollowUp: { spy.followUps.append($0) })
        ports.seal = { _, _, _, _, _, evidence, _, _ in
            spy.sealedEvidence = evidence
            return Data([0xAB])
        }
        ports.accountTarget = { _ in Data(repeating: 2, count: 32) }
        return ReportSheetModel(request: request, ports: ports)
    }

    func testNoReasonNoSend() async {
        let spy = Spy()
        let m = model(spy: spy)
        XCTAssertFalse(m.canSend)
        await m.send()
        XCTAssertEqual(m.phase, .editing)
        XCTAssertTrue(spy.recorded.isEmpty)
    }

    func testASentReportIsRecordedAndItsBlockApplies() async {
        let spy = Spy()
        let m = model(picked: [1, 2], held: [1, 2], spy: spy)
        m.reason = 1
        await m.send()
        XCTAssertEqual(m.phase, .sent)
        XCTAssertEqual(spy.recorded, ["AAAAAAAAAAAAAAAAAAAAAA"])
        XCTAssertEqual(spy.followUps, [.block])
        XCTAssertEqual(spy.sealedEvidence.count, 2)
    }

    func testANetworkFailureKeepsTheSheetAndAppliesNothing() async {
        let spy = Spy()
        let m = model(send: .failed, spy: spy)
        m.reason = 2
        m.comment = "keep me"
        await m.send()
        XCTAssertEqual(m.phase, .failed)
        XCTAssertEqual(m.comment, "keep me")
        XCTAssertTrue(spy.followUps.isEmpty)
    }

    func testTheLimitAndAnUnavailableServerStillApplyTheBlockOnce() async {
        let spy = Spy()
        let limited = model(send: .limited, spy: spy)
        limited.reason = 1
        await limited.send()
        XCTAssertEqual(limited.phase, .limited)
        await limited.send()
        XCTAssertEqual(spy.followUps, [.block], "applied once, not again on a retry")

        let spy2 = Spy()
        let unpinned = model(rootKey: nil, spy: spy2)
        unpinned.reason = 1
        await unpinned.send()
        XCTAssertEqual(unpinned.phase, .unavailable)
        XCTAssertEqual(spy2.followUps, [.block])
    }

    func testAnUncheckedFollowUpIsNotApplied() async {
        let spy = Spy()
        let m = model(target: .group(handle: "cats"), followUp: .leave, spy: spy)
        XCTAssertFalse(m.followUpChecked, "Leave the group is off by default")
        m.reason = 3
        await m.send()
        XCTAssertEqual(m.phase, .sent)
        XCTAssertTrue(spy.followUps.isEmpty)
    }

    func testAMessageNoLongerHeldIsLeftOutAndTheLineSaysSo() async {
        let spy = Spy()
        let m = model(picked: [1, 2, 3], held: [1, 3], spy: spy)
        XCTAssertEqual(m.includedLine, .partial(included: 2, total: 3))
        m.reason = 1
        await m.send()
        XCTAssertEqual(spy.sealedEvidence.count, 2)
    }

    func testTheIncludedLineAndTheTwentyMessageLimit() {
        let spy = Spy()
        let m = model(spy: spy)
        XCTAssertEqual(m.includedLine, .none)
        for id in 1...Int64(ReportSheetModel.maxMessages) { XCTAssertTrue(m.toggle(id)) }
        XCTAssertFalse(m.toggle(99), "the twenty-first pick is refused")
        XCTAssertTrue(m.atLimit)
        XCTAssertTrue(m.toggle(1), "unpicking always works")
        XCTAssertFalse(m.atLimit)
    }

    func testTheCommentIsCutAtFiveHundredCharacters() {
        let spy = Spy()
        let m = model(spy: spy)
        m.comment = String(repeating: "a", count: 600)
        XCTAssertEqual(m.comment.count, ReportSheetModel.maxComment)
    }

    func testTheReasonsAreTheCoresEightInOrder() {
        let reasons = ReportSheetModel.reasons
        XCTAssertEqual(reasons.count, 8)
        XCTAssertEqual(reasons.first?.key, "report.reason.spam")
        XCTAssertEqual(reasons.last?.key, "report.reason.other")
        XCTAssertTrue(reasons.contains { $0.key == "report.reason.child_safety" })
        for reason in reasons {
            XCTAssertNotEqual(Localization.text(reason.key), reason.key, "\(reason.key) is in the catalog")
        }
    }

    // MARK: - Block register through the synced state

    func testTheBlockRegisterRoundTripsBetweenTwoStates() throws {
        let key = Data(repeating: 0xBB, count: 32)
        let a = SyncedState()
        let envelope = try XCTUnwrap(try a.setBlocked(hlc: 10, accountKey: key, blocked: true, nowMs: 1000))
        XCTAssertTrue(isBlockedEnvelope(bytes: envelope))
        XCTAssertTrue(DeviceGroupSyncRepository.serviceEnvelopeSlot(envelope).hasPrefix("blocked:"))
        let b = SyncedState()
        XCTAssertTrue(try b.applyBlockedEnvelope(envelope: envelope))
        XCTAssertTrue(try b.isBlocked(accountKey: key))
        BlockList.shared.adopt(try b.blockedAccounts())
        XCTAssertTrue(BlockList.shared.isBlocked(key))
        XCTAssertEqual(BlockList.shared.verdict(.groupMessage, accountKey: key), .collapse)
        BlockList.shared.adopt([])
        XCTAssertFalse(BlockList.shared.isBlocked(key))
    }
}
