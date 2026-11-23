// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// `docs/ux/second-device.md` section 2 — the history bundle a device received
/// at pairing stays, and shows, through the Welcomes that come after it.
@MainActor
final class PairingHistoryTests: XCTestCase {

    private let bundleSecret = Data(repeating: 0x5C, count: 32)

    // MARK: - A real repository on a device that holds no group yet

    @MainActor
    private struct Device {
        let repo: ChatRepository
        let identity: Identity
        let accountIdB64: String
        let log: MessageLogStore
        let snapshots: IdentitySnapshotStore
        let secrets: IdentitySecrets

        func cleanUp() {
            log.deleteLog(accountIdB64: accountIdB64)
            snapshots.deleteSnapshot(accountIdB64: accountIdB64)
        }

        func thread(_ groupIdHex: String) -> ChatThread? {
            repo.state.threads.first { $0.groupIdHex == groupIdHex }
        }

        func slot(_ groupIdHex: String) -> PersistedGroup? {
            log.getLog(accountIdB64: accountIdB64)?.groups[groupIdHex]
        }

        func rows(_ groupIdHex: String) -> [Int64] {
            log.getLog(accountIdB64: accountIdB64)?.groups[groupIdHex]?.messages.map(\.messageId) ?? []
        }

        func shown(_ groupIdHex: String) -> [Int64] {
            repo.state.threads.first { $0.groupIdHex == groupIdHex }?.messages.map(\.messageId) ?? []
        }

        func join(_ invited: Invited) async throws {
            _ = try await repo.joinGroupFromWelcome(welcomeBytes: invited.welcome)
        }
    }

    private struct Invited {
        let id: Data
        let hex: String
        let welcome: Data
    }

