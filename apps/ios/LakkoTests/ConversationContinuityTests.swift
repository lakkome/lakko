// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// Answers a journal publish with a fresh id and records which journal it
/// went to; everything else gets `204`.
final class RecordingPublishURLProtocol: URLProtocol {
    private static let lock = NSLock()
    private static var _published: [String] = []
    private static var nextId: Int64 = 100

    static var published: [String] {
        lock.lock(); defer { lock.unlock() }
        return _published
    }

    static func reset() {
        lock.lock(); defer { lock.unlock() }
        _published = []
        nextId = 100
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}

    override func startLoading() {
        let url = request.url ?? URL(fileURLWithPath: "/")
        let path = url.path
        guard path.hasSuffix("/messages"), request.httpMethod == "POST" else {
            let response = HTTPURLResponse(
                url: url, statusCode: 204, httpVersion: "HTTP/1.1", headerFields: [:])
            if let response {
                client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            }
            client?.urlProtocolDidFinishLoading(self)
            return
        }
        Self.lock.lock()
        Self._published.append(path)
        Self.nextId += 1
        let id = Self.nextId
        Self.lock.unlock()
        let json = #"{"id":\#(id),"created_at":"2026-10-11T00:00:00Z"}"#
        let response = HTTPURLResponse(
            url: url, statusCode: 200, httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"])
        if let response {
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        }
        client?.urlProtocol(self, didLoad: Data(json.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
}

/// ADR-0065 "Phase 2 revised" — the repository wiring of R.5 (one
/// conversation over several groups), R.6 (the claim receive rule) and R.7
/// case 1 (nothing goes into a dead one-to-one; a message is held and
/// flushed into the successor), over real MLS groups and a stubbed DS.
@MainActor
final class ConversationContinuityTests: XCTestCase {

    private struct Fixture {
        let repo: ChatRepository
        /// The one-to-one with the person who then reset their keys.
        let oldHex: String
        /// A second one-to-one, with that person's NEW identity.
        let successorHex: String
        let accountIdB64: String
        /// The old identity's key (the person key the old roster names).
        let oldPeerKeyB64: String
        let pins: PeerPinStore
        let peerAccountIdB64: String
    }

    private var cleanupAccounts: [String] = []

    override func tearDown() async throws {
        for account in cleanupAccounts {
            HeldSendQueue.clearAccount(ownerAccountId: account)
            GroupClosedStore.clear(ownerAccountIdB64: account)
            ContinuityStore.clear(ownerAccountIdB64: account)
            ConversationLineageMirror.clear(accountIdB64: account)
        }
        cleanupAccounts = []
        ChatClient.deadSessionGuard = nil
        RecordingPublishURLProtocol.reset()
    }

    private func makeFixture() throws -> Fixture {
        RecordingPublishURLProtocol.reset()
        let seed = Data(repeating: 0x31, count: 32)
        let founder = try Identity.fromSeed(seed: seed)
        let oldPeer = try Identity.fromSeed(seed: Data(repeating: 0x52, count: 32))
        let newPeer = try Identity.fromSeed(seed: Data(repeating: 0x53, count: 32))
        var hexes: [String] = []
        for peer in [oldPeer, newPeer] {
            let group = try Group.create(identity: founder, kind: .oneToOne)
            _ = try group.addMemberFromWire(identity: founder, keyPackage: try peer.createKeyPackage())
            hexes.append(DsClient.pseudonymHex(try group.groupId()))
        }
        let snapshotBlob = try founder.snapshotStorage()
        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let accountIdB64 = base64Encode(accountIdBytes)
        cleanupAccounts.append(accountIdB64)
        let suite = "conversation-continuity-\(UUID().uuidString)"
        let snapshotStore = IdentitySnapshotStore()
        let knownGroupsStore = KnownGroupsStore(defaults: UserDefaults(suiteName: suite + "-k")!)
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshotBlob)
        for hex in hexes { knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: hex) }
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [RecordingPublishURLProtocol.self]
        // ONE stubbed DS for both the repository and its chat client: a
        // default `ChatClient()` carries a `DsClient` of its own pointed at
        // the build's API base, and a unit test must publish nowhere real.
        let ds = DsClient(baseURL: "http://127.0.0.1:9", session: URLSession(configuration: config))
        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            // The stubbed session too: a continuity pass asks the directory
            // for the reset identity's devices, and a unit test reaches no
            // real server (the stub's `204` reads as "no device yet").
            discovery: DiscoveryClient(
                baseURL: "http://127.0.0.1:9", session: URLSession(configuration: config)),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(
                rootDir: FileManager.default.temporaryDirectory.appendingPathComponent(suite)),
            pushRouting: PushGroupRouting(),
            beaconStore: BeaconObservationStore(defaults: UserDefaults(suiteName: suite + "-b")!),
            pendingRequestStore: PendingRequestStore(defaults: UserDefaults(suiteName: suite + "-p")!)
        )
        repo.deviceIdentityInstaller = { _, _ in nil }
        let pins = PeerPinStore(
            defaults: UserDefaults(suiteName: suite + "-pins")!, ownerAccountIdB64: accountIdB64)
        repo.readmissionPins = { pins }
        // The test host has no Keychain entitlement to rely on: the queue's
        // file format and ordering are what is under test, not the seal.
        repo.heldSendSealer = HeldSendQueue.Sealer(seal: { $0 }, open: { $0 })
        repo.bootstrap(
            secrets: IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: Data(repeating: 0x22, count: 32),
                accountIdentitySeed: seed,
                sessionToken: "t",
                sessionExpiresAtMs: 0))
        XCTAssertEqual(repo.memberCount(groupIdHex: hexes[0]), 2)
        XCTAssertEqual(repo.memberCount(groupIdHex: hexes[1]), 2)
        return Fixture(
            repo: repo, oldHex: hexes[0], successorHex: hexes[1], accountIdB64: accountIdB64,
            oldPeerKeyB64: base64Encode(oldPeer.signaturePublicKey()), pins: pins,
            peerAccountIdB64: base64Encode(Data(repeating: 0x77, count: 16)))
    }

