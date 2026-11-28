// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// Where a relaunch lands, for every combination of on-device material.
///
/// The defect: a relaunch saw a session marker plus a device key and flipped
/// to `.session` with no `IdentitySecrets`, so chat and contacts were dead
/// until the user logged out and in again. A relaunch must now land either a
/// FULL session (state and secrets together) or the logged-out state — and a
/// missing, undecodable or expired input must fail closed to logged out.
@MainActor
final class SessionRestoreTests: XCTestCase {

    // MARK: - Fixtures

    private final class FakePersistence: SessionPersistence {
        var deviceKey = true
        var seed: Data? = Data(repeating: 0x5A, count: 64)
        var export: Data? = Data(repeating: 0xE7, count: 32)
        var token: PersistedSessionToken? = PersistedSessionToken(
            sessionToken: "tok-live",
            sessionExpiresAtMs: 2_000_000
        )
        private(set) var putTokens: [(PersistedSessionToken, String)] = []
        private(set) var deletedTokens: [String] = []

        func hasDeviceKey(accountIdB64: String) -> Bool { deviceKey }
        func accountIdentitySeed(accountIdB64: String) -> Data? { seed }
        func exportKey(accountIdB64: String) -> Data? { export }
        func sessionToken(accountIdB64: String) -> PersistedSessionToken? { token }
        func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {
            putTokens.append((token, accountIdB64))
        }
        func deleteSessionToken(accountIdB64: String) {
            deletedTokens.append(accountIdB64)
        }
    }

    private let accountId = Data((0..<16).map { UInt8($0) })
    private var accountIdB64: String { base64Encode(accountId) }
    private var marker: PersistedSession {
        PersistedSession(accountIdB64: accountIdB64, username: "alice")
    }
    private let now: Int64 = 1_000_000

    private func decide(
        _ persistence: FakePersistence,
        marker: PersistedSession?? = .none
    ) -> SessionRestoreDecision {
        let resolvedMarker: PersistedSession?
        switch marker {
        case .none: resolvedMarker = self.marker
        case .some(let value): resolvedMarker = value
        }
        return SessionRestore.decide(marker: resolvedMarker, persistence: persistence, nowMs: now)
    }

    // MARK: - Decision table

    func testNoMarkerIsLoggedOutWithNoCachedUsername() {
        XCTAssertEqual(decide(FakePersistence(), marker: .some(nil)), .anon(cachedUsername: nil))
    }

    func testEverythingPresentRestoresTheFullSession() {
        let persistence = FakePersistence()
        let expected = IdentitySecrets(
            accountId: accountId,
            exportKey: Data(repeating: 0xE7, count: 32),
            accountIdentitySeed: Data(repeating: 0x5A, count: 64),
            sessionToken: "tok-live",
            sessionExpiresAtMs: 2_000_000
        )
        XCTAssertEqual(
            decide(persistence),
            .session(
                account: SessionAccount(accountIdB64: accountIdB64, username: "alice"),
                secrets: expected
            )
        )
    }

    func testMissingExportKeyStillRestoresWithAnEmptyExportKey() {
        // The no-password paths (TOTP / device restore) already run sessions
        // with an empty export key, so it is not a required input.
        let persistence = FakePersistence()
        persistence.export = nil
        guard case .session(_, let secrets) = decide(persistence) else {
            return XCTFail("a missing export key must not block the restore")
        }
        XCTAssertEqual(secrets.exportKey, Data())
    }

    func testMalformedAccountIdIsLoggedOut() {
        let bad = PersistedSession(accountIdB64: "not base64 !!", username: "alice")
        XCTAssertEqual(decide(FakePersistence(), marker: .some(bad)), .anon(cachedUsername: "alice"))
        let short = PersistedSession(accountIdB64: base64Encode(Data([1, 2, 3])), username: "alice")
        XCTAssertEqual(decide(FakePersistence(), marker: .some(short)), .anon(cachedUsername: "alice"))
    }

    func testMissingDeviceKeyIsLoggedOut() {
        let persistence = FakePersistence()
        persistence.deviceKey = false
        XCTAssertEqual(decide(persistence), .anon(cachedUsername: "alice"))
    }

