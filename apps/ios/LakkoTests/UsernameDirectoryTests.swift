// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// iOS claims the signup username (the auth login handle first, then the
/// discovery name, ADR-0054 S5) and publishes the identity bundle at signup.
/// Sign-in by name is in `LoginByNameTests`.
///
/// The defect: `DiscoveryClient.registerUsername` had no call site, login
/// refused any username the on-device marker did not already hold, and signup
/// never published the identity bundle — so an iOS signup could not be found
/// by name, could not be sent a contact request, and could not sign in on a
/// second device.
@MainActor
final class UsernameDirectoryTests: XCTestCase {

    // MARK: - Fakes

    /// Records every directory call, in order, and answers from its script.
    private final class FakeDirectory: AccountDirectory {
        enum Call: Equatable {
            case bind(handle: Data, sessionToken: String)
            case release(sessionToken: String)
            case register(accountId: Data, username: String, sessionToken: String)
            case publish(accountId: Data, signaturePubkey: Data, hasLeafAuth: Bool, sessionToken: String)
        }

        var calls: [Call] = []
        var registerOutcome: Result<RegisterResult, Error> =
            .success(RegisterResult(normalisedUsername: "alice"))
        var publishError: Error?

        func registerUsername(
            accountId: Data,
            username: String,
            sessionToken: String
        ) async throws -> RegisterResult {
            calls.append(.register(accountId: accountId, username: username, sessionToken: sessionToken))
            return try registerOutcome.get()
        }

        func publishIdentityBundle(
            accountId: Data,
            signaturePubkey: Data,
            x25519Pubkey: Data, x25519Binding: Data,
            leafAuth: Data?,
            genesisAttest: Data?,
            keyResetAttest: Data?,
            sessionToken: String
        ) async throws {
            calls.append(.publish(
                accountId: accountId,
                signaturePubkey: signaturePubkey,
                hasLeafAuth: leafAuth?.count == 64,
                sessionToken: sessionToken
            ))
            if let publishError { throw publishError }
        }
    }

    /// Records the auth half of a claim into the directory's call log, so one
    /// ordered list shows both registries.
    private final class FakeBinder: LoginHandleBinder {
        let log: FakeDirectory
        var bindError: Error?

        init(log: FakeDirectory) { self.log = log }

        func bindLoginHandle(handle: Data, sessionToken: String) async throws {
            log.calls.append(.bind(handle: handle, sessionToken: sessionToken))
            if let bindError { throw bindError }
        }

        func removeLoginHandle(sessionToken: String) async throws -> Bool {
            log.calls.append(.release(sessionToken: sessionToken))
            return true
        }
    }

    /// Answers the auth service: the login-handle binding routes succeed
    /// (`PUT` answers ``bindStatus``), everything else is 503. Records
    /// `METHOD path` per request.
    final class StubAuthProtocol: URLProtocol {
        private static let lock = NSLock()
        private static var _requests: [String] = []
        private static var _bindStatus = 200

        static func reset() {
            lock.lock(); defer { lock.unlock() }
            _requests = []
            _bindStatus = 200
        }

        static var requests: [String] {
            lock.lock(); defer { lock.unlock() }
            return _requests
        }

        static var bindStatus: Int {
            get { lock.lock(); defer { lock.unlock() }; return _bindStatus }
            set { lock.lock(); defer { lock.unlock() }; _bindStatus = newValue }
        }

        override class func canInit(with request: URLRequest) -> Bool { true }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

