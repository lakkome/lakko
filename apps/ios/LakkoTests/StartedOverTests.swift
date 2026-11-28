// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0101 sections 10 and 11 / UX `identity-recovery.md` 16.11 — what a
/// contact's device keeps about an account's seals and about an UNSEALED
/// reset ("started over"): the announcement floor, the record that closes the
/// old one-to-one, the request from the new keys, and the adoption of those
/// keys when the person accepts it. Plus UX 19.5: saving a phrase changes no
/// key.
@MainActor
final class StartedOverTests: XCTestCase {

    private let account = "YWNjdA=="
    private let old = "b2xkLWtleQ=="
    private let new = "bmV3LWtleQ=="
    private let newer = "bmV3ZXIta2V5"

    private func pins(_ name: String = #function) -> PeerPinStore {
        let suite = "lakko.tests.started-over.\(name)"
        UserDefaults().removePersistentDomain(forName: suite)
        let store = PeerPinStore(defaults: UserDefaults(suiteName: suite)!, ownerAccountIdB64: "b3duZXI=")
        store.setPin(accountIdB64: account, pinnedSignaturePubkeyB64: old, rotationMarker: 10, keyGeneration: 1)
        return store
    }

    // MARK: - The announcement floor

    func testTheFloorOnlyRisesAndOnlyForAPinnedAccount() {
        let store = pins()
        XCTAssertEqual(store.sealSeqFloor(accountIdB64: account), 0)
        XCTAssertTrue(store.raiseSealSeqFloor(accountIdB64: account, seq: 4))
        XCTAssertEqual(store.sealSeqFloor(accountIdB64: account), 4)
        XCTAssertFalse(store.raiseSealSeqFloor(accountIdB64: account, seq: 4), "not higher")
        XCTAssertFalse(store.raiseSealSeqFloor(accountIdB64: account, seq: 2), "an older announcement served again")
        XCTAssertEqual(store.sealSeqFloor(accountIdB64: account), 4)
        XCTAssertFalse(store.raiseSealSeqFloor(accountIdB64: "b3RoZXI=", seq: 9), "no pin, no floor")
        XCTAssertEqual(store.sealSeqFloor(accountIdB64: "b3RoZXI="), 0)
    }

    /// A sealed reset moves the pin and keeps the floor at the announcement
    /// it was sealed under.
    func testASealedResetRepinsAndKeepsTheFloor() {
        let store = pins()
        store.raiseSealSeqFloor(accountIdB64: account, seq: 3)
        let result = store.acceptReset(PeerPinStore.AcceptedReset(
            accountIdB64: account, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5,
            nowMs: 20_000, announcementSeq: 7))
        XCTAssertEqual(result, .accepted)
        XCTAssertEqual(store.readPin(accountIdB64: account)?.pinnedSignaturePubkeyB64, new)
        XCTAssertGreaterThanOrEqual(store.sealSeqFloor(accountIdB64: account), 7)
        XCTAssertNotNil(store.loadSupersededKeys()[old], "the old key is the same person's, retired")
        XCTAssertTrue(store.loadStartedOverKeys().isEmpty, "a sealed reset closes nothing")
    }

    // MARK: - An unsealed reset

