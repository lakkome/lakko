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
}