        override func startLoading() {
            let path = request.url?.path ?? ""
            let method = request.httpMethod ?? "GET"
            Self.lock.lock()
            Self._requests.append("\(method) \(path)")
            let bind = Self._bindStatus
            Self.lock.unlock()

            let status: Int
            let payload: Data
            switch (method, path) {
            case ("PUT", "/auth/login-handle"):
                status = bind
                payload = Data((bind == 200 ? #"{"bound":true}"# : #"{"error":"handle-taken"}"#).utf8)
            case ("DELETE", "/auth/login-handle"):
                status = 200
                payload = Data(#"{"removed":true}"#.utf8)
            default:
                status = 503
                payload = Data("unavailable".utf8)
            }
            let response = HTTPURLResponse(
                url: request.url!, statusCode: status, httpVersion: nil,
                headerFields: ["Content-Type": "application/json"]
            )!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: payload)
            client?.urlProtocolDidFinishLoading(self)
        }

        override func stopLoading() {}
    }

    private final class NoopPersistence: SessionPersistence {
        private(set) var putTokens: [String] = []
        func hasDeviceKey(accountIdB64: String) -> Bool { false }
        func accountIdentitySeed(accountIdB64: String) -> Data? { nil }
        func exportKey(accountIdB64: String) -> Data? { nil }
        func sessionToken(accountIdB64: String) -> PersistedSessionToken? { nil }
        func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {
            putTokens.append(accountIdB64)
        }
        func deleteSessionToken(accountIdB64: String) {}
    }

    // MARK: - Fixtures

    private var accountIdsToWipe: [String] = []

    override func tearDown() {
        for accountIdB64 in accountIdsToWipe {
            DeviceKeyStore.deleteDeviceKey(accountIdB64: accountIdB64)
            ExportKeyStore.deleteExportKey(accountIdB64: accountIdB64)
            AccountIdentitySeedStore.deleteAccountIdentitySeed(accountIdB64: accountIdB64)
            DeviceIdentityStore.deleteDeviceIdentitySeed(accountIdB64: accountIdB64)
        }
        accountIdsToWipe = []
        StubAuthProtocol.reset()
        super.tearDown()
    }

    private func randomAccountId() -> Data {
        Data((0..<16).map { _ in UInt8.random(in: 0...255) })
    }

    private func makeSessionStore(with marker: PersistedSession? = nil) -> SessionStore {
        let defaults = UserDefaults(suiteName: "username-directory-\(UUID().uuidString)")!
        let store = SessionStore(defaults: defaults)
        if let marker { store.save(marker) }
        return store
    }

    private func stubbedAuthClient() -> AuthClient {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StubAuthProtocol.self]
        return AuthClient(baseURL: "http://auth.test", session: URLSession(configuration: config))
    }

    private func makeRepository(
        sessionStore: SessionStore,
        directory: FakeDirectory,
        persistence: NoopPersistence = NoopPersistence()
    ) -> AuthRepository {
        StubAuthProtocol.reset()
        return AuthRepository(
            sessionStore: sessionStore,
            authClientFactory: { self.stubbedAuthClient() },
            persistence: persistence,
            nowMs: { 1_000_000 },
            accountDirectory: directory,
            restoreDirectory: OfflineRestoreDirectory(),
            earlierKeysInputs: skipEarlierKeysGate
        )
    }

    private func signupResult(accountId: Data) -> (SignupResult, Data) {
        let mnemonic = RecoveryMnemonic.generate()
        let result = SignupResult(
            accountId: accountId,
            exportKey: Data(repeating: 0xE7, count: 64),
            recoveryPhrase: mnemonic.phrase(),
            sessionToken: "tok-signup",
            sessionExpiresAtMs: 9_000_000
        )
        return (result, mnemonic.toSeed())
    }

    // MARK: - Signup claim (the pure step): auth first, then discovery

    private func handle(_ name: String) -> Data {
        (try? loginHandle(username: name)) ?? Data()
    }

    func testClaimBindsTheHandleAtAuthBeforeRegisteringAtDiscovery() async throws {
        let directory = FakeDirectory()
        let binder = FakeBinder(log: directory)
        let accountId = randomAccountId()
        let bound = try await claimSignupUsername(
            accountId: accountId,
            username: "@Alice",
            sessionToken: "tok-signup",
            directory: directory,
            binder: binder
        )
        XCTAssertEqual(bound, "alice")
        XCTAssertEqual(directory.calls, [
            .bind(handle: handle("alice"), sessionToken: "tok-signup"),
            .register(accountId: accountId, username: "alice", sessionToken: "tok-signup"),
        ])
        XCTAssertEqual(handle("alice").count, 32)
    }

