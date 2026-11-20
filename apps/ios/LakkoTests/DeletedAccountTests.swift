// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import LakkoCore
import XCTest

@testable import Lakko

// ADR-0098 (`docs/ux/deleted-account.md`) — a deleted account is shown as
// `Deleted account`, learned from a verified statement (final) or the
// directory (reversible), and its username is shown only while recently
// confirmed. The core rules run through the real FFI.
@MainActor
final class DeletedAccountTests: XCTestCase {

    private var suite = ""
    private var defaults: UserDefaults!
    private var now: UInt64 = 1_700_000_000_000
    private let own = "own-account"
    private let day: UInt64 = 24 * 60 * 60 * 1000

    override func setUp() async throws {
        suite = "lakko.tests.deleted-accounts.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() async throws {
        defaults.removePersistentDomain(forName: suite)
    }

    private func makeDeletions() -> DeletedAccounts {
        let deletions = DeletedAccounts(store: DeletedAccountStore(defaults: defaults))
        deletions.nowMs = { [unowned self] in self.now }
        deletions.heldKeysForAccountId = { _ in [] }
        deletions.activate(accountIdB64: own)
        return deletions
    }

    /// An account identity as production holds it: its key is what peers
    /// resolve the sender to, its Ed25519 seed what the statement is signed
    /// with (``ChatRepository/announceAccountDeleted()``).
    private func identity(_ byte: UInt8) -> Identity {
        // swiftlint:disable:next force_try
        try! Identity.fromSeed(seed: Data(repeating: byte, count: 32))
    }

    private func key(_ byte: UInt8) -> Data { identity(byte).signaturePublicKey() }

    private func statement(_ byte: UInt8, group: Data) throws -> Data {
        try composeAccountDeletedEnvelope(accountSigningSeed: identity(byte).ed25519Seed(), groupId: group)
    }

    // MARK: - Send: after the auth delete, before the wipe

    private final class Recorder: @unchecked Sendable {
        private let lock = NSLock()
        private var list: [String] = []
        func add(_ item: String) { lock.withLock { list.append(item) } }
        var items: [String] { lock.withLock { list } }
    }

    func testTheStatementIsPostedAfterTheAuthDeleteAndBeforeTheWipe() async throws {
        let events = Recorder()
        var run = AccountDeletionRun(
            deleteAtAuth: { events.add("auth") },
            cleanups: PasskeysViewModel.deletionCleanups(
                sessionToken: "t", subscriptionIds: [], pushTokens: ReservedAnonTokens([]),
                announceDeletion: { events.add("conversations") }
            ).filter { $0.name == "conversations" },
            wipe: { events.add("wipe") })
        run.retryDelay = .milliseconds(1)
        run.log = { _ in }
        try await run.run()
        XCTAssertEqual(events.items, ["auth", "conversations", "wipe"])
    }

    func testARefusedAuthDeletePostsNoStatement() async {
        struct Refused: Error {}
        let events = Recorder()
        var run = AccountDeletionRun(
            deleteAtAuth: {
                events.add("auth")
                throw Refused()
            },
            cleanups: PasskeysViewModel.deletionCleanups(
                sessionToken: "t", subscriptionIds: [], pushTokens: ReservedAnonTokens([]),
                announceDeletion: { events.add("conversations") }),
            wipe: { events.add("wipe") })
        run.log = { _ in }
        do {
            try await run.run()
            XCTFail("a refused auth delete must throw")
        } catch {}
        XCTAssertEqual(events.items, ["auth"], "never a statement about an account that still exists")
    }

    func testAFailingStatementNeverBlocksTheWipe() async throws {
        struct Unreachable: Error {}
        let events = Recorder()
        var run = AccountDeletionRun(
            deleteAtAuth: { events.add("auth") },
            cleanups: PasskeysViewModel.deletionCleanups(
                sessionToken: "t", subscriptionIds: [], pushTokens: ReservedAnonTokens([]),
                announceDeletion: { throw Unreachable() }
            ).filter { $0.name == "conversations" },
            wipe: { events.add("wipe") })
        run.retryDelay = .milliseconds(1)
        run.log = { _ in }
        try await run.run()
        XCTAssertEqual(events.items, ["auth", "wipe"])
    }

