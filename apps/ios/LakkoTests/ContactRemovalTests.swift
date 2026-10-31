// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import XCTest
@testable import Lakko

/// `docs/ux/conversation-info.md` section 2.1 — "Remove contact".
///
/// iOS never built this: the contact screen carried a comment asserting that
/// no client had a remove-contact operation, which was false and had been for
/// some time (Android's `ContactsRepository.removeContact`, web's
/// `removeAccepted`). These pin the semantics it now mirrors — local only,
/// persisted, and deliberately leaving any existing chat behind.
@MainActor
final class ContactRemovalTests: XCTestCase {

    /// The repository rehydrates the bound account's accepted list from the
    /// store when it binds, and writes back through `state`'s `didSet`, so
    /// seeding it is how a test hands the repository contacts, and reading it
    /// back is how the test sees whether the removal was persisted rather
    /// than only dropped from memory. An isolated suite keeps the real
    /// `UserDefaults` out of it.
    private let account = "removal-test-account"
    private var suiteName = ""

    override func setUp() {
        super.setUp()
        suiteName = "contact-removal-\(UUID().uuidString)"
        AcceptedContactsStore.defaults = UserDefaults(suiteName: suiteName)!
    }

    override func tearDown() {
        UserDefaults().removePersistentDomain(forName: suiteName)
        AcceptedContactsStore.defaults = .standard
        super.tearDown()
    }

    private func seed(_ contacts: [AcceptedContact]) {
        AcceptedContactsStore.save(contacts, accountIdB64: account)
    }

    private func persisted() -> [AcceptedContact] {
        AcceptedContactsStore.load(accountIdB64: account)
    }

    func testRemovingAContactDropsItFromTheListAndLeavesTheRest() {
        seed([contact(1, byte: 0xA1), contact(2, byte: 0xB2)])
        let repository = makeRepository()
        XCTAssertEqual(repository.state.accepted.count, 2)

        repository.removeContact(messageId: 1)

        XCTAssertEqual(repository.state.accepted.map(\.request.messageId), [2])
    }

    /// The list is what the app rehydrates from on the next launch, so a
    /// removal that only happened in memory would come back.
    func testARemovalIsPersisted() {
        seed([contact(1, byte: 0xA1), contact(2, byte: 0xB2)])
        let repository = makeRepository()

        repository.removeContact(messageId: 1)

        XCTAssertEqual(persisted().map(\.request.messageId), [2])
    }

    /// The removal is confined to the accepted list: it reads and rewrites
    /// that and nothing else.
    ///
    /// Stated for what this can actually observe. That an existing chat is
    /// left behind — the property that produces the "person who is not a
    /// contact" state the info layer handles — is a fact about what
    /// `removeContact` does NOT reach: no chat repository is in the path at
    /// all, which the signature shows and no assertion here can. This
    /// complements that rather than proving it.
    func testRemovingAContactTouchesNothingButTheAcceptedList() {
        let withChat = AcceptedContact(
            request: request(1, byte: 0xA1), groupIdHex: "beef", displayName: "Ada")
        let other = AcceptedContact(
            request: request(2, byte: 0xB2), groupIdHex: "cafe", displayName: nil)
        seed([withChat, other])
        let repository = makeRepository()

        repository.removeContact(messageId: 1)

        // The surviving contact is untouched, chat handle and all; nothing in
        // the removal reaches a group.
        XCTAssertEqual(repository.state.accepted.count, 1)
        XCTAssertEqual(repository.state.accepted.first?.groupIdHex, "cafe")
    }

    /// A removal that names nobody must not disturb the list — the control can
    /// be tapped twice, and the second tap is a no-op rather than a rewrite.
    func testRemovingAContactThatIsNotThereChangesNothing() {
        seed([contact(1, byte: 0xA1)])
        let repository = makeRepository()

        repository.removeContact(messageId: 99)
        repository.removeContact(messageId: 1)
        repository.removeContact(messageId: 1)

        XCTAssertTrue(repository.state.accepted.isEmpty)
        XCTAssertTrue(persisted().isEmpty)
    }