    func testAnUnsealedResetIsRecordedAndThePinStaysOnTheOldKey() {
        let store = pins()
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: account, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5),
            .recorded)
        // Nothing moved: the pin, the generation.
        XCTAssertEqual(store.readPin(accountIdB64: account)?.pinnedSignaturePubkeyB64, old)
        XCTAssertEqual(store.acceptedGeneration(accountIdB64: account), 1)
        XCTAssertEqual(
            store.startedOver(accountIdB64: account),
            PeerPinStore.StartedOver(newKeyB64: new, keyGeneration: 2, signedAtMs: 5))
        // The old key closes its one-to-one; it is NOT a superseded key
        // (nothing is returned to the new account on its strength).
        XCTAssertEqual(store.loadStartedOverKeys(), [old: account])
        XCTAssertTrue(store.isStartedOverKey(old))
        XCTAssertFalse(store.isStartedOverKey(new))
        XCTAssertNil(store.loadSupersededKeys()[old])

        // Seen again: nothing to say twice.
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: account, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5),
            .already)
        // A later unsealed reset replaces the pending one; an older does not.
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: account, heldKeyB64: old, newKeyB64: newer, keyGeneration: 3, signedAtMs: 9),
            .recorded)
        XCTAssertEqual(store.startedOver(accountIdB64: account)?.newKeyB64, newer)
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: account, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5),
            .ignored)
        XCTAssertEqual(store.loadStartedOverKeys(), [old: account], "the old key is listed once")
    }

    func testNothingIsRecordedForAnAccountThisDeviceDoesNotPinOrAnotherHeldKey() {
        let store = pins()
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: "b3RoZXI=", heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5),
            .ignored, "a group member known only by a verified binding")
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: account, heldKeyB64: newer, newKeyB64: new, keyGeneration: 2, signedAtMs: 5),
            .ignored, "the held key is not the pin")
        XCTAssertEqual(
            store.noteStartedOver(
                accountIdB64: account, heldKeyB64: old, newKeyB64: old, keyGeneration: 2, signedAtMs: 5),
            .ignored)
        XCTAssertTrue(store.loadStartedOverKeys().isEmpty)
    }

    /// Accepting the contact request of the new keys is the person's own
    /// decision to trust them: the pin moves then, and only to the keys the
    /// pending reset installed.
    func testAcceptingTheRequestOfTheNewKeysAdoptsThemAndTheOldChatStaysClosed() {
        let store = pins()
        store.noteStartedOver(
            accountIdB64: account, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5)

        XCTAssertFalse(store.adoptStartedOver(accountIdB64: account, newKeyB64: newer, nowMs: 30_000), "another key")
        XCTAssertEqual(store.readPin(accountIdB64: account)?.pinnedSignaturePubkeyB64, old)

        XCTAssertTrue(store.adoptStartedOver(accountIdB64: account, newKeyB64: new, nowMs: 30_000))
        let pin = store.readPin(accountIdB64: account)
        XCTAssertEqual(pin?.pinnedSignaturePubkeyB64, new)
        XCTAssertEqual(store.acceptedGeneration(accountIdB64: account), 2)
        XCTAssertNil(store.startedOver(accountIdB64: account), "nothing is pending any more")
        XCTAssertEqual(store.loadStartedOverKeys(), [old: account], "the old one-to-one stays closed")
        XCTAssertNil(store.loadSupersededKeys()[old], "and is still not the same person's retired key")
        XCTAssertFalse(store.adoptStartedOver(accountIdB64: account, newKeyB64: new, nowMs: 30_000), "once")
    }

    // MARK: - Alignment items D, E, F

    /// Item D: adding the person by name takes their new account as a new
    /// contact; the old one-to-one stays closed; a reset not pending for
    /// those keys refuses.
    func testAddingAPersonWhoStartedOverByNameTakesTheirNewAccount() {
        let store = pins()
        XCTAssertFalse(StartedOverAdd.adopt(
            ContactStartedOver(accountIdB64: account, newKeyB64: new), pins: store, nowMs: 1), "nothing pending")
        store.noteStartedOver(
            accountIdB64: account, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 5)
        XCTAssertFalse(StartedOverAdd.adopt(
            ContactStartedOver(accountIdB64: account, newKeyB64: newer), pins: store, nowMs: 1), "other keys")
        XCTAssertTrue(StartedOverAdd.adopt(
            ContactStartedOver(accountIdB64: account, newKeyB64: new), pins: store, nowMs: 30_000))
        XCTAssertEqual(store.readPin(accountIdB64: account)?.pinnedSignaturePubkeyB64, new)
        XCTAssertEqual(store.loadStartedOverKeys(), [old: account], "the old one-to-one stays closed")
        XCTAssertTrue(SentContactRequest(
            recipientUsername: "x", recipientAccountIdB64: account, messageId: 1, createdAt: "",
            startedOver: true).startedOver == true)
    }

    /// Item E: what waits for the old one-to-one is never sent.
    func testWhatWaitsForTheOldOneToOneIsNotSent() {
        XCTAssertEqual(StartedOverChat.unsent(oneToOne: true, peerKeyB64: old, startedOverKeysB64: [old]), .notSent)
        XCTAssertEqual(StartedOverChat.unsent(oneToOne: true, peerKeyB64: new, startedOverKeysB64: [old]), .send)
        XCTAssertEqual(StartedOverChat.unsent(oneToOne: false, peerKeyB64: old, startedOverKeysB64: [old]), .send)
        XCTAssertEqual(StartedOverChat.unsent(oneToOne: true, peerKeyB64: nil, startedOverKeysB64: [old]), .send)
    }

    /// Item F: the pairing state bundle carries the floors and the closed
    /// keys as the agreed JSON, and the receiving device keeps the higher
    /// floor and adds the closed key it does not have.
    func testThePairingBundleCarriesTheFloorsAndTheStartedOverKeys() throws {
        let id = Data(repeating: 0x0A, count: 16)
        let idB64 = id.base64EncodedString()
        let idHex = id.map { String(format: "%02x", $0) }.joined()
        let old = Data(repeating: 0x11, count: 32).base64EncodedString()
        let new = Data(repeating: 0x22, count: 32).base64EncodedString()
        let sender = pins("sender")
        sender.setPin(accountIdB64: idB64, pinnedSignaturePubkeyB64: old, rotationMarker: 1)
        sender.raiseSealSeqFloor(accountIdB64: idB64, seq: 7)
        sender.noteStartedOver(
            accountIdB64: idB64, heldKeyB64: old, newKeyB64: new, keyGeneration: 2, signedAtMs: 1234)

        var bundle = StateBundle.V4(contacts: [], pins: [], knownGroupIdsB64: [])
        let floors = StateBundle.carriedSealFloors(sender)
        bundle.sealFloors = floors.isEmpty ? nil : floors
        bundle.startedOver = StateBundle.carriedStartedOver(sender)
        XCTAssertEqual(bundle.sealFloors, [.init(accountIdHex: idHex, seq: 7)])
        XCTAssertEqual(bundle.startedOver, [.init(accountIdHex: idHex, oldKeyB64: old, atMs: 1234)])

        let json = try XCTUnwrap(
            JSONSerialization.jsonObject(with: StateBundle.encode(bundle)) as? [String: Any])
        let floor = try XCTUnwrap((json["sealFloors"] as? [[String: Any]])?.first)
        XCTAssertEqual(floor["accountIdHex"] as? String, idHex)
        XCTAssertEqual(floor["seq"] as? Int, 7)
        let closed = try XCTUnwrap((json["startedOver"] as? [[String: Any]])?.first)
        XCTAssertEqual(closed["oldKeyB64"] as? String, old)
        XCTAssertEqual(closed["atMs"] as? Int, 1234)
        let decoded = try XCTUnwrap(StateBundle.decode(StateBundle.encode(bundle)))
        XCTAssertEqual(decoded.startedOver, bundle.startedOver)

        // Absent when empty.
        let empty = try XCTUnwrap(JSONSerialization.jsonObject(
            with: StateBundle.encode(StateBundle.V4(contacts: [], pins: [], knownGroupIdsB64: []))) as? [String: Any])
        XCTAssertNil(empty["sealFloors"])
        XCTAssertNil(empty["startedOver"])

        let receiver = pins("receiver")
        receiver.setPin(accountIdB64: idB64, pinnedSignaturePubkeyB64: old, rotationMarker: 1)
        receiver.raiseSealSeqFloor(accountIdB64: idB64, seq: 9)
        XCTAssertEqual(StateBundle.applySealFacts(decoded, pins: receiver), 1, "the closed key only")
        XCTAssertEqual(receiver.sealSeqFloor(accountIdB64: idB64), 9, "the higher seq is kept")
        XCTAssertEqual(receiver.loadStartedOverKeys(), [old: idB64])
        XCTAssertEqual(StateBundle.applySealFacts(decoded, pins: receiver), 0, "nothing twice")
        // A malformed entry refuses the whole bundle (the deviceCards rule,
        // as on Android).
        func refused(_ change: (inout StateBundle.V4) -> Void) -> Bool {
            var junk = decoded
            change(&junk)
            return StateBundle.decode(StateBundle.encode(junk)) == nil
        }
        XCTAssertTrue(refused { $0.startedOver = [.init(accountIdHex: "zz", oldKeyB64: old, atMs: 1)] })
        XCTAssertTrue(refused { $0.startedOver = [.init(accountIdHex: idHex, oldKeyB64: "AAEC", atMs: 1)] })
        XCTAssertTrue(refused { $0.startedOver = [.init(accountIdHex: idHex, oldKeyB64: old, atMs: -1)] })
        XCTAssertTrue(refused { $0.sealFloors = [.init(accountIdHex: idHex.uppercased(), seq: 1)] })
        XCTAssertTrue(refused { $0.sealFloors = [.init(accountIdHex: idHex, seq: 1), .init(accountIdHex: idHex, seq: 2)] })
        XCTAssertTrue(refused {
            $0.sealFloors = (0...StateBundle.maxSealFloors).map {
                .init(accountIdHex: String(format: "%032lx", $0), seq: 1)
            }
        })
        XCTAssertFalse(refused { _ in }, "the well-formed bundle decodes")
    }

    /// UX 16.11 follow-up: the old conversation's person (a started-over
    /// key) is offered no `Add to contacts`, also after the contact was
    /// removed (the removed-person card offers it otherwise).
    func testTheCardOfAStartedOverKeyOffersNoAddToContacts() {
        let removed = personInfoSections(caps: PersonInfoCapabilities(
            membershipEnded: false, isContact: false, contactsOfContactsOn: false, isBlocked: false))
        XCTAssertTrue(removed.contains(.addContact), "a removed person is offered it")
        XCTAssertFalse(StartedOverCard.sections(removed, keyStartedOver: true).contains(.addContact))
        XCTAssertEqual(StartedOverCard.sections(removed, keyStartedOver: false), removed)
        XCTAssertEqual(
            StartedOverCard.sections(removed, keyStartedOver: true),
            removed.filter { $0 != .addContact && $0 != .call }, "nothing else changes but the call")
    }

    // MARK: - The two surfaces (UX 16.11)

    func testOnlyARequestFromTheNewKeysCarriesTheNoteAndAdoptsThem() {
        XCTAssertTrue(StartedOverRequest.isFromNewKeys(requestKeyB64: new, newKeyB64: new))
        XCTAssertFalse(StartedOverRequest.isFromNewKeys(requestKeyB64: old, newKeyB64: new))
        XCTAssertFalse(StartedOverRequest.isFromNewKeys(requestKeyB64: "", newKeyB64: ""))
        XCTAssertTrue(StartedOverRequest.adoptsNewKeys(requestKeyB64: new, pendingNewKeyB64: new))
        XCTAssertFalse(StartedOverRequest.adoptsNewKeys(requestKeyB64: new, pendingNewKeyB64: nil))
        XCTAssertFalse(StartedOverRequest.adoptsNewKeys(requestKeyB64: old, pendingNewKeyB64: new))
        XCTAssertNotEqual(Localization.text(StartedOverRequest.noteKey), StartedOverRequest.noteKey)
    }

    func testTheOldOneToOneLosesItsComposerAndItsCallButtons() {
        XCTAssertEqual(
            StartedOverChat.bottom(oneToOne: true, peerKeyB64: old, startedOverKeysB64: [old]), .closedLine)
        XCTAssertEqual(
            StartedOverChat.bottom(oneToOne: true, peerKeyB64: new, startedOverKeysB64: [old]), .composer,
            "the new one-to-one is an ordinary chat")
        XCTAssertEqual(
            StartedOverChat.bottom(oneToOne: false, peerKeyB64: old, startedOverKeysB64: [old]), .composer,
            "a group is never closed for a member who started over")
        XCTAssertEqual(
            StartedOverChat.bottom(oneToOne: true, peerKeyB64: nil, startedOverKeysB64: [old]), .composer)

        // The line names the person; the composer and its draft are gone.
        let notice = Localization.text(StartedOverChat.closedKey, texts: ["name": "Anya"])
        XCTAssertTrue(notice.contains("Anya"))
        XCTAssertEqual(
            ChatViewModel.composerSlot(peerDeleted: false, startedOverNotice: notice, membership: .active, draft: "hi"),
            .withdrawn(notice: notice, draft: nil, draftLabel: nil))
        XCTAssertEqual(
            ChatViewModel.composerSlot(peerDeleted: false, startedOverNotice: nil, membership: .active, draft: "hi"),
            .composer)
        // A deleted account's line wins (there is nobody at all).
        XCTAssertEqual(
            ChatViewModel.composerSlot(peerDeleted: true, startedOverNotice: notice, membership: .active, draft: ""),
            .withdrawn(notice: ResolvedDisplayName.deletedBody, draft: nil, draftLabel: nil))
        XCTAssertFalse(ChatViewModel.hasSomeoneToCall(
            peerDeleted: false, peerStartedOver: true, isGroup: false, people: 2))
        XCTAssertTrue(ChatViewModel.hasSomeoneToCall(
            peerDeleted: false, peerStartedOver: false, isGroup: false, people: 2))
    }

    // MARK: - Saving a phrase changes no key (UX 19.5)

    func testSavingAPhraseShowsTheWordsTheDeviceHoldsAndCountsOnlyOncePublished() async {
        var published = 0
        var publishes = [false, true]
        let flow = SavePhraseFlow(ports: .init(
            heldPhrase: { "one two three" },
            publishPhraseSeal: {
                published += 1
                return publishes.removeFirst()
            }))
        XCTAssertEqual(flow.state, .idle)
        // Not before the words are on screen.
        let early = await flow.confirmHeldWords()
        XCTAssertFalse(early)
        XCTAssertEqual(published, 0)

        flow.primary()
        XCTAssertEqual(flow.state, .heldWords("one two three"), "the words appear at once: no key changes")

        // The three-word check passed and the publish did not land: the
        // screen stays, with Try again; nothing was saved.
        let first = await flow.confirmHeldWords()
        XCTAssertFalse(first)
        XCTAssertTrue(flow.confirmFailed)
        XCTAssertEqual(flow.state, .heldWords("one two three"))
        let second = await flow.confirmHeldWords()
        XCTAssertTrue(second)
        XCTAssertFalse(flow.confirmFailed)
        XCTAssertEqual(published, 2)
    }

    func testADeviceThatDoesNotHoldTheAccountsWordsSavesNothing() {
        let flow = SavePhraseFlow(ports: .init(heldPhrase: { nil }, publishPhraseSeal: { true }))
        flow.primary()
        XCTAssertEqual(flow.state, .failed)
    }
}