    /// This device accepted a reset of the old one-to-one's other person.
    private func acceptReset(_ f: Fixture) {
        _ = f.pins.setPin(
            accountIdB64: f.peerAccountIdB64, pinnedSignaturePubkeyB64: f.oldPeerKeyB64,
            rotationMarker: 0, keyGeneration: 1)
        _ = f.pins.acceptReset(
            PeerPinStore.AcceptedReset(
                accountIdB64: f.peerAccountIdB64, heldKeyB64: f.oldPeerKeyB64,
                newKeyB64: base64Encode(Data(repeating: 0x09, count: 32)),
                keyGeneration: 2, signedAtMs: 1_000, nowMs: 2_000))
        XCTAssertEqual(f.pins.loadSupersededKeys()[f.oldPeerKeyB64], f.peerAccountIdB64)
    }

    /// The publishes that went to one journal. Counted per journal: the
    /// repository's own background publishers (the profile, the push routing
    /// secret) write to a live group whenever they like.
    private func publishes(into groupIdHex: String) -> Int {
        RecordingPublishURLProtocol.published.filter { $0.contains(groupIdHex) }.count
    }

    private func lineage(_ f: Fixture) -> [LineageEntry] {
        [LineageEntry(successorHex: f.successorHex, rootHex: f.oldHex, hlc: 7)]
    }

    // MARK: - R.5

