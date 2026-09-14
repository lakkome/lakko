// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// iOS registers the signup username in discovery, resolves a login username
/// through discovery when this device holds no marker for it, and publishes
/// the identity bundle at signup.
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
            case register(accountId: Data, username: String, sessionToken: String)
            case lookup(username: String)
            case publish(accountId: Data, signaturePubkey: Data, hasLeafAuth: Bool, sessionToken: String)
        }

        private(set) var calls: [Call] = []
        var registerOutcome: Result<RegisterResult, Error> =
            .success(RegisterResult(normalisedUsername: "alice"))
        var lookupOutcome: Result<LookupResult, Error> =
            .success(LookupResult(normalisedUsername: "alice", account: nil))
        var publishError: Error?

        func registerUsername(
            accountId: Data,
            username: String,
            sessionToken: String
        ) async throws -> RegisterResult {
            calls.append(.register(accountId: accountId, username: username, sessionToken: sessionToken))
            return try registerOutcome.get()
        }

        func lookup(username: String) async throws -> LookupResult {
            calls.append(.lookup(username: username))
            return try lookupOutcome.get()
        }

        func publishIdentityBundle(
            accountId: Data,
            signaturePubkey: Data,
            x25519Pubkey: Data,
            leafAuth: Data?,
            genesisAttest: Data?,
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

        var lookupCount: Int {
            calls.filter { if case .lookup = $0 { return true } else { return false } }.count
        }
    }

    /// Answers the auth service: a zero-effort PoW challenge, then 503 for
    /// everything else, recording the `/login/start` body so a test can read
    /// which account the login targeted.
    final class StubAuthProtocol: URLProtocol {
        private static let lock = NSLock()
        private static var _paths: [String] = []
        private static var _loginStartAccountIdB64: String?

        static func reset() {
            lock.lock(); defer { lock.unlock() }
            _paths = []
            _loginStartAccountIdB64 = nil
        }

        static var paths: [String] {
            lock.lock(); defer { lock.unlock() }
            return _paths
        }

        static var loginStartAccountIdB64: String? {
            lock.lock(); defer { lock.unlock() }
            return _loginStartAccountIdB64
        }

        override class func canInit(with request: URLRequest) -> Bool { true }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

        override func startLoading() {
            let path = request.url?.path ?? ""
            let body = Self.readBody(request)
            Self.lock.lock()
            Self._paths.append(path)
            if path == "/login/start",
               let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                Self._loginStartAccountIdB64 = json["account_id"] as? String
            }
            Self.lock.unlock()

            let status: Int
            let payload: Data
            if path == "/pow/challenge" {
                status = 200
                let seed = Data((0..<16).map { UInt8($0) }).base64EncodedString()
                payload = Data(#"{"seed":"\#(seed)","effort_bits":0,"expires_in":60}"#.utf8)
            } else {
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

        private static func readBody(_ request: URLRequest) -> Data {
            if let body = request.httpBody { return body }
            guard let stream = request.httpBodyStream else { return Data() }
            stream.open()
            defer { stream.close() }
            var data = Data()
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let read = stream.read(&buffer, maxLength: buffer.count)
                if read <= 0 { break }
                data.append(buffer, count: read)
            }
            return data
        }
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
            accountDirectory: directory
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

    // MARK: - Signup claim (the pure step)

    func testClaimRegistersWithTheSessionTokenAndReturnsTheBoundName() async throws {
        let directory = FakeDirectory()
        let accountId = randomAccountId()
        let bound = try await claimSignupUsername(
            accountId: accountId,
            username: "@Alice",
            sessionToken: "tok-signup",
            directory: directory
        )
        XCTAssertEqual(bound, "alice")
        XCTAssertEqual(
            directory.calls,
            [.register(accountId: accountId, username: "@Alice", sessionToken: "tok-signup")]
        )
    }

    func testClaimTurnsAConflictIntoUsernameTakenWithTheCanonicalName() async {
        let directory = FakeDirectory()
        directory.registerOutcome = .failure(
            DiscoveryClientError.http(status: 409, endpoint: "/discovery/register", body: "taken")
        )
        do {
            _ = try await claimSignupUsername(
                accountId: randomAccountId(),
                username: "  @Alice ",
                sessionToken: "tok-signup",
                directory: directory
            )
            XCTFail("a taken username must not claim")
        } catch let taken as UsernameTaken {
            XCTAssertEqual(taken, UsernameTaken(username: "alice"))
            XCTAssertEqual(taken.errorDescription, "The username @alice is already taken. Pick another.")
        } catch {
            XCTFail("expected UsernameTaken, got \(error)")
        }
    }

    func testClaimPassesOtherDirectoryFailuresThrough() async {
        let directory = FakeDirectory()
        directory.registerOutcome = .failure(
            DiscoveryClientError.http(status: 500, endpoint: "/discovery/register", body: "boom")
        )
        do {
            _ = try await claimSignupUsername(
                accountId: randomAccountId(),
                username: "alice",
                sessionToken: "tok-signup",
                directory: directory
            )
            XCTFail("a failed claim must throw")
        } catch DiscoveryClientError.http(let status, _, _) {
            XCTAssertEqual(status, 500)
        } catch {
            XCTFail("expected the directory error unchanged, got \(error)")
        }
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

        XCTAssertEqual(directory.calls, [
            .register(accountId: accountId, username: "@Alice", sessionToken: "tok-signup"),
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

    // MARK: - Login resolution (the pure step)

    private let fakeEncode: (Data) -> String = { "enc:" + $0.map { String($0) }.joined(separator: ",") }

    func testLoginWithAMatchingMarkerSkipsTheLookup() async throws {
        var lookups = 0
        let resolved = try await resolveLoginAccount(
            cached: CachedAccount(accountIdB64: "CACHED", username: "alice"),
            typedUsername: "@Alice",
            lookup: { _ in
                lookups += 1
                return LookupResult(normalisedUsername: "alice", account: Data([1]))
            },
            encodeAccountId: fakeEncode
        )
        XCTAssertEqual(resolved, ResolvedLoginAccount(accountIdB64: "CACHED", username: "alice", fromCache: true))
        XCTAssertEqual(lookups, 0)
    }

    func testLoginWithADifferentMarkerLooksTheUsernameUp() async throws {
        let resolved = try await resolveLoginAccount(
            cached: CachedAccount(accountIdB64: "CACHED", username: "alice"),
            typedUsername: "bob",
            lookup: { LookupResult(normalisedUsername: $0, account: Data([7, 8])) },
            encodeAccountId: fakeEncode
        )
        XCTAssertEqual(resolved, ResolvedLoginAccount(accountIdB64: "enc:7,8", username: "bob", fromCache: false))
    }

    func testAnEmptyCanonicalUsernameNeverMatchesAGuestMarker() async {
        do {
            _ = try await resolveLoginAccount(
                cached: CachedAccount(accountIdB64: "GUEST", username: ""),
                typedUsername: "@",
                lookup: { _ in XCTFail("no lookup for an empty name"); return LookupResult(normalisedUsername: "", account: nil) },
                encodeAccountId: fakeEncode
            )
            XCTFail("an empty canonical username must be refused")
        } catch AuthClientError.argument {
            // expected
        } catch {
            XCTFail("expected an argument error, got \(error)")
        }
    }

    // MARK: - Login through the repository

    func testLoginOnADeviceWithNoMarkerResolvesTheUsernameThroughTheDirectory() async {
        let directory = FakeDirectory()
        let accountId = randomAccountId()
        directory.lookupOutcome = .success(LookupResult(normalisedUsername: "alice", account: accountId))
        let store = makeSessionStore()
        let repo = makeRepository(sessionStore: store, directory: directory)

        do {
            try await repo.login(username: "@Alice", password: Data("password".utf8))
            XCTFail("the stubbed auth service refuses the login")
        } catch AuthClientError.http(let status, let endpoint, _) {
            XCTAssertEqual(status, 503)
            XCTAssertEqual(endpoint, "/login/start")
        } catch {
            XCTFail("expected the auth service's refusal, got \(error)")
        }

        XCTAssertEqual(directory.calls, [.lookup(username: "@Alice")])
        XCTAssertEqual(
            StubAuthProtocol.loginStartAccountIdB64, base64Encode(accountId),
            "OPAQUE login must target the account the lookup resolved"
        )
        XCTAssertNil(store.load(), "no marker is written for a login that did not succeed")
    }

    func testLoginWithAMatchingMarkerDoesNotLookUp() async {
        let directory = FakeDirectory()
        let accountId = randomAccountId()
        let store = makeSessionStore(
            with: PersistedSession(accountIdB64: base64Encode(accountId), username: "alice")
        )
        let repo = makeRepository(sessionStore: store, directory: directory)

        do {
            try await repo.login(username: "alice", password: Data("password".utf8))
            XCTFail("the stubbed auth service refuses the login")
        } catch AuthClientError.http(let status, _, _) {
            XCTAssertEqual(status, 503)
        } catch {
            XCTFail("expected the auth service's refusal, got \(error)")
        }

        XCTAssertEqual(directory.lookupCount, 0)
        XCTAssertEqual(StubAuthProtocol.loginStartAccountIdB64, base64Encode(accountId))
    }

    func testAnUnknownUsernameOnAFreshDeviceFailsAsInvalidCredentialsWithoutContactingAuth() async {
        let directory = FakeDirectory()
        directory.lookupOutcome = .success(LookupResult(normalisedUsername: "nobody", account: nil))
        let repo = makeRepository(sessionStore: makeSessionStore(), directory: directory)

        do {
            try await repo.login(username: "nobody", password: Data("password".utf8))
            XCTFail("an unregistered username must not log in")
        } catch AuthClientError.http(let status, _, let body) {
            XCTAssertEqual(status, 401)
            XCTAssertEqual(body, "invalid credentials")
            XCTAssertNotEqual(body, "no account on this device — sign up first")
        } catch {
            XCTFail("expected invalid credentials, got \(error)")
        }

        XCTAssertEqual(directory.calls, [.lookup(username: "nobody")])
        XCTAssertTrue(StubAuthProtocol.paths.isEmpty, "the auth service is never contacted")
    }

    // MARK: - Sign-in bundle refresh plan

    private let ours = Data(repeating: 0x0A, count: 32)

    func testNothingPublishedAttestsAndDoesNotAlert() {
        XCTAssertEqual(
            planPublishedIdentityRefresh(observed: .absent, derivedSignaturePubkey: ours),
            PublishedIdentityRefreshPlan(raiseAlert: false, mintGenesisAttestation: true)
        )
        XCTAssertEqual(
            planPublishedIdentityRefresh(observed: .unreachable, derivedSignaturePubkey: ours),
            PublishedIdentityRefreshPlan(raiseAlert: false, mintGenesisAttestation: true)
        )
    }

    func testOurOwnPublishedKeyNeitherAlertsNorAttests() {
        XCTAssertEqual(
            planPublishedIdentityRefresh(observed: .present(signaturePubkey: ours), derivedSignaturePubkey: ours),
            PublishedIdentityRefreshPlan(raiseAlert: false, mintGenesisAttestation: false)
        )
    }

    func testAReplacedOrUndecodableKeyAlerts() {
        let theirs = Data(repeating: 0x0B, count: 32)
        XCTAssertTrue(
            planPublishedIdentityRefresh(observed: .present(signaturePubkey: theirs), derivedSignaturePubkey: ours)
                .raiseAlert
        )
        XCTAssertTrue(
            planPublishedIdentityRefresh(observed: .present(signaturePubkey: nil), derivedSignaturePubkey: ours)
                .raiseAlert
        )
    }
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
        XCTAssertTrue(CameraPermission.deniedMessage.contains("Settings > Lakko > Camera"))
    }
}