/// The four defects of the unsealed-reset walk of 2026-11-28 (UX
/// `identity-recovery.md` 16.11, ADR-0101 section 11, ADR-0079): the old
/// contact counts only for its own key, so the new account is added as a new
/// contact; a sealed reset leaves one row; the old card offers no call; the
/// old row says the account started over.
@MainActor
final class StartedOverContactsTests: XCTestCase {

    private let account = "started-over-contacts-account"
    private var suiteName = ""
    private let anyaId = Data(repeating: 0xA1, count: 16)
    private let oldKey = Data(repeating: 0x01, count: 32)
    private let newKey = Data(repeating: 0x02, count: 32)

    override func setUp() {
        super.setUp()
        suiteName = "started-over-contacts-\(UUID().uuidString)"
        AcceptedContactsStore.defaults = UserDefaults(suiteName: suiteName)!
        AcceptedContactsStore.bind(accountIdB64: account)
        PeerPinStore.clear(ownerAccountIdB64: account)
        ContactsInboxStore().clear(accountIdB64: account)
        KeychainSentRequestsStore().clear(accountIdB64: account)
    }

    override func tearDown() {
        PeerPinStore.clear(ownerAccountIdB64: account)
        ContactsInboxStore().clear(accountIdB64: account)
        KeychainSentRequestsStore().clear(accountIdB64: account)
        KnownRequestWaitingStore().clear(accountIdB64: account)
        UserDefaults().removePersistentDomain(forName: suiteName)
        AcceptedContactsStore.defaults = .standard
        AcceptedContactsStore.bind(accountIdB64: nil)
        super.tearDown()
    }