    // MARK: - Receive: verified at receipt

    func testAVerifiedStatementMarksThePersonDeletedForGood() throws {
        let deletions = makeDeletions()
        let group = Data(repeating: 0x33, count: 32)
        let wire = try statement(7, group: group)
        let sender = base64Encode(key(7))
        XCTAssertTrue(ChatRepository.acceptAccountDeleted(
            envelope: wire, groupId: group, groupIdHex: "33", senderKeyB64: sender, into: deletions))
        XCTAssertTrue(deletions.isDeleted(keysB64: [sender]))
        // Final: a later directory answer that the account exists changes nothing.
        deletions.noteAnswer(personKeyB64: sender, accountIdB64: nil, .samePerson)
        XCTAssertTrue(deletions.isDeleted(keysB64: [sender]))
    }

    func testAStatementFromAnotherSenderOrForAnotherGroupIsIgnored() throws {
        let deletions = makeDeletions()
        let group = Data(repeating: 0x33, count: 32)
        let wire = try statement(7, group: group)
        let other = base64Encode(key(8))
        XCTAssertFalse(ChatRepository.acceptAccountDeleted(
            envelope: wire, groupId: group, groupIdHex: "33", senderKeyB64: other, into: deletions))
        XCTAssertFalse(deletions.isDeleted(keysB64: [other]))
        let sender = base64Encode(key(7))
        XCTAssertFalse(ChatRepository.acceptAccountDeleted(
            envelope: wire, groupId: Data(repeating: 0x34, count: 32), groupIdHex: "34",
            senderKeyB64: sender, into: deletions), "moved into another group")
        var tampered = wire
        tampered[tampered.count - 1] ^= 0x01
        XCTAssertFalse(ChatRepository.acceptAccountDeleted(
            envelope: tampered, groupId: group, groupIdHex: "33", senderKeyB64: sender, into: deletions))
        XCTAssertFalse(deletions.isDeleted(keysB64: [sender]))
        XCTAssertNil(deletions.record(personKeyB64: sender), "a refused statement writes nothing")
    }

    func testTheStatementClassifiesAsItsOwnEventNeverABubble() throws {
        let wire = try statement(7, group: Data(repeating: 1, count: 32))
        let event = ChatClient.toEvent(
            messageId: 4, createdAt: "2026-11-20T00:00:00Z",
            processed: .application(data: wire, senderLeaf: 2, senderAccount: nil),
            resolveSender: { "member-\($0)" })
        guard case .accountDeleted(let messageId, let envelope, let senderKey, _) = event else {
            return XCTFail("an account-deleted statement must classify as its own event")
        }
        XCTAssertEqual(messageId, 4)
        XCTAssertEqual(envelope, wire)
        XCTAssertEqual(senderKey, "member-2")
    }

    // MARK: - Pull: the directory's answer

    func testDirectoryAbsentMarksDeletedOtherKeyNeverAndSamePersonReverts() {
        let deletions = makeDeletions()
        let person = base64Encode(key(9))
        deletions.noteAnswer(personKeyB64: person, accountIdB64: nil, .otherKey)
        XCTAssertFalse(deletions.isDeleted(keysB64: [person]), "a reset or rotation is never a deletion")
        now += 1
        deletions.noteAnswer(personKeyB64: person, accountIdB64: nil, .absent)
        XCTAssertTrue(deletions.isDeleted(keysB64: [person]))
        now += 1
        deletions.noteAnswer(personKeyB64: person, accountIdB64: nil, .samePerson)
        XCTAssertFalse(deletions.isDeleted(keysB64: [person]), "a pull-derived absent is reversible")
    }

    func testAnAnswerIsClassifiedAgainstTheHeldKey() {
        let held = key(9)
        XCTAssertEqual(DeletedAccountRules.classify(bundle: nil, heldKey: held), .absent)
        let same = IdentityFetchResponse(signaturePubkey: base64Encode(held), x25519Pubkey: "")
        XCTAssertEqual(DeletedAccountRules.classify(bundle: same, heldKey: held), .samePerson)
        let other = IdentityFetchResponse(signaturePubkey: base64Encode(key(10)), x25519Pubkey: "")
        XCTAssertEqual(DeletedAccountRules.classify(bundle: other, heldKey: held), .otherKey)
    }

