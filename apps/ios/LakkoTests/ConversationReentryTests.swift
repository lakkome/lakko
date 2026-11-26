// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0091 phase 2 — the client rules of a device that fell behind a
/// conversation (`Chat/ConversationReentry.swift`). The decisions the core
/// owns (the verdict, the request cadence, the stop list, the route, the
/// admission) are called through the FFI here, so these tests also pin that
/// the client feeds them the right inputs.
final class ConversationReentryTests: XCTestCase {

    private func suite() -> UserDefaults {
        UserDefaults(suiteName: "conversation-reentry-\(UUID().uuidString)")!
    }

    private let account = base64Encode(Data((0..<16).map { _ in UInt8.random(in: 0...255) }))

    private func evidence(
        refused: UInt32 = 0, aboveLocal: Bool = false, publish: Bool = false, firstAt: UInt64? = nil
    ) -> ReentryObservations.Snapshot {
        ReentryObservations.Snapshot(
            refusedReads: refused, failedRowAboveLocal: aboveLocal, publishRefused: publish,
            firstFailedRowAtMs: firstAt)
    }

    private func observation(
        owed: Bool = false, rows: Int = 0, more: Bool = false, held: Bool = false,
        latest: UInt64?, local: UInt64, evidence e: ReentryObservations.Snapshot
    ) -> FfiBehindObservation {
        ConversationReentryRules.observation(
            ownCommitOwed: owed, rowsReturned: rows, hasMore: more, held: held,
            latestEpoch: latest, localEpoch: local, evidence: e)
    }

    // MARK: - The stored state

    func testTheBehindStateRoundTripsThroughTheCore() {
        for state in [ReentryBehind.inStep, .suspected(sinceMs: 5), .confirmed(sinceMs: 9)] {
            XCTAssertEqual(ReentryBehind(state.ffi), state)
            let data = try? JSONEncoder().encode(state)
            XCTAssertEqual(data.flatMap { try? JSONDecoder().decode(ReentryBehind.self, from: $0) }, state)
        }
        XCTAssertFalse(ReentryBehind.inStep.stopsActing)
        XCTAssertTrue(ReentryBehind.suspected(sinceMs: 1).stopsActing)
        XCTAssertTrue(ReentryBehind.confirmed(sinceMs: 1).stopsActing)
        XCTAssertFalse(ReentryBehind.suspected(sinceMs: 1).isConfirmed)
        XCTAssertTrue(ReentryBehind.confirmed(sinceMs: 1).isConfirmed)
    }

    func testTheRecordIsKeptPerAccountAndAnEmptyRecordGoes() {
        let store = ConversationReentryStore(defaults: suite())
        var record = ConversationReentryRecord()
        record.behind = .confirmed(sinceMs: 10)
        record.gapFromMs = 4
        XCTAssertTrue(store.save(record, accountIdB64: account, groupIdHex: "aa"))
        XCTAssertEqual(store.load(accountIdB64: account, groupIdHex: "aa"), record)
        XCTAssertEqual(store.load(accountIdB64: "other", groupIdHex: "aa"), ConversationReentryRecord())
        XCTAssertEqual(store.groups(accountIdB64: account), ["aa"])
        XCTAssertTrue(store.save(ConversationReentryRecord(), accountIdB64: account, groupIdHex: "aa"))
        XCTAssertTrue(store.groups(accountIdB64: account).isEmpty)
    }

    func testASealedAccountTakesNoRecord() {
        let store = ConversationReentryStore(defaults: suite())
        var record = ConversationReentryRecord()
        record.behind = .suspected(sinceMs: 1)
        AccountStoreGate.seal(account)
        defer { AccountStoreGate.open(account) }
        XCTAssertFalse(store.save(record, accountIdB64: account, groupIdHex: "aa"))
    }

    // MARK: - Detection

    func testOnlyAnEmptyUnheldReadWithNothingMoreIsDrained() {
        let e = evidence()
        XCTAssertTrue(observation(latest: 7, local: 6, evidence: e).drained)
        XCTAssertFalse(observation(rows: 1, latest: 7, local: 6, evidence: e).drained)
        XCTAssertFalse(observation(more: true, latest: 7, local: 6, evidence: e).drained)
        XCTAssertFalse(observation(held: true, latest: 7, local: 6, evidence: e).drained)
    }

    func testADrainedReadNamingALaterEpochMakesTheDeviceSuspected() {
        let e = evidence()
        let folded = ConversationReentryRules.fold(
            ConversationReentryRecord(), observation: observation(latest: 9, local: 6, evidence: e),
            evidence: e, nowMs: 1_000)
        XCTAssertEqual(folded.record.behind, .suspected(sinceMs: 1_000))
        XCTAssertEqual(folded.record.latestSeen, 9)
        XCTAssertFalse(folded.cleared)
    }

    func testAnOwnCommitOwedSuspectsNothing() {
        let e = evidence()
        let folded = ConversationReentryRules.fold(
            ConversationReentryRecord(), observation: observation(owed: true, latest: 9, local: 6, evidence: e),
            evidence: e, nowMs: 1_000)
        XCTAssertEqual(folded.record.behind, .inStep)
    }

    func testEvidenceConfirmsAndIsRemembered() {
        let e = evidence(aboveLocal: true, firstAt: 400)
        let folded = ConversationReentryRules.fold(
            ConversationReentryRecord(), observation: observation(latest: 9, local: 6, evidence: e),
            evidence: e, nowMs: 1_000)
        XCTAssertTrue(folded.record.behind.isConfirmed)
        XCTAssertTrue(folded.record.evidence)
        XCTAssertEqual(folded.record.gapFromMs, 400)
        XCTAssertTrue(ConversationReentryRules.blocksRepair(folded.record))
    }

    func testThreeRefusedReadsSuspect() {
        let two = evidence(refused: reentryRefusedReadsToSuspect() - 1)
        let notYet = ConversationReentryRules.fold(
            ConversationReentryRecord(),
            observation: observation(more: true, latest: nil, local: 6, evidence: two),
            evidence: two, nowMs: 1_000)
        XCTAssertEqual(notYet.record.behind, .inStep)
        let three = evidence(refused: reentryRefusedReadsToSuspect())
        let now = ConversationReentryRules.fold(
            ConversationReentryRecord(),
            observation: observation(more: true, latest: nil, local: 6, evidence: three),
            evidence: three, nowMs: 1_000)
        XCTAssertTrue(now.record.behind.stopsActing)
    }

    func testADrainedReadBackAtTheLocalEpochClearsEverything() {
        var behind = ConversationReentryRecord()
        behind.behind = .confirmed(sinceMs: 5)
        behind.evidence = true
        behind.gapFromMs = 3
        behind.latestSeen = 9
        let e = evidence()
        let folded = ConversationReentryRules.fold(
            behind, observation: observation(latest: 9, local: 9, evidence: e), evidence: e, nowMs: 1_000)
        XCTAssertTrue(folded.cleared)
        XCTAssertEqual(folded.record.behind, .inStep)
        XCTAssertFalse(folded.record.evidence)
        XCTAssertNil(folded.record.gapFromMs)
        XCTAssertNil(folded.record.latestSeen)
    }

    func testTheObservationsCollectEvidenceUntilARowApplies() {
        var clock: UInt64 = 50
        let seen = ReentryObservations(nowMs: { clock })
        seen.noteFailedRow(groupIdHex: "g", sealed: 6, local: 6, createdAtMs: 10)
        XCTAssertFalse(seen.snapshot(groupIdHex: "g").failedRowAboveLocal)
        seen.noteFailedRow(groupIdHex: "g", sealed: 7, local: 6, createdAtMs: 20)
        XCTAssertTrue(seen.snapshot(groupIdHex: "g").failedRowAboveLocal)
        XCTAssertEqual(seen.snapshot(groupIdHex: "g").firstFailedRowAtMs, 10)
        seen.noteApplied(groupIdHex: "g")
        XCTAssertFalse(seen.snapshot(groupIdHex: "g").failedRowAboveLocal)
        XCTAssertNil(seen.snapshot(groupIdHex: "g").firstFailedRowAtMs)

        seen.noteReadRefused(groupIdHex: "g")
        seen.noteReadRefused(groupIdHex: "g")
        XCTAssertEqual(seen.snapshot(groupIdHex: "g").refusedReads, 2)
        seen.noteReadAnswered(groupIdHex: "g")
        XCTAssertEqual(seen.snapshot(groupIdHex: "g").refusedReads, 0)

        clock = 70
        seen.noteLeafActivity(groupIdHex: "g", leaf: 3, epoch: 8)
        XCTAssertEqual(seen.lastActivity(groupIdHex: "g", leaf: 3, epoch: 8), 70)
        XCTAssertNil(seen.lastActivity(groupIdHex: "g", leaf: 3, epoch: 9), "another epoch is not the current one")
        seen.clear(groupIdHex: "g")
        XCTAssertEqual(seen.lastActivity(groupIdHex: "g", leaf: 3, epoch: 8), 70, "activity survives a clear")
    }