    func testTwoGroupsAreTwoConversationsUntilTheLineageNamesOneTheSuccessor() throws {
        let f = try makeFixture()
        XCTAssertEqual(f.repo.state.threads.count, 2)
        f.repo.applyLineage(lineage(f))
        XCTAssertEqual(f.repo.state.threads.count, 1, "one row per conversation")
        let thread = try XCTUnwrap(f.repo.state.threads.first)
        XCTAssertEqual(thread.conversationIdHex, f.oldHex, "the conversation id never changes")
        XCTAssertEqual(thread.groupIdHex, f.successorHex, "the head is the successor")
        XCTAssertEqual(thread.historyGroupIdHexes, [f.oldHex])
        XCTAssertEqual(thread.segmentIndex, 1)
        XCTAssertEqual(f.repo.headGroupIdHex(of: f.oldHex), f.successorHex)
        XCTAssertEqual(f.repo.headGroupIdHex(of: f.successorHex), f.successorHex)
        XCTAssertEqual(f.repo.conversationIdHex(ofGroup: f.successorHex), f.oldHex)
        XCTAssertTrue(f.repo.isOlderSegment(groupIdHex: f.oldHex))
        XCTAssertFalse(f.repo.isOlderSegment(groupIdHex: f.successorHex))
    }

    func testAnOlderSegmentOfAOneToOneIsNoLongerPolled() throws {
        let f = try makeFixture()
        XCTAssertTrue(f.repo.journalPolled(groupIdHex: f.oldHex))
        f.repo.applyLineage(lineage(f))
        XCTAssertFalse(f.repo.journalPolled(groupIdHex: f.oldHex))
        XCTAssertTrue(f.repo.journalPolled(groupIdHex: f.successorHex))
    }

    func testTheLineageIsMirroredForTheNotificationExtension() throws {
        let f = try makeFixture()
        f.repo.applyLineage(lineage(f))
        guard SharedStorage.sharedDefaults() != nil else {
            throw XCTSkip("no App Group container in this test host")
        }
        XCTAssertEqual(
            ConversationLineageMirror.conversationHex(accountIdB64: f.accountIdB64, groupIdHex: f.successorHex),
            f.oldHex)
        XCTAssertEqual(
            ConversationLineageMirror.conversationHex(accountIdB64: f.accountIdB64, groupIdHex: f.oldHex),
            f.oldHex)
    }

    func testDeletingAConversationRemovesEverySegment() throws {
        let f = try makeFixture()
        f.repo.applyLineage(lineage(f))
        // The gate is the current group's: a live conversation is not deleted.
        XCTAssertFalse(f.repo.deleteChat(groupIdHex: f.oldHex))
        XCTAssertEqual(f.repo.state.threads.count, 1)
        XCTAssertTrue(f.repo.hasGroup(groupIdHex: f.oldHex))
    }

    // MARK: - R.7 case 1: the dead session

    func testAOneToOneWhoseOtherPersonResetIsADeadSessionAndTheOtherIsNot() throws {
        let f = try makeFixture()
        XCTAssertFalse(f.repo.isDeadSession(groupIdHex: f.oldHex))
        acceptReset(f)
        XCTAssertTrue(f.repo.isDeadSession(groupIdHex: f.oldHex))
        XCTAssertEqual(f.repo.unrepairableKeyB64(groupIdHex: f.oldHex), f.oldPeerKeyB64)
        XCTAssertFalse(f.repo.isDeadSession(groupIdHex: f.successorHex))
    }

    func testNothingIsPublishedIntoADeadSession() async throws {
        let f = try makeFixture()
        acceptReset(f)
        // Typing and a read receipt are not sent at all.
        try await f.repo.sendTyping(groupIdHex: f.oldHex, isTyping: true)
        try await f.repo.sendReadReceipt(groupIdHex: f.oldHex, upTo: 3)
        // Any other publisher is refused before anything is sealed.
        do {
            try await f.repo.publishApplicationMessage(
                groupIdHex: f.oldHex, plaintextBytes: Data("x".utf8))
            XCTFail("a publish into a dead session must be refused")
        } catch {
            XCTAssertTrue(error is HeldSend.HeldForSuccessor, "\(error)")
        }
        XCTAssertEqual(publishes(into: f.oldHex), 0)
        // The live one-to-one is untouched by the guard.
        let before = publishes(into: f.successorHex)
        try await f.repo.publishApplicationMessage(
            groupIdHex: f.successorHex, plaintextBytes: Data("y".utf8))
        XCTAssertGreaterThan(publishes(into: f.successorHex), before)
        XCTAssertEqual(publishes(into: f.oldHex), 0)
    }

