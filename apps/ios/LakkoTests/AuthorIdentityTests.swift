// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// MARK: - Scripted DS

/// One in-memory group journal behind `URLProtocol`: `POST` appends, `GET`
/// serves everything past the `after` cursor. Its own class rather than a reuse
/// of `ScriptedDsURLProtocol` because that one carries process-global state
/// another suite already owns.
final class AuthorIdentityDsURLProtocol: URLProtocol {

    private static let lock = NSLock()
    private static var rows: [(id: Int64, body: Data)] = []
    private static var nextId: Int64 = 1

    static func reset() {
        lock.lock(); defer { lock.unlock() }
        rows.removeAll()
        nextId = 1
    }

    /// Append a row as if a peer had published it. Returns the assigned id —
    /// which is also the DS `messageId` an edit / delete / reaction op targets.
    @discardableResult
    static func seed(_ body: Data) -> Int64 {
        lock.lock(); defer { lock.unlock() }
        let id = nextId
        nextId += 1
        rows.append((id: id, body: body))
        return id
    }

    private static func after(_ cursor: Int64) -> [(id: Int64, body: Data)] {
        lock.lock(); defer { lock.unlock() }
        return rows.filter { $0.id > cursor }
    }

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url else {
            client?.urlProtocol(self, didFailWithError: URLError(.badURL))
            return
        }
        let body: Data
        if request.httpMethod == "POST" {
            let id = Self.seed(Self.bodyData(of: request))
            body = Data(#"{"id":\#(id),"created_at":"1970-01-01T00:00:00Z"}"#.utf8)
        } else {
            let rows = Self.after(Self.cursor(from: url)).map { row -> String in
                let b64 = row.body.base64EncodedString()
                return #"{"id":\#(row.id),"payload_b64":"\#(b64)","created_at":"1970-01-01T00:00:00Z"}"#
            }
            body = Data(#"{"messages":[\#(rows.joined(separator: ","))],"has_more":false}"#.utf8)
        }
        let response = HTTPURLResponse(
            url: url,
            statusCode: 200,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"]
        )!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}

    private static func bodyData(of request: URLRequest) -> Data {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { return Data() }
        stream.open()
        defer { stream.close() }
        var out = Data()
        let size = 8192
        var buffer = [UInt8](repeating: 0, count: size)
        while stream.hasBytesAvailable {
            let read = stream.read(&buffer, maxLength: size)
            if read <= 0 { break }
            out.append(contentsOf: buffer[0..<read])
        }
        return out
    }

    private static func cursor(from url: URL) -> Int64 {
        guard let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems,
              let raw = items.first(where: { $0.name == "after" })?.value,
              let value = Int64(raw) else { return 0 }
        return value
    }
}

// MARK: - Tests

/// The leaf-reuse impersonation, driven through the REAL receive path.
///
/// ADR-0023's author-only edit / delete gate, ADR-0023's reaction attribution
/// and ADR-0024's read watermark all identified a member by their **MLS leaf
/// index**. A leaf is a position in the ratchet tree, not a person: OpenMLS
/// hands a vacated leaf to the next member added, and ADR-0045 composes a
/// remove and an add into ONE commit, so `remove Bob, add Dave` puts Dave on
/// Bob's index with no intermediate state any client can observe — the whole
/// sequence arrives in a single poll batch.
///
/// Every rule that compared the raw leaf then read Dave as Bob: Dave could edit
/// and delete Bob's history, toggle Bob's reactions off, and inherit Bob's read
/// watermark. On iOS the edit / delete half was **permanent**, because the
/// author leaf was persisted in the message log and reloaded verbatim.
///
/// Nothing here is simulated. A real owner drives real MLS commits, a real
/// scripted DS orders the journal, and the device under test is a real
/// `ChatRepository` polling that journal exactly as the app does. The fixture
/// asserts the leaf reuse it depends on, so a future OpenMLS that stopped
/// reusing leaves would fail loudly rather than let these pass vacuously.
@MainActor
final class AuthorIdentityTests: XCTestCase {

    // MARK: - Fixture

    private struct Fixture {
        let repo: ChatRepository
        let groupIdHex: String
        let accountIdB64: String
        let snapshotStore: IdentitySnapshotStore
        let outboxDir: URL
        /// DS id of Bob's first message — the edit / reaction target.
        let bobFirstMessageId: Int64
        /// DS id of Bob's second message — the delete target.
        let bobSecondMessageId: Int64
        /// The leaf Bob held and Dave inherited. The hazard, named.
        let reusedLeaf: UInt32
        /// Every status event the repository surfaced during the batch, in
        /// journal order. Bob's pair, then Dave's identical-looking pair.
        let statusEvents: [ChatStatusEvent]
    }