    /// A fresh device: no snapshot, so a bootstrap holds no group. The test
    /// makes the device identity, so an invitation can be made, and a bundle
    /// filed, before the repository runs.
    private func device() throws -> Device {
        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let suite = "pairing-history-\(UUID().uuidString)"
        let log = MessageLogStore()
        let snapshots = IdentitySnapshotStore()
        let ds = DsClient(baseURL: Offline.dsBaseURL, session: Offline.session())
        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            discovery: DiscoveryClient(
                baseURL: Offline.discoveryBaseURL, session: Offline.session(), tokens: OfflineAnonTokens()),
            snapshotStore: snapshots,
            knownGroupsStore: KnownGroupsStore(defaults: UserDefaults(suiteName: suite + "-k")!),
            messageLogStore: log,
            commitOutbox: CommitOutboxStore(
                rootDir: FileManager.default.temporaryDirectory.appendingPathComponent(suite)),
            pushRouting: Offline.pushRouting(),
            beaconStore: BeaconObservationStore(defaults: UserDefaults(suiteName: suite + "-b")!))
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x42, count: 32))
        repo.deviceIdentityInstaller = { _, _ in identity }
        return Device(
            repo: repo,
            identity: identity,
            accountIdB64: base64Encode(accountIdBytes),
            log: log,
            snapshots: snapshots,
            secrets: IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: Data(repeating: 0x22, count: 32),
                accountIdentitySeed: Data(repeating: 0x41, count: 32),
                // No token: nothing here uploads a KeyPackage.
                sessionToken: "",
                sessionExpiresAtMs: 0))
    }

    /// A group another member founded, and the Welcome that lets `joiner` in.
    private func invite(_ joiner: Identity, founder byte: UInt8) throws -> Invited {
        let founder = try Identity.fromSeed(seed: Data(repeating: byte, count: 32))
        let group = try Group.create(identity: founder, kind: .group)
        let added = try group.addMemberFromWire(identity: founder, keyPackage: try joiner.createKeyPackage())
        let id = try group.groupId()
        return Invited(id: id, hex: DsClient.pseudonymHex(id), welcome: added.welcome)
    }

    private func row(_ id: Int64, direction: String = "incoming") -> PersistedMessage {
        PersistedMessage(
            messageId: id, direction: direction, text: "m\(id)", attachments: [],
            createdAt: "2026-10-30T10:00:00Z")
    }

    private func sealedBundle(_ groups: [(id: Data, rows: [PersistedMessage])]) throws -> Data {
        try sealHistoryBundleStructured(
            secret: bundleSecret,
            groups: try groups.map { group in
                StructuredHistoryGroup(
                    groupId: group.id,
                    messages: try group.rows.map { row in
                        var be = row.messageId.bigEndian
                        return StructuredHistoryMessage(
                            payloadBytes: try HistoryBundleCodec.encodePlaceholder(row),
                            senderLeaf: 0,
                            messageId: withUnsafeBytes(of: &be) { Data($0) },
                            receivedAtMs: 1)
                    },
                    countCapped: false)
            },
            maxHlc: 0)
    }

    private func file(_ groups: [(id: Data, rows: [PersistedMessage])], on device: Device) throws {
        _ = try HistoryBundleCodec.applyToMessageLog(
            accountIdB64: device.accountIdB64, secret: bundleSecret,
            sealedBytes: try sealedBundle(groups), store: device.log)
    }

    // MARK: - Does a bundle's history survive?

    /// A join by name: the bundle is on disk before the session lands, so the
    /// bootstrap holds no group it could seed. Two groups, joined one by one:
    /// the second must keep its rows through the first join's persist.
    func testABundleFiledBeforeTheBootstrapSurvivesTheWelcomes() async throws {
        let d = try device()
        defer { d.cleanUp() }
        let first = try invite(d.identity, founder: 0x51)
        let second = try invite(d.identity, founder: 0x52)
        try file([(first.id, [row(4), row(5)]), (second.id, [row(7)])], on: d)

        d.repo.bootstrap(secrets: d.secrets)
        try await d.join(first)

        XCTAssertEqual(d.rows(first.hex), [4, 5])
        XCTAssertEqual(d.rows(second.hex), [7], "the group not joined yet keeps its slot")
        XCTAssertEqual(d.shown(first.hex), [4, 5])

        try await d.join(second)
        XCTAssertEqual(d.rows(first.hex), [4, 5])
        XCTAssertEqual(d.rows(second.hex), [7])
        XCTAssertEqual(d.shown(second.hex), [7])
    }

    /// A device pairing while signed in: the repository runs when the bundle
    /// lands, and the Welcome comes after it.
    func testABundleThatLandsWhileTheRepositoryRunsSurvivesTheWelcome() async throws {
        let d = try device()
        defer { d.cleanUp() }
        let invited = try invite(d.identity, founder: 0x53)
        d.repo.bootstrap(secrets: d.secrets)

        try file([(invited.id, [row(4), row(5)])], on: d)
        try await d.join(invited)

        XCTAssertEqual(d.rows(invited.hex), [4, 5])
        XCTAssertEqual(d.shown(invited.hex), [4, 5])
    }

    /// The Welcome came first: the running repository rewrites the whole log
    /// from memory, so the bundle has to reach memory too.
    func testABundleThatLandsAfterTheWelcomeShowsAndStays() async throws {
        let d = try device()
        defer { d.cleanUp() }
        let invited = try invite(d.identity, founder: 0x54)
        d.repo.bootstrap(secrets: d.secrets)
        try await d.join(invited)

        try file([(invited.id, [row(4), row(5)])], on: d)
        await settle()
        XCTAssertEqual(d.shown(invited.hex), [4, 5])

        d.repo.persistMessageLogBestEffort()
        XCTAssertEqual(d.rows(invited.hex), [4, 5])
    }

    /// Placeholder version 7: a call row the bundle brings shows in the
    /// conversation and in the Calls tab, where a live call row is filed,
    /// although the one-time fill of that tab ran before the bundle came.
    func testACallRowABundleBringsIsInTheConversationAndTheCallsTab() async throws {
        let d = try device()
        defer {
            d.cleanUp()
            CallHistoryStore.shared.forget(accountIdB64: d.accountIdB64)
            CallHistoryStore.shared.unbind()
        }
        let invited = try invite(d.identity, founder: 0x5E)
        d.repo.bootstrap(secrets: d.secrets)
        try await d.join(invited)
        d.repo.backfillCallHistory()

        let call = CallTranscriptRow(direction: .outgoing, outcome: .answered(durationMs: 42_000), video: true)
        let row = PersistedMessage(
            messageId: -1, direction: "system", text: call.marker, attachments: [],
            createdAt: "2026-11-23T10:00:00Z")
        try file([(invited.id, [row, self.row(4)])], on: d)
        await settle()

        XCTAssertEqual(d.shown(invited.hex), [4, -1], "the call (2026-11-23) after the message (2026-10-30)")
        let shownCall = d.thread(invited.hex)?.messages.first { $0.messageId == -1 }
        XCTAssertEqual(shownCall.flatMap { CallTranscriptRow.parse($0.text) }, call)
        var entries = CallHistoryStore.shared.entries.filter { $0.conversationId == invited.hex }
        XCTAssertEqual(entries.count, 1)
        XCTAssertEqual(entries.first?.transcriptRow.text, call.text)
        XCTAssertEqual(entries.first?.video, true)

        // The same bundle again files nothing twice.
        try file([(invited.id, [row])], on: d)
        await settle()
        entries = CallHistoryStore.shared.entries.filter { $0.conversationId == invited.hex }
        XCTAssertEqual(entries.count, 1)
    }

    func testAPersistBeforeAnyJoinKeepsTheBundle() async throws {
        let d = try device()
        defer { d.cleanUp() }
        let invited = try invite(d.identity, founder: 0x55)
        try file([(invited.id, [row(4)])], on: d)
        d.repo.bootstrap(secrets: d.secrets)

        d.repo.persistMessageLogBestEffort()

        XCTAssertEqual(d.rows(invited.hex), [4])
        XCTAssertEqual(d.slot(invited.hex)?.syncedThroughMessageId, 4)
    }

    func testARowPolledAfterTheBundleIsNotAddedTwice() throws {
        let group = try Group.create(
            identity: try Identity.fromSeed(seed: Data(repeating: 0x56, count: 32)), kind: .group)
        let record = GroupRecord(group: group, groupId: try group.groupId())
        record.adoptHistory(PersistedGroup(cursor: 0, messages: [row(4), row(5)], syncedThroughMessageId: 5))

        record.consumeEvents([
            application(5),
            .unsupported(messageId: 4, version: 9, senderKey: nil, createdAt: ""),
            .system(messageId: 3, systemKind: .commit, createdAt: ""),
            application(6),
        ])

        XCTAssertEqual(record.messages.map(\.messageId), [4, 5, 3, 6], "3 is not in the bundle, 6 is new")
    }

    /// A call row a bundle brings sits at its time: a call at 09:01 between
    /// the 09:00 and 09:02 messages, in the live record and in the log.
    func testABundleCallRowSitsAtItsTime() throws {
        func at(_ id: Int64, _ time: String, ack: ChatMessage.Ack = .sent) -> ChatMessage {
            ChatMessage(
                messageId: id, direction: id > 0 ? .incoming : .system, text: "m",
                createdAt: "2026-11-23T\(time):00Z", ack: ack, localId: ack == .sent ? 0 : -9)
        }
        let callId = try XCTUnwrap(CallTranscriptRow.logMessageId(callId: Data(repeating: 0x21, count: 16)))
        let live = PairingHistory.mergeLiveRows(
            held: [at(10, "09:00"), at(12, "09:02")], bundle: [at(callId, "09:01")])
        XCTAssertEqual(live.map(\.messageId), [10, callId, 12])
        let fresh = PairingHistory.mergeLiveRows(
            held: [], bundle: [at(12, "09:02"), at(callId, "09:01"), at(10, "09:00")])
        XCTAssertEqual(fresh.map(\.messageId), [10, callId, 12])
        // Never past a row the server has not numbered yet.
        let pending = PairingHistory.mergeLiveRows(
            held: [at(10, "09:00"), at(0, "09:05", ack: .pending)], bundle: [at(callId, "09:07")])
        XCTAssertEqual(pending.map(\.messageId), [10, callId, 0])

        func logRow(_ id: Int64, _ time: String) -> PersistedMessage {
            PersistedMessage(
                messageId: id, direction: id > 0 ? "incoming" : "system", text: "m", attachments: [],
                createdAt: "2026-11-23T\(time):00Z")
        }
        let log = PairingHistory.mergeLogRows(
            [logRow(12, "09:02"), logRow(10, "09:00")], [logRow(callId, "09:01")])
        XCTAssertEqual(log.rows.map(\.messageId), [10, callId, 12])
        XCTAssertEqual(log.added, 1)
    }

    /// A call row of a bundle that landed before the chat was joined is in
    /// the Calls tab from the moment the bundle is filed, and stays there
    /// once through the join and a second copy of the bundle.
    func testABundleCallRowOfAChatJoinedLaterIsInTheCallsTabOnce() async throws {
        let d = try device()
        defer {
            d.cleanUp()
            CallHistoryStore.shared.forget(accountIdB64: d.accountIdB64)
            CallHistoryStore.shared.unbind()
        }
        let invited = try invite(d.identity, founder: 0x5F)
        d.repo.bootstrap(secrets: d.secrets)
        d.repo.backfillCallHistory()
        let call = CallTranscriptRow(direction: .incoming, outcome: .missed)
        let row = PersistedMessage(
            messageId: try XCTUnwrap(CallTranscriptRow.logMessageId(callId: Data(repeating: 0x7A, count: 16))),
            direction: "system", text: call.marker, attachments: [], createdAt: "2026-11-23T09:01:00Z")
        try file([(invited.id, [row])], on: d)
        await settle()
        XCTAssertEqual(CallHistoryStore.shared.entries.filter { $0.conversationId == invited.hex }.count, 1)

        try await d.join(invited)
        try file([(invited.id, [row])], on: d)
        await settle()

        let entries = CallHistoryStore.shared.entries.filter { $0.conversationId == invited.hex }
        XCTAssertEqual(entries.count, 1)
        XCTAssertEqual(entries.first?.outcome, .missed)
        XCTAssertEqual(d.shown(invited.hex), [row.messageId])
    }

    func testBundleRowsGoInOrderAroundWhatIsHeld() {
        let held = [chat(5), chat(9), chat(0, ack: .pending)]
        let merged = PairingHistory.mergeLiveRows(
            held: held, bundle: [chat(7), chat(5), chat(3), chat(11), chat(-2)])
        XCTAssertEqual(merged.map(\.messageId), [-2, 3, 5, 7, 9, 11, 0])
    }

    // MARK: - History is not news

    func testTheFloorIsTheHighestIdOfTheBundleAndNeverMovesDown() {
        let first = HistoryBundleCodec.mergeBundle(held: [:], decoded: ["aa": [row(4), row(9), row(-3)]])
        XCTAssertEqual(first.groups["aa"]?.syncedThroughMessageId, 9)
        XCTAssertEqual(first.applied, 3)

        let second = HistoryBundleCodec.mergeBundle(held: first.groups, decoded: ["aa": [row(2)]])
        XCTAssertEqual(second.groups["aa"]?.syncedThroughMessageId, 9)
        XCTAssertEqual(second.groups["aa"]?.messages.map(\.messageId), [-3, 2, 4, 9])

        let third = HistoryBundleCodec.mergeBundle(held: second.groups, decoded: ["aa": [row(12)]])
        XCTAssertEqual(third.groups["aa"]?.syncedThroughMessageId, 12)

        let calls = HistoryBundleCodec.mergeBundle(held: [:], decoded: ["aa": [row(-3)]])
        XCTAssertNil(calls.groups["aa"]?.syncedThroughMessageId, "no id the server assigned")
    }

    func testTheMergeKeepsWhatIsThisDevices() {
        let slot = PersistedGroup(
            cursor: 7, messages: [row(6)], lastViewedMessageId: 6, feedSeen: [8],
            syncedThroughMessageId: 3)
        let merged = HistoryBundleCodec.mergeBundle(held: ["aa": slot], decoded: ["aa": [row(6), row(5)]])
        let after = merged.groups["aa"]
        XCTAssertEqual(after?.cursor, 7)
        XCTAssertEqual(after?.lastViewedMessageId, 6, "the floor is not a read position")
        XCTAssertEqual(after?.feedSeen, [8])
        XCTAssertEqual(after?.syncedThroughMessageId, 6)
        XCTAssertEqual(merged.applied, 1)
    }

    func testTheFloorSurvivesAnOldLogAndCarriesNoReadPosition() throws {
        // A row written before the field existed.
        let old = #"{"cursor":3,"messages":[]}"#.data(using: .utf8)!
        let decoded = try JSONDecoder().decode(PersistedGroup.self, from: old)
        XCTAssertNil(decoded.syncedThroughMessageId)
        let back = try JSONDecoder().decode(
            PersistedGroup.self,
            from: JSONEncoder().encode(
                PersistedGroup(cursor: 1, messages: [], syncedThroughMessageId: 9)))
        XCTAssertEqual(back.syncedThroughMessageId, 9)
        XCTAssertNil(back.lastViewedMessageId)
    }

    /// With a read cursor, without one, and with a cursor that arrives later:
    /// a row of the bundle is not unread, a later one is.
    func testACursorNeverCountsTheBundle() throws {
        let thread = ChatThread(
            groupIdHex: "aa",
            messages: [chat(4), chat(5), chat(6), chat(7, direction: .outgoing)],
            cursor: 0,
            syncedThroughMessageId: 5)
        let cursor = { (id: Int64) in
            ReadCursorState(upToMessageId: UnreadCounting.cursorBytes(id), readAtMs: 1, updatedAtHlc: 1)
        }

        XCTAssertNil(ChatListScreen.unreadCount(thread: thread, cursor: nil))
        XCTAssertEqual(ChatListScreen.unreadCount(thread: thread, cursor: cursor(0)), 1)
        XCTAssertEqual(ChatListScreen.unreadCount(thread: thread, cursor: cursor(2)), 1)
        XCTAssertEqual(ChatListScreen.unreadCount(thread: thread, cursor: cursor(6)), 0)
    }

    /// A system row (a commit, a membership line) is not a message: with a
    /// read cursor it never counts, so two hidden commit rows put no badge on
    /// a chat whose timeline shows nothing new (seen live 2026-11-22).
    func testACursorNeverCountsSystemRows() {
        let thread = ChatThread(
            groupIdHex: "aa",
            messages: [
                chat(4, direction: .system), chat(5, direction: .system), chat(6),
            ],
            cursor: 0)
        let cursor = ReadCursorState(
            upToMessageId: UnreadCounting.cursorBytes(0), readAtMs: 1, updatedAtHlc: 1)
        XCTAssertEqual(ChatListScreen.unreadCount(thread: thread, cursor: cursor), 1)
    }

    func testWithoutAFloorEveryRowPastTheCursorCounts() {
        let thread = ChatThread(groupIdHex: "aa", messages: [chat(4), chat(5)], cursor: 0)
        let cursor = ReadCursorState(
            upToMessageId: UnreadCounting.cursorBytes(0), readAtMs: 1, updatedAtHlc: 1)
        XCTAssertEqual(ChatListScreen.unreadCount(thread: thread, cursor: cursor), 2)
    }

    func testTheLocalCountLeavesOutWhatTheBundleBrought() {
        let next = UnreadCounting.next(
            current: [], applied: [application(4), application(5), application(6)],
            chatOpen: false, syncedThrough: 5)
        XCTAssertEqual(next, [6])
        XCTAssertEqual(
            UnreadCounting.next(current: [], applied: [application(4)], chatOpen: false), [4])
    }

    /// A row counted before the bundle came, that the bundle also carried.
    func testABundleTakesBackWhatWasCountedBeforeIt() async throws {
        let d = try device()
        defer { d.cleanUp() }
        let invited = try invite(d.identity, founder: 0x57)
        d.repo.bootstrap(secrets: d.secrets)
        try await d.join(invited)
        d.repo.unreadMessageIds[invited.hex] = [5, 8]

        try file([(invited.id, [row(4), row(5)])], on: d)
        await settle()

        XCTAssertEqual(d.repo.unreadMessageIds[invited.hex], [8])
        XCTAssertEqual(d.thread(invited.hex)?.unread, 1)
        XCTAssertEqual(d.thread(invited.hex)?.syncedThroughMessageId, 5)
    }

    /// The floor is written with the log, comes back with a restart, and is
    /// no read position.
    func testTheFloorComesBackWithARestart() async throws {
        let d = try device()
        defer { d.cleanUp() }
        let invited = try invite(d.identity, founder: 0x58)
        try file([(invited.id, [row(4), row(5)])], on: d)
        d.repo.bootstrap(secrets: d.secrets)
        try await d.join(invited)
        XCTAssertEqual(d.slot(invited.hex)?.syncedThroughMessageId, 5)

        d.repo.clear()
        d.repo.bootstrap(secrets: d.secrets)

        XCTAssertEqual(d.thread(invited.hex)?.syncedThroughMessageId, 5)
        XCTAssertEqual(d.shown(invited.hex), [4, 5])
        XCTAssertNil(d.thread(invited.hex)?.lastViewedMessageId)
    }

    func testAnotherAccountsBundleIsNotTakenIn() async throws {
        let d = try device()
        defer { d.cleanUp() }
        let invited = try invite(d.identity, founder: 0x59)
        d.repo.bootstrap(secrets: d.secrets)
        try await d.join(invited)

        d.repo.adoptHistoryBundle(
            accountIdB64: "another",
            slots: [invited.hex: PersistedGroup(cursor: 0, messages: [row(4)], syncedThroughMessageId: 4)])

        XCTAssertEqual(d.shown(invited.hex), [])
        XCTAssertNil(d.thread(invited.hex)?.syncedThroughMessageId)
    }

    func testTheFloorGoesWithTheAccount() async throws {
        let d = try device()
        defer { d.cleanUp() }
        let invited = try invite(d.identity, founder: 0x5A)
        try file([(invited.id, [row(4)])], on: d)
        d.repo.bootstrap(secrets: d.secrets)
        d.repo.clear()
        XCTAssertTrue(d.repo.waitingLogSlots.slots.isEmpty)

        d.log.deleteLog(accountIdB64: d.accountIdB64)
        XCTAssertNil(d.slot(invited.hex))
    }

    private func application(_ id: Int64) -> IncomingChatEvent {
        .application(
            messageId: id, text: "hi", attachments: [], senderKey: nil, createdAt: "", reply: nil,
            forward: nil)
    }

    private func chat(_ id: Int64, direction: Lakko.Direction = .incoming, ack: ChatMessage.Ack = .sent) -> ChatMessage {
        ChatMessage(
            messageId: id, direction: direction, text: "m", createdAt: "2026-10-30T10:00:00Z",
            ack: ack, localId: ack == .sent ? 0 : -9)
    }

    /// A bundle posted off the main actor lands on the next turn of it.
    private func settle() async {
        await Task.yield()
        try? await Task.sleep(nanoseconds: 20_000_000)
    }
}
