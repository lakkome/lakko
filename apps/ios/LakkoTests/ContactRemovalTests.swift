// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

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

    /// The repository rehydrates its accepted list from this store at init,
    /// and writes back through `state`'s `didSet`, so seeding it is how a
    /// test hands the repository contacts, and reading it back is how the
    /// test sees whether the removal was persisted rather than only dropped
    /// from memory.
    private static let acceptedDefaultsKey = "lakko.contacts.accepted.v1"

    override func setUp() {
        super.setUp()
        UserDefaults.standard.removeObject(forKey: Self.acceptedDefaultsKey)
    }

    override func tearDown() {
        UserDefaults.standard.removeObject(forKey: Self.acceptedDefaultsKey)
        super.tearDown()
    }

    func testRemovingAContactDropsItFromTheListAndLeavesTheRest() {
        AcceptedContactsStore.save([contact(1, byte: 0xA1), contact(2, byte: 0xB2)])
        let repository = makeRepository()
        XCTAssertEqual(repository.state.accepted.count, 2)

        repository.removeContact(messageId: 1)

        XCTAssertEqual(repository.state.accepted.map(\.request.messageId), [2])
    }

    /// The list is what the app rehydrates from on the next launch, so a
    /// removal that only happened in memory would come back.
    func testARemovalIsPersisted() {
        AcceptedContactsStore.save([contact(1, byte: 0xA1), contact(2, byte: 0xB2)])
        let repository = makeRepository()

        repository.removeContact(messageId: 1)

        XCTAssertEqual(AcceptedContactsStore.load().map(\.request.messageId), [2])
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
        AcceptedContactsStore.save([withChat, other])
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
        AcceptedContactsStore.save([contact(1, byte: 0xA1)])
        let repository = makeRepository()

        repository.removeContact(messageId: 99)
        repository.removeContact(messageId: 1)
        repository.removeContact(messageId: 1)

        XCTAssertTrue(repository.state.accepted.isEmpty)
        XCTAssertTrue(AcceptedContactsStore.load().isEmpty)
    }

    // MARK: - Fixtures

    private func makeRepository() -> ContactsRepository {
        ContactsRepository(
            authRepository: AuthRepository(
                authClientFactory: { AuthClient(baseURL: "http://auth.test") },
                persistence: NoSessionPersistence(),
                nowMs: { 1_000_000 }))
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