    /// Owner O (drives every commit) + the observing device (a plain member,
    /// so the poll's tail sweep short-circuits and nothing but the receive path
    /// is under test) + Bob (writes, then removed) + Dave (added into Bob's
    /// vacated leaf).
    ///
    /// The whole journal is seeded BEFORE the single poll, so the removal, the
    /// add and Dave's ops all land in ONE batch — the ADR-0045 shape.
    private func makeFixture() async throws -> Fixture {
        AuthorIdentityDsURLProtocol.reset()

        let observerSeed = Data(repeating: 0x55, count: 32)
        let owner = try Identity.fromSeed(seed: Data(repeating: 0x11, count: 32))
        let observer = try Identity.fromSeed(seed: observerSeed)
        let bob = try Identity.fromSeed(seed: Data(repeating: 0x77, count: 32))
        let dave = try Identity.fromSeed(seed: Data(repeating: 0x99, count: 32))

        // Owner founds the group (`Group.create` seeds the ADR-0022 role
        // table), then adds the observer and Bob. Those two commits are applied
        // directly rather than journalled: the batch under test must contain
        // only the rows the impersonation needs.
        let ownerGroup = try Group.create(identity: owner)
        let addObserver = try ownerGroup.addMemberFromWire(
            identity: owner, keyPackage: try observer.createKeyPackage())
        let observerGroup = try Group.joinFromWelcome(
            welcome: addObserver.welcome, identity: observer)
        let addBob = try ownerGroup.addMemberFromWire(
            identity: owner, keyPackage: try bob.createKeyPackage())
        let bobGroup = try Group.joinFromWelcome(welcome: addBob.welcome, identity: bob)
        _ = try observerGroup.processIncomingVerifyingRoles(
            identity: observer, bytes: addBob.commit)

        let bobKey = try bob.signaturePublicKey()
        let bobLeaf = try XCTUnwrap(
            try ownerGroup.members().first { $0.signaturePublicKey == bobKey }?.leafIndex,
            "Bob must be in the roster or there is no leaf to vacate")
        XCTAssertEqual(
            try observerGroup.epoch(), try ownerGroup.epoch(),
            "the observer must start at the owner's epoch, or every seeded row below "
            + "would be dropped as wrong-epoch and the suite would pass vacuously")

        // ---- Rows Bob authors, while Bob is still a member ----
        let bobFirst = AuthorIdentityDsURLProtocol.seed(
            try bobGroup.createApplicationMessage(
                identity: bob,
                plaintext: try AttachmentBridge.composeEnvelopeBytes(
                    text: "written by Bob", attachments: [])))
        let bobSecond = AuthorIdentityDsURLProtocol.seed(
            try bobGroup.createApplicationMessage(
                identity: bob,
                plaintext: try AttachmentBridge.composeEnvelopeBytes(
                    text: "also written by Bob", attachments: [])))
        // Bob reacts to his own first message — the reaction Dave must not be
        // able to destroy.
        AuthorIdentityDsURLProtocol.seed(
            try bobGroup.createApplicationMessage(
                identity: bob,
                plaintext: try AttachmentBridge.composeReactionBytes(
                    target: bobFirst, emoji: "👍")))
        // Bob's presence signals. Deliberately the SAME values Dave sends
        // below, so under a leaf key the two are byte-identical events.
        AuthorIdentityDsURLProtocol.seed(
            try bobGroup.createApplicationMessage(
                identity: bob,
                plaintext: try composeReadReceiptEnvelope(upTo: bobSecond)))
        AuthorIdentityDsURLProtocol.seed(
            try bobGroup.createApplicationMessage(
                identity: bob, plaintext: try composeTypingEnvelope(isTyping: true)))

        // ---- One commit: remove Bob, add Dave ----
        //
        // Through ADR-0045 proposals so the shape is the real one. The two
        // proposal rows have to be on the journal too: by-reference resolution
        // runs at commit receipt on EVERY member, so a peer that never stored
        // them would refuse the commit outright.
        AuthorIdentityDsURLProtocol.seed(
            try ownerGroup.proposeRemoveMember(identity: owner, leafIndex: bobLeaf))
        AuthorIdentityDsURLProtocol.seed(
            try ownerGroup.proposeAddMemberFromWire(
                identity: owner, keyPackage: try dave.createKeyPackage()))
        let swept = try XCTUnwrap(
            try ownerGroup.commitPendingProposals(identity: owner),
            "the sweep must produce a commit carrying both proposals")
        AuthorIdentityDsURLProtocol.seed(swept.commit)
        let daveGroup = try Group.joinFromWelcome(
            welcome: try XCTUnwrap(swept.welcome, "the sweep carried an Add, so it must "
                                   + "carry a Welcome"),
            identity: dave)

        // THE HAZARD, ASSERTED. Everything below is only meaningful because
        // Dave now holds the index Bob vacated.
        XCTAssertEqual(
            try daveGroup.ownLeafIndex(), bobLeaf,
            "the fixture depends on OpenMLS handing the vacated leaf to the next Add; "
            + "if that ever stops being true this whole suite is vacuous and must be "
            + "rebuilt around whatever the new assignment is")

        // ---- Rows Dave authors, from Bob's old leaf ----
        AuthorIdentityDsURLProtocol.seed(
            try daveGroup.createApplicationMessage(
                identity: dave,
                plaintext: try AttachmentBridge.composeEditBytes(
                    target: bobFirst, newText: "REWRITTEN BY DAVE")))
        AuthorIdentityDsURLProtocol.seed(
            try daveGroup.createApplicationMessage(
                identity: dave,
                plaintext: try AttachmentBridge.composeDeleteBytes(target: bobSecond)))
        AuthorIdentityDsURLProtocol.seed(
            try daveGroup.createApplicationMessage(
                identity: dave,
                plaintext: try AttachmentBridge.composeReactionBytes(
                    target: bobFirst, emoji: "👍")))
        AuthorIdentityDsURLProtocol.seed(
            try daveGroup.createApplicationMessage(
                identity: dave,
                plaintext: try composeReadReceiptEnvelope(upTo: bobSecond)))
        AuthorIdentityDsURLProtocol.seed(
            try daveGroup.createApplicationMessage(
                identity: dave, plaintext: try composeTypingEnvelope(isTyping: true)))

        // ---- Rehydrate the observer into a real repository ----
        let groupId = try observerGroup.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)
        let snapshotBlob = try observer.snapshotStorage()
        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let accountIdB64 = base64Encode(accountIdBytes)

