// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import LakkoCore
import XCTest

@testable import Lakko

// Work started under one signed-in account must not finish as, or into, the
// next one. `BindingEpoch` is the one rule; these drive it on its own and
// through the owners that advance it.

// MARK: - The primitive

@MainActor
final class BindingEpochTests: XCTestCase {

    func testACaptureIsCurrentUntilTheBindingAdvances() {
        let epoch = BindingEpoch()
        let first = epoch.capture()
        XCTAssertTrue(first.isCurrent)
        XCTAssertNoThrow(try first.require())

        epoch.advance()
        XCTAssertFalse(first.isCurrent)
        XCTAssertThrowsError(try first.require()) { XCTAssertTrue($0 is StaleBindingError) }
    }

    func testACaptureTakenAfterAnAdvanceIsItsOwnBinding() {
        let epoch = BindingEpoch()
        let first = epoch.capture()
        epoch.advance()
        let second = epoch.capture()
        XCTAssertFalse(first.isCurrent)
        XCTAssertTrue(second.isCurrent)

        // Signing out and back in to the same account is a new binding too:
        // nothing brings the first one back.
        epoch.advance()
        XCTAssertFalse(first.isCurrent)
        XCTAssertFalse(second.isCurrent)
    }

    func testTwoOwnersDoNotShareAnEpoch() {
        let auth = BindingEpoch()
        let chat = BindingEpoch()
        let held = chat.capture()
        auth.advance()
        XCTAssertTrue(held.isCurrent)
    }

    func testAStaleBindingIsACancellationNotAFailure() {
        XCTAssertTrue(Cancellation.isCancellation(StaleBindingError()))
        XCTAssertTrue(Cancellation.isCancellation(
            ChatError.wrapped("founding", underlying: StaleBindingError())))
        XCTAssertTrue(Cancellation.isCancellation(ContactsError.wrap(StaleBindingError())))
    }
}

// MARK: - The wipe keeps another account's session

final class AccountWipeScopeTests: XCTestCase {

    func testTheSignedInAccountIsWipedWhole() {
        XCTAssertEqual(AccountWipeScope.decide(target: "a", signedIn: "a"), .wholeDevice)
    }

    func testNobodySignedInIsWipedWhole() {
        XCTAssertEqual(AccountWipeScope.decide(target: "a", signedIn: nil), .wholeDevice)
        XCTAssertEqual(AccountWipeScope.decide(target: nil, signedIn: nil), .wholeDevice)
    }

    func testAnotherAccountThatSignedInMeanwhileKeepsItsSession() {
        XCTAssertEqual(AccountWipeScope.decide(target: "a", signedIn: "b"), .otherAccountSignedIn)
    }

    func testNoTargetNeverEndsSomeoneElsesSessionByItself() {
        // No account named and one signed in: the caller falls back to the
        // session record, which is that account's.
        XCTAssertEqual(AccountWipeScope.decide(target: nil, signedIn: "b"), .wholeDevice)
    }
}

// MARK: - A restore overtaken by a sign-out lands nothing

/// Discovery for the restore gates, held open until the test lets it go.
@MainActor
private final class GatedRestoreDirectory: DeviceRestoreDirectory {
    var publishedKey: Data?
    var resolvesTo: Data?
    var holdIdentity = false
    var holdLookup = false
    private(set) var entered: [String] = []
    private var waiting: [CheckedContinuation<Void, Never>] = []

    func release() {
        let parked = waiting
        waiting = []
        parked.forEach { $0.resume() }
    }

    private func park() async {
        await withCheckedContinuation { waiting.append($0) }
    }

    func fetchIdentityBundle(accountId: Data, sessionToken: String?) async throws -> IdentityFetchResponse? {
        entered.append("identity")
        if holdIdentity { await park() }
        guard let publishedKey else { return nil }
        return IdentityFetchResponse(signaturePubkey: base64Encode(publishedKey), x25519Pubkey: "")
    }

    func lookupAccount(username: String, accountId: Data, sessionToken: String) async throws -> Data? {
        entered.append("lookup")
        if holdLookup { await park() }
        return resolvesTo
    }
}

