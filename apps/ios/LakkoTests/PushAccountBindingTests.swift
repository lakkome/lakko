// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0042, "A wake is shown only for the signed-in account (amended
/// 2026-09-30)" — the iOS half.
///
/// Covers the gate rule both background paths share
/// (``PushAccountBinding/boundAccount(wakeDevicePseudonymHex:binding:)``), the
/// persisted binding, the reverse index keyed by `(account, push_gid)` with
/// two accounts in one group, and Log out / Forget device clearing the binding
/// first and removing delivered notifications. The extension's use of the
/// gate is covered in `NsePushDecryptTests`, the VoIP handler's in
/// `MutedCallRingTests`.
@MainActor
final class PushAccountBindingTests: XCTestCase {

    private var suiteName: String!
    private var defaults: UserDefaults!

    private let pseudonym = Data((0..<32).map { UInt8($0) })
    private var pseudonymHex: String { PushAccountBinding.hex(pseudonym) }
    private let accountA = "AAAAAAAAAAAAAAAAAAAAAA=="
    private let accountB = "BBBBBBBBBBBBBBBBBBBBBB=="

    override func setUp() {
        super.setUp()
        suiteName = "push-account-binding-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        UserDefaults(suiteName: "\(suiteName!)-session")?
            .removePersistentDomain(forName: "\(suiteName!)-session")
        defaults = nil
        super.tearDown()
    }

    // MARK: - The gate

    func testNoBindingRefusesEveryWake() {
        XCTAssertNil(PushAccountBinding.boundAccount(wakeDevicePseudonymHex: pseudonymHex, binding: nil))
    }

    func testAWakeForAnotherPseudonymIsRefused() {
        let binding = PushAccountBinding(devicePseudonymHex: pseudonymHex, accountIdB64: accountA)
        let other = String(repeating: "e", count: 64)
        XCTAssertNil(PushAccountBinding.boundAccount(wakeDevicePseudonymHex: other, binding: binding))
    }

    func testAWakeWithoutDOrWithAMalformedDIsRefused() {
        let binding = PushAccountBinding(devicePseudonymHex: pseudonymHex, accountIdB64: accountA)
        XCTAssertNil(PushAccountBinding.boundAccount(wakeDevicePseudonymHex: nil, binding: binding))
        XCTAssertNil(PushAccountBinding.boundAccount(wakeDevicePseudonymHex: "", binding: binding))
        XCTAssertNil(PushAccountBinding.boundAccount(
            wakeDevicePseudonymHex: String(pseudonymHex.dropLast()), binding: binding))
        XCTAssertNil(PushAccountBinding.boundAccount(
            wakeDevicePseudonymHex: String(repeating: "z", count: 64), binding: binding))
    }

    func testTheBoundPseudonymYieldsTheBoundAccount() {
        let binding = PushAccountBinding(devicePseudonymHex: pseudonymHex, accountIdB64: accountA)
        XCTAssertEqual(
            PushAccountBinding.boundAccount(wakeDevicePseudonymHex: pseudonymHex, binding: binding),
            accountA)
        // Normalised hex: case and surrounding whitespace do not matter.
        XCTAssertEqual(
            PushAccountBinding.boundAccount(
                wakeDevicePseudonymHex: " \(pseudonymHex.uppercased())\n", binding: binding),
            accountA)
    }

    func testABindingWithNoAccountRefuses() {
        let binding = PushAccountBinding(devicePseudonymHex: pseudonymHex, accountIdB64: "")
        XCTAssertNil(PushAccountBinding.boundAccount(wakeDevicePseudonymHex: pseudonymHex, binding: binding))
    }

    // MARK: - The persisted binding

    func testTheStoreRoundTripsAndOneBindingReplacesTheLast() {
        let store = PushAccountBindingStore(defaults: defaults)
        XCTAssertNil(store.current())

        store.bind(devicePseudonym: pseudonym, accountIdB64: accountA)
        XCTAssertEqual(
            store.current(),
            PushAccountBinding(devicePseudonymHex: pseudonymHex, accountIdB64: accountA))

        let next = Data(repeating: 0x7F, count: 32)
        store.bind(devicePseudonym: next, accountIdB64: accountB)
        XCTAssertEqual(
            store.current(),
            PushAccountBinding(devicePseudonymHex: PushAccountBinding.hex(next), accountIdB64: accountB),
            "a device has at most one active account")

        store.clear()
        XCTAssertNil(store.current())
    }