        let snapshotStore = IdentitySnapshotStore()
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshotBlob)
        let knownGroupsStore = KnownGroupsStore(
            defaults: UserDefaults(suiteName: "author-identity-k-\(UUID().uuidString)")!)
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("author-identity-\(UUID().uuidString)")

        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [AuthorIdentityDsURLProtocol.self]
        let ds = DsClient(
            baseURL: "http://127.0.0.1:9", session: URLSession(configuration: config))
        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            discovery: DiscoveryClient(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: outboxDir),
            pushRouting: PushGroupRouting(),
            beaconStore: BeaconObservationStore(
                defaults: UserDefaults(suiteName: "author-identity-b-\(UUID().uuidString)")!),
            pendingRequestStore: PendingRequestStore(
                defaults: UserDefaults(suiteName: "author-identity-p-\(UUID().uuidString)")!)
        )
        var captured: [ChatStatusEvent] = []
        repo.chatStatusSink = { _, status in captured.append(status) }
        // Phase 1 fixture: the chats run on the account identity the snapshot
        // was taken under (ADR-0050 device identities are exercised elsewhere).
        repo.deviceIdentityInstaller = { _, _ in nil }
        repo.bootstrap(
            secrets: IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: Data(repeating: 0x22, count: 32),
                accountIdentitySeed: observerSeed,
                sessionToken: "t",
                sessionExpiresAtMs: 0
            )
        )
        XCTAssertNil(
            repo.ownCommitterRank(groupIdHex: groupIdHex),
            "the device under test must be a PLAIN member, so the poll's tail sweep "
            + "short-circuits and only the receive path is exercised")

        // ONE poll. Every row above — Bob's messages, the removal, the add, and
        // Dave's ops from Bob's old leaf — arrives in a single batch.
        try await repo.pollGroup(groupIdHex: groupIdHex)

        return Fixture(
            repo: repo,
            groupIdHex: groupIdHex,
            accountIdB64: accountIdB64,
            snapshotStore: snapshotStore,
            outboxDir: outboxDir,
            bobFirstMessageId: bobFirst,
            bobSecondMessageId: bobSecond,
            reusedLeaf: bobLeaf,
            statusEvents: captured
        )
    }

    private func tearDown(_ fx: Fixture) {
        fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64)
        try? FileManager.default.removeItem(at: fx.outboxDir)
        AuthorIdentityDsURLProtocol.reset()
    }

    private func message(_ fx: Fixture, _ id: Int64) throws -> ChatMessage {
        let thread = try XCTUnwrap(
            fx.repo.state.threads.first { $0.groupIdHex == fx.groupIdHex },
            "the group must have a thread, or nothing was applied at all")
        return try XCTUnwrap(
            thread.messages.first { $0.messageId == id },
            "message \(id) must be on the timeline — got ids "
            + "\(thread.messages.map(\.messageId))")
    }

    // MARK: - The headline: edit and delete

    func testANewcomerAtAVacatedLeafCannotEditTheDepartedMembersMessage() async throws {
        let fx = try await makeFixture()
        defer { tearDown(fx) }

        let target = try message(fx, fx.bobFirstMessageId)
        XCTAssertEqual(
            target.text, "written by Bob",
            "Dave was added into leaf \(fx.reusedLeaf), which Bob vacated in the same "
            + "commit; his edit of Bob's message must be REFUSED, not applied")
        XCTAssertFalse(
            target.edited,
            "an edit that was refused must not leave the `(edited)` marker either")
    }

    func testANewcomerAtAVacatedLeafCannotDeleteTheDepartedMembersMessage() async throws {
        let fx = try await makeFixture()
        defer { tearDown(fx) }

        let target = try message(fx, fx.bobSecondMessageId)
        XCTAssertFalse(
            target.deleted,
            "Dave was added into leaf \(fx.reusedLeaf), which Bob vacated in the same "
            + "commit; his delete of Bob's message must be REFUSED, not applied")
    }

    // MARK: - Sibling 1 — reactions

    func testANewcomerAtAVacatedLeafDoesNotDestroyTheDepartedMembersReaction() async throws {
        let fx = try await makeFixture()
        defer { tearDown(fx) }

        let target = try message(fx, fx.bobFirstMessageId)
        // Bob reacted 👍; Dave, on Bob's old leaf, reacted 👍. Two different
        // people, so two reactions — not one toggled off, and not a merge.
        XCTAssertEqual(
            target.reactions.count, 2,
            "Bob and Dave are two people who each reacted once; under a leaf key Dave's "
            + "reaction toggled Bob's off instead, and because the last reactor's "
            + "departure prunes the set the chip DISAPPEARED entirely")
        XCTAssertEqual(
            target.reactionSummary(own: nil).first?.count, 2,
            "the chip must count both reactors")
    }

    // MARK: - Sibling 2 — read watermarks

    func testTheDepartedMemberAndTheNewcomerAreNotTheSameReader() async throws {
        let fx = try await makeFixture()
        defer { tearDown(fx) }

        let receipts = fx.statusEvents.filter {
            if case .readReceipt = $0 { return true }
            return false
        }
        XCTAssertEqual(receipts.count, 2, "Bob reported one read, Dave reported one read")
        XCTAssertNotEqual(
            receipts[0], receipts[1],
            "Bob and Dave reported the SAME watermark from the SAME leaf, so under a leaf "
            + "key these two events are indistinguishable and Dave silently inherits "
            + "Bob's entire read history")
    }

    // MARK: - Sibling 3 — typing

    func testTheDepartedMemberAndTheNewcomerAreNotTheSameTyper() async throws {
        let fx = try await makeFixture()
        defer { tearDown(fx) }

        let typings = fx.statusEvents.filter {
            if case .typing = $0 { return true }
            return false
        }
        XCTAssertEqual(typings.count, 2, "Bob typed, then Dave typed")
        XCTAssertNotEqual(
            typings[0], typings[1],
            "same leaf, same flag, same DS timestamp — under a leaf key Dave's typing "
            + "signal is Bob's, and one member's stop clears the other's state")
    }
}