    func testMissingSeedIsLoggedOutNotAHalfSession() {
        // The pre-fix outcome for exactly this state was `.session` with nil
        // secrets. The seed is the identity; without it there is no session.
        let persistence = FakePersistence()
        persistence.seed = nil
        XCTAssertEqual(decide(persistence), .anon(cachedUsername: "alice"))
    }

    func testWrongLengthSeedIsLoggedOut() {
        let persistence = FakePersistence()
        persistence.seed = Data(repeating: 0x5A, count: 32)
        XCTAssertEqual(decide(persistence), .anon(cachedUsername: "alice"))
    }

    func testMissingTokenIsLoggedOut() {
        // Also the post-logout state: logout deletes the token, so a relaunch
        // must not resurrect the session.
        let persistence = FakePersistence()
        persistence.token = nil
        XCTAssertEqual(decide(persistence), .anon(cachedUsername: "alice"))
    }

    func testExpiredTokenIsLoggedOutAndDiscarded() {
        let persistence = FakePersistence()
        persistence.token = PersistedSessionToken(sessionToken: "tok-old", sessionExpiresAtMs: now - 1)
        XCTAssertEqual(decide(persistence), .anonDiscardingToken(cachedUsername: "alice"))
    }

    func testTokenExpiringExactlyNowIsExpired() {
        let persistence = FakePersistence()
        persistence.token = PersistedSessionToken(sessionToken: "tok-edge", sessionExpiresAtMs: now)
        XCTAssertEqual(decide(persistence), .anonDiscardingToken(cachedUsername: "alice"))
    }

    func testEmptyTokenIsLoggedOutAndDiscarded() {
        let persistence = FakePersistence()
        persistence.token = PersistedSessionToken(sessionToken: "", sessionExpiresAtMs: now + 10_000)
        XCTAssertEqual(decide(persistence), .anonDiscardingToken(cachedUsername: "alice"))
    }

    // MARK: - Applied by AuthRepository at launch

    private func makeSessionStore(with marker: PersistedSession?) -> SessionStore {
        let defaults = UserDefaults(suiteName: "session-restore-\(UUID().uuidString)")!
        let store = SessionStore(defaults: defaults)
        if let marker { store.save(marker) }
        return store
    }