    func testAnAuthConflictIsTakenAndDiscoveryIsNeverAsked() async {
        let directory = FakeDirectory()
        let binder = FakeBinder(log: directory)
        binder.bindError = AuthClientError.http(
            status: 409, endpoint: "/auth/login-handle", body: #"{"error":"handle-taken"}"#)
        do {
            _ = try await claimSignupUsername(
                accountId: randomAccountId(),
                username: "  @Alice ",
                sessionToken: "tok-signup",
                directory: directory,
                binder: binder
            )
            XCTFail("a taken handle must not claim")
        } catch let taken as UsernameTaken {
            XCTAssertEqual(taken, UsernameTaken(username: "alice"))
            XCTAssertEqual(taken.errorDescription, "The username @alice is already taken. Pick another.")
        } catch {
            XCTFail("expected UsernameTaken, got \(error)")
        }
        XCTAssertEqual(directory.calls, [.bind(handle: handle("alice"), sessionToken: "tok-signup")])
    }

    /// `docs/ux/anti-abuse.md` section 2 — `429 handle-bind-limited` stops the
    /// claim before discovery, like a taken name, with its own copy.
    func testAHandleBindLimitStopsBeforeDiscoveryWithItsCopy() async {
        let directory = FakeDirectory()
        let binder = FakeBinder(log: directory)
        binder.bindError = AuthClientError.http(
            status: 429, endpoint: "/auth/login-handle",
            body: #"{"error":"handle-bind-limited","retry_after_secs":125}"#)
        do {
            _ = try await claimSignupUsername(
                accountId: randomAccountId(),
                username: "alice",
                sessionToken: "tok-signup",
                directory: directory,
                binder: binder
            )
            XCTFail("a limited bind must not claim")
        } catch let limited as UsernameBindLimited {
            XCTAssertEqual(limited.retryAfterSecs, 125)
            XCTAssertEqual(
                limited.errorDescription,
                Localization.text("username.error.bind_limited_minutes", counts: ["minutes": 3]))
        } catch {
            XCTFail("expected UsernameBindLimited, got \(error)")
        }
        XCTAssertEqual(directory.calls, [.bind(handle: handle("alice"), sessionToken: "tok-signup")],
                       "discovery is never asked and nothing is released")
    }

    /// `docs/ux/beta-account-cap.md` — `403 registration-closed` stops the
    /// claim before discovery: no name is left in the directory.
    func testRegistrationClosedStopsBeforeDiscovery() async {
        let directory = FakeDirectory()
        let binder = FakeBinder(log: directory)
        binder.bindError = AuthClientError.http(
            status: 403, endpoint: "/auth/login-handle", body: #"{"error":"registration-closed"}"#)
        do {
            _ = try await claimSignupUsername(
                accountId: randomAccountId(),
                username: "alice",
                sessionToken: "tok-signup",
                directory: directory,
                binder: binder
            )
            XCTFail("a refused bind must not claim")
        } catch is RegistrationClosed {
        } catch {
            XCTFail("expected RegistrationClosed, got \(error)")
        }
        XCTAssertEqual(directory.calls, [.bind(handle: handle("alice"), sessionToken: "tok-signup")])
    }

    func testTheHandleBindLimitCopy() {
        XCTAssertEqual(
            AuthRateLimit.handleBindLimitedCopy(retryAfterSecs: 60),
            Localization.text("username.error.bind_limited_minutes", counts: ["minutes": 1]))
        XCTAssertEqual(
            AuthRateLimit.handleBindLimitedCopy(retryAfterSecs: 61),
            Localization.text("username.error.bind_limited_minutes", counts: ["minutes": 2]))
        XCTAssertEqual(
            AuthRateLimit.handleBindLimitedCopy(retryAfterSecs: 3601),
            Localization.text("username.error.bind_limited_later"))
        XCTAssertEqual(
            AuthRateLimit.handleBindLimitedCopy(retryAfterSecs: nil),
            Localization.text("username.error.bind_limited_later"))
        let error = AuthClientError.http(
            status: 429, endpoint: "/auth/login-handle",
            body: #"{"error":"handle-bind-limited","retry_after_secs":600}"#)
        XCTAssertEqual(
            AuthRateLimit.copy(for: error),
            Localization.text("username.error.bind_limited_minutes", counts: ["minutes": 10]))
    }

    func testADiscoveryConflictReleasesTheHandleAndIsTaken() async {
        let directory = FakeDirectory()
        let binder = FakeBinder(log: directory)
        directory.registerOutcome = .failure(
            DiscoveryClientError.http(status: 409, endpoint: "/discovery/register", body: "taken")
        )
        let accountId = randomAccountId()
        do {
            _ = try await claimSignupUsername(
                accountId: accountId,
                username: "alice",
                sessionToken: "tok-signup",
                directory: directory,
                binder: binder
            )
            XCTFail("a taken name must not claim")
        } catch let taken as UsernameTaken {
            XCTAssertEqual(taken, UsernameTaken(username: "alice"))
        } catch {
            XCTFail("expected UsernameTaken, got \(error)")
        }
        XCTAssertEqual(directory.calls, [
            .bind(handle: handle("alice"), sessionToken: "tok-signup"),
            .register(accountId: accountId, username: "alice", sessionToken: "tok-signup"),
            .release(sessionToken: "tok-signup"),
        ])
    }

    func testAnyOtherDiscoveryFailureReleasesTheHandleAndPassesThrough() async {
        let directory = FakeDirectory()
        let binder = FakeBinder(log: directory)
        directory.registerOutcome = .failure(
            DiscoveryClientError.http(status: 500, endpoint: "/discovery/register", body: "boom")
        )
        do {
            _ = try await claimSignupUsername(
                accountId: randomAccountId(),
                username: "alice",
                sessionToken: "tok-signup",
                directory: directory,
                binder: binder
            )
            XCTFail("a failed claim must throw")
        } catch DiscoveryClientError.http(let status, _, _) {
            XCTAssertEqual(status, 500)
        } catch {
            XCTFail("expected the directory error unchanged, got \(error)")
        }
        XCTAssertEqual(directory.calls.last, .release(sessionToken: "tok-signup"))
    }

    func testANameThatFailsTheRuleClaimsNothing() async {
        let directory = FakeDirectory()
        let binder = FakeBinder(log: directory)
        do {
            _ = try await claimSignupUsername(
                accountId: randomAccountId(),
                username: "p\u{0430}vel",
                sessionToken: "tok-signup",
                directory: directory,
                binder: binder
            )
            XCTFail("an invalid name must not claim")
        } catch let violation as UsernameRuleViolation {
            XCTAssertEqual(violation.reason, .invalidCharacters)
        } catch {
            XCTFail("expected a UsernameRuleViolation, got \(error)")
        }
        XCTAssertTrue(directory.calls.isEmpty)
    }

    // MARK: - Signup through the repository

    func testATakenUsernameAbortsSignupBeforeAnythingLandsOnTheDevice() async throws {
        let directory = FakeDirectory()
        directory.registerOutcome = .failure(
            DiscoveryClientError.http(status: 409, endpoint: "/discovery/register", body: "taken")
        )
        let accountId = randomAccountId()
        let accountIdB64 = base64Encode(accountId)
        accountIdsToWipe.append(accountIdB64)
        let store = makeSessionStore()
        let persistence = NoopPersistence()
        let repo = makeRepository(sessionStore: store, directory: directory, persistence: persistence)
        let (result, _) = signupResult(accountId: accountId)

        do {
            _ = try await repo.completeSignup(result: result, username: "alice")
            XCTFail("a taken username must fail signup")
        } catch is UsernameTaken {
            // expected
        }

        XCTAssertEqual(
            directory.calls,
            [.register(accountId: accountId, username: "alice", sessionToken: "tok-signup")],
            "nothing past the claim may run — in particular no bundle publish"
        )
        XCTAssertEqual(
            StubAuthProtocol.requests,
            ["PUT /auth/login-handle", "DELETE /auth/login-handle"],
            "the handle is bound first and released when discovery refuses the name"
        )
        XCTAssertEqual(repo.state, .anon(cachedUsername: nil))
        XCTAssertNil(repo.identitySecrets)
        XCTAssertNil(store.load(), "no account marker for an account nobody can find")
        XCTAssertNil(try DeviceKeyStore.getDeviceKey(accountIdB64: accountIdB64))
        XCTAssertNil(try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: accountIdB64))
        XCTAssertTrue(persistence.putTokens.isEmpty)
    }

    func testSignupClaimsThenPublishesTheBundleAndAFailedPublishLandsNoSession() async throws {
        let directory = FakeDirectory()
        directory.publishError = DiscoveryClientError.http(
            status: 503, endpoint: "/discovery/identity", body: "unavailable"
        )
        let accountId = randomAccountId()
        let accountIdB64 = base64Encode(accountId)
        accountIdsToWipe.append(accountIdB64)
        let store = makeSessionStore()
        let persistence = NoopPersistence()
        let repo = makeRepository(sessionStore: store, directory: directory, persistence: persistence)
        let (result, seed) = signupResult(accountId: accountId)
        let expectedPubkey = try Identity.fromSeed(seed: seed).signaturePublicKey()

        do {
            _ = try await repo.completeSignup(result: result, username: "@Alice")
            XCTFail("a failed bundle publish must fail signup")
        } catch DiscoveryClientError.http(let status, _, _) {
            XCTAssertEqual(status, 503)
        }

        XCTAssertEqual(StubAuthProtocol.requests.first, "PUT /auth/login-handle",
                       "the handle is bound before anything else")
        XCTAssertFalse(StubAuthProtocol.requests.contains("DELETE /auth/login-handle"),
                       "a claim that succeeded keeps its handle")
        XCTAssertEqual(directory.calls, [
            .register(accountId: accountId, username: "alice", sessionToken: "tok-signup"),
            .publish(
                accountId: accountId,
                signaturePubkey: expectedPubkey,
                hasLeafAuth: true,
                sessionToken: "tok-signup"
            ),
        ])
        // The session did not land: an account that cannot be contacted is
        // not signed in.
        XCTAssertEqual(repo.state, .anon(cachedUsername: nil))
        XCTAssertNil(repo.identitySecrets)
        XCTAssertTrue(persistence.putTokens.isEmpty)
        // The marker holds the name the server bound, so the next password
        // sign-in on this device takes the cached path and re-publishes.
        XCTAssertEqual(store.load(), PersistedSession(accountIdB64: accountIdB64, username: "alice"))
    }

    func testATakenHandleAtAuthAbortsSignupBeforeDiscovery() async throws {
        let directory = FakeDirectory()
        let accountId = randomAccountId()
        accountIdsToWipe.append(base64Encode(accountId))
        let store = makeSessionStore()
        let repo = makeRepository(sessionStore: store, directory: directory)
        StubAuthProtocol.bindStatus = 409
        let (result, _) = signupResult(accountId: accountId)

        do {
            _ = try await repo.completeSignup(result: result, username: "alice")
            XCTFail("a taken handle must fail signup")
        } catch is UsernameTaken {
            // expected
        }
        XCTAssertTrue(directory.calls.isEmpty, "discovery is never asked")
        XCTAssertEqual(StubAuthProtocol.requests, ["PUT /auth/login-handle"])
        XCTAssertNil(store.load())
        XCTAssertNil(repo.identitySecrets)
    }

    // The sign-in decision about the published identity is ADR-0073's
    // `KeyResetLanding`, pinned in `KeyResetTests`.
}

/// The camera gate every video entry point runs.
final class CameraPermissionTests: XCTestCase {

    func testGrantedProceedsWithoutPrompting() async {
        var prompted = false
        let ok = await CameraPermission.ensureGranted(
            status: { .granted },
            request: { prompted = true; return false }
        )
        XCTAssertTrue(ok)
        XCTAssertFalse(prompted)
    }

    func testDeniedRefusesWithoutPrompting() async {
        var prompted = false
        let ok = await CameraPermission.ensureGranted(
            status: { .denied },
            request: { prompted = true; return true }
        )
        XCTAssertFalse(ok)
        XCTAssertFalse(prompted)
    }

    func testUndeterminedFollowsThePrompt() async {
        let granted = await CameraPermission.ensureGranted(status: { .undetermined }, request: { true })
        let refused = await CameraPermission.ensureGranted(status: { .undetermined }, request: { false })
        XCTAssertTrue(granted)
        XCTAssertFalse(refused)
    }

    func testAnAudioOnlyStartNeverReadsOrPromptsForTheCamera() async {
        var read = false
        var prompted = false
        let ok = await CameraPermission.ensureGranted(
            forVideo: false,
            status: { read = true; return .denied },
            request: { prompted = true; return false }
        )
        XCTAssertTrue(ok)
        XCTAssertFalse(read)
        XCTAssertFalse(prompted)
    }

    func testAVideoStartIsGatedOnTheCamera() async {
        let denied = await CameraPermission.ensureGranted(
            forVideo: true, status: { .denied }, request: { true })
        let granted = await CameraPermission.ensureGranted(
            forVideo: true, status: { .granted }, request: { false })
        XCTAssertFalse(denied)
        XCTAssertTrue(granted)
    }

    func testTheDeniedMessageNamesTheSettingAndTheAudioOnlyWayOut() {
        // The system mapping reads the live permission database, so it is not
        // driven here; the copy every surface shows is.
        XCTAssertTrue(CameraPermission.deniedMessage.contains("audio only"))
        XCTAssertTrue(CameraPermission.deniedMessage.contains("app settings"))
    }
}