    private func request(_ id: Int64, key: Data, accountId: Data?) -> IncomingContactRequest {
        IncomingContactRequest(
            messageId: id, fromPubkey: key, fromAccountId: accountId,
            fromPseudonym: Data(count: 32), validAfterMs: 0, intro: nil,
            createdAt: "2026-11-28T10:00:00Z")
    }

    private func row(_ id: Int64, key: Data, accountId: Data?, previous: [Data] = []) -> AcceptedContact {
        AcceptedContact(
            request: request(id, key: key, accountId: accountId),
            groupIdHex: nil, displayName: nil,
            previousKeysB64: previous.isEmpty ? nil : previous.map(base64Encode))
    }

    private func makeRepository() -> ContactsRepository {
        let repository = ContactsRepository(
            authRepository: AuthRepository.offline(
                authClientFactory: { Offline.auth() },
                persistence: StartedOverNoSession(),
                nowMs: { 1_000_000 }),
            accountBinding: Just<String?>(account).eraseToAnyPublisher())
        // The offline auth repository has no session and unbinds the stores
        // the pin store keys on: bound again to the account under test.
        AcceptedContactsStore.bind(accountIdB64: account)
        return repository
    }

    /// Anya's old row, pinned under the old key, and her unsealed reset
    /// recorded here.
    private func startedOverRepository() -> ContactsRepository {
        AcceptedContactsStore.save([row(7, key: oldKey, accountId: anyaId)], accountIdB64: account)
        let pins = PeerPinStore()
        pins.setPin(
            accountIdB64: base64Encode(anyaId), pinnedSignaturePubkeyB64: base64Encode(oldKey),
            rotationMarker: 10, keyGeneration: 1)
        XCTAssertEqual(
            pins.noteStartedOver(
                accountIdB64: base64Encode(anyaId), heldKeyB64: base64Encode(oldKey),
                newKeyB64: base64Encode(newKey), keyGeneration: 2, signedAtMs: 5),
            .recorded)
        return makeRepository()
    }