    func testLaunchWithFullMaterialLandsStateAndSecretsTogether() {
        let persistence = FakePersistence()
        let repo = AuthRepository.offline(
            sessionStore: makeSessionStore(with: marker),
            authClientFactory: { NameRouteStub.client(NameRouteStub.scope { _, _, _ in (503, "") }) },
            persistence: persistence,
            nowMs: { self.now }
        )
        XCTAssertEqual(repo.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
        XCTAssertEqual(repo.identitySecrets?.accountIdentitySeed, Data(repeating: 0x5A, count: 64))
        XCTAssertEqual(repo.identitySecrets?.sessionToken, "tok-live")
        XCTAssertEqual(repo.identitySecrets?.accountId, accountId)
        XCTAssertTrue(persistence.deletedTokens.isEmpty)
    }

    /// UX 19.1c — a session restored at launch lands before the composition
    /// root wires the protection hook; wiring it then runs the landing once,
    /// so an account with no second way in gets the step at a relaunch.
    func testARestoredSessionRunsTheProtectionLandingOnceTheHookIsWired() {
        let repo = AuthRepository.offline(
            sessionStore: makeSessionStore(with: marker),
            authClientFactory: { NameRouteStub.client(NameRouteStub.scope { _, _, _ in (503, "") }) },
            persistence: FakePersistence(),
            nowMs: { self.now }
        )
        XCTAssertEqual(repo.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
        var landed: [String] = []
        repo.onProtectedSessionLanded = { landed.append($0) }
        XCTAssertEqual(landed, [accountIdB64])
        // Setting it again does not run the landing a second time.
        repo.onProtectedSessionLanded = { landed.append($0) }
        XCTAssertEqual(landed, [accountIdB64])
    }

    func testNoHookRunsWithoutASession() {
        let repo = AuthRepository.offline(
            sessionStore: makeSessionStore(with: nil),
            authClientFactory: { NameRouteStub.client(NameRouteStub.scope { _, _, _ in (503, "") }) },
            persistence: FakePersistence(),
            nowMs: { self.now }
        )
        var landed: [String] = []
        repo.onProtectedSessionLanded = { landed.append($0) }
        XCTAssertTrue(landed.isEmpty)
    }

    func testLaunchWithMarkerAndDeviceKeyButNoTokenIsLoggedOut() {
        // The exact on-device state that used to produce the dead session.
        let persistence = FakePersistence()
        persistence.token = nil
        let repo = AuthRepository.offline(
            sessionStore: makeSessionStore(with: marker),
            authClientFactory: { NameRouteStub.client(NameRouteStub.scope { _, _, _ in (503, "") }) },
            persistence: persistence,
            nowMs: { self.now }
        )
        XCTAssertEqual(repo.state, .anon(cachedUsername: "alice"))
        XCTAssertNil(repo.identitySecrets)
    }

    func testLaunchWithExpiredTokenIsLoggedOutAndDeletesIt() {
        let persistence = FakePersistence()
        persistence.token = PersistedSessionToken(sessionToken: "tok-old", sessionExpiresAtMs: now - 1)
        let repo = AuthRepository.offline(
            sessionStore: makeSessionStore(with: marker),
            authClientFactory: { NameRouteStub.client(NameRouteStub.scope { _, _, _ in (503, "") }) },
            persistence: persistence,
            nowMs: { self.now }
        )
        XCTAssertEqual(repo.state, .anon(cachedUsername: "alice"))
        XCTAssertNil(repo.identitySecrets)
        XCTAssertEqual(persistence.deletedTokens, [accountIdB64])
    }

    func testPersistedTokenRoundTripsThroughItsWireShape() throws {
        let token = PersistedSessionToken(pair: .fixture(
            access: "v2.abc.1.def", accessExpiresAtMs: 1_766_000_000_000,
            refresh: "r1.abc.1.ghi", refreshExpiresAtMs: 1_768_000_000_000, generation: 3))
        let data = try JSONEncoder().encode(token)
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(
            Set(json.keys),
            ["token", "expires_at_ms", "refresh_token", "refresh_expires_at_ms", "key_generation"])
        XCTAssertEqual(try JSONDecoder().decode(PersistedSessionToken.self, from: data), token)
    }

    // MARK: - ADR-0073: the refresh credential restores the session

    func testAnExpiredAccessTokenWithALiveRefreshCredentialRestores() {
        // The silent restore refreshes instead of reusing the access token.
        let persistence = FakePersistence()
        persistence.token = PersistedSessionToken(pair: .fixture(
            access: "tok-expired", accessExpiresAtMs: now - 1, refreshExpiresAtMs: now + 10_000, generation: 2))
        guard case .session(_, let secrets) = decide(persistence) else {
            return XCTFail("a live refresh credential restores the session")
        }
        XCTAssertEqual(secrets.session.refreshToken, "r1.refresh")
        XCTAssertEqual(secrets.session.keyGeneration, 2)
    }

    func testBothTokensExpiredIsLoggedOutAndDiscarded() {
        let persistence = FakePersistence()
        persistence.token = PersistedSessionToken(pair: .fixture(
            access: "tok-expired", accessExpiresAtMs: now - 1, refreshExpiresAtMs: now))
        XCTAssertEqual(decide(persistence), .anonDiscardingToken(cachedUsername: "alice"))
    }
}

/// ADR-0054 S5 - a session that lands on a path that never names the account
/// by its login handle (here: the relaunch restore) still binds the handle,
/// so the person can sign in by name after a logout. One `PUT` per
/// `(session, username)` per process; a failure changes nothing.
@MainActor
final class LoginHandleRebindOnLandingTests: XCTestCase {

    private final class RestorablePersistence: SessionPersistence {
        let token: String
        private(set) var putTokens = 0
        private(set) var deletedTokens = 0

        init(token: String) { self.token = token }

        func hasDeviceKey(accountIdB64: String) -> Bool { true }
        func accountIdentitySeed(accountIdB64: String) -> Data? { Data(repeating: 0x5A, count: 64) }
        func exportKey(accountIdB64: String) -> Data? { Data(repeating: 0xE7, count: 32) }
        func sessionToken(accountIdB64: String) -> PersistedSessionToken? {
            PersistedSessionToken(sessionToken: token, sessionExpiresAtMs: 2_000_000)
        }
        func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws { putTokens += 1 }
        func deleteSessionToken(accountIdB64: String) { deletedTokens += 1 }
    }

    /// Any discovery call fails the test: a restore reaches auth only.
    private final class NoDirectory: AccountDirectory {
        private(set) var calls = 0
        func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
            calls += 1
            throw AuthClientError.argument("discovery must not be reached")
        }
        func publishIdentityBundle(
            accountId: Data, signaturePubkey: Data, x25519Pubkey: Data, x25519Binding: Data,
            leafAuth: Data?, genesisAttest: Data?, keyResetAttest: Data?, sessionToken: String
        ) async throws {
            calls += 1
            throw AuthClientError.argument("discovery must not be reached")
        }
    }

    private let accountIdB64 = base64Encode(Data((0..<16).map { UInt8($0) }))

    private func store(username: String) -> SessionStore {
        let store = SessionStore(defaults: UserDefaults(suiteName: "handle-rebind-\(UUID().uuidString)")!)
        store.save(PersistedSession(accountIdB64: accountIdB64, username: username))
        return store
    }

    private func restore(
        host: String,
        store: SessionStore,
        persistence: RestorablePersistence,
        ledger: LoginHandleBindLedger,
        directory: NoDirectory = NoDirectory()
    ) -> AuthRepository {
        AuthRepository(
            sessionStore: store,
            authClientFactory: { NameRouteStub.client(host) },
            persistence: persistence,
            nowMs: { 1_000_000 },
            accountDirectory: directory,
            loginHandleBinds: ledger,
            restoreDirectory: OfflineRestoreDirectory(),
            earlierKeysInputs: skipEarlierKeysGate
        )
    }

    private func binds(_ host: String) -> [RecordedAuthRequest] {
        NameRouteStub.requests(host).filter { $0.method == "PUT" && $0.path == "/auth/login-handle" }
    }

    private func waitUntil(_ condition: () -> Bool, timeout: TimeInterval = 5) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        return condition()
    }