// MARK: - The rules themselves, in isolation

/// The pure rules the end-to-end cases above exercise through the real receive
/// path, pinned directly so a regression names the rule that broke rather than
/// only the symptom.
///
/// The roster pair below is the whole hazard in two constants: one leaf index,
/// two different people, either side of one commit.
final class AuthorIdentityRuleTests: XCTestCase {

    /// Distinct 32-byte signature keys, byte-filled so each is obvious in a
    /// failure message.
    private static func key(_ fill: UInt8) -> Data { Data(repeating: fill, count: 32) }

    private static let aliceKey = key(0xA1)
    private static let bobKey = key(0xB0)
    private static let daveKey = key(0xDA)

    /// Epoch N: Alice at leaf 0, Bob at leaf 2.
    private static let rosterBefore = [
        RosterEntry(leafIndex: 0, signaturePublicKey: aliceKey),
        RosterEntry(leafIndex: 2, signaturePublicKey: bobKey),
    ]

    /// Epoch N+1, after one commit removing Bob and adding Dave. OpenMLS hands
    /// the vacated leaf to the next Add, so Dave is at leaf 2 — the index Bob
    /// held. This is the whole hazard.
    private static let rosterAfter = [
        RosterEntry(leafIndex: 0, signaturePublicKey: aliceKey),
        RosterEntry(leafIndex: 2, signaturePublicKey: daveKey),
    ]