private final class UnusedAccountDirectory: AccountDirectory {
    func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
        throw AuthClientError.argument("not reached")
    }
    func publishIdentityBundle(
        accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
        leafAuth: Data?, genesisAttest: Data?, sessionToken: String
    ) async throws {}
}

private final class RecordingSessionPersistence: SessionPersistence {
    private(set) var tokens: [String: PersistedSessionToken] = [:]
    func hasDeviceKey(accountIdB64: String) -> Bool { false }
    func accountIdentitySeed(accountIdB64: String) -> Data? { nil }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { tokens[accountIdB64] }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws { tokens[accountIdB64] = token }
    func deleteSessionToken(accountIdB64: String) { tokens[accountIdB64] = nil }
}

@MainActor
final class AuthBindingTests: XCTestCase {

    private var accountIdsToClean: [String] = []

    override func tearDown() async throws {
        for b64 in accountIdsToClean {
            DeviceKeyStore.deleteDeviceKey(accountIdB64: b64)
            AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: b64)
            DeviceIdentityStore.deleteDeviceIdentitySeed(accountIdB64: b64)
        }
        accountIdsToClean = []
        try await super.tearDown()
    }

    private struct Harness {
        let repository: AuthRepository
        let store: SessionStore
        let persistence: RecordingSessionPersistence
        let directory: GatedRestoreDirectory
    }

    private func harness() -> Harness {
        let store = SessionStore(defaults: UserDefaults(suiteName: "account-binding-\(UUID().uuidString)")!)
        let persistence = RecordingSessionPersistence()
        let directory = GatedRestoreDirectory()
        let repository = AuthRepository(
            sessionStore: store,
            authClientFactory: { AuthClient(baseURL: "http://auth.test") },
            persistence: persistence,
            nowMs: { 1_000_000 },
            accountDirectory: UnusedAccountDirectory(),
            loginHandleBinds: LoginHandleBindLedger(),
            restoreDirectory: directory,
            pushBinding: PushAccountBindingStore(defaults: nil),
            removeDeliveredNotifications: {}
        )
        return Harness(repository: repository, store: store, persistence: persistence, directory: directory)
    }

    private func randomBytes(_ count: Int) -> Data {
        Data((0..<count).map { _ in UInt8.random(in: 0...255) })
    }

    private func waitUntil(_ condition: () -> Bool, timeout: TimeInterval = 5) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 5_000_000)
        }
        return condition()
    }

    /// Start a restore of a fresh account; returns the task and what it
    /// would have landed.
    private func startRestore(
        _ h: Harness, persistJoin: @escaping () -> Void = {}
    ) throws -> (task: Task<Error?, Never>, accountId: Data) {
        let seed = randomBytes(64)
        let accountId = randomBytes(16)
        accountIdsToClean.append(base64Encode(accountId))
        h.directory.publishedKey = try Identity.fromSeed(seed: seed).signaturePublicKey()
        h.directory.resolvesTo = accountId
        let repository = h.repository
        let task = Task { @MainActor () -> Error? in
            do {
                try await repository.completeFromDeviceRestore(
                    accountIdBytes: accountId,
                    accountIdB64: base64Encode(accountId),
                    username: "alice",
                    phraseSeed: seed,
                    sessionToken: "redeemed-token",
                    sessionExpiresAtMs: 9_000_000,
                    persistJoin: { persistJoin() })
                return nil
            } catch {
                return error
            }
        }
        return (task, accountId)
    }

    private func assertNothingLanded(_ h: Harness, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertNil(h.store.load(), "no account marker", file: file, line: line)
        XCTAssertNil(h.repository.identitySecrets, "no session installed", file: file, line: line)
        XCTAssertTrue(h.persistence.tokens.isEmpty, "no session token persisted", file: file, line: line)
        if case .anon = h.repository.state {} else {
            XCTFail("state is \(h.repository.state)", file: file, line: line)
        }
    }

    func testARestoreOvertakenBySignOutWhileTheIdentityIsFetchedLandsNothing() async throws {
        let h = harness()
        h.directory.holdIdentity = true
        var joined = false
        let (task, _) = try startRestore(h) { joined = true }
        let reached = await waitUntil { h.directory.entered == ["identity"] }
        XCTAssertTrue(reached)

        // The device was signed out (or forgotten) while the fetch was out.
        h.repository.logout()
        h.directory.release()

        let error = await task.value
        XCTAssertTrue(error is StaleBindingError, "\(String(describing: error))")
        XCTAssertEqual(h.directory.entered, ["identity"], "the name check never ran")
        XCTAssertFalse(joined, "the pairing's held state was not written")
        assertNothingLanded(h)
    }

    func testARestoreOvertakenWhileTheNameIsCheckedWritesNothing() async throws {
        let h = harness()
        h.directory.holdLookup = true
        var joined = false
        let (task, _) = try startRestore(h) { joined = true }
        let reached = await waitUntil { h.directory.entered == ["identity", "lookup"] }
        XCTAssertTrue(reached)

        h.repository.endSessionAfterLocalWipe()
        h.directory.release()

        let error = await task.value
        XCTAssertTrue(error is StaleBindingError, "\(String(describing: error))")
        XCTAssertFalse(joined)
        assertNothingLanded(h)
    }

    func testARestoreNobodyOvertookLandsAndIsANewBinding() async throws {
        let h = harness()
        let before = h.repository.binding.capture()
        let (task, accountId) = try startRestore(h)

        let error = await task.value
        XCTAssertNil(error, "\(String(describing: error))")
        XCTAssertEqual(h.repository.identitySecrets?.accountId, accountId)
        XCTAssertFalse(before.isCurrent, "a landing ends whatever was bound before it")

        let session = h.repository.binding.capture()
        h.repository.logout()
        XCTAssertFalse(session.isCurrent, "a sign-out ends the session's binding")
    }
}

