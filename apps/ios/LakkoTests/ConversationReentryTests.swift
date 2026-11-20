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
        XCTAssertEqual(reentryRoute(ownDeviceInConversation: true, kind: .group), .ownDevice)
        XCTAssertEqual(reentryRoute(ownDeviceInConversation: false, kind: .oneToOne), .mailbox)
        XCTAssertEqual(reentryRoute(ownDeviceInConversation: false, kind: .group), .knock)
    }

    @MainActor
    func testARequestEnvelopeTravelsAsItsOwnServiceEnvelopeSlot() throws {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32))
        let group = try Group.create(identity: identity, kind: .group)
        let groupId = try group.groupId()
        let first = try composeConversationReentryEnvelope(
            groupId: groupId, epochLeft: try group.epoch(), gapFromMs: 5,
            keyPackage: try identity.createReentryKeyPackage(groupId: groupId, epochLeft: try group.epoch()))
        let second = try composeConversationReentryEnvelope(
            groupId: groupId, epochLeft: try group.epoch(), gapFromMs: 6,
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
    }

    // MARK: - The waiting note (UX 2.2)

    private func text(_ key: String, _ args: [String: String]) -> String {
        args.isEmpty ? key : key + "|" + args.sorted { $0.key < $1.key }.map { "\($0.key)=\($0.value)" }.joined(separator: ",")
    }

    func testTheNoteIsShownOnlyOnceConfirmed() {
        XCTAssertNil(ConversationReentryRules.note(behind: .inStep, waiter: .ownDevice, nowMs: 0, text: text))
        XCTAssertNil(ConversationReentryRules.note(
            behind: .suspected(sinceMs: 0), waiter: .ownDevice, nowMs: 99_000, text: text))
    }

    func testTheSecondLineWaitsThirtySeconds() {
        let since: UInt64 = 1_000
        XCTAssertEqual(
            ConversationReentryRules.note(
                behind: .confirmed(sinceMs: since), waiter: .ownerOrAdmin, nowMs: since + 29_999, text: text),
            ["chat.reentry.waiting"])
        XCTAssertEqual(
            ConversationReentryRules.note(
                behind: .confirmed(sinceMs: since), waiter: .ownerOrAdmin, nowMs: since + 30_000, text: text),
            ["chat.reentry.waiting", "chat.reentry.second.group"])
        XCTAssertEqual(
            ConversationReentryRules.note(
                behind: .confirmed(sinceMs: since), waiter: .ownDevice, nowMs: since + 30_000, text: text),
            ["chat.reentry.waiting", "chat.reentry.second.own_device"])
        XCTAssertEqual(
            ConversationReentryRules.note(
                behind: .confirmed(sinceMs: since), waiter: .person(name: "Anna"), nowMs: since + 30_000, text: text),
            ["chat.reentry.waiting", "chat.reentry.second.person|name=Anna"])
        XCTAssertEqual(
            ConversationReentryRules.note(
                behind: .confirmed(sinceMs: since), waiter: nil, nowMs: since + 90_000, text: text),
            ["chat.reentry.waiting"], "no second line when nobody is known")
    }

    func testTheCatalogHoldsTheNoteAndTheLine() {
        for key in ["chat.reentry.waiting", "chat.reentry.second.own_device", "chat.reentry.second.group"] {
            XCTAssertNotEqual(Localization.text(key), key, key)
        }
        XCTAssertTrue(Localization.text("chat.reentry.second.person", texts: ["name": "Anna"]).contains("Anna"))
    }

    // MARK: - What a waiting chat offers (UX 2.2)

    func testAWaitingChatMenuOffersCopyForwardSaveAndPinOnly() {
        let all = MessageMenuAction.allCases
        let waiting = ConversationReentryRules.waitingMenu(all)
        XCTAssertEqual(Set(waiting), [.copy, .copyCoordinates, .forward, .save, .unsave, .pin, .unpin])
        for gone in [MessageMenuAction.reply, .edit, .delete, .select, .translate, .retractVote,
                     .closePoll, .editChecklist, .closeChecklist, .ban] {
            XCTAssertFalse(waiting.contains(gone), gone.rawValue)
        }
        XCTAssertEqual(
            ConversationReentryRules.waitingMenu([.reply, .forward, .pin, .copy, .edit, .delete]),
            [.forward, .pin, .copy], "the order is kept")
    }

    func testAWaitingChatInfoHidesEveryChangingBlock() {
        let sections: [InfoSection] = [
            .groupName, .handle, .renameGroup, .memberList, .addMember, .inviteLink, .roles,
            .transferOwnership, .groupType, .reactions, .membersCanFindMe, .leaveGroup, .deleteGroup,
        ]
        XCTAssertEqual(
            ConversationReentryRules.waitingInfoSections(sections),
            [.groupName, .handle, .memberList, .membersCanFindMe])
        XCTAssertFalse(ConversationReentryRules.waitingInfoSections([.call, .invitePeople]).contains(.call))
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

    func testTheSameDayShowsTheDateOnce() {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        let locale = Locale(identifier: "en_GB")
        var captured: [String: String] = [:]
        _ = ConversationReentryRules.gapSentence(
            fromMs: 1_762_866_600_000, toMs: 1_762_870_200_000, calendar: calendar, locale: locale,
            text: { _, args in captured = args; return "" })
        XCTAssertEqual(captured["from"], "11 Nov, 13:10")
        XCTAssertEqual(captured["to"], "14:10")
        _ = ConversationReentryRules.gapSentence(
            fromMs: 1_762_866_600_000, toMs: 1_763_474_520_000, calendar: calendar, locale: locale,
            text: { _, args in captured = args; return "" })
        XCTAssertEqual(captured["to"], "18 Nov, 14:02")
    }

    // MARK: - The gap history (section 7)

    func testTheHistoryRendezvousFollowsTheSecret() {
        let a = ConversationReentryRules.historyRendezvous(secret: Data(repeating: 1, count: 32))
        let b = ConversationReentryRules.historyRendezvous(secret: Data(repeating: 2, count: 32))
        XCTAssertEqual(a.count, 32)
        XCTAssertNotEqual(a, b)
        XCTAssertEqual(a, ConversationReentryRules.historyRendezvous(secret: Data(repeating: 1, count: 32)))
        let leaf = Data(repeating: 7, count: 32)
        XCTAssertEqual(ConversationReentryRules.historyContext(leafKey: leaf), leaf)
    }

    /// The cross-client golden: web and Android derive the same id.
    func testTheHistoryRendezvousGolden() {
        let id = ConversationReentryRules.historyRendezvous(secret: Data(repeating: 0x42, count: 32))
        XCTAssertEqual(
            id.map { String(format: "%02x", $0) }.joined(),
            "8b4c89a03f20c038dfe71cf88eeee780a4be0af619b852cedbc63ad0dc6eb7dd")
        XCTAssertEqual(ConversationReentryRules.historyExporterLabel, "lakko/conversation-reentry/history/v1")
        XCTAssertEqual(ConversationReentryRules.historySecretLength, 32)
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
        let row = ReentryOwedHistory(
            groupIdHex: "g", keyPackage: Data([1]), leafKey: Data(repeating: 2, count: 32),
            targetEpoch: 8, gapFromMs: 1, owedAtMs: 100)
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

    /// A wrap-key provider standing in for the Keychain: one fixed key.
    private func lookStore(_ defaults: UserDefaults, key: Data = Data(repeating: 0x5A, count: 32))
        -> ReentryGapLookStore {
        ReentryGapLookStore(defaults: defaults, wrapKey: { _, _ in key })
    }

    func testAGapLookRoundTripsAndItsSecretIsNeverStoredInTheClear() throws {
        let defaults = suite()
        let store = lookStore(defaults)
        let secret = Data((0..<32).map { UInt8($0) &* 7 &+ 3 })
        let look = ReentryGapLook(groupIdHex: "aa", secret: secret, gapFromMs: 1_000, gapToMs: 5_000)
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
        let look = ReentryGapLook(groupIdHex: "aa", secret: Data(repeating: 1, count: 32), gapFromMs: 1, gapToMs: 2)
        XCTAssertFalse(store.save(look, accountIdB64: account))
        XCTAssertNil(defaults.data(forKey: ReentryGapLookStore.key(account)))
    }

    func testAGapLookRunsAtMostOncePerMinute() {
        XCTAssertEqual(ConversationReentryRules.gapHistoryLookIntervalMs, 60_000)
        var look = ReentryGapLook(groupIdHex: "aa", secret: Data(repeating: 1, count: 32), gapFromMs: 1, gapToMs: 100)
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(look, nowMs: 100), .look, "the first look is at once")
        look.lastLookMs = 1_000
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(look, nowMs: 1_000), .wait)
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(look, nowMs: 60_999), .wait)
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(look, nowMs: 61_000), .look)
    }

    func testAGapLookIsDroppedThirtyDaysAfterTheGapsEnd() {
        XCTAssertEqual(ConversationReentryRules.gapHistoryLookRetentionMs, 30 * day)
        let look = ReentryGapLook(
            groupIdHex: "aa", secret: Data(repeating: 1, count: 32), gapFromMs: 1, gapToMs: 10_000, lastLookMs: nil)
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(look, nowMs: 10_000 + 30 * day - 1), .look)
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(look, nowMs: 10_000 + 30 * day), .drop)
        var recent = look
        recent.lastLookMs = 10_000 + 30 * day - 10
        XCTAssertEqual(ConversationReentryRules.gapLookVerdict(recent, nowMs: 10_000 + 30 * day), .drop,
                       "retention wins over the throttle")
    }

    func testAnOwedHistoryWaitsThirtyDays() {
        XCTAssertEqual(ReentryHistoryOwed.lifetimeMs, 30 * day)
        let row = ReentryOwedHistory(
            groupIdHex: "g", keyPackage: Data([1]), leafKey: Data(repeating: 2, count: 32),
            targetEpoch: 8, gapFromMs: 1, owedAtMs: 100)
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
        let secret = Data(repeating: 0x42, count: 32)
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

        for heldBefore in [true, false] {
            let sealed = try XCTUnwrap(HistoryBundleCodec.buildForGroup(
                hex: hex, rows: rows, accountIdB64: account, secret: secret, heldBeforeGap: heldBefore))
            let opened = try openHistoryBundleStructured(secret: secret, sealedBytes: sealed)
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

    /// R5 — only the moment a chat starts waiting clears its errors.
    func testAChatStartingToWaitClearsItsErrors() {
        XCTAssertTrue(ConversationReentryRules.clearsChatErrors(wasWaiting: false, isWaiting: true))
        XCTAssertFalse(ConversationReentryRules.clearsChatErrors(wasWaiting: true, isWaiting: true))
        XCTAssertFalse(ConversationReentryRules.clearsChatErrors(wasWaiting: false, isWaiting: false))
        XCTAssertFalse(ConversationReentryRules.clearsChatErrors(wasWaiting: true, isWaiting: false))
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
            groupIdHex: "aa", secret: Data(repeating: 1, count: 32), gapFromMs: 1, gapToMs: 2, route: "mailbox")
        XCTAssertTrue(store.save(look, accountIdB64: account))
        XCTAssertEqual(store.load(accountIdB64: account, groupIdHex: "aa")?.route, "mailbox")
    }
}