    /// The leaf Bob held and Dave inherited.
    private static let reusedLeaf: UInt32 = 2

    private var alice: MemberIdentity {
        AuthorIdentity.resolve(roster: Self.rosterBefore, leafIndex: 0) ?? ""
    }
    private var bob: MemberIdentity {
        AuthorIdentity.resolve(roster: Self.rosterBefore, leafIndex: Self.reusedLeaf) ?? ""
    }
    private var dave: MemberIdentity {
        AuthorIdentity.resolve(roster: Self.rosterAfter, leafIndex: Self.reusedLeaf) ?? ""
    }

    // MARK: resolution

    func testOneLeafResolvesToDifferentMembersAcrossTheCommit() {
        // The premise of everything below, stated once.
        XCTAssertNotEqual(bob, dave, "the same leaf index names two different people")
        XCTAssertEqual(
            AuthorIdentity.resolve(roster: Self.rosterBefore, leafIndex: Self.reusedLeaf), bob)
        XCTAssertEqual(
            AuthorIdentity.resolve(roster: Self.rosterAfter, leafIndex: Self.reusedLeaf), dave)
    }

    func testAnUnknownLeafResolvesToNothing() {
        XCTAssertNil(AuthorIdentity.resolve(roster: Self.rosterBefore, leafIndex: 7))
    }

    func testAnUnreadableRosterResolvesEveryLeafToNothing() {
        // What `AuthorIdentity.roster(of:)` yields when `group.members()`
        // throws — the fail-closed direction, since an unattributable message
        // can never be edited or deleted by anyone.
        XCTAssertNil(AuthorIdentity.resolve(roster: [], leafIndex: Self.reusedLeaf))
    }

    func testLeafZeroIsARealLeafNotAMiss() {
        XCTAssertNotNil(AuthorIdentity.resolve(roster: Self.rosterBefore, leafIndex: 0))
    }

    // MARK: the gate

    func testTheGateRefusesANewcomerAtAVacatedLeaf() {
        // Bob authored at epoch N; the receive loop resolved his leaf THERE.
        let author = AuthorIdentity.resolve(
            roster: Self.rosterBefore, leafIndex: Self.reusedLeaf)
        // One commit later Dave holds leaf 2 and sends an op. His op's leaf is
        // 2 — indistinguishable from Bob's, which is precisely why the leaf
        // cannot be the thing compared.
        let opSender = AuthorIdentity.resolve(
            roster: Self.rosterAfter, leafIndex: Self.reusedLeaf)
        XCTAssertFalse(
            AuthorIdentity.editDeleteAuthorized(author: author, opSender: opSender))
    }

    func testTheGateStillLetsTheRealAuthorEditTheirOwnMessage() {
        // The refusal must not be "refuse everything".
        let author = AuthorIdentity.resolve(
            roster: Self.rosterBefore, leafIndex: Self.reusedLeaf)
        XCTAssertTrue(
            AuthorIdentity.editDeleteAuthorized(author: author, opSender: bob))
    }

    func testTheGateStillRefusesAPlainNonAuthor() {
        XCTAssertFalse(AuthorIdentity.editDeleteAuthorized(author: bob, opSender: alice))
    }