    /// The person's one-to-one stays after the removal, and the adoption
    /// rule ("a one-to-one peer is a contact") must not bring them back, not
    /// now and not after a relaunch. Seen on web 2026-10-24.
    func testARemovedContactIsNotBroughtBackByTheirOneToOne() {
        seed([AcceptedContact(request: request(1, byte: 0xA1), groupIdHex: "beef", displayName: "Ada")])
        let repository = makeRepository()
        repository.removeContact(messageId: 1)

        XCTAssertFalse(repository.adoptOneToOnePeer(
            signatureKey: Data(repeating: 0xA1, count: 32), accountId: nil,
            groupIdHex: "beef", nowMs: 1, createdAt: "2026-10-24T00:00:00Z"))
        XCTAssertTrue(repository.state.accepted.isEmpty)

        let relaunched = makeRepository()
        XCTAssertFalse(relaunched.adoptOneToOnePeer(
            signatureKey: Data(repeating: 0xA1, count: 32), accountId: nil,
            groupIdHex: "beef", nowMs: 2, createdAt: "2026-10-24T00:00:00Z"))
        XCTAssertTrue(persisted().isEmpty)
        // Another person's one-to-one is still adopted.
        XCTAssertTrue(relaunched.adoptOneToOnePeer(
            signatureKey: Data(repeating: 0xB2, count: 32), accountId: nil,
            groupIdHex: "cafe", nowMs: 3, createdAt: "2026-10-24T00:00:00Z"))
    }

    /// `Add to contacts` (amended 2026-10-24): the removal is forgotten and
    /// the row comes back with the one-to-one's id; a second call changes
    /// nothing; the adoption rule then holds for them again after a relaunch.
    func testAddToContactsPutsARemovedPersonBackWithTheirConversation() {
        seed([AcceptedContact(request: request(1, byte: 0xA1), groupIdHex: "beef", displayName: "Ada")])
        let repository = makeRepository()
        let key = Data(repeating: 0xA1, count: 32)
        repository.removeContact(messageId: 1)
        XCTAssertTrue(repository.isRemovedContact(key))

        XCTAssertTrue(repository.restoreRemovedContact(
            signatureKey: key, accountId: Data(repeating: 0xA1, count: 16),
            groupIdHex: "beef", nowMs: 10, createdAt: "2026-10-24T00:00:00Z"))
        XCTAssertFalse(repository.isRemovedContact(key))
        XCTAssertEqual(repository.state.accepted.map(\.request.fromPubkey), [key])
        XCTAssertEqual(repository.state.accepted.first?.groupIdHex, "beef")
        XCTAssertEqual(
            repository.state.accepted.first?.request.messageId,
            ContactsRepository.syntheticContactMessageId(signatureKey: key))
        XCTAssertEqual(persisted().map(\.request.fromPubkey), [key])

        // Nothing more to do for a contact already on the list.
        XCTAssertFalse(repository.restoreRemovedContact(
            signatureKey: key, accountId: nil, groupIdHex: "beef", nowMs: 11,
            createdAt: "2026-10-24T00:00:00Z"))

        let relaunched = makeRepository()
        XCTAssertFalse(relaunched.isRemovedContact(key))
        XCTAssertEqual(relaunched.state.accepted.map(\.request.fromPubkey), [key])
    }

    // MARK: - One person is one row (QA re-walk 2026-10-30)

    private func adopted(byte: UInt8, group: String) -> AcceptedContact {
        let key = Data(repeating: byte, count: 32)
        return AcceptedContact(
            request: IncomingContactRequest(
                messageId: ContactsRepository.syntheticContactMessageId(signatureKey: key),
                fromPubkey: key, fromAccountId: nil, fromPseudonym: Data(count: 32),
                validAfterMs: 0, intro: nil, createdAt: ""),
            groupIdHex: group)
    }