    func testTheStoreRefusesAMalformedBinding() {
        let store = PushAccountBindingStore(defaults: defaults)
        store.bind(devicePseudonym: Data(repeating: 1, count: 31), accountIdB64: accountA)
        XCTAssertNil(store.current())
        store.bind(devicePseudonym: pseudonym, accountIdB64: "")
        XCTAssertNil(store.current())
    }

    func testAnUnprovisionedSuiteStoresNothingSoEveryWakeIsRefused() {
        let store = PushAccountBindingStore(defaults: nil)
        store.bind(devicePseudonym: pseudonym, accountIdB64: accountA)
        XCTAssertNil(store.current())
        XCTAssertNil(PushAccountBinding.boundAccount(
            wakeDevicePseudonymHex: pseudonymHex, binding: store.current()))
    }

    // MARK: - The reverse index, per account

    func testTwoAccountsInOneGroupEachKeepTheirOwnRoute() {
        let routes = PushRoutingStore(defaults: defaults)
        let pushGid = String(repeating: "f", count: 64)
        let groupA = String(repeating: "1", count: 64)
        let groupB = String(repeating: "2", count: 64)
        let secret = Data(repeating: 0x44, count: 32)

        routes.putRouting(
            accountIdB64: accountA, groupIdHex: groupA, secret: secret, pushGidHex: pushGid,
            distributed: true, mintEpoch: 0)
        routes.putRouting(
            accountIdB64: accountB, groupIdHex: groupB, secret: secret, pushGidHex: pushGid,
            distributed: true, mintEpoch: 0)

        // The second write did not replace the first: each account reads its own.
        XCTAssertEqual(routes.lookupRoute(accountIdB64: accountA, pushGidHex: pushGid), groupA)
        XCTAssertEqual(routes.lookupRoute(accountIdB64: accountB, pushGidHex: pushGid), groupB)
        // An account with no row misses, whoever else holds one.
        XCTAssertNil(routes.lookupRoute(accountIdB64: "CCCCCCCCCCCCCCCCCCCCCC==", pushGidHex: pushGid))
    }

    func testARouteDeleteIsScopedToTheAccount() {
        let routes = PushRoutingStore(defaults: defaults)
        let pushGid = String(repeating: "f", count: 64)
        let secret = Data(repeating: 0x44, count: 32)
        routes.putRouting(
            accountIdB64: accountA, groupIdHex: "aa", secret: secret, pushGidHex: pushGid,
            distributed: true, mintEpoch: 0)
        routes.putRouting(
            accountIdB64: accountB, groupIdHex: "bb", secret: secret, pushGidHex: pushGid,
            distributed: true, mintEpoch: 0)

        routes.removeRoute(accountIdB64: accountA, pushGidHex: pushGid)
        XCTAssertNil(routes.lookupRoute(accountIdB64: accountA, pushGidHex: pushGid))
        XCTAssertEqual(routes.lookupRoute(accountIdB64: accountB, pushGidHex: pushGid), "bb")

        routes.clear(accountIdB64: accountB)
        XCTAssertNil(routes.lookupRoute(accountIdB64: accountB, pushGidHex: pushGid))
    }

    func testTheLookupIgnoresTheCaseOfThePushGid() {
        let routes = PushRoutingStore(defaults: defaults)
        let pushGid = String(repeating: "f", count: 64)
        routes.putRouting(
            accountIdB64: accountA, groupIdHex: "aa", secret: Data(repeating: 1, count: 32),
            pushGidHex: pushGid, distributed: true, mintEpoch: 0)
        XCTAssertEqual(routes.lookupRoute(accountIdB64: accountA, pushGidHex: pushGid.uppercased()), "aa")
    }

    // MARK: - Log out and Forget device