    func testTheGateFailsClosedOnAnUnknownAuthorOrSender() {
        XCTAssertFalse(AuthorIdentity.editDeleteAuthorized(author: nil, opSender: dave))
        XCTAssertFalse(AuthorIdentity.editDeleteAuthorized(author: bob, opSender: nil))
        // BOTH unknown is the one a bare `author == opSender` on optionals gets
        // wrong: two unattributable things are not the same person, and reading
        // them as one would let any unresolvable op rewrite any unresolvable
        // message.
        XCTAssertFalse(AuthorIdentity.editDeleteAuthorized(author: nil, opSender: nil))
    }

    func testResolvingTheAuthorLATEWouldAuthorizeTheImpersonation() {
        // THE CONTRAST, kept permanently. The wrong shape: store the author's
        // LEAF and resolve it when the op arrives, against whatever roster is
        // current then. Both sides then resolve to Dave and the gate says yes.
        //
        // The rule that stops the impersonation is therefore not the key
        // comparison on its own — it is resolving the author's leaf at RECEIPT
        // and carrying the key. Delete either half and it is back.
        let resolvedLate = AuthorIdentity.resolve(
            roster: Self.rosterAfter, leafIndex: Self.reusedLeaf)
        let opSender = AuthorIdentity.resolve(
            roster: Self.rosterAfter, leafIndex: Self.reusedLeaf)
        XCTAssertTrue(
            AuthorIdentity.editDeleteAuthorized(author: resolvedLate, opSender: opSender),
            "resolved late, Dave passes as Bob — this is the defect, pinned")
        // ...whereas the identity resolved at receipt refuses the same op.
        let resolvedAtReceipt = AuthorIdentity.resolve(
            roster: Self.rosterBefore, leafIndex: Self.reusedLeaf)
        XCTAssertFalse(
            AuthorIdentity.editDeleteAuthorized(
                author: resolvedAtReceipt, opSender: opSender))
    }

    // MARK: the own-reaction sentinel

    func testTheOwnReactionSentinelCanNeverCollideWithARealIdentity() {
        // A real identity is base64; `?` is outside that alphabet. This is the
        // property the old leaf value space had against a real leaf.
        for id in [alice, bob, dave] {
            XCTAssertNotEqual(id, AuthorIdentity.ownReactionSentinel)
        }
        XCTAssertTrue(AuthorIdentity.ownReactionSentinel.contains("?"))
    }

    func testAnUnresolvedOwnReactionStillHighlightsAsMine() {
        // The sentinel's whole job: with no resolvable own key the chip must
        // still read as ours, exactly as it did under the old leaf fallback.
        let message = ChatMessage(
            messageId: 1,
            direction: .incoming,
            text: "t",
            createdAt: "1970-01-01T00:00:00Z",
            reactions: [Reaction(reactor: AuthorIdentity.ownReactionSentinel, emoji: "👍")])
        XCTAssertEqual(message.reactionSummary(own: nil).first?.reactedByMe, true)
        // ...and a real member's reaction never does.
        let theirs = ChatMessage(
            messageId: 1,
            direction: .incoming,
            text: "t",
            createdAt: "1970-01-01T00:00:00Z",
            reactions: [Reaction(reactor: bob, emoji: "👍")])
        XCTAssertEqual(theirs.reactionSummary(own: nil).first?.reactedByMe, false)
        XCTAssertEqual(theirs.reactionSummary(own: dave).first?.reactedByMe, false)
        XCTAssertEqual(theirs.reactionSummary(own: bob).first?.reactedByMe, true)
    }
}

// MARK: - ADR-0024 presence folds

/// The two presence registries, keyed on identity. Both used to be keyed on the
/// MLS leaf; the watermark's mis-binding was permanent for the session and the
/// typing one self-healed in ~6 s, but they are the same defect and are fixed
/// the same way.
final class PresenceOpsIdentityTests: XCTestCase {

    private let alice: MemberIdentity = "AAAA"
    private let bob: MemberIdentity = "BBBB"
    /// The newcomer at Bob's vacated leaf. A different person; under a leaf key
    /// it was the same map entry.
    private let dave: MemberIdentity = "DDDD"

    func testANewcomerDoesNotInheritTheDepartedMembersWatermark() {
        // Bob read up to message 50, then was removed.
        let m = PresenceOps.foldReadWatermark([:], sender: bob, upTo: 50)
        // Dave arrives having read nothing.
        XCTAssertNil(m[dave], "Dave has reported no read at all")
        XCTAssertEqual(m[bob], 50)
    }