    // MARK: - a1: the invite page

    func testARowOfAKeyThatStartedOverCountsOnlyForItsOwnKey() {
        let old = KnownContactRequest.ContactRow(
            signaturePubkeyB64: "old", accountIdHex: "a1")
        let verified = KnownContactRequest.Verified(pinKeysB64: ["old"], bindingKeyB64: nil)
        XCTAssertNil(KnownContactRequest.establishedContactOfAccount(
            accepted: [old], accountIdHex: "a1", verified: verified,
            presentedKeyB64: "new", startedOverKeysB64: ["old"]), "the new account is not this contact")
        XCTAssertEqual(KnownContactRequest.establishedContactOfAccount(
            accepted: [old], accountIdHex: "a1", verified: verified,
            presentedKeyB64: "old", startedOverKeysB64: ["old"]), old, "its own key still is")
        XCTAssertEqual(KnownContactRequest.establishedContactOfAccount(
            accepted: [old], accountIdHex: "a1", verified: verified,
            presentedKeyB64: "new", startedOverKeysB64: []), old, "no unsealed reset: by the account, as before")
        XCTAssertTrue(KnownContactRequest.rowCountsFor(
            rowKeyB64: "k", presentedKeyB64: nil, startedOverKeysB64: []))
        XCTAssertFalse(KnownContactRequest.rowCountsFor(
            rowKeyB64: "k", presentedKeyB64: nil, startedOverKeysB64: ["k"]))
    }

    func testTheNewAccountsLinkIsNotAnEstablishedContactHere() {
        let repository = startedOverRepository()
        XCTAssertEqual(repository.state.accepted.count, 1)
        XCTAssertNil(
            repository.establishedContact(ofAccountId: anyaId, presentedKey: newKey),
            "Add, not \"already in your contacts\"")
        XCTAssertEqual(
            repository.establishedContact(ofAccountId: anyaId, presentedKey: oldKey)?.request.messageId, 7)
    }

    func testAPendingContactIsTheInviterOnlyUnderTheLinksKey() {
        let pending = PendingContact(
            accountIdHex: "a1", signaturePubkey: oldKey, x25519Pubkey: Data(count: 32),
            displayName: "Anya", username: "anya", addedAtMs: 1, linkId: Data(count: 16))
        XCTAssertTrue(ContactsRepository.pendingIsInviter(pending, accountIdHex: "a1", signatureKey: oldKey))
        XCTAssertFalse(ContactsRepository.pendingIsInviter(pending, accountIdHex: "a1", signatureKey: newKey))
        XCTAssertFalse(ContactsRepository.pendingIsInviter(pending, accountIdHex: "b2", signatureKey: oldKey))
    }

    // MARK: - a2: the request from the new keys

    func testARequestFromTheNewKeysIsAStrangersAndIsNotSwallowed() async {
        let repository = startedOverRepository()
        let fromNew = request(40, key: newKey, accountId: anyaId)
        let handled = await repository.handleKnownContactRequest(
            fromNew, client: ContactsClient(pinStore: PeerPinStore()))
        XCTAssertFalse(handled, "listed (or decided through the link), never answered or dropped as the old contact")
        XCTAssertTrue(
            KnownRequestWaitingStore().load(accountIdB64: account).isEmpty, "not held as a known contact's")
    }