// MARK: - The repositories advance their own epoch with the account

private struct NoSessionPersistence: SessionPersistence {
    func hasDeviceKey(accountIdB64: String) -> Bool { false }
    func accountIdentitySeed(accountIdB64: String) -> Data? { nil }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { nil }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {}
    func deleteSessionToken(accountIdB64: String) {}
}

@MainActor
final class RepositoryBindingTests: XCTestCase {

    private var suiteName = ""
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        suiteName = "repository-binding-\(UUID().uuidString)"
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

    private func contacts(_ session: CurrentValueSubject<String?, Never>) -> ContactsRepository {
        ContactsRepository(
            authRepository: AuthRepository(
                authClientFactory: { AuthClient(baseURL: "http://auth.test") },
                persistence: NoSessionPersistence(),
                nowMs: { 1_000_000 }),
            inboxStore: ContactsInboxStore(defaults: defaults),
            accountBinding: session.eraseToAnyPublisher())
    }

    func testTheContactsBindingEndsWhenTheAccountChanges() {
        let session = CurrentValueSubject<String?, Never>("account-alice")
        let repository = contacts(session)
        let alice = repository.binding.capture()
        XCTAssertTrue(alice.isCurrent)

        // The same account announced again is the same binding.
        session.send("account-alice")
        XCTAssertTrue(alice.isCurrent)

        session.send("account-bob")
        XCTAssertFalse(alice.isCurrent, "work started for alice does not write into bob's list")
        let bob = repository.binding.capture()

        session.send(nil)
        XCTAssertFalse(bob.isCurrent, "a sign-out ends it too")
    }

    func testTheContactsBindingEndsOnASignOutAndBackInToTheSameAccount() {
        let session = CurrentValueSubject<String?, Never>("account-alice")
        let repository = contacts(session)
        let first = repository.binding.capture()
        session.send(nil)
        session.send("account-alice")
        XCTAssertFalse(first.isCurrent)
        XCTAssertTrue(repository.binding.capture().isCurrent)
    }

    func testClearingTheContactsRepositoryEndsItsBinding() {
        let session = CurrentValueSubject<String?, Never>("account-alice")
        let repository = contacts(session)
        let held = repository.binding.capture()
        repository.clear()
        XCTAssertFalse(held.isCurrent)
    }

    func testClearingTheChatRepositoryEndsItsBinding() {
        let repository = ChatRepository()
        let held = repository.binding.capture()
        repository.clear()
        XCTAssertFalse(held.isCurrent, "a region queued before a sign-out is not run after it")
        XCTAssertTrue(repository.binding.capture().isCurrent)
    }
}