    func testAMessageIsNotReportedAsReadBySomeoneWhoNeverSawIt() {
        // Alice's view. Bob read to 50, then was removed; Dave was added into
        // his slot and has reported NOTHING. The roster is now Alice + Dave.
        //
        // This is the exact shape the leaf key got wrong: the departed
        // member's watermark sat under the leaf Dave now holds, so the check
        // read Dave as having read to 50 and flipped the message to "Read".
        // The answer must come from the CURRENT roster, and a member with no
        // watermark of their own must hold it at "Sent".
        var m = PresenceOps.foldReadWatermark([:], sender: bob, upTo: 50)
        XCTAssertFalse(
            PresenceOps.isReadByEveryOther(
                watermarks: m, messageId: 50, roster: [alice, dave], own: alice),
            "Dave has reported no read at all — message 50 must stay Sent")
        // Still Sent once Dave reports a LOWER watermark of his own.
        m = PresenceOps.foldReadWatermark(m, sender: dave, upTo: 10)
        XCTAssertFalse(
            PresenceOps.isReadByEveryOther(
                watermarks: m, messageId: 50, roster: [alice, dave], own: alice),
            "message 50 must stay Sent until Dave himself has read it")
        // It flips only once Dave genuinely catches up.
        m = PresenceOps.foldReadWatermark(m, sender: dave, upTo: 50)
        XCTAssertTrue(
            PresenceOps.isReadByEveryOther(
                watermarks: m, messageId: 50, roster: [alice, dave], own: alice))
    }

    func testTheWatermarkStaysMonotonePerMemberAcrossTheMembershipChange() {
        var m = PresenceOps.foldReadWatermark([:], sender: bob, upTo: 50)
        // Dave's first receipt is LOWER than Bob's — under a leaf key it would
        // be swallowed as a non-advance and Dave would keep Bob's 50.
        m = PresenceOps.foldReadWatermark(m, sender: dave, upTo: 10)
        XCTAssertEqual(m[bob], 50)
        XCTAssertEqual(m[dave], 10)
    }

    func testAReplayedOlderReceiptIsIdempotent() {
        var m = PresenceOps.foldReadWatermark([:], sender: bob, upTo: 50)
        m = PresenceOps.foldReadWatermark(m, sender: bob, upTo: 10)
        XCTAssertEqual(m[bob], 50, "a re-synced older row must not lower the watermark")
    }

    func testANewcomerDoesNotInheritTheDepartedMembersTypingState() {
        let t0 = Date()
        let m = PresenceOps.foldTyping([:], sender: bob, isTyping: true, rowDate: t0, now: t0)
        XCTAssertEqual(PresenceOps.activeTypers(m, now: t0.addingTimeInterval(1)), [bob])
        XCTAssertNil(m[dave])
    }

    func testANewcomersStopDoesNotClearTheDepartedMembersEntry() {
        let t0 = Date()
        var m = PresenceOps.foldTyping([:], sender: bob, isTyping: true, rowDate: t0, now: t0)
        m = PresenceOps.foldTyping(m, sender: dave, isTyping: false, rowDate: t0, now: t0)
        XCTAssertNotNil(m[bob], "Dave's stop is Dave's, not Bob's")
    }

    func testAReSyncedStaleTypingSignalIsDropped() {
        // The ADR-0024 replay guard, preserved verbatim through the extraction.
        let now = Date()
        let old = now.addingTimeInterval(-(PresenceOps.typingExpiry + 1))
        let m = PresenceOps.foldTyping([:], sender: bob, isTyping: true, rowDate: old, now: now)
        XCTAssertTrue(m.isEmpty)
    }

    func testAnUnparseableRowTimestampKeepsTheSignal() {
        // Fail-open on the freshness check only: the receive-time expiry sweep
        // still bounds display. Preserved from the pre-extraction behaviour.
        let now = Date()
        let m = PresenceOps.foldTyping([:], sender: bob, isTyping: true, rowDate: nil, now: now)
        XCTAssertNotNil(m[bob])
    }
}

// MARK: - Persistence

/// What the message log stores, and what a row written before it stored an
/// identity does on reload.
final class AuthorIdentityPersistenceTests: XCTestCase {

    private let bob: MemberIdentity = "Ym9i"

    private func persisted(_ json: String) throws -> PersistedMessage {
        try JSONDecoder().decode(PersistedMessage.self, from: Data(json.utf8))
    }