    func testAcceptingTheNewKeysTakesThemAndLeavesTheOldRowAlone() {
        let repository = startedOverRepository()
        repository.adoptStartedOverKeys(of: request(40, key: newKey, accountId: anyaId))
        let pins = PeerPinStore()
        XCTAssertEqual(pins.readPin(accountIdB64: base64Encode(anyaId))?.pinnedSignaturePubkeyB64, base64Encode(newKey))
        XCTAssertTrue(pins.isStartedOverKey(base64Encode(oldKey)), "the old one-to-one stays closed")
        // A new row for the new keys; the old row untouched (never re-keyed).
        let accepted = ContactsRepository.accepting(
            request(40, key: newKey, accountId: anyaId), into: repository.state.accepted, groupIdHex: "new")
        XCTAssertEqual(accepted.map(\.request.fromPubkey), [oldKey, newKey])
        XCTAssertEqual(accepted.first, repository.state.accepted.first)
        // Afterwards the old row establishes nothing, the new one does.
        XCTAssertNil(repository.establishedContact(ofAccountId: anyaId, presentedKey: oldKey))
        // A later reset of the NEW account never re-keys the old row.
        let rekeyed = ContactRekey.afterReset(
            accepted: [row(7, key: oldKey, accountId: anyaId)] + [row(40, key: newKey, accountId: anyaId)],
            accountIdB64: base64Encode(anyaId), previousKeyB64: base64Encode(Data(repeating: 9, count: 32)),
            newKey: Data(repeating: 3, count: 32), startedOverKeysB64: [base64Encode(oldKey)])
        XCTAssertEqual(rekeyed.map(\.request.fromPubkey), [oldKey, Data(repeating: 3, count: 32)])
    }

    func testARequestFromAnyOtherKeyIsNotMarkedAsTheNewAccount() {
        XCTAssertFalse(StartedOverRequest.adoptsNewKeys(
            requestKeyB64: base64Encode(oldKey), pendingNewKeyB64: base64Encode(newKey)))
    }

    // MARK: - b: one row after a sealed reset

    func testTheOneToOneOfTheKeyBeforeASealedResetAddsNoSecondRow() {
        // The row re-keyed by the sealed reset, the old key an alias of it.
        let rekeyed = row(7, key: newKey, accountId: anyaId, previous: [oldKey])
        XCTAssertNil(ContactsRepository.adopting(
            [rekeyed], signatureKey: oldKey, accountId: anyaId, groupIdHex: "old-chat",
            nowMs: 5, createdAt: "2026-11-28T10:00:00Z"), "the old one-to-one is the same person")
    }

    func testASecondRowLeftByASealedResetFoldsIntoThePersonsRow() {
        let rekeyed = row(7, key: newKey, accountId: anyaId, previous: [oldKey])
        let stray = AcceptedContact(
            request: request(ContactsRepository.syntheticContactMessageId(signatureKey: oldKey),
                             key: oldKey, accountId: nil),
            groupIdHex: "old-chat", displayName: "Anya Seal")
        for rows in [[rekeyed, stray], [stray, rekeyed]] {
            let out = ContactsRepository.reconciled(accepted: rows, pending: []).accepted
            XCTAssertEqual(out.count, 1, "one person, one row")
            XCTAssertEqual(out.first?.request.fromPubkey, newKey, "under the key of after the reset")
            XCTAssertEqual(out.first?.request.messageId, 7)
            XCTAssertEqual(out.first?.displayName, "Anya Seal", "the rename is kept")
            XCTAssertEqual(out.first?.previousKeysB64, [base64Encode(oldKey)])
        }
    }

    func testTheOldRowOfAnUnsealedResetAndTheNewRowStayTwo() {
        let old = row(7, key: oldKey, accountId: anyaId)
        let new = row(40, key: newKey, accountId: anyaId)
        XCTAssertEqual(ContactsRepository.reconciled(accepted: [old, new], pending: []).accepted, [old, new])
        XCTAssertNotNil(ContactsRepository.adopting(
            [new], signatureKey: oldKey, accountId: anyaId, groupIdHex: "old-chat",
            nowMs: 5, createdAt: "2026-11-28T10:00:00Z"), "the old account keeps its own row")
    }

    // MARK: - b: the row's second line

    func testTheOldRowSaysTheAccountStartedOver() {
        XCTAssertEqual(StartedOverRow.secondLine(usernameLine: "@anya", keyStartedOver: true), .startedOver)
        XCTAssertEqual(StartedOverRow.secondLine(usernameLine: "@anya", keyStartedOver: false), .username("@anya"))
        XCTAssertEqual(StartedOverRow.secondLine(usernameLine: nil, keyStartedOver: false), .none)
        XCTAssertNotEqual(Localization.text(StartedOverRow.lineKey), StartedOverRow.lineKey, "the key is in the catalog")
    }

    // MARK: - c: the old card