    func testOnlyAnUnskewed401IsARefusal() {
        let refused = DsClientError.http(status: 401, endpoint: "/x", body: "")
        XCTAssertTrue(ChatRepository.isUnskewedRefusal(refused))
        XCTAssertTrue(ChatRepository.isUnskewedRefusal(ChatError.wrapped("DS fetch failed", underlying: refused)))
        XCTAssertFalse(ChatRepository.isUnskewedRefusal(DsClientError.clockSkew(endpoint: "/x")))
        XCTAssertFalse(ChatRepository.isUnskewedRefusal(DsClientError.http(status: 403, endpoint: "/x", body: "")))
    }

    // MARK: - The stop list

    func testWhatAWaitingChatStillSends() {
        XCTAssertTrue(ConversationReentryRules.sendAllowed(behind: .inStep, isPersonContent: false))
        XCTAssertTrue(ConversationReentryRules.sendAllowed(behind: .suspected(sinceMs: 1), isPersonContent: true))
        XCTAssertFalse(ConversationReentryRules.sendAllowed(behind: .suspected(sinceMs: 1), isPersonContent: false))
        XCTAssertFalse(ConversationReentryRules.sendAllowed(behind: .confirmed(sinceMs: 1), isPersonContent: true))
    }

    func testRepairIsHeldOnlyWithEvidence() {
        var record = ConversationReentryRecord()
        record.behind = .suspected(sinceMs: 1)
        XCTAssertFalse(ConversationReentryRules.blocksRepair(record), "a fake epoch must stay repairable")
        record.evidence = true
        XCTAssertTrue(ConversationReentryRules.blocksRepair(record))
        record.behind = .inStep
        XCTAssertFalse(ConversationReentryRules.blocksRepair(record))
    }

    // MARK: - The request

    func testTheNewestRequestsAreKeptAndTheOlderForgotten() {
        func request(_ byte: UInt8) -> ReentrySentRequest {
            ReentrySentRequest(
                keyPackage: Data([byte]), epochLeft: 1, sentAtMs: UInt64(byte), route: "knock",
                cursorAtSend: Int64(byte))
        }
        let one = ConversationReentryRules.keeping([], adding: request(1), kept: 2)
        XCTAssertEqual(one.requests.count, 1)
        XCTAssertTrue(one.dropped.isEmpty)
        let three = ConversationReentryRules.keeping([request(1), request(2)], adding: request(3), kept: 2)
        XCTAssertEqual(three.requests.map(\.keyPackage), [Data([2]), Data([3])])
        XCTAssertEqual(three.dropped, [Data([1])])
        var record = ConversationReentryRecord()
        record.requests = three.requests
        XCTAssertEqual(record.readFromAfterReentry, 2)
        XCTAssertEqual(record.recordedKeyPackages, [Data([2]), Data([3])])
        XCTAssertEqual(Int(reentryRequestsKept()), 2)
    }

    func testTheRouteNamesAreStable() {
        XCTAssertEqual(ConversationReentryRules.routeName(.ownDevice), "own-device")
        XCTAssertEqual(ConversationReentryRules.routeName(.mailbox), "mailbox")
        XCTAssertEqual(ConversationReentryRules.routeName(.knock), "knock")
        XCTAssertEqual(reentryRoute(ownDeviceInConversation: true, kind: .group, ownDeviceUnanswered: false), .ownDevice)
        XCTAssertEqual(reentryRoute(ownDeviceInConversation: false, kind: .oneToOne, ownDeviceUnanswered: false), .mailbox)
        XCTAssertEqual(reentryRoute(ownDeviceInConversation: false, kind: .group, ownDeviceUnanswered: false), .knock)
    }

    /// Live walk 5 (2026-11-20): the own device is asked once; 2 minutes
    /// later the request goes by the conversation's own route, and keeps
    /// going that way until the episode ends.
    private func walkFiveEpisode(kind: GroupKind) -> [FfiReentryRoute] {
        var record = ConversationReentryRecord()
        record.behind = .confirmed(sinceMs: 0)
        var routes: [FfiReentryRoute] = []
        let wait = reentryOwnDeviceAnswerWaitMs()
        XCTAssertEqual(wait, 120_000)
        for now in [UInt64(0), wait - 1, wait, wait + 60_000, wait + reentryRequestIntervalMs()] {
            guard reentryRequestDue(
                state: record.behind.ffi, lastSentMs: record.lastSentMs,
                lastRouteWasOwnDevice: record.lastRouteWasOwnDevice, nowMs: now) else { continue }
            let route = reentryRoute(
                ownDeviceInConversation: true, kind: kind, ownDeviceUnanswered: record.ownDeviceUnanswered)
            routes.append(route)
            record.lastSentMs = now
            record.lastRoute = ConversationReentryRules.routeName(route)
            // As `sendReentryRequestIfDue` does after a send went out.
            if route == .ownDevice { record.ownDeviceAsked = true }
        }
        return routes
    }

    func testAnUnansweredOwnDeviceHandsTheRequestToTheKnockAfterTwoMinutes() {
        XCTAssertEqual(walkFiveEpisode(kind: .group), [.ownDevice, .knock, .knock])
    }

    func testAnUnansweredOwnDeviceHandsTheRequestToTheMailboxInAOneToOne() {
        XCTAssertEqual(walkFiveEpisode(kind: .oneToOne), [.ownDevice, .mailbox, .mailbox])
    }

    func testWithNoDeviceGroupSenderTheFirstRequestGoesByTheChatsOwnRoute() {
        let reachable = ConversationReentryRules.ownDeviceReachable(inConversation: true, senderRegistered: false)
        XCTAssertFalse(reachable)
        XCTAssertEqual(reentryRoute(ownDeviceInConversation: reachable, kind: .group, ownDeviceUnanswered: false), .knock)
        XCTAssertEqual(reentryRoute(ownDeviceInConversation: reachable, kind: .oneToOne, ownDeviceUnanswered: false), .mailbox)
        XCTAssertTrue(ConversationReentryRules.ownDeviceReachable(inConversation: true, senderRegistered: true))
        XCTAssertFalse(ConversationReentryRules.ownDeviceReachable(inConversation: false, senderRegistered: true))
    }

    func testAFailedOwnDeviceSendIsNotCountedAsAsked() {
        var record = ConversationReentryRecord()
        record.lastRoute = "own-device"
        record.lastSentMs = 10
        XCTAssertFalse(record.lastRouteWasOwnDevice)
        XCTAssertFalse(record.ownDeviceUnanswered)
    }