    func testAMessageWrittenIntoADeadSessionIsHeldThenSentIntoTheSuccessor() async throws {
        let f = try makeFixture()
        acceptReset(f)
        try await f.repo.sendMessage(
            groupIdHex: f.oldHex, plaintext: "still there?",
            reply: ReplyRef(target: 41, author: "Ann", excerpt: "earlier"))
        XCTAssertEqual(publishes(into: f.oldHex), 0, "nothing went into the old session")
        let successorBefore = publishes(into: f.successorHex)
        // Shown in the ordinary sending state, where it was written.
        let held = try XCTUnwrap(
            f.repo.state.threads.first { $0.groupIdHex == f.oldHex }?.messages.last)
        XCTAssertEqual(held.text, "still there?")
        XCTAssertEqual(held.ack, .pending)

        // The successor exists and the lineage names it: the message goes out
        // there, once, and nowhere else.
        f.repo.applyLineage(lineage(f))
        await f.repo.flushHeldSends()
        XCTAssertEqual(publishes(into: f.oldHex), 0)
        XCTAssertGreaterThan(publishes(into: f.successorHex), successorBefore)
        let thread = try XCTUnwrap(f.repo.state.threads.first)
        XCTAssertEqual(thread.groupIdHex, f.successorHex)
        let sent = thread.messages.filter { $0.text == "still there?" }
        XCTAssertEqual(sent.count, 1, "drawn once")
        XCTAssertEqual(sent.first?.ack, .sent)
        XCTAssertTrue((sent.first?.messageId ?? 0) > 0)
        // The quote is kept; the jump target belonged to the old journal.
        XCTAssertEqual(sent.first?.reply?.target, HeldSend.replyNoJumpTarget)
        XCTAssertEqual(sent.first?.reply?.author, "Ann")
        XCTAssertEqual(sent.first?.reply?.excerpt, "earlier")
        // A second flush sends nothing more: the message is still drawn once.
        await f.repo.flushHeldSends()
        XCTAssertEqual(
            f.repo.state.threads.first?.messages.filter { $0.text == "still there?" }.count, 1)
        XCTAssertEqual(publishes(into: f.oldHex), 0)
    }

    func testAnEditInADeadSessionIsAppliedLocallyAndNotPublished() async throws {
        let f = try makeFixture()
        // One delivered message while the session was alive.
        try await f.repo.sendMessage(groupIdHex: f.oldHex, plaintext: "first")
        let published = publishes(into: f.oldHex)
        let id = try XCTUnwrap(
            f.repo.state.threads.first { $0.groupIdHex == f.oldHex }?.messages.last?.messageId)
        acceptReset(f)
        try await f.repo.editMessage(groupIdHex: f.oldHex, target: id, newText: "second")
        XCTAssertEqual(publishes(into: f.oldHex), published, "the edit was not sent")
        let row = f.repo.state.threads.first { $0.groupIdHex == f.oldHex }?
            .messages.first { $0.messageId == id }
        XCTAssertEqual(row?.text, "second")
    }

    // MARK: - R.5: history rows

    func testRowsOfAnOlderSegmentAreDrawnUnderHistoryIdsBeforeTheHead() async throws {
        let f = try makeFixture()
        try await f.repo.sendMessage(groupIdHex: f.oldHex, plaintext: "old one")
        try await f.repo.sendMessage(groupIdHex: f.successorHex, plaintext: "new one")
        f.repo.applyLineage(lineage(f))
        let thread = try XCTUnwrap(f.repo.state.threads.first)
        XCTAssertEqual(thread.messages.map(\.text), ["old one", "new one"])
        let history = try XCTUnwrap(thread.messages.first)
        XCTAssertTrue(LineageRules.isHistoryRowId(history.messageId))
        XCTAssertEqual(LineageRules.historyRowOrigin(history.messageId)?.distanceFromHead, 1)
        XCTAssertFalse(LineageRules.isHistoryRowId(thread.messages.last?.messageId))
        // A history row is never pinnable anew, and a pin it already had is
        // found under its on-screen id.
        let originalId = try XCTUnwrap(LineageRules.historyRowOrigin(history.messageId)?.messageId)
        f.repo.setPersonalPin(groupIdHex: f.oldHex, messageId: originalId, pinned: true)
        let spanned = f.repo.pinsAcrossSegments(headGroupIdHex: f.successorHex)
        XCTAssertNotNil(spanned.personal[history.messageId])
        let entries = PinnedBar.entries(shared: spanned.shared, personal: spanned.personal, held: thread.messages)
        XCTAssertEqual(entries.map(\.messageId), [history.messageId])
    }