    func testTheAuthorIdentityRoundTripsSoEditDeleteSurvivesAReload() throws {
        let live = ChatMessage(
            messageId: 7,
            direction: .incoming,
            text: "hello",
            createdAt: "1970-01-01T00:00:00Z",
            authorKeyB64: bob,
            reactions: [Reaction(reactor: bob, emoji: "👍")])
        let blob = try JSONEncoder().encode(PersistedMessage(from: live))
        let restored = try XCTUnwrap(
            JSONDecoder().decode(PersistedMessage.self, from: blob).toChatMessage())

        XCTAssertEqual(restored.authorKeyB64, bob)
        XCTAssertEqual(restored.reactions, [Reaction(reactor: bob, emoji: "👍")])
        // Which is the point of persisting it at all: before this, every
        // reloaded row had an unknown author and the gate refused Bob's own
        // edit of his own message.
        XCTAssertTrue(
            AuthorIdentity.editDeleteAuthorized(
                author: restored.authorKeyB64, opSender: bob))
    }

    func testAPreExistingRowFailsTheGateClosed() throws {
        // A row written before the field existed: the key is simply not there.
        // Not a migration, not a wipe — it reads as an unknown author, exactly
        // as every reloaded row did before.
        let row = try persisted(#"""
        {"messageId":7,"direction":"incoming","text":"hello","attachments":[],
         "createdAt":"1970-01-01T00:00:00Z"}
        """#)
        XCTAssertNil(row.authorKeyB64)
        XCTAssertFalse(
            AuthorIdentity.editDeleteAuthorized(author: row.authorKeyB64, opSender: bob))
        XCTAssertNotNil(row.toChatMessage(), "the message itself must still load")
    }

    func testAPreExistingRowsStoredLEAFIsNotResurrectedAsAnAuthor() throws {
        // The old shape, verbatim: `authorLeaf` plus leaf-keyed reactions. The
        // leaf must not come back in any form — it would name whoever inherited
        // it. The row still loads; it simply has no attributable author, and
        // its unattributable reactions are dropped rather than misattributed.
        let row = try persisted(#"""
        {"messageId":7,"direction":"incoming","text":"hello","attachments":[],
         "createdAt":"1970-01-01T00:00:00Z","authorLeaf":2,
         "reactions":[{"reactorLeaf":2,"emoji":"\#u{1F44D}"}]}
        """#)
        XCTAssertNil(row.authorKeyB64)
        XCTAssertTrue(row.reactions.isEmpty)
        let restored = try XCTUnwrap(
            row.toChatMessage(), "a legacy row must still load rather than cost the transcript")
        XCTAssertNil(restored.authorKeyB64)
        XCTAssertTrue(restored.reactions.isEmpty)
    }

    func testAnUnresolvedAuthorStoresAsNothingNotAsAKey() throws {
        let live = ChatMessage(
            messageId: 7,
            direction: .outgoing,
            text: "hello",
            createdAt: "1970-01-01T00:00:00Z",
            authorKeyB64: nil)
        let blob = try JSONEncoder().encode(PersistedMessage(from: live))
        let restored = try XCTUnwrap(
            JSONDecoder().decode(PersistedMessage.self, from: blob).toChatMessage())
        // The tempting shortcut for an OUTGOING row — "it is ours, so assume
        // our current key" — would invent an author the store never recorded.
        XCTAssertNil(restored.authorKeyB64)
        XCTAssertFalse(
            AuthorIdentity.editDeleteAuthorized(
                author: restored.authorKeyB64, opSender: bob))
    }

    func testTheStoredValueIsNeverALeafIndex() throws {
        let blob = try JSONEncoder().encode(
            PersistedMessage(
                messageId: 7, direction: "incoming", text: "t", attachments: [],
                createdAt: "1970-01-01T00:00:00Z", authorKeyB64: bob,
                reactions: [PersistedReaction(reactorKeyB64: bob, emoji: "👍")]))
        let json = try XCTUnwrap(String(data: blob, encoding: .utf8))
        // A leaf would round-trip cleanly and then name whoever inherited it.
        XCTAssertFalse(json.contains("authorLeaf"))
        XCTAssertFalse(json.contains("reactorLeaf"))
        XCTAssertTrue(json.contains("authorKeyB64"))
        XCTAssertTrue(json.contains("reactorKeyB64"))
    }
}