    func testThePullAsksOnceRecordsTheAttemptAndNotAgainWithinTheRetryWindow() async {
        let deletions = makeDeletions()
        let asked = Recorder()
        deletions.fetchIdentity = { _ in
            asked.add("ask")
            return nil
        }
        let person = key(9)
        let accountId = Data(repeating: 0xAB, count: 16)
        deletions.checkIfDue(personKey: person, accountId: accountId)
        deletions.checkIfDue(personKey: person, accountId: accountId)
        XCTAssertEqual(deletions.record(personKeyB64: base64Encode(person))?.lastAttemptAtMs, now)
        for _ in 0..<20 { await Task.yield() }
        XCTAssertEqual(asked.items.count, 1, "one request in flight per person")
        XCTAssertTrue(deletions.isDeleted(personKey: person))
        now += 60 * 1000
        deletions.checkIfDue(personKey: person, accountId: accountId)
        for _ in 0..<20 { await Task.yield() }
        XCTAssertEqual(asked.items.count, 1, "no new check within the hour")
    }

    func testANetworkFailureRecordsNoAnswer() async {
        struct NoNetwork: Error {}
        let deletions = makeDeletions()
        deletions.fetchIdentity = { _ in throw NoNetwork() }
        let person = key(9)
        deletions.checkIfDue(personKey: person, accountId: Data(repeating: 0xAB, count: 16))
        for _ in 0..<20 { await Task.yield() }
        XCTAssertNil(deletions.record(personKeyB64: base64Encode(person))?.lastAnswer)
        XCTAssertFalse(deletions.isDeleted(personKey: person))
    }

    // MARK: - Username: only while recently confirmed

    func testTheUsernameIsHiddenWhenNotConfirmedWithinTwentyOneDays() {
        let deletions = makeDeletions()
        let person = base64Encode(key(9))
        XCTAssertFalse(deletions.mayShowUsername(keysB64: [person]), "never confirmed")
        deletions.noteAnswer(personKeyB64: person, accountIdB64: nil, .samePerson)
        now += 20 * day
        XCTAssertTrue(deletions.mayShowUsername(keysB64: [person]))
        now += 2 * day
        XCTAssertFalse(deletions.mayShowUsername(keysB64: [person]), "older than 21 days")
    }

    func testTheDirectoryShowsDeletedAccountWithNoUsernameAndNoRename() async {
        let deletions = makeDeletions()
        let person = key(9)
        let verifier = UsernameVerifier(resolve: { _ in .signatureKey(person) }, defaults: defaults)
        final class Persistence: ProfilePersistence {
            var blobs: [String: Data] = [:]
            func load(accountIdB64: String) -> Data? { blobs[accountIdB64] }
            func save(_ data: Data, accountIdB64: String) { blobs[accountIdB64] = data }
            func delete(accountIdB64: String) { blobs[accountIdB64] = nil }
        }
        let directory = ProfileDirectory(
            received: ReceivedProfileStore(
                persistence: Persistence(),
                isNewer: { profileIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1) }),
            verifier: verifier,
            ownStore: OwnProfileStore(defaults: defaults),
            deletions: deletions)
        directory.verifiesOnResolve = false
        directory.activate(accountIdB64: own, username: "me")
        directory.acceptReceived(
            ProfileClaim(displayName: "Alice", username: "alice", updatedAtMs: 5),
            signatureKeyB64: base64Encode(person), accountIdB64: own)
        await verifier.verify(signatureKeyB64: base64Encode(person), username: "alice")
        XCTAssertNil(directory.resolve(signatureKey: person).verifiedUsername, "verified but not confirmed")
        deletions.noteAnswer(personKeyB64: base64Encode(person), accountIdB64: nil, .samePerson)
        XCTAssertEqual(directory.resolve(signatureKey: person).listLabel, "Alice · @alice")