    func testTheOldCardHasNoCallAndOffersDeleteChat() {
        let caps = PersonInfoCapabilities(
            membershipEnded: StartedOverCard.membershipEnded(chatEnded: false, keyStartedOver: true),
            isContact: true, contactsOfContactsOn: false, isBlocked: false)
        let sections = StartedOverCard.sections(personInfoSections(caps: caps), keyStartedOver: true, hasChat: true)
        XCTAssertFalse(sections.contains(.call))
        XCTAssertTrue(sections.contains(.message), "Message opens the closed history")
        XCTAssertTrue(sections.contains(.safetyNumber))
        XCTAssertTrue(sections.contains(.renameContact))
        XCTAssertTrue(sections.contains(.block))
        XCTAssertTrue(sections.contains(.removeContact))
        XCTAssertTrue(sections.contains(.deleteChat))
        XCTAssertFalse(
            StartedOverCard.sections(personInfoSections(caps: caps), keyStartedOver: true, hasChat: false)
                .contains(.deleteChat), "no one-to-one here, nothing to delete")
        XCTAssertFalse(StartedOverCard.membershipEnded(chatEnded: false, keyStartedOver: false))
        // The conversation header of the closed one-to-one has no call buttons.
        XCTAssertFalse(ChatViewModel.hasSomeoneToCall(
            peerDeleted: false, peerStartedOver: true, isGroup: false, people: 2))
    }
}

/// A ``SessionPersistence`` that holds nothing, so building an
/// ``AuthRepository`` here never reads or writes the real Keychain.
private struct StartedOverNoSession: SessionPersistence {
    func hasDeviceKey(accountIdB64: String) -> Bool { false }
    func accountIdentitySeed(accountIdB64: String) -> Data? { nil }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { nil }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {}
    func deleteSessionToken(accountIdB64: String) {}
}

/// ADR-0101 sections 10-11 / UX 16.11 (2026-11-28) — nothing moves to an
/// account after an UNSEALED reset: every action a peer takes toward a reset
/// person (channel returns, continuity-mailbox writes, successors, history
/// answers) reads ``PeerPinStore/sealedResetInForce(_:)``, which holds only
/// while a SEALED reset reached the accepted generation and the account never
/// started over. Seen live: after the person's new keys were accepted, the
/// earlier sealed reset's window made a channel return go to the new account.
@MainActor
final class UnsealedResetMovesNothingTests: XCTestCase {

    private let account = "YWNjdA=="
    private let k0 = base64Encode(Data(repeating: 0x10, count: 32))
    private let k1 = base64Encode(Data(repeating: 0x11, count: 32))
    private let k2 = base64Encode(Data(repeating: 0x12, count: 32))
    private let now: Int64 = 1_800_000_000_000
    private var suite = ""

    override func setUp() {
        super.setUp()
        suite = "lakko.tests.unsealed-moves-nothing.\(UUID().uuidString)"
    }

    override func tearDown() {
        UserDefaults().removePersistentDomain(forName: suite)
        super.tearDown()
    }

    private func pins() -> PeerPinStore {
        let store = PeerPinStore(defaults: UserDefaults(suiteName: suite)!, ownerAccountIdB64: "b3duZXI=")
        store.setPin(accountIdB64: account, pinnedSignaturePubkeyB64: k0, rotationMarker: 10, keyGeneration: 1)
        return store
    }

    private func sealedReset(_ store: PeerPinStore, held: String, new: String, generation: UInt64) {
        XCTAssertEqual(
            store.acceptReset(PeerPinStore.AcceptedReset(
                accountIdB64: account, heldKeyB64: held, newKeyB64: new, keyGeneration: generation,
                signedAtMs: now - 1000, nowMs: now, announcementSeq: 1)),
            .accepted)
    }

    func testASealedResetStillProducesTheReturn() {
        let store = pins()
        sealedReset(store, held: k0, new: k1, generation: 2)
        XCTAssertEqual(
            store.sealedResetInForce(accountIdB64: account),
            PeerPinStore.SealedReset(keyGeneration: 2, signedAtMs: now - 1000))
        XCTAssertEqual(ChannelReturnRuntime.liveGeneration(account, pins: store, nowMs: now), 2)
        XCTAssertEqual(ChannelReturnRuntime.liveSupersededKeys(pins: store, nowMs: now), [k0: account])
        XCTAssertEqual(
            ReachCenter.shared.acceptedContinuity(ofNewKey: base64Decode(k1)!, nowMs: now, pins: store)?.keyGeneration,
            2, "the continuity mailbox is written to")
    }

    func testNoVerdictOrARefusedResetProducesNothing() {
        // A first pin at a generation, no reset accepted (refused, or no
        // verdict yet: `acceptReset` runs only on a sealed verdict).
        let store = pins()
        XCTAssertNil(store.sealedResetInForce(accountIdB64: account))
        XCTAssertNil(ChannelReturnRuntime.liveGeneration(account, pins: store, nowMs: now))
        XCTAssertTrue(ChannelReturnRuntime.liveSupersededKeys(pins: store, nowMs: now).isEmpty)
        XCTAssertNil(ReachCenter.shared.acceptedContinuity(ofNewKey: base64Decode(k0)!, nowMs: now, pins: store))
    }

    func testAnUnsealedResetProducesNothing() {
        let store = pins()
        store.noteStartedOver(accountIdB64: account, heldKeyB64: k0, newKeyB64: k1, keyGeneration: 2, signedAtMs: now)
        XCTAssertNil(store.sealedResetInForce(accountIdB64: account))
        XCTAssertNil(ChannelReturnRuntime.liveGeneration(account, pins: store, nowMs: now))
        XCTAssertNil(ReachCenter.shared.acceptedContinuity(ofNewKey: base64Decode(k1)!, nowMs: now, pins: store))
    }