    /// Give a fire-and-forget bind that should NOT happen time to show up.
    private func settle() async {
        try? await Task.sleep(nanoseconds: 300_000_000)
    }

    func testARestoredSessionBindsTheHandleOfItsCanonicalNameOnce() async throws {
        // ADR-0076: the landing asks first; the account has no handle, so
        // this is a first bind.
        let host = NameRouteStub.scope { method, path, _ in
            if path == "/auth/login-handle/check" { return (200, #"{"state":"none"}"#) }
            return method == "PUT" && path == "/auth/login-handle" ? (200, #"{"bound":true}"#) : (503, "")
        }
        let directory = NoDirectory()
        let repo = restore(
            host: host, store: store(username: "alice"),
            persistence: RestorablePersistence(token: "tok-restored"),
            ledger: LoginHandleBindLedger(), directory: directory)

        XCTAssertEqual(repo.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
        let bound = await waitUntil { !binds(host).isEmpty }
        XCTAssertTrue(bound, "a restored session binds the login handle")
        await settle()

        // ADR-0074: a landing also records the account key at auth (and binds
        // the device when the pair can refresh); this test is about the
        // handle, so it reads the handle requests.
        let requests = binds(host)
        XCTAssertEqual(requests.map { "\($0.method) \($0.path)" }, ["PUT /auth/login-handle"])
        XCTAssertEqual(requests.first?.authorization, "Bearer tok-restored")
        let expected = base64UrlEncodeNoPad(try loginHandle(username: "alice"))
        XCTAssertEqual(requests.first?.json["handle"] as? String, expected)
        XCTAssertEqual(directory.calls, 0)
    }

    /// ADR-0076 section 2 — a device whose name is stale (the account was
    /// renamed on another device) must not bind its old handle back.
    func testARestoredSessionWithAStaleNameBindsNothing() async {
        let host = NameRouteStub.scope { _, path, _ in
            path == "/auth/login-handle/check" ? (200, #"{"state":"other"}"#) : (200, #"{"bound":true}"#)
        }
        let repo = restore(
            host: host, store: store(username: "alice"),
            persistence: RestorablePersistence(token: "tok-stale"),
            ledger: LoginHandleBindLedger())
        XCTAssertEqual(repo.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
        let asked = await waitUntil {
            NameRouteStub.requests(host).contains { $0.path == "/auth/login-handle/check" }
        }
        XCTAssertTrue(asked)
        await settle()
        XCTAssertTrue(binds(host).isEmpty, "the rename is not reverted")

        // The bound handle is this name's: nothing to do either.
        let same = NameRouteStub.scope { _, path, _ in
            path == "/auth/login-handle/check" ? (200, #"{"state":"same"}"#) : (200, #"{"bound":true}"#)
        }
        _ = restore(
            host: same, store: store(username: "alice"),
            persistence: RestorablePersistence(token: "tok-same"),
            ledger: LoginHandleBindLedger())
        await settle()
        XCTAssertTrue(binds(same).isEmpty)
    }

    func testAGuestWithNoUsernameBindsNothing() async {
        let host = NameRouteStub.scope { _, _, _ in (200, #"{"bound":true}"#) }
        let repo = restore(
            host: host, store: store(username: ""),
            persistence: RestorablePersistence(token: "tok-guest"),
            ledger: LoginHandleBindLedger())

        XCTAssertEqual(repo.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "")))
        XCTAssertNotNil(repo.identitySecrets)
        await settle()
        XCTAssertTrue(binds(host).isEmpty, "a guest has no handle to bind")
    }

    func testATakenHandleChangesNothing() async {
        let host = NameRouteStub.scope { method, path, _ in
            if path == "/auth/login-handle/check" { return (200, #"{"state":"none"}"#) }
            return method == "PUT" && path == "/auth/login-handle" ? (409, #"{"error":"handle-taken"}"#) : (503, "")
        }
        let sessionStore = store(username: "alice")
        let persistence = RestorablePersistence(token: "tok-409")
        let repo = restore(host: host, store: sessionStore, persistence: persistence, ledger: LoginHandleBindLedger())
        let secretsBefore = repo.identitySecrets

        let tried = await waitUntil { !binds(host).isEmpty }
        XCTAssertTrue(tried)
        await settle()

        XCTAssertEqual(repo.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
        XCTAssertEqual(repo.identitySecrets?.sessionToken, secretsBefore?.sessionToken)
        XCTAssertEqual(repo.identitySecrets?.accountIdentitySeed, secretsBefore?.accountIdentitySeed)
        XCTAssertEqual(sessionStore.load(), PersistedSession(accountIdB64: accountIdB64, username: "alice"))
        XCTAssertEqual(persistence.deletedTokens, 0)
        XCTAssertEqual(persistence.putTokens, 0)
        XCTAssertEqual(binds(host).count, 1, "a 409 is not retried")
    }

    func testTheSameSessionInstalledTwiceBindsOnce() async {
        // ADR-0076: the landing asks first; the account has no handle, so
        // this is a first bind.
        let host = NameRouteStub.scope { method, path, _ in
            if path == "/auth/login-handle/check" { return (200, #"{"state":"none"}"#) }
            return method == "PUT" && path == "/auth/login-handle" ? (200, #"{"bound":true}"#) : (503, "")
        }
        let ledger = LoginHandleBindLedger()
        let sessionStore = store(username: "alice")
        let first = restore(
            host: host, store: sessionStore,
            persistence: RestorablePersistence(token: "tok-twice"), ledger: ledger)
        let second = restore(
            host: host, store: sessionStore,
            persistence: RestorablePersistence(token: "tok-twice"), ledger: ledger)
        XCTAssertEqual(first.state, second.state)

        // The same pair offered again through the repository hook.
        first.ensureLoginHandleBound(username: "alice", sessionToken: "tok-twice", context: "test")
        second.ensureLoginHandleBound(username: "@Alice", sessionToken: "tok-twice", context: "test")

        let bound = await waitUntil { !binds(host).isEmpty }
        XCTAssertTrue(bound)
        await settle()
        XCTAssertEqual(binds(host).count, 1, "one bind per (session, username) per process")

        // A different session for the same name is a new pair.
        first.ensureLoginHandleBound(username: "alice", sessionToken: "tok-next", context: "test")
        let rebound = await waitUntil { binds(host).count == 2 }
        XCTAssertTrue(rebound)
        XCTAssertEqual(binds(host).last?.authorization, "Bearer tok-next")
    }
}