    func testLogoutClearsTheBindingFirstAndRemovesDeliveredNotifications() {
        let store = PushAccountBindingStore(defaults: defaults)
        store.bind(devicePseudonym: pseudonym, accountIdB64: accountA)
        let persistence = FakePersistence()
        var bindingAtRemoval: PushAccountBinding?? = .none
        var tokensDeletedAtRemoval: [String]?
        var removals = 0
        let repo = makeRepository(persistence: persistence, binding: store) {
            removals += 1
            bindingAtRemoval = .some(store.current())
            tokensDeletedAtRemoval = persistence.deletedTokens
        }
        XCTAssertEqual(repo.signedInAccountIdB64, accountA)

        repo.logout()

        XCTAssertNil(store.current(), "the binding is gone")
        XCTAssertEqual(removals, 1, "delivered notifications removed")
        XCTAssertEqual(bindingAtRemoval, .some(nil), "the binding was cleared before anything else")
        XCTAssertEqual(tokensDeletedAtRemoval, [], "before the session token was dropped")
        XCTAssertNil(repo.signedInAccountIdB64)
    }

    func testForgetDeviceClearsTheBindingFirstAndRemovesDeliveredNotifications() {
        let store = PushAccountBindingStore(defaults: defaults)
        store.bind(devicePseudonym: pseudonym, accountIdB64: accountA)
        let persistence = FakePersistence()
        let sessionStore = makeSessionStore()
        var bindingAtRemoval: PushAccountBinding?? = .none
        var markerAtRemoval: PersistedSession?
        var removals = 0
        let repo = makeRepository(
            persistence: persistence, binding: store, sessionStore: sessionStore
        ) {
            removals += 1
            bindingAtRemoval = .some(store.current())
            markerAtRemoval = sessionStore.load()
        }

        repo.forgetDevice()

        XCTAssertNil(store.current())
        XCTAssertEqual(removals, 1)
        XCTAssertEqual(bindingAtRemoval, .some(nil))
        XCTAssertNotNil(markerAtRemoval, "before the on-device wipe started")
        XCTAssertNil(sessionStore.load(), "the wipe still ran")
    }

    // MARK: - Fixtures

    private final class FakePersistence: SessionPersistence {
        private(set) var deletedTokens: [String] = []
        func hasDeviceKey(accountIdB64: String) -> Bool { true }
        func accountIdentitySeed(accountIdB64: String) -> Data? { Data(repeating: 0x5A, count: 64) }
        func exportKey(accountIdB64: String) -> Data? { Data(repeating: 0xE7, count: 32) }
        func sessionToken(accountIdB64: String) -> PersistedSessionToken? {
            PersistedSessionToken(sessionToken: "tok-live", sessionExpiresAtMs: 2_000_000)
        }
        func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {}
        func deleteSessionToken(accountIdB64: String) { deletedTokens.append(accountIdB64) }
    }

    private final class NoDirectory: AccountDirectory {
        func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
            throw AuthClientError.argument("discovery must not be reached")
        }
        func publishIdentityBundle(
            accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
            leafAuth: Data?, genesisAttest: Data?, sessionToken: String
        ) async throws {
            throw AuthClientError.argument("discovery must not be reached")
        }
    }

    private func makeSessionStore() -> SessionStore {
        let store = SessionStore(defaults: UserDefaults(suiteName: "\(suiteName!)-session")!)
        store.save(PersistedSession(accountIdB64: accountA, username: "alice"))
        return store
    }

    private func makeRepository(
        persistence: FakePersistence,
        binding: PushAccountBindingStore,
        sessionStore: SessionStore? = nil,
        removeDeliveredNotifications: @escaping () -> Void
    ) -> AuthRepository {
        AuthRepository(
            sessionStore: sessionStore ?? makeSessionStore(),
            authClientFactory: { AuthClient(baseURL: "http://auth.test") },
            persistence: persistence,
            nowMs: { 1_000_000 },
            accountDirectory: NoDirectory(),
            pushBinding: binding,
            removeDeliveredNotifications: removeDeliveredNotifications
        )
    }
}