    func testBackInStepEndsTheEpisodeAndTheOwnDeviceIsAskedFirstAgain() throws {
        var behind = ConversationReentryRecord()
        behind.behind = .confirmed(sinceMs: 5)
        behind.ownDeviceAsked = true
        behind.lastRoute = "knock"
        let e = evidence()
        let folded = ConversationReentryRules.fold(
            behind, observation: observation(latest: 9, local: 9, evidence: e), evidence: e, nowMs: 1_000)
        XCTAssertTrue(folded.cleared)
        XCTAssertFalse(folded.record.ownDeviceUnanswered)
        XCTAssertEqual(
            reentryRoute(ownDeviceInConversation: true, kind: .group,
                         ownDeviceUnanswered: folded.record.ownDeviceUnanswered),
            .ownDevice)
        // A record written before the field decodes and reads as not asked.
        var old = ConversationReentryRecord()
        old.lastRoute = "own-device"
        var json = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(old)) as? [String: Any])
        json.removeValue(forKey: "ownDeviceAsked")
        let decoded = try JSONDecoder().decode(
            ConversationReentryRecord.self, from: JSONSerialization.data(withJSONObject: json))
        XCTAssertFalse(decoded.ownDeviceUnanswered)
    }

    @MainActor
    func testARequestEnvelopeTravelsAsItsOwnServiceEnvelopeSlot() throws {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32))
        let group = try Group.create(identity: identity, kind: .group)
        let groupId = try group.groupId()
        let gapKey = generateGapKey()
        let first = try composeConversationReentryEnvelope(
            groupId: groupId, epochLeft: try group.epoch(), gapFromMs: 5, gapKey: gapKey.publicKey,
            keyPackage: try identity.createReentryKeyPackage(groupId: groupId, epochLeft: try group.epoch()))
        let second = try composeConversationReentryEnvelope(
            groupId: groupId, epochLeft: try group.epoch(), gapFromMs: 6, gapKey: generateGapKey().publicKey,
            keyPackage: try identity.createReentryKeyPackage(groupId: groupId, epochLeft: try group.epoch()))
        XCTAssertTrue(DeviceGroupSyncRepository.isConversationReentry(first))
        XCTAssertFalse(DeviceGroupSyncRepository.isKeyRotation(first))
        XCTAssertNotEqual(
            DeviceGroupSyncRepository.serviceEnvelopeSlot(first),
            DeviceGroupSyncRepository.serviceEnvelopeSlot(second),
            "two re-entry requests never replace one another unsent")
        let decoded = try decodeConversationReentryEnvelope(plaintext: first)
        XCTAssertEqual(decoded.groupId, groupId)
        XCTAssertEqual(decoded.gapFromMs, 5)
        XCTAssertEqual(decoded.gapKey, gapKey.publicKey, "the request carries the public gap key")
        XCTAssertNil(first.range(of: gapKey.secret), "never the secret")
    }

    // MARK: - The waiting note (UX 2.2)

    private func text(_ key: String, _ args: [String: String]) -> String {
        args.isEmpty ? key : key + "|" + args.sorted { $0.key < $1.key }.map { "\($0.key)=\($0.value)" }.joined(separator: ",")
    }

    /// Maintainer 2026-11-20 — no waiting note: the catalog no longer
    /// holds one, and the gap line is a plain fact.
    func testNoWaitingNoteAndThePlainGapLine() {
        for key in ["chat.reentry.waiting", "chat.reentry.second.own_device", "chat.reentry.second.group",
                    "chat.reentry.second.person"] {
            XCTAssertEqual(Localization.text(key), key, "\(key) must be gone from the catalog")
        }
        XCTAssertNotEqual(Localization.text("chat.reentry.gap"), "chat.reentry.gap")
        XCTAssertEqual(ConversationReentryRules.gapSentence(text: text), "chat.reentry.gap")
    }

    // MARK: - The gap line (UX 2.4)

    func testTheGapMarkerRoundTripsAndStaysOutOfBundles() {
        let marker = ConversationReentryRules.gapMarker(fromMs: 10, toMs: 20)
        XCTAssertEqual(ConversationReentryRules.parseGap(marker)?.fromMs, 10)
        XCTAssertEqual(ConversationReentryRules.parseGap(marker)?.toMs, 20)
        XCTAssertNil(ConversationReentryRules.parseGap("[mls reentry-gap from=20 to=10]"))
        XCTAssertNil(ConversationReentryRules.parseGap("[mls commit]"))
        XCTAssertNil(ConversationReentryRules.parseGap("[mls reentry-gap from=x to=1]"))
        let row = PersistedMessage(messageId: -1, direction: "system", text: marker, attachments: [], createdAt: "")
        XCTAssertTrue(HistoryBundleCodec.isBookkeepingSystemRow(row), "the line is never sent in a bundle")
    }

    func testTheGapLineIsDrawnButNeverTheChatListPreview() {
        let line = ChatMessage(
            messageId: -2, direction: .system,
            text: ConversationReentryRules.gapMarker(fromMs: 1, toMs: 2), createdAt: "")
        XCTAssertFalse(ChatTimeline.isHiddenSystemRow(line))
        XCTAssertFalse(ChatTimeline.isHiddenSystemRow(line, isGroup: true))
        XCTAssertNotNil(ChatTimeline.reentryGapRow(line))
        XCTAssertFalse(ChatTimeline.isListPreviewRow(line))
        let plain = ChatMessage(messageId: 4, direction: .incoming, text: "hi", createdAt: "")
        XCTAssertTrue(ChatTimeline.isListPreviewRow(plain))
    }

    // MARK: - The gap history (section 7)

    /// Security review 2026-11-21 — the rendezvous follows the request's
    /// public gap key (the core's derivation), not any group secret.
    func testTheHistoryRendezvousFollowsTheGapKey() throws {
        let a = try XCTUnwrap(ConversationReentryRules.gapRendezvous(gapKey: Data(repeating: 1, count: 32)))
        let b = try XCTUnwrap(ConversationReentryRules.gapRendezvous(gapKey: Data(repeating: 2, count: 32)))
        XCTAssertEqual(a.count, 32)
        XCTAssertNotEqual(a, b)
        XCTAssertEqual(a, try gapHistoryRendezvous(gapKey: Data(repeating: 1, count: 32)))
        XCTAssertNil(ConversationReentryRules.gapRendezvous(gapKey: Data(repeating: 1, count: 31)))
        let golden = try XCTUnwrap(ConversationReentryRules.gapRendezvous(gapKey: Data(repeating: 0x42, count: 32)))
        let hex = golden.map { String(format: "%02x", $0) }.joined()
        XCTAssertTrue(hex.hasPrefix("4060ae14") && hex.hasSuffix("1019"), "the core's golden")
    }

    /// Only the holder of the gap secret opens the history, and it names the
    /// leaf that signed it.
    func testAGapHistoryOpensOnlyUnderItsGapKeyAndNamesItsSigner() throws {
        let readmitter = try Identity.fromSeed(seed: Data(repeating: 0x61, count: 32))
        let gapKey = generateGapKey()
        let groupId = Data(repeating: 0xAB, count: 32)
        let hex = String(repeating: "ab", count: 32)
        let rows = [PersistedMessage(messageId: 7, direction: "incoming", text: "one", attachments: [],
                                     createdAt: "2026-11-11T13:10:00Z")]
        let sealed = try XCTUnwrap(HistoryBundleCodec.buildGapForGroup(
            hex: hex, rows: rows, accountIdB64: account, readmitter: readmitter, gapKey: gapKey.publicKey,
            groupId: groupId))
        let opened = try openGapHistoryStructured(gapSecret: gapKey.secret, groupId: groupId, sealed: sealed)
        XCTAssertEqual(opened.signerLeafKey, readmitter.signaturePublicKey())
        XCTAssertEqual(opened.bundle.groups.first?.messages.count, 1)
        XCTAssertThrowsError(
            try openGapHistoryStructured(gapSecret: generateGapKey().secret, groupId: groupId, sealed: sealed),
            "another gap key opens nothing")
        XCTAssertThrowsError(
            try openGapHistoryStructured(
                gapSecret: gapKey.secret, groupId: Data(repeating: 0xAC, count: 32), sealed: sealed),
            "nor for another conversation")
        XCTAssertNil(sealed.range(of: readmitter.signaturePublicKey()), "the signer is not in the clear")
    }

    /// The returning device keeps a history only from a signer the group it
    /// joined accepts: the owner here, never a plain member.
    func testAGapHistoryFromAPlainMemberIsNotKept() throws {
        let owner = try Identity.fromSeed(seed: Data(repeating: 0x71, count: 32))
        let back = try Identity.fromSeed(seed: Data(repeating: 0x72, count: 32))
        let member = try Identity.fromSeed(seed: Data(repeating: 0x73, count: 32))
        let ownerGroup = try Group.create(identity: owner, kind: .group)
        let addBack = try ownerGroup.addMemberFromWire(identity: owner, keyPackage: try back.createKeyPackage())
        let addMember = try ownerGroup.addMemberFromWire(identity: owner, keyPackage: try member.createKeyPackage())
        let backGroup = try Group.joinFromWelcome(welcome: addBack.welcome, identity: back)
        _ = try backGroup.processIncomingVerifyingRoles(identity: back, bytes: addMember.commit)
        XCTAssertTrue(try backGroup.gapHistorySignerAccepted(
            identity: back, signerLeafKey: owner.signaturePublicKey()))
        XCTAssertFalse(try backGroup.gapHistorySignerAccepted(
            identity: back, signerLeafKey: member.signaturePublicKey()))
    }

    /// The re-admitter never sends rows from before its own evidence that
    /// the leaf was in the conversation, and nothing without any.
    func testTheGapHistoryStartIsClampedByWhatTheReadmitterSaw() {
        XCTAssertNil(gapHistoryStart(claimedMs: 1_000, leafFirstSeenMs: nil, leafLastWriteMs: nil),
                     "nothing seen here: nothing sent")
        XCTAssertEqual(gapHistoryStart(claimedMs: 1_000, leafFirstSeenMs: 5_000, leafLastWriteMs: nil), 5_000)
        XCTAssertEqual(gapHistoryStart(claimedMs: 9_000, leafFirstSeenMs: 5_000, leafLastWriteMs: nil), 9_000)
        XCTAssertEqual(gapHistoryStart(claimedMs: 1_000, leafFirstSeenMs: nil, leafLastWriteMs: 4_000), 4_000)
    }

    func testTheFirstTimeALeafWasSeenIsKeptAndNeverMoved() {
        let store = ReentryLeafFirstSeenStore(defaults: suite())
        let a = Data(repeating: 1, count: 32)
        let b = Data(repeating: 2, count: 32)
        store.noteRoster(accountIdB64: account, groupIdHex: "g", leafKeys: [a], nowMs: 100)
        store.noteRoster(accountIdB64: account, groupIdHex: "g", leafKeys: [a, b], nowMs: 200)
        XCTAssertEqual(store.firstSeen(accountIdB64: account, groupIdHex: "g", leafKey: a), 100, "only the first")
        XCTAssertEqual(store.firstSeen(accountIdB64: account, groupIdHex: "g", leafKey: b), 200)
        XCTAssertNil(store.firstSeen(accountIdB64: account, groupIdHex: "h", leafKey: a))
        store.remove(accountIdB64: account, groupIdHex: "g")
        XCTAssertNil(store.firstSeen(accountIdB64: account, groupIdHex: "g", leafKey: a))
    }

    func testTheLastWriteOfALeafIsReadAtAnyEpoch() {
        var clock: UInt64 = 10
        let seen = ReentryObservations(nowMs: { clock })
        XCTAssertNil(seen.lastWrite(groupIdHex: "g", leaf: 3))
        seen.noteLeafActivity(groupIdHex: "g", leaf: 3, epoch: 4)
        clock = 20
        seen.noteLeafActivity(groupIdHex: "g", leaf: 3, epoch: 5)
        XCTAssertEqual(seen.lastWrite(groupIdHex: "g", leaf: 3), 20)
    }

    /// No epoch is needed to seal: the history goes once the group moved
    /// past the epoch the replacement was proposed at, with the leaf in it.
    func testAnOwedHistoryGoesOnceTheReplacementLanded() {
        let row = owedRow(owedAtMs: 100)
        XCTAssertEqual(ConversationReentryRules.owedHistoryVerdict(row, epoch: 8, rosterHasLeaf: true, nowMs: 200),
                       .wait, "still at the proposal's epoch")
        XCTAssertEqual(ConversationReentryRules.owedHistoryVerdict(row, epoch: 9, rosterHasLeaf: true, nowMs: 200),
                       .send)
        XCTAssertEqual(ConversationReentryRules.owedHistoryVerdict(row, epoch: 12, rosterHasLeaf: true, nowMs: 200),
                       .send, "any later epoch")
        XCTAssertEqual(ConversationReentryRules.owedHistoryVerdict(row, epoch: 9, rosterHasLeaf: false, nowMs: 200),
                       .drop, "the leaf left")
        XCTAssertEqual(ConversationReentryRules.owedHistoryVerdict(
            row, epoch: 9, rosterHasLeaf: true, nowMs: 100 + ReentryHistoryOwed.lifetimeMs), .drop)
    }

    /// The gap keys a returning device looks under: those of its requests
    /// whose sealed secret opens.
    func testTheGapKeysComeFromTheRequestsWhoseSecretOpens() {
        let wrap = Data(repeating: 0x5A, count: 32)
        let store = ReentryGapLookStore(defaults: suite(), wrapKey: { _, _ in wrap })
        let one = generateGapKey()
        let two = generateGapKey()
        let requests = [
            ReentrySentRequest(keyPackage: Data([1]), epochLeft: 1, sentAtMs: 1, route: "knock", cursorAtSend: 0),
            ReentrySentRequest(
                keyPackage: Data([2]), epochLeft: 1, sentAtMs: 2, route: "knock", cursorAtSend: 0,
                gapPublicKey: one.publicKey, sealedGapSecret: store.sealSecret(one.secret, accountIdB64: account)),
            ReentrySentRequest(
                keyPackage: Data([3]), epochLeft: 1, sentAtMs: 3, route: "knock", cursorAtSend: 0,
                gapPublicKey: two.publicKey, sealedGapSecret: store.sealSecret(two.secret, accountIdB64: account)),
        ]
        XCTAssertNotEqual(requests[1].sealedGapSecret, one.secret, "sealed at rest")
        let pairs = ConversationReentryRules.gapKeyPairs(requests) { store.openSecret($0, accountIdB64: account) }
        XCTAssertEqual(pairs, [ReentryGapKeyPair(publicKey: one.publicKey, secret: one.secret),
                               ReentryGapKeyPair(publicKey: two.publicKey, secret: two.secret)])
        let otherWrap = ReentryGapLookStore(defaults: suite(), wrapKey: { _, _ in Data(repeating: 1, count: 32) })
        XCTAssertTrue(ConversationReentryRules.gapKeyPairs(requests) {
            otherWrap.openSecret($0, accountIdB64: self.account)
        }.isEmpty, "a lost wrap key opens none")
    }

    /// The old carrier (a group exporter every member holds) is gone.
    func testTheGroupExporterCarrierIsGone() throws {
        let source = try String(
            contentsOf: URL(fileURLWithPath: #filePath).deletingLastPathComponent()
                .appendingPathComponent("../Lakko/Chat/ConversationReentry.swift").standardized, encoding: .utf8)
        XCTAssertFalse(source.contains("lakko/conversation-reentry/history/v1"))
        XCTAssertFalse(source.contains("lakko/conversation-reentry/history-rendezvous/v1"))
    }

    private func owedRow(owedAtMs: UInt64) -> ReentryOwedHistory {
        ReentryOwedHistory(
            groupIdHex: "g", keyPackage: Data([1]), leafKey: Data(repeating: 2, count: 32),
            gapKey: Data(repeating: 3, count: 32), proposedAtEpoch: 8, gapFromMs: 1, owedAtMs: owedAtMs)
    }

    func testTheGapRowsAreTheConversationsOwnSinceTheGap() {
        let rows = [
            PersistedMessage(messageId: 1, direction: "incoming", text: "old", attachments: [],
                             createdAt: "2026-11-11T12:00:00Z"),
            PersistedMessage(messageId: 2, direction: "incoming", text: "in the gap", attachments: [],
                             createdAt: "2026-11-11T14:00:00Z"),
            PersistedMessage(messageId: 3, direction: "system", text: "[mls commit]", attachments: [],
                             createdAt: "2026-11-11T14:01:00Z"),
            PersistedMessage(messageId: -4, direction: "system", text: "call", attachments: [],
                             createdAt: "2026-11-11T14:02:00Z"),
        ]
        let from = UInt64(ChatRepository.parseDsTimestampMs("2026-11-11T13:00:00Z")!)
        XCTAssertEqual(ConversationReentryRules.gapRows(rows, gapFromMs: from).map(\.messageId), [2])
    }

    func testAnOwedHistoryIsKeptOncePerKeyPackageAndExpires() {
        let owed = ReentryHistoryOwed(defaults: suite())
        let row = owedRow(owedAtMs: 100)
        owed.add(row, accountIdB64: account)
        owed.add(row, accountIdB64: account)
        XCTAssertEqual(owed.load(accountIdB64: account).count, 1)
        XCTAssertTrue(ConversationReentryRules.historyStillOwed(row, nowMs: 100 + ReentryHistoryOwed.lifetimeMs - 1))
        XCTAssertFalse(ConversationReentryRules.historyStillOwed(row, nowMs: 100 + ReentryHistoryOwed.lifetimeMs))
    }

    // MARK: - The re-admitter's memory and the knock key

    func testTheAdmissionMemoryIsPerLeafAndEpoch() {
        let memory = ReentryAdmissionMemory(defaults: suite())
        let leaf = Data(repeating: 4, count: 32)
        XCTAssertFalse(memory.handledAt(accountIdB64: account, groupIdHex: "g", leafKey: leaf, epoch: 3))
        memory.markHandled(accountIdB64: account, groupIdHex: "g", leafKey: leaf, epoch: 3)
        XCTAssertTrue(memory.handledAt(accountIdB64: account, groupIdHex: "g", leafKey: leaf, epoch: 3))
        XCTAssertFalse(memory.handledAt(accountIdB64: account, groupIdHex: "g", leafKey: leaf, epoch: 4))
        XCTAssertFalse(memory.handledAt(accountIdB64: account, groupIdHex: "h", leafKey: leaf, epoch: 3))
        memory.markAccepted(accountIdB64: account, groupIdHex: "g", leafKey: leaf, atMs: 77)
        XCTAssertEqual(memory.lastAcceptedMs(accountIdB64: account, groupIdHex: "g", leafKey: leaf), 77)
        memory.removeGroup(accountIdB64: account, groupIdHex: "g")
        XCTAssertNil(memory.lastAcceptedMs(accountIdB64: account, groupIdHex: "g", leafKey: leaf))
    }

    func testTheAdmissionVerdictTakesTheFactsThisClientSupplies() {
        let base = FfiReentryAdmissionFacts(
            deviceStillTheAccounts: true, lastCurrentEpochMessageFromLeafMs: nil,
            handledForLeafAtThisEpoch: false, lastAcceptedForLeafMs: nil)
        XCTAssertEqual(reentryAdmissionVerdict(facts: base, nowMs: 10_000_000), .admit)
        var gone = base
        gone.deviceStillTheAccounts = false
        XCTAssertEqual(reentryAdmissionVerdict(facts: gone, nowMs: 10_000_000), .notTheAccountsDevice)
        var handled = base
        handled.handledForLeafAtThisEpoch = true
        XCTAssertEqual(reentryAdmissionVerdict(facts: handled, nowMs: 10_000_000), .alreadyHandled)
    }

    func testAKnockKeyIsKeptOnlyWhenNewer() {
        let store = ReentryKnockKeyStore(defaults: suite())
        let one = ReentryKnockKeyStore.Held(version: 1, publicKey: Data(repeating: 1, count: 32))
        let zero = ReentryKnockKeyStore.Held(version: 0, publicKey: Data(repeating: 9, count: 32))
        XCTAssertTrue(store.keepIfNewer(one, accountIdB64: account, groupIdHex: "g"))
        XCTAssertFalse(store.keepIfNewer(zero, accountIdB64: account, groupIdHex: "g"))
        XCTAssertFalse(store.keepIfNewer(one, accountIdB64: account, groupIdHex: "g"))
        XCTAssertEqual(store.load(accountIdB64: account, groupIdHex: "g"), one)
        store.remove(accountIdB64: account, groupIdHex: "g")
        XCTAssertNil(store.load(accountIdB64: account, groupIdHex: "g"))
    }

    func testAKnockKeyEnvelopeRoutesAsAGroupSetting() throws {
        let envelope = try composeKnockKeyEnvelope(knockVersion: 3, knockPk: Data(repeating: 5, count: 32))
        guard case .knockKey(let key)? = ChatClient.decodeGroupSettingIfEnvelope(envelope) else {
            return XCTFail("a knock-key envelope is a group setting")
        }
        XCTAssertEqual(key.knockVersion, 3)
        XCTAssertEqual(key.knockPk, Data(repeating: 5, count: 32))
    }

    // MARK: - The 2026-11-19 walk fix

    private let day: UInt64 = 24 * 60 * 60 * 1000
    private let onePair = ReentryGapKeyPair(publicKey: Data(repeating: 2, count: 32), secret: Data(repeating: 1, count: 32))

    /// A wrap-key provider standing in for the Keychain: one fixed key.
    private func lookStore(_ defaults: UserDefaults, key: Data = Data(repeating: 0x5A, count: 32))
        -> ReentryGapLookStore {
        ReentryGapLookStore(defaults: defaults, wrapKey: { _, _ in key })
    }

    func testAGapLookRoundTripsAndItsSecretIsNeverStoredInTheClear() throws {
        let defaults = suite()
        let store = lookStore(defaults)
        let secret = Data((0..<32).map { UInt8($0) &* 7 &+ 3 })
        let look = ReentryGapLook(
            groupIdHex: "aa", keys: [ReentryGapKeyPair(publicKey: Data(repeating: 4, count: 32), secret: secret)],
            gapFromMs: 1_000, gapToMs: 5_000)
        XCTAssertTrue(store.save(look, accountIdB64: account))
        XCTAssertEqual(store.load(accountIdB64: account, groupIdHex: "aa"), look)
        XCTAssertEqual(store.groupIds(accountIdB64: account), ["aa"])

        let raw = try XCTUnwrap(defaults.data(forKey: ReentryGapLookStore.key(account)))
        XCTAssertNil(raw.range(of: secret), "the stored bytes do not hold the secret")
        XCTAssertFalse(String(decoding: raw, as: UTF8.self).contains(secret.base64EncodedString()),
                       "nor its base64 form")

        // Another wrap key opens nothing: the look is unreadable, not wrong.
        let other = lookStore(defaults, key: Data(repeating: 0x11, count: 32))
        XCTAssertNil(other.load(accountIdB64: account, groupIdHex: "aa"))

        store.markLooked(accountIdB64: account, groupIdHex: "aa", atMs: 9_000)
        XCTAssertEqual(store.load(accountIdB64: account, groupIdHex: "aa")?.lastLookMs, 9_000)
        store.remove(accountIdB64: account, groupIdHex: "aa")
        XCTAssertNil(store.load(accountIdB64: account, groupIdHex: "aa"))
        XCTAssertNil(defaults.data(forKey: ReentryGapLookStore.key(account)))
    }

    func testAGapLookWithoutAWrapKeyIsNotWritten() {
        let defaults = suite()
        let store = ReentryGapLookStore(defaults: defaults, wrapKey: { _, _ in nil })
        let look = ReentryGapLook(groupIdHex: "aa", keys: [onePair], gapFromMs: 1, gapToMs: 2)
        XCTAssertFalse(store.save(look, accountIdB64: account))
        XCTAssertNil(defaults.data(forKey: ReentryGapLookStore.key(account)))
    }

    func testAGapLookRunsAtMostOncePerMinute() {
        XCTAssertEqual(ConversationReentryRules.gapHistoryLookIntervalMs, 60_000)
        var look = ReentryGapLook(groupIdHex: "aa", keys: [onePair], gapFromMs: 1, gapToMs: 100)
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(look, nowMs: 100), .look, "the first look is at once")
        look.lastLookMs = 1_000
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(look, nowMs: 1_000), .wait)
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(look, nowMs: 60_999), .wait)
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(look, nowMs: 61_000), .look)
    }

    func testAGapLookIsDroppedThirtyDaysAfterTheGapsEnd() {
        XCTAssertEqual(ConversationReentryRules.gapHistoryLookRetentionMs, 30 * day)
        let look = ReentryGapLook(
            groupIdHex: "aa", keys: [onePair], gapFromMs: 1, gapToMs: 10_000, lastLookMs: nil)
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(look, nowMs: 10_000 + 30 * day - 1), .look)
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(look, nowMs: 10_000 + 30 * day), .drop)
        var recent = look
        recent.lastLookMs = 10_000 + 30 * day - 10
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(recent, nowMs: 10_000 + 30 * day), .drop,
                       "retention wins over the throttle")
    }

    func testAnOwedHistoryWaitsThirtyDays() {
        XCTAssertEqual(ReentryHistoryOwed.lifetimeMs, 30 * day)
        let row = owedRow(owedAtMs: 100)
        XCTAssertTrue(ConversationReentryRules.historyStillOwed(row, nowMs: 100 + 29 * day))
        XCTAssertFalse(ConversationReentryRules.historyStillOwed(row, nowMs: 100 + 30 * day))
    }

    func testOnlyAnOwnerOrAnAdminResendsThePushRoutingSecret() {
        XCTAssertTrue(ConversationReentryRules.resendsPushRoutingSecret(ownRole: .owner))
        XCTAssertTrue(ConversationReentryRules.resendsPushRoutingSecret(ownRole: .admin))
        XCTAssertFalse(ConversationReentryRules.resendsPushRoutingSecret(ownRole: .member))
        XCTAssertFalse(ConversationReentryRules.resendsPushRoutingSecret(ownRole: nil),
                       "no role table (a one-to-one) sends none")
    }

    @MainActor
    func testABehindRefusalIsRecognisedWrappedOrNot() {
        struct Other: Error {}
        let behind = ChatRepository.BehindInConversation()
        XCTAssertTrue(ConversationReentryRules.isBehindRefusal(behind))
        XCTAssertTrue(ConversationReentryRules.isBehindRefusal(ChatError.wrapped("send", underlying: behind)))
        XCTAssertTrue(ConversationReentryRules.isBehindRefusal(
            ChatError.wrapped("outer", underlying: ChatError.wrapped("inner", underlying: behind))))
        XCTAssertFalse(ConversationReentryRules.isBehindRefusal(Other()))
        XCTAssertFalse(ConversationReentryRules.isBehindRefusal(ChatError.argument("x")))
        XCTAssertFalse(ConversationReentryRules.isBehindRefusal(ChatError.wrapped("send", underlying: Other())))
    }

    func testAGapHistoryNarrowsOnlyItsOwnGapLine() {
        let line = ConversationReentryRules.gapMarker(fromMs: 1_000, toMs: 9_000)
        XCTAssertEqual(ConversationReentryRules.narrowGap(text: line, gapToMs: 9_000, complete: true, earliestMs: nil),
                       .remove)
        XCTAssertEqual(ConversationReentryRules.narrowGap(text: line, gapToMs: 9_000, complete: false, earliestMs: 4_000),
                       .replace(ConversationReentryRules.gapMarker(fromMs: 1_000, toMs: 4_000)))
        XCTAssertEqual(ConversationReentryRules.narrowGap(text: line, gapToMs: 9_000, complete: false, earliestMs: 1_000),
                       .remove, "the bundle reaches the gap's start")
        XCTAssertEqual(ConversationReentryRules.narrowGap(text: line, gapToMs: 9_000, complete: false, earliestMs: 9_500),
                       .keep, "nothing earlier than the line's end")
        XCTAssertEqual(ConversationReentryRules.narrowGap(text: line, gapToMs: 9_000, complete: false, earliestMs: nil),
                       .keep)
        XCTAssertEqual(ConversationReentryRules.narrowGap(text: line, gapToMs: 8_000, complete: true, earliestMs: nil),
                       .keep, "another re-entry's line")
        XCTAssertEqual(ConversationReentryRules.narrowGap(text: "hello", gapToMs: 9_000, complete: true, earliestMs: nil),
                       .keep)
    }

    func testAGroupRecordNarrowsAndRemovesItsGapLine() throws {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x52, count: 32))
        let record = GroupRecord(group: try Group.create(identity: identity, kind: .group),
                                 groupId: Data(repeating: 0x78, count: 32))
        record.appendLocalRow(
            text: ConversationReentryRules.gapMarker(fromMs: 1_000, toMs: 9_000), messageId: -1, createdAt: "")
        XCTAssertFalse(record.narrowReentryGap(gapToMs: 9_000, complete: false, earliestMs: nil))
        XCTAssertTrue(record.narrowReentryGap(gapToMs: 9_000, complete: false, earliestMs: 5_000))
        XCTAssertEqual(record.messages.map(\.text), [ConversationReentryRules.gapMarker(fromMs: 1_000, toMs: 5_000)])
        XCTAssertEqual(record.messages.first?.messageId, -1, "the row keeps its place")
        XCTAssertTrue(record.narrowReentryGap(gapToMs: 5_000, complete: true, earliestMs: nil))
        XCTAssertTrue(record.messages.isEmpty)
    }

    func testAGapBundleSaysWhetherItCarriesEverythingAndItsEarliestRow() throws {
        let hex = String(repeating: "ab", count: 32)
        let rows = [
            PersistedMessage(messageId: 7, direction: "incoming", text: "one", attachments: [],
                             createdAt: "2026-11-11T13:10:00Z"),
            PersistedMessage(messageId: 8, direction: "incoming", text: "two", attachments: [],
                             createdAt: "2026-11-11T13:20:00Z"),
        ]
        let earliest = ChatRepository.parseDsTimestampMs("2026-11-11T13:10:00Z")
        let gapFrom = UInt64(ChatRepository.parseDsTimestampMs("2026-11-11T13:00:00Z")!)
        XCTAssertFalse(ConversationReentryRules.gapHeldBefore(rows, gapFromMs: gapFrom))
        let older = [PersistedMessage(messageId: 6, direction: "incoming", text: "zero", attachments: [],
                                      createdAt: "2026-11-11T12:00:00Z")] + rows
        XCTAssertTrue(ConversationReentryRules.gapHeldBefore(older, gapFromMs: gapFrom))

        let readmitter = try Identity.fromSeed(seed: Data(repeating: 0x62, count: 32))
        let gapKey = generateGapKey()
        let groupId = Data(repeating: 0xAB, count: 32)
        for heldBefore in [true, false] {
            let sealed = try XCTUnwrap(HistoryBundleCodec.buildGapForGroup(
                hex: hex, rows: rows, accountIdB64: account, readmitter: readmitter, gapKey: gapKey.publicKey,
                groupId: groupId, heldBeforeGap: heldBefore))
            let opened = try openGapHistoryStructured(
                gapSecret: gapKey.secret, groupId: groupId, sealed: sealed).bundle
            XCTAssertEqual(opened.groups.first?.countCapped, !heldBefore)
            let summary = HistoryBundleCodec.gapSummary(opened)
            XCTAssertEqual(summary.complete, heldBefore)
            XCTAssertEqual(summary.earliestMs, earliest)
        }
    }

    func testAChatWhoseOnlyRowIsTheGapLineDrawsIt() {
        let gap = ChatMessage(
            messageId: -1, direction: .system,
            text: ConversationReentryRules.gapMarker(fromMs: 1_000, toMs: 9_000),
            createdAt: "2026-11-19T10:00:00Z")
        for isGroup in [true, false] {
            let shown = [gap].filter { !ChatTimeline.isHiddenSystemRow($0, isGroup: isGroup) }
            let entries = ChatTimeline.layout(
                messages: shown, nowMs: 0, calendar: Calendar(identifier: .gregorian),
                locale: Locale(identifier: "en_GB"), parseMs: ChatRepository.parseDsTimestampMs)
            XCTAssertEqual(entries.count, 1)
            XCTAssertEqual(entries.first?.message.messageId, -1)
        }
    }

    // MARK: - Walk 2 (2026-11-20)

    /// R1 — the gap starts no later than when the device last knew the
    /// conversation; a device behind through refused reads skipped no row.
    func testTheGapStartsNoLaterThanWhenTheDeviceLastKnewTheConversation() {
        // Refused reads, nothing skipped: the newest row applied.
        XCTAssertEqual(ConversationReentryRules.gapStart(
            firstSkippedMs: nil, lastKnownRowMs: 5_000, lastReadOkMs: 7_000,
            suspicionSinceMs: 9_000, nowMs: 10_000), 5_000)
        // No row applied: the last good read, then the suspicion, then now.
        XCTAssertEqual(ConversationReentryRules.gapStart(
            firstSkippedMs: nil, lastKnownRowMs: nil, lastReadOkMs: 7_000,
            suspicionSinceMs: 9_000, nowMs: 10_000), 7_000)
        XCTAssertEqual(ConversationReentryRules.gapStart(
            firstSkippedMs: nil, lastKnownRowMs: nil, lastReadOkMs: nil,
            suspicionSinceMs: 9_000, nowMs: 10_000), 9_000)
        XCTAssertEqual(ConversationReentryRules.gapStart(
            firstSkippedMs: nil, lastKnownRowMs: nil, lastReadOkMs: nil,
            suspicionSinceMs: nil, nowMs: 10_000), 10_000)
        // A skipped row decides only when it is earlier.
        XCTAssertEqual(ConversationReentryRules.gapStart(
            firstSkippedMs: 6_000, lastKnownRowMs: 5_000, lastReadOkMs: nil,
            suspicionSinceMs: 9_000, nowMs: 10_000), 5_000)
        XCTAssertEqual(ConversationReentryRules.gapStart(
            firstSkippedMs: 4_000, lastKnownRowMs: 5_000, lastReadOkMs: nil,
            suspicionSinceMs: 9_000, nowMs: 10_000), 4_000)
        // Never in the future.
        XCTAssertEqual(ConversationReentryRules.gapStart(
            firstSkippedMs: nil, lastKnownRowMs: 12_000, lastReadOkMs: nil,
            suspicionSinceMs: nil, nowMs: 10_000), 10_000)
    }

    func testTheRowClockKeepsTheNewestRowAndTheLastGoodReadAtMostOnceAMinute() {
        let store = ReentryRowClockStore(defaults: suite())
        store.noteRowKnown(accountIdB64: account, groupIdHex: "aa", atMs: 5_000)
        store.noteRowKnown(accountIdB64: account, groupIdHex: "aa", atMs: 4_000)
        XCTAssertEqual(store.load(accountIdB64: account, groupIdHex: "aa").lastKnownRowMs, 5_000, "never backward")
        store.noteReadOk(accountIdB64: account, groupIdHex: "aa", nowMs: 100_000)
        store.noteReadOk(accountIdB64: account, groupIdHex: "aa", nowMs: 130_000)
        XCTAssertEqual(store.load(accountIdB64: account, groupIdHex: "aa").lastReadOkMs, 100_000,
                       "written at most once a minute")
        store.noteReadOk(accountIdB64: account, groupIdHex: "aa", nowMs: 160_000)
        XCTAssertEqual(store.load(accountIdB64: account, groupIdHex: "aa").lastReadOkMs, 160_000)
        XCTAssertEqual(store.load(accountIdB64: account, groupIdHex: "bb"), ReentryRowClock())

        // Back by a Welcome: no row of the new group applied yet.
        store.noteReentryJoined(accountIdB64: account, groupIdHex: "aa", nowMs: 200_000)
        XCTAssertEqual(store.load(accountIdB64: account, groupIdHex: "aa"),
                       ReentryRowClock(lastKnownRowMs: nil, lastReadOkMs: 200_000))

        store.remove(accountIdB64: account, groupIdHex: "aa")
        XCTAssertEqual(store.load(accountIdB64: account, groupIdHex: "aa"), ReentryRowClock())

        AccountStoreGate.seal(account)
        defer { AccountStoreGate.open(account) }
        store.noteRowKnown(accountIdB64: account, groupIdHex: "aa", atMs: 9_000)
        XCTAssertNil(store.load(accountIdB64: account, groupIdHex: "aa").lastKnownRowMs, "a sealed account takes none")
    }

    func testTheObservationsKeepTheNewestAppliedRowUntilTaken() {
        let seen = ReentryObservations(nowMs: { 0 })
        seen.noteApplied(groupIdHex: "g", createdAtMs: 30)
        seen.noteApplied(groupIdHex: "g", createdAtMs: 20)
        seen.noteApplied(groupIdHex: "g")
        XCTAssertEqual(seen.takeLastAppliedRowAtMs(groupIdHex: "g"), 30)
        XCTAssertNil(seen.takeLastAppliedRowAtMs(groupIdHex: "g"), "taken once")
    }

    /// The fold keeps the EARLIEST failed row, also after a request stored a
    /// start.
    func testTheGapStartOnlyMovesEarlier() {
        var behind = ConversationReentryRecord()
        behind.behind = .confirmed(sinceMs: 5)
        behind.gapFromMs = 800
        let e = evidence(aboveLocal: true, firstAt: 400)
        let folded = ConversationReentryRules.fold(
            behind, observation: observation(latest: 9, local: 6, evidence: e), evidence: e, nowMs: 1_000)
        XCTAssertEqual(folded.record.gapFromMs, 400)
        let later = evidence(aboveLocal: true, firstAt: 900)
        XCTAssertEqual(ConversationReentryRules.fold(
            folded.record, observation: observation(latest: 9, local: 6, evidence: later),
            evidence: later, nowMs: 1_000).record.gapFromMs, 400)
    }

    /// R2 — the line goes after the last row at or before its start, so a
    /// message sent after the replacement but before this device came back
    /// sits below it.
    func testAGapLineSitsWhereTheGapWasByTime() throws {
        func row(_ id: Int64, _ ms: Int64) -> ChatMessage {
            ChatMessage(messageId: id, direction: .incoming, text: "m\(id)",
                        createdAt: ChatRepository.isoTimestamp(ms: ms))
        }
        let rows = [row(1, 1_000), row(2, 5_000), row(3, 6_000), row(4, 9_000)]
        XCTAssertEqual(ConversationReentryRules.gapLineIndex(rows, fromMs: 5_000), 2, "after the row at its start")
        XCTAssertEqual(ConversationReentryRules.gapLineIndex(rows, fromMs: 500), 0)
        XCTAssertEqual(ConversationReentryRules.gapLineIndex(rows, fromMs: 10_000), 4, "at the end")
        let unstamped = ChatMessage(messageId: 9, direction: .incoming, text: "x", createdAt: "")
        XCTAssertEqual(ConversationReentryRules.gapLineIndex([unstamped] + rows, fromMs: 500), 1,
                       "a row with no stamp does not decide")

        let identity = try Identity.fromSeed(seed: Data(repeating: 0x53, count: 32))
        let record = GroupRecord(group: try Group.create(identity: identity, kind: .group),
                                 groupId: Data(repeating: 0x79, count: 32))
        // The second cycle: an earlier gap line, then a message sent after the
        // replacement (stamped 6 000) but held before this device came back
        // at 8 000.
        record.appendLocalRow(
            text: ConversationReentryRules.gapMarker(fromMs: 100, toMs: 200), messageId: -1,
            createdAt: ChatRepository.isoTimestamp(ms: 100))
        record.appendContentRow(row(1, 1_000))
        record.appendContentRow(row(2, 6_000))
        record.insertReentryGapLine(fromMs: 5_000, toMs: 8_000)
        let texts = record.messages.map(\.text)
        XCTAssertEqual(texts, [
            ConversationReentryRules.gapMarker(fromMs: 100, toMs: 200), "m1",
            ConversationReentryRules.gapMarker(fromMs: 5_000, toMs: 8_000), "m2",
        ])
        let line = try XCTUnwrap(record.messages.first { $0.text.contains("from=5000") })
        XCTAssertEqual(ChatRepository.parseDsTimestampMs(line.createdAt), 5_000, "stamped with its start")
        XCTAssertLessThan(line.messageId, -1, "a fresh local id below every other")
    }

    /// R4 — due until a delivery reached every current member at the
    /// current version.
    func testTheKnockKeyIsDueUntilADeliveryReachedEveryMember() {
        XCTAssertTrue(ConversationReentryRules.knockKeySendDue(mark: nil, version: 0, members: ["a"]))
        let mark = ReentryKnockKeySentMark(version: 1, members: ["b", "a"])
        XCTAssertFalse(ConversationReentryRules.knockKeySendDue(mark: mark, version: 1, members: ["a", "b"]))
        XCTAssertFalse(ConversationReentryRules.knockKeySendDue(mark: mark, version: 1, members: ["a"]),
                       "a member who left changes nothing")
        XCTAssertTrue(ConversationReentryRules.knockKeySendDue(mark: mark, version: 1, members: ["a", "b", "c"]),
                      "a newcomer makes it due")
        XCTAssertTrue(ConversationReentryRules.knockKeySendDue(mark: mark, version: 2, members: ["a", "b"]),
                      "a new version makes it due")

        let store = ReentryKnockKeySentStore(defaults: suite())
        XCTAssertNil(store.load(accountIdB64: account, groupIdHex: "aa"))
        store.save(mark, accountIdB64: account, groupIdHex: "aa")
        XCTAssertEqual(store.load(accountIdB64: account, groupIdHex: "aa"), mark)
        store.remove(accountIdB64: account, groupIdHex: "aa")
        XCTAssertNil(store.load(accountIdB64: account, groupIdHex: "aa"))
    }

    // MARK: - Walk 3 (2026-11-20)

    /// W3-R2 — a refused read with no answered read since holds every send
    /// that is not the person's own content, even in step; the person's own
    /// content is unchanged.
    func testAnAutomaticSendIsHeldWhileAReadOfTheJournalIsRefused() {
        XCTAssertFalse(ConversationReentryRules.sendAllowed(
            behind: .inStep, isPersonContent: false, readRefused: true))
        XCTAssertTrue(ConversationReentryRules.sendAllowed(
            behind: .inStep, isPersonContent: true, readRefused: true))
        XCTAssertTrue(ConversationReentryRules.sendAllowed(
            behind: .suspected(sinceMs: 1), isPersonContent: true, readRefused: true))
        XCTAssertFalse(ConversationReentryRules.sendAllowed(
            behind: .confirmed(sinceMs: 1), isPersonContent: true, readRefused: true))
        XCTAssertTrue(ConversationReentryRules.sendAllowed(
            behind: .inStep, isPersonContent: false, readRefused: false))

        // What feeds it: a refusal counts until a read is answered.
        let observations = ReentryObservations()
        observations.noteReadRefused(groupIdHex: "aa")
        XCTAssertGreaterThan(observations.snapshot(groupIdHex: "aa").refusedReads, 0)
        observations.noteReadAnswered(groupIdHex: "aa")
        XCTAssertEqual(observations.snapshot(groupIdHex: "aa").refusedReads, 0)
    }

    /// W3-R5 — the person's own act is held only while behind, never on a
    /// refused read alone.
    func testAPersonActionIsHeldOnlyWhileBehind() {
        XCTAssertTrue(ConversationReentryRules.sendAllowed(
            behind: .inStep, isPersonContent: false, readRefused: true, personAction: true))
        XCTAssertFalse(ConversationReentryRules.sendAllowed(
            behind: .suspected(sinceMs: 1), isPersonContent: false, personAction: true))
        XCTAssertFalse(ConversationReentryRules.sendAllowed(
            behind: .confirmed(sinceMs: 1), isPersonContent: false, personAction: true))
        XCTAssertTrue(ConversationReentryRules.sendAllowed(
            behind: .inStep, isPersonContent: false, personAction: true))
    }

    /// W3-R3 — what author key a bundled row carries.
    func testABundledRowCarriesItsAuthorKey() {
        let them = Data(repeating: 0x21, count: 32)
        let stamped = Data(repeating: 0x22, count: 32)
        let own = Data(repeating: 0x23, count: 32)
        func row(_ direction: String, _ author: Data?) -> PersistedMessage {
            PersistedMessage(messageId: 1, direction: direction, text: "x", attachments: [],
                             createdAt: "2026-11-20T10:00:00Z", authorKeyB64: author.map(base64Encode))
        }
        XCTAssertEqual(HistoryBundleCodec.placeholderAuthorKey(row("incoming", them), ownKey: own), them)
        XCTAssertNil(HistoryBundleCodec.placeholderAuthorKey(row("incoming", nil), ownKey: own))
        XCTAssertEqual(HistoryBundleCodec.placeholderAuthorKey(row("outgoing", stamped), ownKey: own), own,
                       "an outgoing row carries this account's key in the conversation")
        XCTAssertEqual(HistoryBundleCodec.placeholderAuthorKey(row("outgoing", stamped), ownKey: nil), stamped)
        XCTAssertNil(HistoryBundleCodec.placeholderAuthorKey(row("system", them), ownKey: own))
        XCTAssertNil(HistoryBundleCodec.placeholderAuthorKey(row("incoming", Data([1, 2, 3])), ownKey: own),
                     "only a 32-byte key travels")
    }

    /// W3-R3 — a gap bundle row on the device that came back: the author
    /// decides the direction; without one, the route rule.
    func testAGapBundleRowTakesItsAuthorAndDirection() {
        let own = Data(repeating: 0x31, count: 32)
        let other = Data(repeating: 0x32, count: 32)
        func row(_ direction: String) -> PersistedMessage {
            PersistedMessage(messageId: 5, direction: direction, text: "hi", attachments: [],
                             createdAt: "2026-11-20T10:00:00Z")
        }
        // The re-admitter's own message, written by someone else.
        let theirs = HistoryBundleCodec.gapRow(row("outgoing"), author: other, ownKey: own, route: "knock")
        XCTAssertEqual(theirs.direction, "incoming")
        XCTAssertEqual(theirs.authorKeyB64, base64Encode(other))
        // This person's own message, which the re-admitter received.
        let mine = HistoryBundleCodec.gapRow(row("incoming"), author: own, ownKey: own, route: "knock")
        XCTAssertEqual(mine.direction, "outgoing")
        XCTAssertEqual(mine.authorKeyB64, base64Encode(own))
        XCTAssertEqual(HistoryBundleCodec.gapRow(row("system"), author: other, ownKey: own, route: "knock").direction,
                       "system")
        // No author: the route rule.
        XCTAssertEqual(HistoryBundleCodec.gapRow(row("outgoing"), author: nil, ownKey: own, route: "mailbox").direction,
                       "incoming")
        XCTAssertEqual(HistoryBundleCodec.gapRow(row("incoming"), author: nil, ownKey: own, route: "mailbox").direction,
                       "outgoing")
        XCTAssertEqual(HistoryBundleCodec.gapRow(row("outgoing"), author: nil, ownKey: own, route: "own-device").direction,
                       "outgoing")
        XCTAssertEqual(HistoryBundleCodec.gapRow(row("outgoing"), author: nil, ownKey: own, route: "knock").direction,
                       "incoming")
        XCTAssertEqual(HistoryBundleCodec.gapRow(row("outgoing"), author: nil, ownKey: own, route: nil).direction,
                       "incoming")
        XCTAssertNil(HistoryBundleCodec.gapRow(row("incoming"), author: nil, ownKey: own, route: nil).authorKeyB64)
        // A pairing bundle (the same account): incoming rows take the author,
        // directions stay.
        let paired = HistoryBundleCodec.pairingRow(row("incoming"), author: other)
        XCTAssertEqual(paired.direction, "incoming")
        XCTAssertEqual(paired.authorKeyB64, base64Encode(other))
        XCTAssertEqual(HistoryBundleCodec.pairingRow(row("outgoing"), author: other).direction, "outgoing")
    }

    /// W3-R3 — the route that brought the device back survives in the gap
    /// look, so a row without an author is still placed by it.
    func testAGapLookKeepsTheRouteOfTheRequest() throws {
        let store = ReentryGapLookStore(defaults: suite(), wrapKey: { _, _ in Data(repeating: 9, count: 32) })
        let look = ReentryGapLook(
            groupIdHex: "aa", keys: [onePair], gapFromMs: 1, gapToMs: 2, route: "mailbox")
        XCTAssertTrue(store.save(look, accountIdB64: account))
        XCTAssertEqual(store.load(accountIdB64: account, groupIdHex: "aa")?.route, "mailbox")
    }

    /// ADR-0101 section 9, live walk 2026-11-26 D2 — a history page row the
    /// reset person wrote under the key the reset retired is drawn as their
    /// own; a row of the other person stays received.
    func testAHistoryRowUnderAFormerOwnKeyIsDrawnAsOwn() {
        let current = Data(repeating: 0x41, count: 32)
        let retired = Data(repeating: 0x42, count: 32)
        let other = Data(repeating: 0x43, count: 32)
        let row = PersistedMessage(messageId: 7, direction: "incoming", text: "a1", attachments: [],
                                   createdAt: "2026-11-26T10:00:00Z")
        let mine = HistoryBundleCodec.gapRow(
            row, author: retired, ownKey: current, route: "history-return", formerOwnKeys: [retired])
        XCTAssertEqual(mine.direction, "outgoing")
        XCTAssertEqual(mine.authorKeyB64, base64Encode(retired))
        let theirs = HistoryBundleCodec.gapRow(
            row, author: other, ownKey: current, route: "history-return", formerOwnKeys: [retired])
        XCTAssertEqual(theirs.direction, "incoming")
        // Without the former keys (the rule before D2) the own row read as
        // received.
        XCTAssertEqual(
            HistoryBundleCodec.gapRow(row, author: retired, ownKey: current, route: "history-return").direction,
            "incoming")
    }
}