    func testASecondAcceptLeavesOneRowCarryingTheRequest() {
        var held = adopted(byte: 0xA1, group: "g1")
        held.displayName = "Renamed"
        let rows = ContactsRepository.accepting(
            request(7, byte: 0xA1), into: [held], groupIdHex: "g1")
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows[0].request.messageId, 7, "its owed ack is found by this id")
        XCTAssertEqual(rows[0].groupIdHex, "g1")
        XCTAssertEqual(rows[0].displayName, "Renamed")
        XCTAssertEqual(rows[0].request.fromAccountId, Data(repeating: 0xA1, count: 16))
    }

    func testARequestWhoseContactExistsIsGone() {
        let other = request(8, byte: 0xB2)
        let rows = ContactsRepository.reconciled(
            accepted: [adopted(byte: 0xA1, group: "g1")],
            pending: [request(7, byte: 0xA1), other])
        XCTAssertEqual(rows.pending, [other])
        XCTAssertEqual(rows.accepted.map(\.request.messageId), [7])
    }

    func testTwoRowsOfOnePersonBecomeOneKeepingWhatEitherKnew() {
        var named = contact(7, byte: 0xA1)
        named.displayName = "Renamed"
        let rows = ContactsRepository.reconciled(
            accepted: [named, adopted(byte: 0xA1, group: "g1"), contact(9, byte: 0xB2)], pending: [])
        XCTAssertEqual(rows.accepted.count, 2)
        XCTAssertEqual(rows.accepted[0].request.messageId, 7, "a mailbox request beats a synthetic one")
        XCTAssertEqual(rows.accepted[0].groupIdHex, "g1")
        XCTAssertEqual(rows.accepted[0].displayName, "Renamed")
    }

    func testRowsStoredTwiceAreOneAfterARestart() {
        seed([contact(7, byte: 0xA1), adopted(byte: 0xA1, group: "g1")])
        let repository = makeRepository()
        XCTAssertEqual(repository.state.accepted.map(\.request.messageId), [7])
        XCTAssertEqual(persisted().count, 1, "healed on disk too")
    }

    func testAOneToOneWithSomeoneWhoAskedTakesTheirRequestOntoTheContact() {
        let inbox = ContactsInboxStore(defaults: UserDefaults(suiteName: suiteName)!)
        inbox.save(PersistedContactsInbox(pending: [request(7, byte: 0xA1)]), accountIdB64: account)
        let repository = ContactsRepository(
            authRepository: AuthRepository.offline(
                authClientFactory: { Offline.auth() },
                persistence: NoSessionPersistence(),
                nowMs: { 1_000_000 }),
            inboxStore: inbox,
            accountBinding: Just<String?>(account).eraseToAnyPublisher())
        XCTAssertEqual(repository.state.pending.count, 1)
        repository.adoptOneToOnePeer(
            signatureKey: Data(repeating: 0xA1, count: 32), accountId: nil, groupIdHex: "g1",
            nowMs: 1, createdAt: "")
        XCTAssertTrue(repository.state.pending.isEmpty)
        XCTAssertEqual(repository.state.accepted.map(\.request.messageId), [7])
        XCTAssertEqual(repository.state.accepted.first?.groupIdHex, "g1")
    }

    // MARK: - The card closes (section 2.1)

    /// Remove closes the card in every case, also for a person the
    /// conversation left behind still reads back under this row id.
    func testRemoveClosesTheCardWhateverStaysBehindTheName() {
        let person = contact(-5, byte: 0xA1)
        XCTAssertEqual(ContactDetailScreen.cardPerson(person, removedHere: false), person)
        XCTAssertNil(ContactDetailScreen.cardPerson(person, removedHere: true))
        XCTAssertNil(ContactDetailScreen.cardPerson(nil, removedHere: false))
    }

    // MARK: - A new contact is not a stand-in

    private final class FakeSentStore: SentRequestsStoring {
        var rows: [SentContactRequest] = []
        func load(accountIdB64: String) -> [SentContactRequest] { rows }
        func save(_ sent: [SentContactRequest], accountIdB64: String) throws { rows = sent }
        func clear(accountIdB64: String) { rows = [] }
    }

    private func ack(key: Data, accountId: Data) -> IncomingContactAck {
        IncomingContactAck(
            messageId: 9, accepterPubkey: key, accepterAccountId: accountId,
            accepterX25519Pubkey: Data(count: 32), ackToPseudonym: Data(count: 32),
            acceptedAtMs: 1, createdAt: "2026-10-31T00:00:00Z")
    }

    func testAnAckFilesTheNameBeforeTheSentRowGoes() {
        ProfileDirectory.shared.activate(accountIdB64: account, username: nil)
        defer { ProfileDirectory.shared.forget(accountIdB64: account) }
        let key = Data(repeating: 0xC1, count: 32)
        let accountId = Data(repeating: 0xC2, count: 16)
        let sent = FakeSentStore()
        sent.rows = [SentContactRequest(
            recipientUsername: "", recipientAccountIdB64: base64Encode(accountId), messageId: 1,
            createdAt: "", recipientName: "Anna Weber", recipientKeyB64: base64Encode(key))]
        let repository = makeRepository(sentStore: sent)
        XCTAssertEqual(repository.state.sent.count, 1)

        repository.applyContactAck(ack(key: key, accountId: accountId))

        XCTAssertEqual(repository.state.accepted.map(\.request.fromPubkey), [key])
        XCTAssertTrue(repository.state.sent.isEmpty)
        XCTAssertEqual(ProfileDirectory.shared.resolve(signatureKey: key).name, "Anna Weber")
    }

    /// An ack names its account itself: under a key the row did not find, the
    /// found name is not theirs.
    func testAnAckUnderAnotherKeyGetsNoName() {
        ProfileDirectory.shared.activate(accountIdB64: account, username: nil)
        defer { ProfileDirectory.shared.forget(accountIdB64: account) }
        let key = Data(repeating: 0xC3, count: 32)
        let stranger = Data(repeating: 0xC4, count: 32)
        let accountId = Data(repeating: 0xC5, count: 16)
        let sent = FakeSentStore()
        sent.rows = [SentContactRequest(
            recipientUsername: "", recipientAccountIdB64: base64Encode(accountId), messageId: 1,
            createdAt: "", recipientName: "Anna Weber", recipientKeyB64: base64Encode(key))]
        let repository = makeRepository(sentStore: sent)

        repository.applyContactAck(ack(key: stranger, accountId: accountId))

        XCTAssertTrue(ProfileDirectory.shared.resolve(signatureKey: stranger).isPlaceholder)
    }

    // MARK: - Fixtures

    private func makeRepository(sentStore: SentRequestsStoring? = nil) -> ContactsRepository {
        if let sentStore {
            return ContactsRepository(
                authRepository: AuthRepository.offline(
                    authClientFactory: { Offline.auth() },
                    persistence: NoSessionPersistence(),
                    nowMs: { 1_000_000 }),
                sentStore: sentStore,
                accountBinding: Just<String?>(account).eraseToAnyPublisher())
        }
        return ContactsRepository(
            authRepository: AuthRepository.offline(
                authClientFactory: { Offline.auth() },
                persistence: NoSessionPersistence(),
                nowMs: { 1_000_000 }),
            accountBinding: Just<String?>(account).eraseToAnyPublisher())
    }

    private func request(_ messageId: Int64, byte: UInt8) -> IncomingContactRequest {
        IncomingContactRequest(
            messageId: messageId,
            fromPubkey: Data(repeating: byte, count: 32),
            fromAccountId: Data(repeating: byte, count: 16),
            fromPseudonym: Data(repeating: byte, count: 32),
            validAfterMs: 0,
            intro: nil,
            createdAt: "2026-09-23T00:00:00Z")
    }

    private func contact(_ messageId: Int64, byte: UInt8) -> AcceptedContact {
        AcceptedContact(request: request(messageId, byte: byte))
    }
}

/// A ``SessionPersistence`` that holds nothing, so building an
/// ``AuthRepository`` here never reads or writes the real Keychain.
private struct NoSessionPersistence: SessionPersistence {
    func hasDeviceKey(accountIdB64: String) -> Bool { false }
    func accountIdentitySeed(accountIdB64: String) -> Data? { nil }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { nil }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {}
    func deleteSessionToken(accountIdB64: String) {}
}
