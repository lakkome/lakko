// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import XCTest
@testable import Lakko

/// The accepted-contact list and its identity pins belong to an ACCOUNT, not
/// to the device (found live on web 2026-09-30: an account signed in where
/// another had been treated that account's contacts as its own; the iOS store
/// had the same single device-wide key).
@MainActor
final class ContactsPerAccountTests: XCTestCase {

    private let alice = "account-alice"
    private let bob = "account-bob"
    private var suiteName = ""
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suiteName = "contacts-per-account-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)!
        AcceptedContactsStore.defaults = defaults
        AcceptedContactsStore.bind(accountIdB64: nil)
    }

    override func tearDown() {
        AcceptedContactsStore.bind(accountIdB64: nil)
        AcceptedContactsStore.defaults = .standard
        UserDefaults().removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    // MARK: - The store

    func testTwoAccountsInOneStoreNeverSeeEachOthersContacts() {
        AcceptedContactsStore.save([contact(1, byte: 0xA1, name: "Dan")], accountIdB64: alice)
        AcceptedContactsStore.save([contact(2, byte: 0xB2)], accountIdB64: bob)

        XCTAssertEqual(AcceptedContactsStore.load(accountIdB64: alice).map(\.displayName), ["Dan"])
        XCTAssertEqual(AcceptedContactsStore.load(accountIdB64: bob).map(\.request.messageId), [2])

        // Through the bound account, which is how most callers read.
        AcceptedContactsStore.bind(accountIdB64: bob)
        XCTAssertEqual(AcceptedContactsStore.load().map(\.request.messageId), [2])
        AcceptedContactsStore.save([contact(3, byte: 0xC3)])
        XCTAssertEqual(AcceptedContactsStore.load(accountIdB64: alice).map(\.request.messageId), [1],
                       "a write for the bound account never reaches another account's list")
    }

    func testAReadBeforeBindingIsEmptyAndAWriteIsDropped() {
        AcceptedContactsStore.save([contact(1, byte: 0xA1)], accountIdB64: alice)

        XCTAssertNil(AcceptedContactsStore.boundAccountIdB64)
        XCTAssertTrue(AcceptedContactsStore.load().isEmpty, "no account, no contacts")

        AcceptedContactsStore.save([contact(9, byte: 0x99)])
        XCTAssertEqual(AcceptedContactsStore.load(accountIdB64: alice).map(\.request.messageId), [1])
        XCTAssertNil(defaults.data(forKey: AcceptedContactsStore.legacyGlobalKey),
                     "an unbound write never falls back to a shared key")
    }

    func testForgetClearsOnlyTheCurrentAccount() {
        AcceptedContactsStore.save([contact(1, byte: 0xA1)], accountIdB64: alice)
        AcceptedContactsStore.save([contact(2, byte: 0xB2)], accountIdB64: bob)
        defaults.set(Data([1]), forKey: AcceptedContactsStore.legacyGlobalKey)

        AcceptedContactsStore.clear(accountIdB64: alice)

        XCTAssertTrue(AcceptedContactsStore.load(accountIdB64: alice).isEmpty)
        XCTAssertEqual(AcceptedContactsStore.load(accountIdB64: bob).map(\.request.messageId), [2])
        XCTAssertNil(defaults.data(forKey: AcceptedContactsStore.legacyGlobalKey))
    }

    // MARK: - The identity pins beside it

    func testPinsAreKeptPerAccount() {
        let alicePins = PeerPinStore(defaults: defaults, ownerAccountIdB64: alice)
        let bobPins = PeerPinStore(defaults: defaults, ownerAccountIdB64: bob)
        alicePins.setPin(accountIdB64: "peer", pinnedSignaturePubkeyB64: "key-a", rotationMarker: 0)

        XCTAssertEqual(alicePins.readPin(accountIdB64: "peer")?.pinnedSignaturePubkeyB64, "key-a")
        XCTAssertNil(bobPins.readPin(accountIdB64: "peer"), "another account starts with no trust decision")

        // A store with no owner follows the bound account, and is empty and
        // write-dropping while nothing is bound.
        let following = PeerPinStore(defaults: defaults)
        XCTAssertNil(following.readPin(accountIdB64: "peer"))
        following.setPin(accountIdB64: "stray", pinnedSignaturePubkeyB64: "k", rotationMarker: 0)
        XCTAssertNil(alicePins.readPin(accountIdB64: "stray"))
        XCTAssertNil(bobPins.readPin(accountIdB64: "stray"))
        XCTAssertNil(defaults.data(forKey: PeerPinStore.legacyGlobalKey))
        AcceptedContactsStore.bind(accountIdB64: alice)
        XCTAssertEqual(following.readPin(accountIdB64: "peer")?.pinnedSignaturePubkeyB64, "key-a")

        PeerPinStore.clear(ownerAccountIdB64: alice, defaults: defaults)
        XCTAssertNil(alicePins.readPin(accountIdB64: "peer"))
        bobPins.setPin(accountIdB64: "peer", pinnedSignaturePubkeyB64: "key-b", rotationMarker: 0)
        PeerPinStore.clear(ownerAccountIdB64: alice, defaults: defaults)
        XCTAssertEqual(bobPins.readPin(accountIdB64: "peer")?.pinnedSignaturePubkeyB64, "key-b",
                       "forget clears only the current account's pins")
    }

    // MARK: - The repository follows the signed-in account

    func testTheRepositoryShowsOnlyTheSignedInAccountsContacts() {
        AcceptedContactsStore.save([contact(1, byte: 0xA1, name: "Dan")], accountIdB64: alice)
        let session = CurrentValueSubject<String?, Never>(nil)
        let repository = makeRepository(session)
        XCTAssertTrue(repository.state.accepted.isEmpty, "before a session there is no list")

        session.send(alice)
        XCTAssertEqual(repository.state.accepted.map(\.displayName), ["Dan"])

        session.send(nil)
        XCTAssertTrue(repository.state.accepted.isEmpty, "logout shows nobody's contacts")
        XCTAssertEqual(AcceptedContactsStore.load(accountIdB64: alice).count, 1,
                       "logout keeps the list for the next sign-in of the same account")

        session.send(bob)
        XCTAssertTrue(repository.state.accepted.isEmpty, "another account never sees Dan")
        XCTAssertEqual(repository.mergeBundledContacts([contact(2, byte: 0xB2)], accountIdB64: bob), 1)
        XCTAssertEqual(repository.mergeBundledContacts([contact(3, byte: 0xC3)], accountIdB64: alice), 0,
                       "contacts filed for another account are not merged into this one")
        XCTAssertEqual(AcceptedContactsStore.load(accountIdB64: bob).map(\.request.messageId), [2])
        XCTAssertEqual(AcceptedContactsStore.load(accountIdB64: alice).map(\.request.messageId), [1])

        session.send(alice)
        XCTAssertEqual(repository.state.accepted.map(\.request.messageId), [1])
    }

    func testAPairingBundleIsFiledUnderTheAccountBeingPaired() {
        AcceptedContactsStore.bind(accountIdB64: bob)
        let bundle = StateBundle.V3(
            contacts: [StateBundle.CarriedContact(
                signaturePubkeyB64: base64Encode(Data(repeating: 0xD4, count: 32)),
                displayName: "Dan",
                acceptedAtMs: 1_700_000_000_000,
                accountIdHex: "d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4")],
            pins: [StateBundle.CarriedPin(
                accountIdB64: "peer", pinnedSignaturePubkeyB64: "key", rotationMarker: 0)],
            knownGroupIdsB64: [])

        let applied = StateBundle.apply(
            bundle, accountIdB64: alice,
            pinStore: PeerPinStore(defaults: defaults, ownerAccountIdB64: alice))

        XCTAssertEqual(applied.contacts, 1)
        XCTAssertEqual(AcceptedContactsStore.load(accountIdB64: alice).map(\.displayName), ["Dan"])
        XCTAssertTrue(AcceptedContactsStore.load(accountIdB64: bob).isEmpty)
        XCTAssertEqual(StateBundle.build(
            accountIdB64: alice,
            pinStore: PeerPinStore(defaults: defaults, ownerAccountIdB64: alice)).contacts.count, 1)
        XCTAssertTrue(StateBundle.build(
            accountIdB64: bob,
            pinStore: PeerPinStore(defaults: defaults, ownerAccountIdB64: bob)).contacts.isEmpty)
    }

    // MARK: - A failed accept

    /// Nothing delivered and no group: the request stays listed, no contact
    /// without a chat, and Accept works again.
    func testAFailedAcceptLeavesTheRequestListed() async {
        let request = contact(7, byte: 0xA1).request
        ContactsInboxStore(defaults: defaults).save(
            PersistedContactsInbox(cursor: 0, pending: [request]), accountIdB64: alice)
        let session = CurrentValueSubject<String?, Never>(alice)
        let repository = makeRepository(session)
        await Task.yield()
        XCTAssertEqual(repository.state.pending.map(\.messageId), [7])

        let first = await repository.acceptIncomingRequest(request)
        XCTAssertEqual(first, .failed)
        XCTAssertEqual(repository.state.pending.map(\.messageId), [7])
        XCTAssertTrue(repository.state.accepted.isEmpty)

        let again = await repository.acceptIncomingRequest(request)
        XCTAssertEqual(again, .failed)
        XCTAssertEqual(repository.state.pending.map(\.messageId), [7])
    }

    // MARK: - Fixtures

    private func makeRepository(_ session: CurrentValueSubject<String?, Never>) -> ContactsRepository {
        ContactsRepository(
            authRepository: AuthRepository.offline(
                authClientFactory: { Offline.auth() },
                persistence: EmptyPersistence(),
                nowMs: { 1_000_000 }),
            inboxStore: ContactsInboxStore(defaults: defaults),
            accountBinding: session.eraseToAnyPublisher())
    }

    private func contact(_ messageId: Int64, byte: UInt8, name: String? = nil) -> AcceptedContact {
        AcceptedContact(
            request: IncomingContactRequest(
                messageId: messageId,
                fromPubkey: Data(repeating: byte, count: 32),
                fromAccountId: Data(repeating: byte, count: 16),
                fromPseudonym: Data(repeating: byte, count: 32),
                validAfterMs: 0,
                intro: nil,
                createdAt: "2026-09-30T00:00:00Z"),
            groupIdHex: nil,
            displayName: name)
    }
}

/// A ``SessionPersistence`` that holds nothing, so no real Keychain is read.
private struct EmptyPersistence: SessionPersistence {
    func hasDeviceKey(accountIdB64: String) -> Bool { false }
    func accountIdentitySeed(accountIdB64: String) -> Data? { nil }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { nil }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {}
    func deleteSessionToken(accountIdB64: String) {}
}
