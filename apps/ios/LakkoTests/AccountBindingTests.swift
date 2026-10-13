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

// MARK: - A fresh identity published after another account signed in

final class FreshIdentityLandingTests: XCTestCase {

    func testTheMintNobodyOvertookShowsItsPhrase() {
        XCTAssertEqual(
            FreshIdentityLanding.decide(mintedFor: "alice", bindingIsCurrent: true, signedIn: "alice"),
            .showPhrase(rebinds: false))
    }

    /// Web's rule: once published, the phrase is shown even after a sign-out.
    func testAfterASignOutThePhraseIsStillShownAndBindsTheDeviceAgain() {
        XCTAssertEqual(
            FreshIdentityLanding.decide(mintedFor: "alice", bindingIsCurrent: false, signedIn: nil),
            .showPhrase(rebinds: true))
    }

    func testTheSameAccountParkedAgainStillSeesItsPhrase() {
        XCTAssertEqual(
            FreshIdentityLanding.decide(mintedFor: "alice", bindingIsCurrent: false, signedIn: "alice"),
            .showPhrase(rebinds: true))
    }

    /// Never one account's phrase screen over another account's session.
    func testAnotherAccountsSessionIsNeverCoveredByThePhraseScreen() {
        XCTAssertEqual(
            FreshIdentityLanding.decide(mintedFor: "alice", bindingIsCurrent: false, signedIn: "bob"),
            .keepForNextSignIn)
    }
}

// MARK: - The VoIP registration across a sign-out and the next sign-in

/// push-gw as the VoIP registration sees it: records every call in arrival
/// order and can hold an unregister open.
@MainActor
private final class GatedVoipGateway {
    private(set) var calls: [String] = []
    private(set) var registered: [Data: Data] = [:]
    var holdUnregister = false
    private var waiting: [CheckedContinuation<Void, Never>] = []

    func release() {
        let parked = waiting
        waiting = []
        parked.forEach { $0.resume() }
    }

    var gateway: VoipPushGateway {
        VoipPushGateway(
            register: { [self] pseudonym, token in
                calls.append("register \(pseudonym.first ?? 0)")
                registered[pseudonym] = token
            },
            unregister: { [self] pseudonym in
                calls.append("unregister \(pseudonym.first ?? 0)")
                if holdUnregister { await withCheckedContinuation { waiting.append($0) } }
                registered[pseudonym] = nil
            })
    }
}

@MainActor
final class VoipTokenRegistrationTests: XCTestCase {

    private var suiteName = ""
    private var store: PushAccountBindingStore!
    private var pseudonym: Data?
    private let first = Data(repeating: 1, count: 32)
    private let second = Data(repeating: 2, count: 32)
    private let token = Data([0xAA, 0xBB])

    override func setUp() {
        super.setUp()
        suiteName = "voip-registration-\(UUID().uuidString)"
        store = PushAccountBindingStore(defaults: UserDefaults(suiteName: suiteName))
        pseudonym = nil
    }

