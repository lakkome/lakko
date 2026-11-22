// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import LakkoCore
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
        // The inbox and the sent log persist per account outside the suite:
        // each test starts from none.
        ContactsInboxStore().clear(accountIdB64: account)
        KeychainSentRequestsStore().clear(accountIdB64: account)
        AcceptedContactsStore.defaults = UserDefaults(suiteName: suiteName)!
    }

    override func tearDown() {
        UserDefaults().removePersistentDomain(forName: suiteName)
        ContactsInboxStore().clear(accountIdB64: account)
        KeychainSentRequestsStore().clear(accountIdB64: account)
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

    /// 2026-11-05 — the accept has just founded the one-to-one: that is
    /// their chat, whatever link the row held (it may name a group).
    func testAnAcceptReplacesAHeldLinkWithTheFoundedOneToOne() {
        let rows = ContactsRepository.accepting(
            request(7, byte: 0xA1), into: [adopted(byte: 0xA1, group: "group")], groupIdHex: "g1")
        XCTAssertEqual(rows.map(\.groupIdHex), ["g1"])
    }

    func testAnAcceptAfterTheConversationKeyWasAdoptedLeavesOneRow() {
        let rows = ContactsRepository.accepting(
            request(7, byte: 0xB2), into: [adopted(byte: 0xA1, group: "g1")], groupIdHex: "g1",
            samePerson: { _, _ in true })
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows[0].request.messageId, 7)
        XCTAssertEqual(rows[0].request.fromPubkey, Data(repeating: 0xB2, count: 32))
        XCTAssertEqual(rows[0].previousKeysB64, [base64Encode(Data(repeating: 0xA1, count: 32))])
    }

    func testTwoRowsUnderTwoKeysOfOnePersonBecomeOne() {
        let rows = ContactsRepository.reconciled(
            accepted: [contact(7, byte: 0xB2), adopted(byte: 0xA1, group: "g1")], pending: [],
            samePerson: { _, _ in true })
        XCTAssertEqual(rows.accepted.count, 1)
        XCTAssertEqual(rows.accepted[0].request.messageId, 7)
        XCTAssertEqual(rows.accepted[0].groupIdHex, "g1")
    }

    func testARequestWhoseContactExistsIsGone() {
        let other = request(8, byte: 0xB2)
        let rows = ContactsRepository.reconciled(
            accepted: [adopted(byte: 0xA1, group: "g1")],
            pending: [request(7, byte: 0xA1), other])
        XCTAssertEqual(rows.pending, [other])
        XCTAssertEqual(rows.accepted.map(\.request.messageId), [7])
    }

    /// Base scenario 7 — a request listed because no one-to-one was held (or
    /// one through a group) is not folded onto the contact's row, until a
    /// one-to-one with them is adopted; and the merge keeps it although the
    /// sender is a contact.
    func testARequestAskingForAChatStaysUntilAOneToOneIsAdopted() {
        var asking = request(7, byte: 0xA1)
        asking.asksForChat = true
        let rows = ContactsRepository.reconciled(
            accepted: [adopted(byte: 0xA1, group: "g1")], pending: [asking])
        XCTAssertEqual(rows.pending, [asking])
        let answered = ContactsRepository.reconciled(
            accepted: [adopted(byte: 0xA1, group: "g1")], pending: [asking],
            answered: [Data(repeating: 0xA1, count: 32)])
        XCTAssertTrue(answered.pending.isEmpty)
        let merged = ContactsInboxMerge.merge(
            fetched: [asking], pending: [], accepted: [adopted(byte: 0xA1, group: "g1")])
        XCTAssertEqual(merged.kept, [asking])
        let plain = request(8, byte: 0xA1)
        XCTAssertTrue(ContactsInboxMerge.merge(
            fetched: [plain], pending: [], accepted: [adopted(byte: 0xA1, group: "g1")]).kept.isEmpty)
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

    /// Decided 2026-11-04 — the ack makes no contact: the request stays under
    /// Sent, pending, with the key that signed the ack recorded on it. The
    /// found name is filed for that key all the same.
    func testAnAckKeepsTheRequestPendingAndFilesTheName() {
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

        XCTAssertTrue(repository.state.accepted.isEmpty, "an ack alone makes no contact")
        XCTAssertEqual(repository.state.sent.count, 1, "the request stays pending")
        XCTAssertEqual(repository.state.sent.first?.ackedKeyB64, base64Encode(key))
        XCTAssertEqual(sent.rows.first?.ackedKeyB64, base64Encode(key), "and is stored so")
        XCTAssertEqual(ProfileDirectory.shared.resolve(signatureKey: key).name, "Anna Weber")
    }

    /// The Welcome of the one-to-one the person founded is matched to the
    /// pending row by the key their ack carried, even with no pin for the
    /// account: joining it is what retires the row (and makes the contact).
    func testTheWelcomeIsMatchedToTheRowByTheAckedKey() {
        let key = Data(repeating: 0xD1, count: 32)
        let row = SentContactRequest(
            recipientUsername: "anna", recipientAccountIdB64: "acct", messageId: 1, createdAt: "")
        let acked = SentRequests.acknowledged(recipientAccountIdB64: "acct", byKey: key, in: [row])
        XCTAssertEqual(acked.first?.ackedKeyB64, base64Encode(key))
        let answered = SentRequests.answered(
            byPeer: key, in: acked, keyOfAccount: { _ in nil }, current: { $0 })
        XCTAssertTrue(answered.remaining.isEmpty)
        XCTAssertEqual(answered.accountIdB64, "acct")
        let unacked = SentRequests.answered(
            byPeer: key, in: [row], keyOfAccount: { _ in nil }, current: { $0 })
        XCTAssertEqual(unacked.remaining.count, 1, "no pin and no ack: nothing to match by")
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

    // MARK: - The silent answer (base scenario 7, 2026-11-22)

    /// This account removed a person and asked them again. Their device still
    /// has this account as a contact and answers with the ack alone; the
    /// one-to-one both still hold is the answer: they are back, with it, and
    /// the request is gone.
    func testASilentAnswerFromARemovedPersonPutsThemBackWithTheirOneToOne() {
        let key = Data(repeating: 0xA1, count: 32)
        let accountId = Data(repeating: 0xA2, count: 16)
        seed([AcceptedContact(request: request(1, byte: 0xA1), groupIdHex: "beef", displayName: "Ada")])
        let sent = FakeSentStore()
        sent.rows = [SentContactRequest(
            recipientUsername: "ada", recipientAccountIdB64: base64Encode(accountId), messageId: 2,
            createdAt: "", recipientKeyB64: base64Encode(key))]
        let repository = makeRepository(sentStore: sent)
        repository.removeContact(messageId: 1)
        XCTAssertTrue(repository.isRemovedContact(key))
        XCTAssertEqual(repository.state.sent.count, 1)
        repository.liveOneToOneLookup = { $0 == key ? "beef" : nil }

        repository.applyContactAck(ack(key: key, accountId: accountId))

        XCTAssertFalse(repository.isRemovedContact(key))
        XCTAssertEqual(repository.state.accepted.map(\.request.fromPubkey), [key])
        XCTAssertEqual(repository.state.accepted.first?.groupIdHex, "beef")
        XCTAssertTrue(repository.state.sent.isEmpty)
        XCTAssertTrue(sent.rows.isEmpty, "and the Sent list is stored so")
        XCTAssertEqual(persisted().map(\.request.fromPubkey), [key])
    }

    /// A person with a one-to-one here whom this device never asked (or an
    /// ack naming the asked account under another key) is not brought back.
    func testAnAckThatAnsweredNoRequestOfThisDeviceBringsNobodyBack() {
        let key = Data(repeating: 0xA1, count: 32)
        let asked = Data(repeating: 0xA3, count: 32)
        let accountId = Data(repeating: 0xA2, count: 16)
        seed([AcceptedContact(request: request(1, byte: 0xA1), groupIdHex: "beef", displayName: "Ada")])
        let sent = FakeSentStore()
        sent.rows = [SentContactRequest(
            recipientUsername: "ada", recipientAccountIdB64: base64Encode(accountId), messageId: 2,
            createdAt: "", recipientKeyB64: base64Encode(asked))]
        let repository = makeRepository(sentStore: sent)
        repository.removeContact(messageId: 1)
        repository.liveOneToOneLookup = { $0 == key ? "beef" : nil }

        repository.applyContactAck(ack(key: key, accountId: accountId))

        XCTAssertTrue(repository.isRemovedContact(key))
        XCTAssertTrue(repository.state.accepted.isEmpty)
    }

    /// The silent answer while this device holds NO one-to-one with them:
    /// the ack asks for one to be founded, and founding it makes them a
    /// contact with it and retires the request.
    func testASilentAnswerWithNoOneToOneHereFoundsOne() async {
        let key = Data(repeating: 0xA1, count: 32)
        let accountId = Data(repeating: 0xA2, count: 16)
        seed([AcceptedContact(request: request(1, byte: 0xA1), groupIdHex: "gone", displayName: "Ada")])
        let sent = FakeSentStore()
        sent.rows = [SentContactRequest(
            recipientUsername: "ada", recipientAccountIdB64: base64Encode(accountId), messageId: 2,
            createdAt: "", recipientKeyB64: base64Encode(key))]
        let repository = makeRepository(sentStore: sent)
        repository.removeContact(messageId: 1)
        repository.liveOneToOneLookup = { _ in nil }
        let founded = FoundedKeys()
        repository.silentAnswerFounder = { ack in
            founded.keys.append(ack.accepterPubkey)
            return "fresh"
        }

        XCTAssertTrue(repository.applyContactAck(ack(key: key, accountId: accountId)))
        XCTAssertTrue(repository.state.accepted.isEmpty, "nothing until a chat stands")
        await repository.foundForSilentAnswers([
            ack(key: key, accountId: accountId), ack(key: key, accountId: accountId),
        ])

        XCTAssertEqual(founded.keys, [key], "one chat per person")
        XCTAssertFalse(repository.isRemovedContact(key))
        XCTAssertEqual(repository.state.accepted.map(\.request.fromPubkey), [key])
        XCTAssertEqual(repository.state.accepted.first?.groupIdHex, "fresh")
        XCTAssertTrue(repository.state.sent.isEmpty)
    }

    /// An ack for nothing this device asked founds nothing.
    func testAnAckForNoRequestOfThisDeviceFoundsNothing() {
        let repository = makeRepository(sentStore: FakeSentStore())
        repository.liveOneToOneLookup = { _ in nil }
        XCTAssertFalse(repository.applyContactAck(
            ack(key: Data(repeating: 0xA1, count: 32), accountId: Data(repeating: 0xA2, count: 16))))
    }

    func testAskedAckerMatchesTheKeyOrAnAccountWithNoKey() {
        let key = Data(repeating: 0x01, count: 32)
        let withKey = SentContactRequest(
            recipientUsername: "", recipientAccountIdB64: "acct", messageId: 1, createdAt: "",
            recipientKeyB64: base64Encode(key))
        let noKey = SentContactRequest(
            recipientUsername: "", recipientAccountIdB64: "acct", messageId: 1, createdAt: "")
        XCTAssertTrue(SentRequests.askedAcker(in: [withKey], key: key, accountIdB64: "other"))
        XCTAssertFalse(SentRequests.askedAcker(
            in: [withKey], key: Data(repeating: 0x02, count: 32), accountIdB64: "acct"))
        XCTAssertTrue(SentRequests.askedAcker(in: [noKey], key: key, accountIdB64: "acct"))
        XCTAssertFalse(SentRequests.askedAcker(in: [noKey], key: key, accountIdB64: "other"))
    }

    // MARK: - Message on the contact card (2026-11-05)

    /// A contact with an account and no chat here: Message founds a new
    /// one-to-one once and hands back its group id.
    func testMessageFoundsAOneToOneWhenThereIsNoChat() async {
        let model = ContactsViewModel(repository: makeRepository())
        model.route = { _ in .inbox }
        var founded: [Int64] = []
        model.founder = { contact in
            founded.append(contact.request.messageId)
            return "newchat"
        }

        let opened = await model.openOrStartChat(for: contact(5, byte: 0xB5))

        XCTAssertEqual(opened, "newchat")
        XCTAssertEqual(founded, [5])
        XCTAssertTrue(model.startingChat.isEmpty, "the progress clears once founded")
        XCTAssertTrue(model.startChatFailed.isEmpty)
    }

    /// A contact whose chat is here opens it and founds nothing.
    func testMessageOpensTheExistingChatWithoutFounding() async {
        let model = ContactsViewModel(repository: makeRepository())
        var founded = 0
        model.founder = { _ in founded += 1; return "other" }
        model.conversation = { _ in "beef" }

        let opened = await model.openOrStartChat(
            for: AcceptedContact(request: request(6, byte: 0xB6), groupIdHex: "beef"))

        XCTAssertEqual(opened, "beef")
        XCTAssertEqual(founded, 0)
    }

    /// 2026-11-05 — a stored link the one-to-one rule does not answer (it
    /// names a group the person is in) is no chat: Message founds a new
    /// one-to-one instead of opening the group.
    func testAStoredLinkThatIsNotTheirOneToOneIsNotOpened() async {
        let model = ContactsViewModel(repository: makeRepository())
        model.route = { _ in .inbox }
        var founded = 0
        model.founder = { _ in founded += 1; return "newchat" }
        model.conversation = { _ in nil }

        let opened = await model.openOrStartChat(
            for: AcceptedContact(request: request(9, byte: 0xB9), groupIdHex: "group"))

        XCTAssertEqual(opened, "newchat")
        XCTAssertEqual(founded, 1)
    }

    /// With no chat repository there is no verified chat: the stored link
    /// alone is never handed out.
    func testWithNoChatRepositoryAStoredLinkIsNotAChat() {
        let model = ContactsViewModel(repository: makeRepository())
        XCTAssertNil(model.chatGroupIdHex(
            for: AcceptedContact(request: request(10, byte: 0xBA), groupIdHex: "group")))
        XCTAssertNil(model.contact(forOneToOneChat: "group"))
    }

    /// No account id: nothing to found with, Message stays unavailable.
    func testMessageFoundsNothingWithoutAnAccount() async {
        let model = ContactsViewModel(repository: makeRepository())
        var founded = 0
        model.founder = { _ in founded += 1; return "other" }
        let noAccount = AcceptedContact(request: IncomingContactRequest(
            messageId: 7, fromPubkey: Data(repeating: 0xB7, count: 32), fromAccountId: nil,
            fromPseudonym: Data(count: 32), validAfterMs: 0, intro: nil, createdAt: ""))

        let opened = await model.openOrStartChat(for: noAccount)

        XCTAssertNil(opened)
        XCTAssertEqual(founded, 0)
        XCTAssertTrue(model.startChatFailed.isEmpty)
    }

    /// A failed founding marks the row (the card shows the error) and the
    /// button stays usable: the next try runs and, on success, clears it.
    func testAFailedFoundingCanBeTriedAgain() async {
        let model = ContactsViewModel(repository: makeRepository())
        model.route = { _ in .inbox }
        struct Refused: Error {}
        var fail = true
        model.founder = { _ in
            if fail { throw Refused() }
            return "newchat"
        }
        let card = contact(8, byte: 0xB8)

        let first = await model.openOrStartChat(for: card)
        XCTAssertNil(first)
        XCTAssertEqual(model.startChatFailed, [8])
        XCTAssertTrue(model.startingChat.isEmpty, "the button is usable again")

        fail = false
        let second = await model.openOrStartChat(for: card)
        XCTAssertEqual(second, "newchat")
        XCTAssertTrue(model.startChatFailed.isEmpty)
    }

    /// 2026-11-05 — a contact carried in a pairing bundle brings no chat
    /// link: another client may have linked a group the person is in; this
    /// device links the one-to-one it joins by its own roster.
    func testAPairingBundleContactCarriesNoChatLink() {
        let key = Data(repeating: 0xC1, count: 32)
        let carried = StateBundle.CarriedContact(
            signaturePubkeyB64: base64Encode(key), displayName: "Anna", acceptedAtMs: 0,
            accountIdHex: nil, x25519PubkeyB64: nil,
            groupIdB64: base64Encode(Data(repeating: 0x0F, count: 32)))
        let row = StateBundle.accepted(carried)
        XCTAssertEqual(row?.request.fromPubkey, key)
        XCTAssertNil(row?.groupIdHex)
    }

    // MARK: - A request inside a shared group (`docs/ux/who-can-find-me.md` 6.4)

    func testTheRouteIsTheInboxWhenAGrantReachesIt() {
        XCTAssertEqual(
            ContactRequestRoute.decide(
                reachesInbox: true, sharedGroups: [SharedGroup(groupIdHex: "g1", newestMessageMs: 5)]),
            .inbox)
    }

    func testWithoutAGrantTheRouteIsTheSharedGroupWithTheNewestMessage() {
        let groups = [
            SharedGroup(groupIdHex: "g1", newestMessageMs: 5),
            SharedGroup(groupIdHex: "g2", newestMessageMs: 9),
            SharedGroup(groupIdHex: "g3", newestMessageMs: 1),
        ]
        XCTAssertEqual(ContactRequestRoute.decide(reachesInbox: false, sharedGroups: groups), .group(groupIdHex: "g2"))
        XCTAssertEqual(
            ContactRequestRoute.decide(reachesInbox: false, sharedGroups: groups.reversed()),
            .group(groupIdHex: "g2"), "the order the groups are listed in decides nothing")
        let tie = [SharedGroup(groupIdHex: "gb", newestMessageMs: 3), SharedGroup(groupIdHex: "ga", newestMessageMs: 3)]
        XCTAssertEqual(ContactRequestRoute.decide(reachesInbox: false, sharedGroups: tie), .group(groupIdHex: "ga"))
    }

    func testWithNeitherNothingIsSent() {
        XCTAssertEqual(ContactRequestRoute.decide(reachesInbox: false, sharedGroups: []), ContactRequestRoute.none)
    }

    private final class MessageWorld {
        var founded = 0
        var groupSends: [String] = []
    }

    private func messageModel(route: ContactRequestRoute, world: MessageWorld, repository: ContactsRepository? = nil)
        -> ContactsViewModel {
        let repository = repository ?? makeRepository()
        let model = ContactsViewModel(repository: repository)
        model.conversation = { _ in nil }
        model.route = { _ in route }
        model.founder = { _ in world.founded += 1; return "newchat" }
        model.groupRequester = { contact, groupIdHex in
            world.groupSends.append(groupIdHex)
            repository.recordGroupRequestSent(
                recipientAccountId: contact.request.fromAccountId ?? Data(),
                recipientAccountKey: contact.request.fromPubkey, recipientName: nil)
        }
        return model
    }

    func testMessageFoundsThroughTheInboxWhenAGrantReachesIt() async {
        let world = MessageWorld()
        let model = messageModel(route: .inbox, world: world)
        let opened = await model.openOrStartChat(for: contact(11, byte: 0xC1))
        XCTAssertEqual(opened, "newchat")
        XCTAssertEqual(world.founded, 1)
        XCTAssertEqual(world.groupSends, [])
    }

    func testMessageSendsTheRequestInsideTheSharedGroupAndOpensNoChat() async {
        let world = MessageWorld()
        let model = messageModel(route: .group(groupIdHex: "g1"), world: world)
        let card = contact(12, byte: 0xC2)
        let opened = await model.openOrStartChat(for: card)
        XCTAssertNil(opened, "no chat is created")
        XCTAssertEqual(world.groupSends, ["g1"])
        XCTAssertEqual(world.founded, 0)
        XCTAssertTrue(model.hasWaitingRequest(for: card), "it stands under Sent as Waiting")
        XCTAssertEqual(
            ContactDetailScreen.messageLineKey(
                hasChat: false, waitingRequest: model.hasWaitingRequest(for: card),
                notice: model.messageNotice[card.request.messageId])?.key,
            "contacts.detail.request_sent")
    }

    func testMessagePressedTwiceSendsOnce() async {
        let world = MessageWorld()
        let model = messageModel(route: .group(groupIdHex: "g1"), world: world)
        let card = contact(13, byte: 0xC3)
        _ = await model.openOrStartChat(for: card)
        _ = await model.openOrStartChat(for: card)
        XCTAssertEqual(world.groupSends, ["g1"], "one request per person until it is answered or removed")
    }

    func testAWaitingRequestFromAnotherEntryPointIsNotSentAgain() async {
        let sent = FakeSentStore()
        let card = contact(14, byte: 0xC4)
        sent.rows = [SentContactRequest(
            recipientUsername: "", recipientAccountIdB64: base64Encode(card.request.fromAccountId!),
            messageId: 5, createdAt: "")]
        let world = MessageWorld()
        let model = messageModel(route: .group(groupIdHex: "g1"), world: world, repository: makeRepository(sentStore: sent))
        _ = await model.openOrStartChat(for: card)
        XCTAssertEqual(world.groupSends, [])
        XCTAssertEqual(world.founded, 0)
    }

    func testWithNoWayToThemTheCardSaysSoAndNothingIsSent() async {
        let world = MessageWorld()
        let model = messageModel(route: .none, world: world)
        let card = contact(15, byte: 0xC5)
        let opened = await model.openOrStartChat(for: card)
        XCTAssertNil(opened)
        XCTAssertEqual(world.groupSends, [])
        XCTAssertEqual(world.founded, 0)
        XCTAssertEqual(model.messageNotice[card.request.messageId], .cannotMessage)
        XCTAssertEqual(
            ContactDetailScreen.messageLineKey(
                hasChat: false, waitingRequest: false, notice: .cannotMessage)?.key,
            "contacts.detail.cannot_message")
    }

    func testAFailedGroupSendSaysSoAndMessageWorksAgain() async {
        let world = MessageWorld()
        let model = messageModel(route: .group(groupIdHex: "g1"), world: world)
        struct Offline: Error {}
        var fail = true
        model.groupRequester = { _, groupIdHex in
            if fail { throw Offline() }
            world.groupSends.append(groupIdHex)
        }
        let card = contact(16, byte: 0xC6)
        _ = await model.openOrStartChat(for: card)
        XCTAssertEqual(model.messageNotice[card.request.messageId], .requestFailed)
        XCTAssertTrue(model.startingChat.isEmpty)
        let line = ContactDetailScreen.messageLineKey(hasChat: false, waitingRequest: false, notice: .requestFailed)
        XCTAssertEqual(line?.key, "contacts.detail.request_failed")
        XCTAssertEqual(line?.danger, true)
        fail = false
        _ = await model.openOrStartChat(for: card)
        XCTAssertEqual(world.groupSends, ["g1"])
        XCTAssertNil(model.messageNotice[card.request.messageId])
    }

    func testNoLineWhenAChatIsHere() {
        XCTAssertNil(ContactDetailScreen.messageLineKey(hasChat: true, waitingRequest: true, notice: .cannotMessage))
    }

    // Receiving.

    private func groupEnvelope(from sender: Identity, to recipient: Identity, senderAccountId: Data) throws -> Data {
        let request = try composeContactRequest(
            senderSigningSeed: sender.ed25519Seed(), senderAccountId: senderAccountId,
            recipientX25519Pub: recipient.x25519PublicKey(), fromPseudonym: Data(repeating: 0x5A, count: 32),
            validAfterMs: 1, privacyPassToken: Data(), profile: nil, replyGrant: nil, linkAnswer: nil,
            introMessage: Data())
        return try composeGroupContactRequestEnvelope(
            recipientX25519Pub: recipient.x25519PublicKey(), request: request)
    }

    private let groupHex = String(repeating: "ab", count: 32)

    func testARequestForMeAllowedByMySettingJoinsRequestsWithItsGroup() throws {
        let me = try Identity.fromSeed(seed: Data(repeating: 0x81, count: 32))
        let them = try Identity.fromSeed(seed: Data(repeating: 0x82, count: 32))
        let repository = makeRepository()
        repository.allowsGroupMembers = { _ in true }
        let envelope = try groupEnvelope(from: them, to: me, senderAccountId: Data(repeating: 0x82, count: 16))
        XCTAssertTrue(repository.fileGroupContactRequest(
            groupIdHex: groupHex, envelope: envelope, senderKey: them.signaturePublicKey(),
            ownX25519Secret: me.x25519SecretKey(), createdAt: "2026-11-05T00:00:00Z"))
        XCTAssertEqual(repository.state.pending.count, 1)
        let request = try XCTUnwrap(repository.state.pending.first)
        XCTAssertEqual(request.fromPubkey, them.signaturePublicKey())
        XCTAssertEqual(request.viaGroupIdHex, groupHex)
        XCTAssertLessThan(request.messageId, 0, "never meets a mailbox id")
        XCTAssertEqual(
            IncomingRequestRow.viaLine(viaGroupIdHex: request.viaGroupIdHex, groupName: "Hiking")?.key,
            "contacts.request.via_group")
        XCTAssertEqual(
            IncomingRequestRow.viaLine(viaGroupIdHex: request.viaGroupIdHex, groupName: "Hiking")?.texts,
            ["group": "Hiking"])
        XCTAssertEqual(
            IncomingRequestRow.viaLine(viaGroupIdHex: request.viaGroupIdHex, groupName: nil)?.key,
            "contacts.request.via_unnamed_group")
        XCTAssertNil(IncomingRequestRow.viaLine(viaGroupIdHex: nil, groupName: "Hiking"), "a mailbox request")

        // The same envelope read again (a re-fetch) is one row.
        XCTAssertFalse(repository.fileGroupContactRequest(
            groupIdHex: groupHex, envelope: envelope, senderKey: them.signaturePublicKey(),
            ownX25519Secret: me.x25519SecretKey(), createdAt: "2026-11-05T00:00:01Z"))
        XCTAssertEqual(repository.state.pending.count, 1)
    }

    /// ADR-0099 step 4 — a group-carried request from a sender already known
    /// suspended is dropped unseen before anything else decides; one not yet
    /// known is filed and its sender asked about.
    func testAGroupRequestFromAKnownSuspendedSenderIsDroppedFirst() throws {
        let me = try Identity.fromSeed(seed: Data(repeating: 0x8A, count: 32))
        let them = try Identity.fromSeed(seed: Data(repeating: 0x8B, count: 32))
        let repository = makeRepository()
        repository.allowsGroupMembers = { _ in true }
        var asked: [Data] = []
        repository.askedAboutGroupRequestSender = { asked.append($0) }
        repository.groupRequestSenderSuspended = { $0 == them.signaturePublicKey() }
        let envelope = try groupEnvelope(from: them, to: me, senderAccountId: Data(repeating: 0x8B, count: 16))
        XCTAssertFalse(repository.fileGroupContactRequest(
            groupIdHex: groupHex, envelope: envelope, senderKey: them.signaturePublicKey(),
            ownX25519Secret: me.x25519SecretKey(), createdAt: "2026-11-20T00:00:00Z"))
        XCTAssertTrue(repository.state.pending.isEmpty)
        XCTAssertTrue(asked.isEmpty)

        repository.groupRequestSenderSuspended = { _ in false }
        XCTAssertTrue(repository.fileGroupContactRequest(
            groupIdHex: groupHex, envelope: envelope, senderKey: them.signaturePublicKey(),
            ownX25519Secret: me.x25519SecretKey(), createdAt: "2026-11-20T00:00:01Z"))
        XCTAssertEqual(repository.state.pending.count, 1)
        XCTAssertEqual(asked, [them.signaturePublicKey()])
    }

    /// ADR-0099 step 4 — an undecided link-answer request held from an
    /// earlier poll is dropped when it is decided and its sender is verified
    /// suspended; another sender's row is decided as before (here: waits).
    func testAHeldLinkRequestFromAVerifiedSuspendedSenderIsDroppedWhenDecided() async {
        let repository = makeRepository()
        let inbox = LinkRequestInbox(accountIdB64: account)
        defer { for row in inbox.rows() { inbox.remove(messageId: row.request.messageId) } }
        let row = { (byte: UInt8) in
            LinkRequestInbox.Row(
                request: IncomingContactRequest(
                    messageId: Int64(byte), fromPubkey: Data(repeating: byte, count: 32), fromAccountId: nil,
                    fromPseudonym: Data(repeating: 0, count: 32), validAfterMs: 0, intro: nil, createdAt: "t"),
                linkId: Data(repeating: 1, count: 16), linkSecret: Data(repeating: 2, count: 12))
        }
        XCTAssertTrue(inbox.add(row(0x91)))
        XCTAssertTrue(inbox.add(row(0x92)))
        var asked: [[Data]] = []
        repository.suspendedRequestSenders = { keys in
            asked.append(keys)
            return [Data(repeating: 0x91, count: 32)]
        }
        await repository.decideLinkRequests()
        XCTAssertEqual(asked.count, 1, "every held row asked about at once")
        XCTAssertEqual(Set(asked.first ?? []), [Data(repeating: 0x91, count: 32), Data(repeating: 0x92, count: 32)])
        XCTAssertEqual(inbox.rows().map(\.request.messageId), [0x92])

        // An unknown standing decides the row as before.
        repository.suspendedRequestSenders = { _ in [] }
        await repository.decideLinkRequests()
        XCTAssertEqual(inbox.rows().map(\.request.messageId), [0x92])
    }

    func testARequestForAnotherMemberIsDroppedSilently() throws {
        let me = try Identity.fromSeed(seed: Data(repeating: 0x83, count: 32))
        let other = try Identity.fromSeed(seed: Data(repeating: 0x84, count: 32))
        let them = try Identity.fromSeed(seed: Data(repeating: 0x85, count: 32))
        let repository = makeRepository()
        repository.allowsGroupMembers = { _ in true }
        let envelope = try groupEnvelope(from: them, to: other, senderAccountId: Data(repeating: 0x85, count: 16))
        XCTAssertFalse(repository.fileGroupContactRequest(
            groupIdHex: groupHex, envelope: envelope, senderKey: them.signaturePublicKey(),
            ownX25519Secret: me.x25519SecretKey(), createdAt: ""))
        XCTAssertTrue(repository.state.pending.isEmpty)
    }

    func testARequestPostedByAnotherMemberThanItsSenderIsDropped() throws {
        let me = try Identity.fromSeed(seed: Data(repeating: 0x86, count: 32))
        let them = try Identity.fromSeed(seed: Data(repeating: 0x87, count: 32))
        let poster = try Identity.fromSeed(seed: Data(repeating: 0x88, count: 32))
        let repository = makeRepository()
        repository.allowsGroupMembers = { _ in true }
        let envelope = try groupEnvelope(from: them, to: me, senderAccountId: Data(repeating: 0x87, count: 16))
        XCTAssertFalse(repository.fileGroupContactRequest(
            groupIdHex: groupHex, envelope: envelope, senderKey: poster.signaturePublicKey(),
            ownX25519Secret: me.x25519SecretKey(), createdAt: ""))
        XCTAssertTrue(repository.state.pending.isEmpty)
    }

    func testARequestMySettingHidesFromThatGroupIsDropped() throws {
        let me = try Identity.fromSeed(seed: Data(repeating: 0x89, count: 32))
        let them = try Identity.fromSeed(seed: Data(repeating: 0x8A, count: 32))
        let repository = makeRepository()
        repository.allowsGroupMembers = { _ in false }
        let envelope = try groupEnvelope(from: them, to: me, senderAccountId: Data(repeating: 0x8A, count: 16))
        XCTAssertFalse(repository.fileGroupContactRequest(
            groupIdHex: groupHex, envelope: envelope, senderKey: them.signaturePublicKey(),
            ownX25519Secret: me.x25519SecretKey(), createdAt: ""))
        XCTAssertTrue(repository.state.pending.isEmpty)
    }

    func testAContactsRequestShowsEvenWhenMySettingHidesThatGroup() throws {
        let me = try Identity.fromSeed(seed: Data(repeating: 0x8B, count: 32))
        let them = try Identity.fromSeed(seed: Data(repeating: 0x8C, count: 32))
        seed([AcceptedContact(request: IncomingContactRequest(
            messageId: 3, fromPubkey: them.signaturePublicKey(), fromAccountId: Data(repeating: 0x8C, count: 16),
            fromPseudonym: Data(count: 32), validAfterMs: 0, intro: nil, createdAt: ""))])
        let repository = makeRepository()
        repository.allowsGroupMembers = { _ in false }
        let envelope = try groupEnvelope(from: them, to: me, senderAccountId: Data(repeating: 0x8C, count: 16))
        XCTAssertTrue(repository.fileGroupContactRequest(
            groupIdHex: groupHex, envelope: envelope, senderKey: them.signaturePublicKey(),
            ownX25519Secret: me.x25519SecretKey(), createdAt: ""))
        XCTAssertEqual(repository.state.pending.map(\.fromPubkey), [them.signaturePublicKey()])
    }

    func testARequestFromSomeoneWhoseMailboxRequestIsPendingIsOneRow() throws {
        let me = try Identity.fromSeed(seed: Data(repeating: 0x8D, count: 32))
        let them = try Identity.fromSeed(seed: Data(repeating: 0x8E, count: 32))
        let repository = makeRepository()
        repository.allowsGroupMembers = { _ in true }
        let envelope = try groupEnvelope(from: them, to: me, senderAccountId: Data(repeating: 0x8E, count: 16))
        XCTAssertTrue(repository.fileGroupContactRequest(
            groupIdHex: groupHex, envelope: envelope, senderKey: them.signaturePublicKey(),
            ownX25519Secret: me.x25519SecretKey(), createdAt: ""))
        // A second request from them (another group, or re-sent) replaces the
        // row: one request per person.
        let again = try groupEnvelope(from: them, to: me, senderAccountId: Data(repeating: 0x8E, count: 16))
        _ = repository.fileGroupContactRequest(
            groupIdHex: groupHex, envelope: again, senderKey: them.signaturePublicKey(),
            ownX25519Secret: me.x25519SecretKey(), createdAt: "")
        XCTAssertEqual(repository.state.pending.filter { $0.fromPubkey == them.signaturePublicKey() }.count, 1)
    }

    private func contactRow(_ identity: Identity, byte: UInt8, chat: String? = nil) -> AcceptedContact {
        AcceptedContact(request: IncomingContactRequest(
            messageId: 3, fromPubkey: identity.signaturePublicKey(),
            fromAccountId: Data(repeating: byte, count: 16),
            fromPseudonym: Data(count: 32), validAfterMs: 0, intro: nil, createdAt: ""), groupIdHex: chat)
    }

    /// Found by Android: the mailbox answers a known contact's request with a
    /// silent ack. A group request from a contact with no chat here must not
    /// go that way (an ack, no chat on either side, and the next Message asks
    /// again, forever): it is a Requests row whose Accept founds the chat.
    func testAContactWithNoChatHereGetsARequestRowNotASilentAck() throws {
        let me = try Identity.fromSeed(seed: Data(repeating: 0x91, count: 32))
        let them = try Identity.fromSeed(seed: Data(repeating: 0x92, count: 32))
        seed([contactRow(them, byte: 0x92, chat: "group-not-their-chat")])
        let repository = makeRepository()
        repository.allowsGroupMembers = { _ in true }
        repository.heldOneToOne = { _ in nil }
        var silentlyAnswered = 0
        repository.knownContactRequestHandler = { _ in silentlyAnswered += 1 }
        let envelope = try groupEnvelope(from: them, to: me, senderAccountId: Data(repeating: 0x92, count: 16))
        XCTAssertTrue(repository.fileGroupContactRequest(
            groupIdHex: groupHex, envelope: envelope, senderKey: them.signaturePublicKey(),
            ownX25519Secret: me.x25519SecretKey(), createdAt: ""))
        XCTAssertEqual(silentlyAnswered, 0)
        let row = try XCTUnwrap(repository.state.pending.first { $0.fromPubkey == them.signaturePublicKey() })

        // Accept files the founded one-to-one on their ONE contact row.
        repository.acceptContactRequest(messageId: row.messageId, groupIdHex: "founded-1x1")
        XCTAssertTrue(repository.state.pending.isEmpty)
        let contacts = repository.state.accepted.filter { $0.request.fromPubkey == them.signaturePublicKey() }
        XCTAssertEqual(contacts.map(\.groupIdHex), ["founded-1x1"])
    }

    /// A contact this device already has a chat with goes the mailbox's way:
    /// no row.
    func testAContactWithAChatHereGoesTheMailboxWay() throws {
        let me = try Identity.fromSeed(seed: Data(repeating: 0x93, count: 32))
        let them = try Identity.fromSeed(seed: Data(repeating: 0x94, count: 32))
        seed([contactRow(them, byte: 0x94, chat: "their-1x1")])
        let repository = makeRepository()
        repository.allowsGroupMembers = { _ in true }
        repository.heldOneToOne = { _ in "their-1x1" }
        var silentlyAnswered = 0
        repository.knownContactRequestHandler = { _ in silentlyAnswered += 1 }
        let envelope = try groupEnvelope(from: them, to: me, senderAccountId: Data(repeating: 0x94, count: 16))
        XCTAssertFalse(repository.fileGroupContactRequest(
            groupIdHex: groupHex, envelope: envelope, senderKey: them.signaturePublicKey(),
            ownX25519Secret: me.x25519SecretKey(), createdAt: ""))
        XCTAssertTrue(repository.state.pending.isEmpty)
        XCTAssertEqual(silentlyAnswered, 1)
    }

    /// The request is on disk when the filing call returns: the group's event
    /// fold calls it before its cursor moves past the row.
    func testAFiledGroupRequestIsPersistedWhenTheCallReturns() throws {
        let me = try Identity.fromSeed(seed: Data(repeating: 0x95, count: 32))
        let them = try Identity.fromSeed(seed: Data(repeating: 0x96, count: 32))
        let repository = makeRepository()
        repository.allowsGroupMembers = { _ in true }
        let envelope = try groupEnvelope(from: them, to: me, senderAccountId: Data(repeating: 0x96, count: 16))
        XCTAssertTrue(repository.fileGroupContactRequest(
            groupIdHex: groupHex, envelope: envelope, senderKey: them.signaturePublicKey(),
            ownX25519Secret: me.x25519SecretKey(), createdAt: ""))
        let stored = ContactsInboxStore().load(accountIdB64: account).pending
        XCTAssertEqual(stored.map(\.fromPubkey), [them.signaturePublicKey()])
        XCTAssertEqual(stored.first?.viaGroupIdHex, groupHex, "the Via line survives a relaunch")
    }

    /// The fold hands the request on synchronously (so it is filed before the
    /// cursor gate runs), never draws a row, and skips this account's own.
    func testTheEventFoldHandsTheRequestOnAtOnceAndDrawsNothing() throws {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x97, count: 32))
        let record = GroupRecord(group: try Group.create(identity: identity, kind: .group),
                                 groupId: Data(repeating: 0x78, count: 32))
        let before = record.messages.count
        var handed: [String] = []
        record.consumeEvents(
            [
                .groupContactRequest(messageId: 1, envelope: Data([1]), senderKey: "them", createdAt: ""),
                .groupContactRequest(messageId: 2, envelope: Data([2]), senderKey: "me", createdAt: ""),
                .groupContactRequest(messageId: 3, envelope: Data([3]), senderKey: nil, createdAt: ""),
            ],
            ownAuthorKey: "me",
            groupContactRequestSink: { _, senderKey in handed.append(senderKey) })
        XCTAssertEqual(handed, ["them"])
        XCTAssertEqual(record.messages.count, before)
    }

    // Add contact on a people-search row follows the same order.

    private func searchRepository(groups: [SharedGroup], sends: @escaping (String) -> Void) -> ContactsRepository {
        let repository = makeRepository()
        repository.sharedGroupsOf = { _ in groups }
        repository.groupRequestSender = { [weak repository] groupIdHex, accountId, accountKey, name, username in
            sends(groupIdHex)
            repository?.recordGroupRequestSent(
                recipientAccountId: accountId, recipientAccountKey: accountKey,
                recipientName: name, recipientUsername: username)
        }
        return repository
    }

    private let found = (key: Data(repeating: 0xD1, count: 32), id: Data(repeating: 0xD1, count: 16))

    func testSearchAddGoesToTheInboxWhenAGrantReachesIt() async throws {
        var groupSends: [String] = []
        let repository = searchRepository(
            groups: [SharedGroup(groupIdHex: "g1", newestMessageMs: 1)], sends: { groupSends.append($0) })
        let result = try await repository.fallingBackToSharedGroup(
            accountKey: found.key, accountId: found.id, name: "Anna", username: "anna"
        ) {
            SendResult(recipientUsername: "anna", recipientAccountId: self.found.id, messageId: 9, createdAt: "")
        }
        XCTAssertEqual(result?.messageId, 9)
        XCTAssertEqual(groupSends, [])
    }

    func testARefusedSearchAddGoesInsideTheNewestSharedGroupAndWaits() async throws {
        var groupSends: [String] = []
        let repository = searchRepository(
            groups: [SharedGroup(groupIdHex: "old", newestMessageMs: 1),
                     SharedGroup(groupIdHex: "new", newestMessageMs: 7)],
            sends: { groupSends.append($0) })
        let result = try await repository.fallingBackToSharedGroup(
            accountKey: found.key, accountId: found.id, name: "Anna", username: "anna"
        ) { throw ContactsError.requestRefused }
        XCTAssertNil(result)
        XCTAssertEqual(groupSends, ["new"])
        XCTAssertTrue(
            SentRequests.holds(repository.state.sent, accountKey: found.key, accountId: found.id),
            "it stands under Sent as Waiting, which is what the search row reads")
        XCTAssertEqual(SentRequests.row(repository.state.sent, accountKey: found.key, accountId: found.id)?
            .recipientUsername, "anna")
    }

    func testARefusedSearchAddWithNoSharedGroupKeepsTheRefusal() async {
        var groupSends: [String] = []
        let repository = searchRepository(groups: [], sends: { groupSends.append($0) })
        do {
            _ = try await repository.fallingBackToSharedGroup(
                accountKey: found.key, accountId: found.id, name: nil, username: nil
            ) { throw ContactsError.requestRefused }
            XCTFail("the refusal must stand")
        } catch ContactsError.requestRefused {
        } catch {
            XCTFail("unexpected \(error)")
        }
        XCTAssertEqual(groupSends, [])
        XCTAssertTrue(repository.state.sent.isEmpty)
    }

    func testASearchAddThatFailsOtherwiseIsNotRoutedThroughAGroup() async {
        struct Offline: Error {}
        var groupSends: [String] = []
        let repository = searchRepository(
            groups: [SharedGroup(groupIdHex: "g1", newestMessageMs: 1)], sends: { groupSends.append($0) })
        do {
            _ = try await repository.fallingBackToSharedGroup(
                accountKey: found.key, accountId: found.id, name: nil, username: nil
            ) { throw Offline() }
            XCTFail("the failure must stand")
        } catch is Offline {
        } catch {
            XCTFail("unexpected \(error)")
        }
        XCTAssertEqual(groupSends, [])
    }

    func testAGroupRequestClassifiesAsItsOwnKindNeverAChatRow() throws {
        let me = try Identity.fromSeed(seed: Data(repeating: 0x8F, count: 32))
        let them = try Identity.fromSeed(seed: Data(repeating: 0x90, count: 32))
        let envelope = try groupEnvelope(from: them, to: me, senderAccountId: Data(repeating: 0x90, count: 16))
        let event = ChatClient.toEvent(
            messageId: 7, createdAt: "2026-11-05T00:00:00Z",
            processed: .application(data: envelope, senderLeaf: 2, senderAccount: nil),
            resolveSender: { "member-\($0)" })
        guard case .groupContactRequest(let messageId, let carried, let senderKey, _) = event else {
            return XCTFail("a request inside a group must classify as its own kind")
        }
        XCTAssertEqual(messageId, 7)
        XCTAssertEqual(carried, envelope)
        XCTAssertEqual(senderKey, "member-2")
    }

    // MARK: - Publisher requests (ADR-0045 decision 2)

    /// The mailbox loop holds its cursor when a rendezvous row arrives while
    /// publisher requests are pending and nothing can land them yet; a plain
    /// Welcome goes on to the chat join.
    func testTheMailboxLoopHoldsARendezvousRowWhenTheLanderIsNotReady() throws {
        PendingPublisherRequests.clear(accountIdB64: account)
        defer { PendingPublisherRequests.clear(accountIdB64: account) }
        let contacts = makeRepository()
        XCTAssertFalse(try contacts.landPublisherWelcome(Data([1]), accountIdB64: account, holdWhenNotReady: true))
        try PendingPublisherRequests.record(
            PendingPublisherRequest(
                channelIdHex: "ee", groupIdHex: "0c", joinerKeyB64: base64Encode(Data(repeating: 1, count: 32)),
                name: "", description: "", requestedAtMs: 1),
            accountIdB64: account)
        XCTAssertThrowsError(
            try contacts.landPublisherWelcome(Data([1]), accountIdB64: account, holdWhenNotReady: true))
        XCTAssertFalse(try contacts.landPublisherWelcome(Data([1]), accountIdB64: account, holdWhenNotReady: false))
        contacts.publisherWelcomeLander = { (_: Data, _: String) throws -> Bool in true }
        XCTAssertTrue(try contacts.landPublisherWelcome(Data([1]), accountIdB64: account, holdWhenNotReady: true))
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

/// The keys a test founder was called with.
private final class FoundedKeys: @unchecked Sendable {
    var keys: [Data] = []
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