    // MARK: - R.6: the receive rule

    func testAClaimFromTheSupersededIdentityIsDropped() throws {
        let f = try makeFixture()
        acceptReset(f)
        let record = try XCTUnwrap(f.repo.groups[f.oldHex])
        let peerLeaf = try XCTUnwrap(
            try record.group.members().first {
                base64Encode($0.signaturePublicKey) == f.oldPeerKeyB64
            }?.leafIndex)
        f.repo.receiveGroupClosed(
            groupIdHex: f.oldHex, messageId: 9,
            successorGroupId: Data(repeating: 0xAB, count: 32), senderLeaf: peerLeaf)
        let state = f.repo.groupClosedStore.read(groupIdB64: base64Encode(record.groupId))
        XCTAssertNil(state?.accepted)
        XCTAssertEqual(state?.pending ?? [], [])
    }

    func testAClaimWithNoAcceptedResetWaitsAndOneFromThisAccountIsAcceptedAfterIt() throws {
        let f = try makeFixture()
        let record = try XCTUnwrap(f.repo.groups[f.oldHex])
        let ownLeaf = try record.group.ownLeafIndex()
        let oldB64 = base64Encode(record.groupId)
        let successor = Data(repeating: 0xCD, count: 32)
        // No reset accepted here yet: the claim is kept, unjudged.
        f.repo.receiveGroupClosed(
            groupIdHex: f.oldHex, messageId: 12, successorGroupId: successor, senderLeaf: ownLeaf)
        XCTAssertNil(f.repo.groupClosedStore.read(groupIdB64: oldB64)?.accepted)
        XCTAssertEqual(f.repo.groupClosedStore.read(groupIdB64: oldB64)?.pending.count, 1)
        // The reset is accepted: the kept claim, from the surviving account,
        // becomes the conversation's successor.
        acceptReset(f)
        f.repo.rejudgeClaims(groupIdHex: f.oldHex)
        let state = try XCTUnwrap(f.repo.groupClosedStore.read(groupIdB64: oldB64))
        XCTAssertEqual(state.accepted?.messageId, 12)
        XCTAssertEqual(state.accepted?.successorGroupIdB64, base64Encode(successor))
        XCTAssertEqual(state.pending, [])
        // A later claim never replaces it.
        f.repo.receiveGroupClosed(
            groupIdHex: f.oldHex, messageId: 20,
            successorGroupId: Data(repeating: 0xEE, count: 32), senderLeaf: ownLeaf)
        XCTAssertEqual(f.repo.groupClosedStore.read(groupIdB64: oldB64)?.accepted?.messageId, 12)
    }

    // MARK: - Calls

    func testACallInADeadSessionDialsTheSuccessorWhenThereIsOne() throws {
        let f = try makeFixture()
        XCTAssertEqual(f.repo.oneToOneCallTarget(groupIdHex: f.oldHex), .dial(groupIdB64: f.oldHex))
        acceptReset(f)
        XCTAssertEqual(f.repo.oneToOneCallTarget(groupIdHex: f.oldHex), .unavailable)
        f.repo.applyLineage(lineage(f))
        XCTAssertEqual(
            f.repo.oneToOneCallTarget(groupIdHex: f.oldHex), .dial(groupIdB64: f.successorHex))
    }
}