    override func tearDown() {
        UserDefaults().removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    private func registration(_ gateway: GatedVoipGateway) -> VoipTokenRegistration {
        VoipTokenRegistration(
            gateway: gateway.gateway, bindingStore: store,
            devicePseudonym: { [unowned self] in self.pseudonym })
    }

    /// What a sign-in does to the push state before it registers.
    private func signIn(_ devicePseudonym: Data, account: String) {
        pseudonym = devicePseudonym
        store.bind(devicePseudonym: devicePseudonym, accountIdB64: account)
    }

    private func settle() async {
        for _ in 0..<20 { await Task.yield() }
    }

    func testATokenDeliveredWithNobodySignedInIsHeldForTheSignIn() async {
        let gateway = GatedVoipGateway()
        let registration = registration(gateway)

        XCTAssertNil(registration.tokenDelivered(token), "nothing is registered for nobody")
        XCTAssertEqual(gateway.calls, [])

        signIn(first, account: "alice")
        await registration.registerForSignIn(devicePseudonym: first)
        XCTAssertEqual(gateway.calls, ["register 1"])
        XCTAssertEqual(gateway.registered[first], token)
    }

    func testARestoredSessionRegistersWhenTheTokenArrives() async {
        let gateway = GatedVoipGateway()
        let registration = registration(gateway)
        signIn(first, account: "alice")

        await registration.tokenDelivered(token)?.value
        XCTAssertEqual(gateway.calls, ["register 1"])

        // The same token again, for the same sign-in: not sent twice.
        await registration.tokenDelivered(token)?.value
        await registration.registerForSignIn(devicePseudonym: first)
        XCTAssertEqual(gateway.calls, ["register 1"])
    }

    /// The race: Log out's unregister is still in flight when the next
    /// account signs in and registers. It names the old pseudonym, and the
    /// new registration is made after it, under the new one.
    func testALateUnregisterDoesNotRemoveTheNextSignInsRegistration() async {
        let gateway = GatedVoipGateway()
        let registration = registration(gateway)
        signIn(first, account: "alice")
        await registration.tokenDelivered(token)?.value

        // Log out: the binding goes first, the unregister is held open.
        gateway.holdUnregister = true
        store.clear()
        let leaving = Task { await registration.unregister(devicePseudonym: self.first) }
        await settle()
        XCTAssertEqual(gateway.calls, ["register 1", "unregister 1"])

        // The next account signs in while it is still out.
        signIn(second, account: "bob")
        let arriving = Task { await registration.registerForSignIn(devicePseudonym: self.second) }
        await settle()
        XCTAssertEqual(
            gateway.calls, ["register 1", "unregister 1"],
            "the registration waits for the unregister ahead of it")

        gateway.release()
        await leaving.value
        await arriving.value
        XCTAssertEqual(gateway.calls, ["register 1", "unregister 1", "register 2"])
        XCTAssertNil(gateway.registered[first])
        XCTAssertEqual(gateway.registered[second], token, "the same token, under the new pseudonym")
    }

    /// A registration whose sign-in ended while it waited its turn is never
    /// sent.
    func testARegistrationWhoseSignInEndedIsNotSent() async {
        let gateway = GatedVoipGateway()
        let registration = registration(gateway)
        signIn(first, account: "alice")
        await registration.tokenDelivered(token)?.value

        gateway.holdUnregister = true
        store.clear()
        let leaving = Task { await registration.unregister(devicePseudonym: self.first) }
        await settle()
        signIn(second, account: "bob")
        let arriving = Task { await registration.registerForSignIn(devicePseudonym: self.second) }
        await settle()
        // ...and that account signs out again before the queue reaches it.
        store.clear()

        gateway.release()
        await leaving.value
        await arriving.value
        XCTAssertEqual(gateway.calls, ["register 1", "unregister 1"])
        XCTAssertTrue(gateway.registered.isEmpty)
    }

    func testOnlyTheBoundPseudonymIsRegisteredUnder() {
        let binding = PushAccountBinding(
            devicePseudonymHex: PushAccountBinding.hex(first), accountIdB64: "alice")
        XCTAssertTrue(VoipPushRegistration.isBound(devicePseudonym: first, binding: binding))
        XCTAssertFalse(VoipPushRegistration.isBound(devicePseudonym: second, binding: binding))
        XCTAssertFalse(VoipPushRegistration.isBound(devicePseudonym: first, binding: nil))
    }
}

// MARK: - A reach sync left behind by its account

/// The reach routes, with the owner's read held open until released.
@MainActor
private final class GatedReachDs: ReachDsTransport {
    private(set) var reads = 0
    private var waiting: [CheckedContinuation<Void, Never>] = []

    func releaseOne() {
        guard !waiting.isEmpty else { return }
        waiting.removeFirst().resume()
    }

    func releaseAll() {
        let parked = waiting
        waiting = []
        parked.forEach { $0.resume() }
    }

    func updateReach(
        pseudonym: Data, add: [DsReachKeyAdd], remove: [Data], open: Bool?, signer: DsRequestSigner
    ) async throws {}

    func fetchReach(pseudonym: Data, signer: DsRequestSigner) async throws -> DsReachListing {
        reads += 1
        await withCheckedContinuation { waiting.append($0) }
        return DsReachListing(keys: [], open: false)
    }
}

@MainActor
final class ReachSyncBindingTests: XCTestCase {

    private func settle() async {
        for _ in 0..<50 { await Task.yield() }
        try? await Task.sleep(nanoseconds: 50_000_000)
        for _ in 0..<50 { await Task.yield() }
    }

    /// A sync the account left behind, finishing later, must not drop the
    /// reference to the next account's run: a third `sync()` would then start
    /// beside that run instead of asking it for one more pass.
    func testARunLeftBehindDoesNotLetASecondRunStartBesideTheNextAccounts() async throws {
        let ds = GatedReachDs()
        let center = ReachCenter(persistence: InMemoryReachPersistence(), ds: ds)
        let alice = try Identity.fromSeed(seed: Data(repeating: 0x31, count: 32))
        let bob = try Identity.fromSeed(seed: Data(repeating: 0x32, count: 32))

        center.activate(accountIdB64: "alice", accountIdentity: alice)
        center.sync()
        await settle()
        XCTAssertEqual(ds.reads, 1, "the first account's run is out on its read")

        center.deactivate()
        center.activate(accountIdB64: "bob", accountIdentity: bob)
        center.sync()
        await settle()
        XCTAssertEqual(ds.reads, 2, "the next account's run is out on its own read")

        // The first account's run comes back and ends.
        ds.releaseOne()
        await settle()

        center.sync()
        await settle()
        XCTAssertEqual(ds.reads, 2, "coalesced into the run in flight, not started beside it")

        ds.releaseAll()
        await settle()
        ds.releaseAll()
        await settle()
        center.deactivate()
        ds.releaseAll()
    }
}