    /// The walk of 2026-11-28: a sealed reset (k0 -> k1), then an unsealed one
    /// (k1 -> k2), then the new keys accepted as a new contact. The earlier
    /// sealed reset's window must not make anything go to the new account.
    func testAStartedOverAccountProducesNothingAfterItsNewKeysAreAccepted() {
        let store = pins()
        sealedReset(store, held: k0, new: k1, generation: 2)
        store.noteStartedOver(accountIdB64: account, heldKeyB64: k1, newKeyB64: k2, keyGeneration: 3, signedAtMs: now)
        XCTAssertNil(store.sealedResetInForce(accountIdB64: account), "a pending unsealed reset")
        XCTAssertTrue(store.adoptStartedOver(accountIdB64: account, newKeyB64: k2, nowMs: now))
        XCTAssertEqual(store.readPin(accountIdB64: account)?.acceptedKeyGeneration, 3)
        XCTAssertNil(store.sealedResetInForce(accountIdB64: account), "the adopted generation is no sealed one")
        XCTAssertNil(ChannelReturnRuntime.liveGeneration(account, pins: store, nowMs: now))
        XCTAssertTrue(
            ChannelReturnRuntime.liveSupersededKeys(pins: store, nowMs: now).isEmpty, "no channel return")
        XCTAssertTrue(store.loadSupersededKeysOfSealedResets().isEmpty, "no successor, no profile carried")
        XCTAssertEqual(store.loadSupersededKeys(), [k0: account], "the retired key is still refused")
        XCTAssertNil(
            ReachCenter.shared.acceptedContinuity(ofNewKey: base64Decode(k2)!, nowMs: now, pins: store),
            "nothing is written to its continuity mailbox")
    }

    /// Once the new keys were accepted, the account is a new relationship: a
    /// LATER sealed reset of the new account moves things as any sealed reset
    /// does, but only what it retired (never the earlier relationship's keys
    /// or the keys that started over).
    func testALaterSealedResetOfTheNewAccountMovesWhatItRetiredOnly() {
        let store = pins()
        sealedReset(store, held: k0, new: k1, generation: 2)
        store.noteStartedOver(accountIdB64: account, heldKeyB64: k1, newKeyB64: k2, keyGeneration: 3, signedAtMs: now)
        XCTAssertTrue(store.adoptStartedOver(accountIdB64: account, newKeyB64: k2, nowMs: now))
        let k3 = base64Encode(Data(repeating: 0x13, count: 32))
        sealedReset(store, held: k2, new: k3, generation: 4)
        XCTAssertEqual(store.sealedResetInForce(accountIdB64: account)?.keyGeneration, 4)
        XCTAssertEqual(ChannelReturnRuntime.liveGeneration(account, pins: store, nowMs: now), 4)
        XCTAssertEqual(
            ChannelReturnRuntime.liveSupersededKeys(pins: store, nowMs: now), [k2: account],
            "the return for the new account's retired key; k0 and k1 never count again")
        XCTAssertEqual(
            ReachCenter.shared.acceptedContinuity(ofNewKey: base64Decode(k3)!, nowMs: now, pins: store)?.keyGeneration,
            4, "its continuity mailbox is written to")
        XCTAssertEqual(Set(store.loadSupersededKeys().keys), [k0, k2], "both retired keys are still refused")
    }

    func testAProvenRotationAfterASealedResetKeepsItInForce() {
        let store = pins()
        sealedReset(store, held: k0, new: k1, generation: 2)
        store.setPin(accountIdB64: account, pinnedSignaturePubkeyB64: k2, rotationMarker: 99_999_999_999_999)
        XCTAssertEqual(store.sealedResetInForce(accountIdB64: account)?.keyGeneration, 2)
    }

    func testTheOwnUnsealedResetActsOnNothingThatArrives() {
        XCTAssertTrue(ResetContinuityRules.ownResetUnsealed(
            ResetContinuity(expiresAtMs: now, previousKeyB64: k0, keyGeneration: 2, sealed: false)))
        XCTAssertFalse(ResetContinuityRules.ownResetUnsealed(
            ResetContinuity(expiresAtMs: now, previousKeyB64: k0, keyGeneration: 2, sealed: true)))
        XCTAssertFalse(ResetContinuityRules.ownResetUnsealed(
            ResetContinuity(expiresAtMs: now, previousKeyB64: k0, keyGeneration: 2)), "a record that does not say")
        XCTAssertFalse(ResetContinuityRules.ownResetUnsealed(nil))
        let defaults = UserDefaults(suiteName: suite)!
        let records = ResetContinuityStore(defaults: defaults)
        records.write(
            ResetContinuity(expiresAtMs: now, previousKeyB64: k0, keyGeneration: 2, sealed: true),
            accountIdB64: account)
        records.markUnsealed(accountIdB64: account)
        XCTAssertEqual(records.read(accountIdB64: account)?.sealed, false, "the seal was refused at publish")
    }
}