        // The viewer's own rename of them.
        directory.setRenames(from: [AcceptedContact(
            request: IncomingContactRequest(
                messageId: 1, fromPubkey: person, fromAccountId: nil, fromPseudonym: Data(repeating: 1, count: 32),
                validAfterMs: 0, intro: nil, createdAt: "2026-11-20T00:00:00Z"),
            displayName: "My Alice")])
        XCTAssertEqual(directory.resolve(signatureKey: person).name, "My Alice")

        now += 1
        deletions.noteAnswer(personKeyB64: base64Encode(person), accountIdB64: nil, .absent)
        let resolved = directory.resolve(signatureKey: person)
        XCTAssertTrue(resolved.isDeletedAccount)
        XCTAssertEqual(resolved.name, ResolvedDisplayName.deletedName)
        XCTAssertNil(resolved.verifiedUsername)
        XCTAssertNil(directory.avatar(signatureKey: person))
        XCTAssertFalse(ContactListPresentation.matches(
            ContactListPresentation.Item(
                id: 1, name: resolved.name, username: resolved.verifiedUsername,
                isPlaceholder: resolved.isPlaceholder),
            query: "alice"), "never found by the old name or username")
    }

    // MARK: - A deleted one-to-one

    func testADeletedOneToOneHasNoComposerAndNoCallButtons() {
        let slot = ChatViewModel.composerSlot(peerDeleted: true, membership: .active, draft: "hello")
        XCTAssertEqual(slot, .withdrawn(notice: ResolvedDisplayName.deletedBody, draft: nil, draftLabel: nil))
        XCTAssertEqual(ChatViewModel.composerSlot(peerDeleted: false, membership: .active, draft: ""), .composer)
        XCTAssertFalse(ChatViewModel.hasSomeoneToCall(peerDeleted: true, isGroup: false, people: 2))
        XCTAssertTrue(ChatViewModel.hasSomeoneToCall(peerDeleted: false, isGroup: false, people: 2))
    }

    func testADeletedPersonsCardOffersOnlyOpenDeleteAndRemove() {
        XCTAssertEqual(
            Set(deletedPersonInfoSections(hasOneToOne: true, isContact: true)),
            [.message, .deleteChat, .removeContact])
        XCTAssertTrue(deletedPersonInfoSections(hasOneToOne: false, isContact: false).isEmpty)
    }

    // MARK: - Groups: the owner's or an admin's device proposes the removal

    func testAnOwnerOrAdminProposesRemovalOnceAndNeverOnAbsent() {
        let deletions = makeDeletions()
        let statement = base64Encode(key(7))
        let pulled = base64Encode(key(9))
        deletions.noteAnswer(personKeyB64: pulled, accountIdB64: nil, .absent)
        XCTAssertTrue(deletions.removalsOwed(groupIdHex: "g1").isEmpty, "a directory answer never removes")
        deletions.noteStatement(personKeyB64: statement, groupIdHex: "g1")
        XCTAssertEqual(deletions.removalsOwed(groupIdHex: "g1"), [statement])
        XCTAssertTrue(deletions.removalsOwed(groupIdHex: "g2").isEmpty, "only where it was verified")
        deletions.noteRemovalProposed(personKeyB64: statement, groupIdHex: "g1")
        XCTAssertTrue(deletions.removalsOwed(groupIdHex: "g1").isEmpty, "proposed once")

        let person = key(7)
        let owner = key(1)
        let roles = RoleTable(owner: owner, admins: [key(2)])
        XCTAssertTrue(ChatRepository.shouldProposeRemoval(
            ownRole: .owner, roles: roles, personKey: person, inRoster: true, oneToOne: false))
        XCTAssertTrue(ChatRepository.shouldProposeRemoval(
            ownRole: .admin, roles: roles, personKey: person, inRoster: true, oneToOne: false))
        XCTAssertFalse(ChatRepository.shouldProposeRemoval(
            ownRole: .member, roles: roles, personKey: person, inRoster: true, oneToOne: false))
        XCTAssertFalse(ChatRepository.shouldProposeRemoval(
            ownRole: .admin, roles: roles, personKey: owner, inRoster: true, oneToOne: false),
            "the owner is unremovable (C-63)")
        XCTAssertFalse(ChatRepository.shouldProposeRemoval(
            ownRole: .owner, roles: roles, personKey: person, inRoster: true, oneToOne: true))
        XCTAssertFalse(ChatRepository.shouldProposeRemoval(
            ownRole: .owner, roles: roles, personKey: person, inRoster: false, oneToOne: false))
    }

    // MARK: - Requests and the records

    func testAnIncomingRequestFromADeletedAccountIsDroppedSilently() async {
        let account = "acct-\(UUID().uuidString)"
        let gone = request(1, byte: 0xA1)
        let live = request(2, byte: 0xB2)
        ContactsInboxStore(defaults: defaults).save(
            PersistedContactsInbox(cursor: 0, pending: [gone, live]), accountIdB64: account)
        let session = CurrentValueSubject<String?, Never>(account)
        let repository = ContactsRepository(
            authRepository: AuthRepository.offline(
                authClientFactory: { Offline.auth() }, persistence: NoSession(), nowMs: { 1_000_000 }),
            discoveryClientFactory: { Offline.discovery() },
            inboxStore: ContactsInboxStore(defaults: defaults),
            accountBinding: session.eraseToAnyPublisher())
        await Task.yield()
        XCTAssertEqual(repository.state.pending.map(\.messageId), [1, 2])
        repository.applyDeletedAccounts(isDeleted: { $0 == gone.fromPubkey })
        XCTAssertEqual(repository.state.pending.map(\.messageId), [2])
        XCTAssertTrue(repository.state.accepted.isEmpty, "dropped, never accepted")
    }

    /// Accepting a request whose account the directory answers `404` for:
    /// the card goes, nothing is founded, and the screen says so.
    func testAcceptingARequestWhoseAccountIsGoneRemovesTheCard() async {
        let account = "acct-\(UUID().uuidString)"
        let gone = request(3, byte: 0xC3)
        ContactsInboxStore(defaults: defaults).save(
            PersistedContactsInbox(cursor: 0, pending: [gone]), accountIdB64: account)
        let session = CurrentValueSubject<String?, Never>(account)
        let repository = ContactsRepository(
            authRepository: AuthRepository.offline(
                authClientFactory: { Offline.auth() }, persistence: NoSession(), nowMs: { 1_000_000 }),
            // Every directory answer here is a `404`.
            discoveryClientFactory: { Offline.discovery() },
            inboxStore: ContactsInboxStore(defaults: defaults),
            accountBinding: session.eraseToAnyPublisher())
        await Task.yield()
        let outcome = await repository.acceptIncomingRequest(gone)
        XCTAssertEqual(outcome, .deletedAccount)
        XCTAssertTrue(repository.state.pending.isEmpty)
        XCTAssertTrue(repository.state.accepted.isEmpty)
    }

    private func request(_ messageId: Int64, byte: UInt8) -> IncomingContactRequest {
        IncomingContactRequest(
            messageId: messageId,
            fromPubkey: Data(repeating: byte, count: 32),
            fromAccountId: Data(repeating: byte, count: 16),
            fromPseudonym: Data(repeating: byte, count: 32),
            validAfterMs: 0,
            intro: nil,
            createdAt: "2026-11-20T00:00:00Z")
    }

    func testTheRecordsSurviveAReloadAndGoWithTheAccount() {
        let deletions = makeDeletions()
        let person = base64Encode(key(9))
        deletions.noteStatement(personKeyB64: person, groupIdHex: "g1")
        let reloaded = DeletedAccounts(store: DeletedAccountStore(defaults: defaults))
        reloaded.activate(accountIdB64: own)
        XCTAssertTrue(reloaded.isDeleted(keysB64: [person]))
        DeletedAccountStore.clear(accountIdB64: own, defaults: defaults)
        XCTAssertTrue(DeletedAccountStore(defaults: defaults).load(accountIdB64: own).isEmpty)
    }
}

/// A ``SessionPersistence`` that holds nothing, so no real Keychain is read.
private struct NoSession: SessionPersistence {
    func hasDeviceKey(accountIdB64: String) -> Bool { false }
    func accountIdentitySeed(accountIdB64: String) -> Data? { nil }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { nil }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {}
    func deleteSessionToken(accountIdB64: String) {}
}
