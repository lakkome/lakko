// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

// ADR-0054 S5 - sign in and recover by name. Every pre-session route names the
// account by its login handle (base64url, 32 bytes) and carries no account id;
// no discovery lookup runs before a session; the account id is learned from
// the finish; a successful sign-in by name re-binds the handle.

/// One recorded auth request.
struct RecordedAuthRequest {
    let method: String
    let path: String
    let json: [String: Any]
    let authorization: String?
}

/// A per-host scripted auth service.
final class NameRouteStub: URLProtocol {
    typealias Handler = (_ method: String, _ path: String, _ json: [String: Any]) -> (Int, String)

    private static let lock = NSLock()
    private static var handlers: [String: Handler] = [:]
    private static var recorded: [String: [RecordedAuthRequest]] = [:]

    static func scope(_ handler: @escaping Handler) -> String {
        let host = "n\(UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()).test"
        lock.withLock { handlers[host] = handler }
        return host
    }

    static func requests(_ host: String) -> [RecordedAuthRequest] {
        lock.withLock { recorded[host] ?? [] }
    }

    static func client(_ host: String) -> AuthClient {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [NameRouteStub.self]
        return AuthClient(baseURL: "http://\(host):9", session: URLSession(configuration: config))
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url else { return }
        let host = url.host?.lowercased() ?? "?"
        let method = request.httpMethod ?? "GET"
        let body = Self.readBody(request)
        let json = ((try? JSONSerialization.jsonObject(with: body)) as? [String: Any]) ?? [:]
        let handler: Handler? = Self.lock.withLock {
            Self.recorded[host, default: []].append(RecordedAuthRequest(
                method: method, path: url.path, json: json,
                authorization: request.value(forHTTPHeaderField: "Authorization")))
            return Self.handlers[host]
        }
        let (status, text) = handler?(method, url.path, json) ?? (503, "")
        let response = HTTPURLResponse(
            url: url, statusCode: status, httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(text.utf8))
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

private func wireHandle(_ name: String) -> String {
    base64UrlEncodeNoPad((try? loginHandle(username: name)) ?? Data())
}

// MARK: - The handle

final class LoginNameTests: XCTestCase {

    func testTheHandleIsTheCoresAndItsWireIsBase64UrlOf32Bytes() throws {
        let name = try LoginName.make("  @Alice_1 ", emptyError: AuthClientError.argument("empty"))
        XCTAssertEqual(name.canonical, "alice_1")
        XCTAssertEqual(name.handle, try loginHandle(username: "alice_1"))
        XCTAssertEqual(name.handle.count, 32)
        XCTAssertEqual(name.wire, base64UrlEncodeNoPad(name.handle))
        XCTAssertFalse(name.wire.contains("="))
        XCTAssertFalse(name.wire.contains("+"))
        XCTAssertFalse(name.wire.contains("/"))
        XCTAssertEqual(base64UrlDecodeNoPad(name.wire), name.handle)
        XCTAssertEqual(try LoginName.make("alice_1", emptyError: AuthClientError.argument("e")).handle,
                       name.handle, "the same account for any spelling of the name")
    }

    func testEachRuleRefusalFromTheHandleMapsToItsReasonCopy() {
        let cases: [(FfiError, UsernameRuleReason?)] = [
            (.UsernameTooShort(message: ""), .tooShort),
            (.UsernameTooLong(message: ""), .tooLong),
            (.UsernameInvalidCharacters(message: ""), .invalidCharacters),
            (.UsernameEmpty(message: ""), nil),
        ]
        for (error, reason) in cases {
            do {
                _ = try LoginName.make(
                    "alice", emptyError: AuthClientError.argument("empty"),
                    validate: { $0 }, derive: { _ in throw error })
                XCTFail("\(error) must refuse")
            } catch let violation as UsernameRuleViolation {
                XCTAssertEqual(violation.reason, reason, "\(error)")
            } catch AuthClientError.argument(let message) {
                XCTAssertNil(reason)
                XCTAssertEqual(message, "empty")
            } catch {
                XCTFail("unexpected \(error)")
            }
        }
    }

    func testTheTypedNameWinsAndAnEmptyFieldFallsBackToTheCachedOne() throws {
        let cached = CachedAccount(accountIdB64: "CACHED", username: "alice")
        XCTAssertEqual(
            try signInName(typed: "bob", cached: cached, emptyError: AuthClientError.argument("e")).canonical,
            "bob")
        XCTAssertEqual(
            try signInName(typed: "  ", cached: cached, emptyError: AuthClientError.argument("e")).canonical,
            "alice")
        XCTAssertEqual(
            try signInName(typed: nil, cached: cached, emptyError: AuthClientError.argument("e")).canonical,
            "alice")
    }

    func testAGuestOrAFreshDeviceWithNothingTypedHasNoName() {
        for cached in [nil, CachedAccount(accountIdB64: "GUEST", username: "")] {
            do {
                _ = try signInName(typed: "", cached: cached, emptyError: AuthClientError.argument("type it"))
                XCTFail("no name, no handle")
            } catch AuthClientError.argument(let message) {
                XCTAssertEqual(message, "type it")
            } catch {
                XCTFail("unexpected \(error)")
            }
        }
    }

    func testLoginByNameDisabledShowsTheGenericFailureCopy() {
        let err = AuthClientError.http(
            status: 503, endpoint: "/login/start", body: #"{"error":"login-by-name-disabled"}"#)
        XCTAssertEqual(AuthRateLimit.copy(for: err), AuthRateLimit.unavailableCopy)
        XCTAssertTrue(AuthRateLimit.isLoginByNameDisabled(err))
    }
}

// MARK: - The routes

final class NameRouteWireTests: XCTestCase {

    private let handle = (try? loginHandle(username: "alice")) ?? Data()

    private func assertNamesTheHandle(_ request: RecordedAuthRequest?, _ path: String) {
        guard let request else { return XCTFail("no request to \(path)") }
        XCTAssertEqual(request.path, path)
        XCTAssertEqual(request.json["handle"] as? String, wireHandle("alice"), path)
        XCTAssertNil(request.json["account_id"], path)
        XCTAssertNil(request.json["account_id_b64"], path)
        XCTAssertNil(request.authorization, "\(path) is anonymous")
    }

    func testLoginStartNamesTheHandle() async {
        let host = NameRouteStub.scope { _, _, _ in (401, "") }
        do {
            _ = try await NameRouteStub.client(host).login(handle: handle, password: Data("password1".utf8))
            XCTFail("401")
        } catch {}
        assertNamesTheHandle(NameRouteStub.requests(host).first, "/login/start")
        XCTAssertNotNil(NameRouteStub.requests(host).first?.json["request"])
    }

    func testPasswordResetStartNamesTheHandle() async {
        let host = NameRouteStub.scope { _, _, _ in (401, "") }
        let seed = RecoveryMnemonic.generate().toSeed()
        do {
            _ = try await NameRouteStub.client(host).recover(
                handle: handle, mnemonicSeed: seed, newPassword: Data("password1".utf8))
            XCTFail("401")
        } catch {}
        let start = NameRouteStub.requests(host).first
        assertNamesTheHandle(start, "/password/reset/start")
        XCTAssertNotNil(start?.json["recovery_request"])
        XCTAssertNotNil(start?.json["new_password_request"])
    }

    func testTotpRecoverNamesTheHandleAndLearnsTheAccountIdFromTheResponse() async throws {
        let accountId = Data((0..<16).map { UInt8($0) })
        let host = NameRouteStub.scope { _, _, _ in
            (200, #"{"account_id_b64":"\#(base64UrlEncodeNoPad(accountId))","session_token":"t","session_expires_at_ms":9}"#)
        }
        let result = try await NameRouteStub.client(host).totpRecover(handle: handle, code: "123456")
        XCTAssertEqual(result.accountId, accountId)
        let request = NameRouteStub.requests(host).first
        assertNamesTheHandle(request, "/totp/recover")
        XCTAssertEqual(request?.json["code"] as? String, "123456")
    }

    func testRecoveryContactStartAndFinishNameTheHandle() async throws {
        let accountId = Data(repeating: 0xAB, count: 16)
        let host = NameRouteStub.scope { _, path, _ in
            if path == "/recovery-contact/recover/start" { return (200, #"{"sent":true}"#) }
            return (200, #"{"account_id_b64":"\#(base64UrlEncodeNoPad(accountId))","session_token":"t","session_expires_at_ms":9}"#)
        }
        let client = NameRouteStub.client(host)
        try await client.recoveryContactRecoverStart(handle: handle, kind: .email, contact: "a@b.co")
        let result = try await client.recoveryContactRecoverFinish(handle: handle, kind: .email, code: "123456")
        XCTAssertEqual(result.accountId, accountId)
        let requests = NameRouteStub.requests(host)
        assertNamesTheHandle(requests.first, "/recovery-contact/recover/start")
        assertNamesTheHandle(requests.last, "/recovery-contact/recover/finish")
        XCTAssertEqual(requests.first?.json["contact"] as? String, "a@b.co")
    }

    func testAHandleThatIsNot32BytesIsNeverSent() async {
        let host = NameRouteStub.scope { _, _, _ in (200, "{}") }
        do {
            _ = try await NameRouteStub.client(host).totpRecover(handle: Data(count: 16), code: "123456")
            XCTFail("a 16-byte handle")
        } catch AuthClientError.argument {
        } catch {
            XCTFail("unexpected \(error)")
        }
        XCTAssertTrue(NameRouteStub.requests(host).isEmpty)
    }

    func testTheBindingRoutes() async throws {
        let host = NameRouteStub.scope { method, _, _ in
            method == "PUT" ? (200, #"{"bound":true}"#) : (200, #"{"removed":true}"#)
        }
        let client = NameRouteStub.client(host)
        try await client.bindLoginHandle(handle: handle, sessionToken: "tok")
        let removed = try await client.removeLoginHandle(sessionToken: "tok")
        XCTAssertTrue(removed)
        let requests = NameRouteStub.requests(host)
        XCTAssertEqual(requests.map { "\($0.method) \($0.path)" },
                       ["PUT /auth/login-handle", "DELETE /auth/login-handle"])
        XCTAssertEqual(requests.first?.json["handle"] as? String, wireHandle("alice"))
        XCTAssertEqual(requests.map(\.authorization), ["Bearer tok", "Bearer tok"])
    }

    func testATakenHandleIs409() async {
        let host = NameRouteStub.scope { _, _, _ in (409, #"{"error":"handle-taken"}"#) }
        do {
            try await NameRouteStub.client(host).bindLoginHandle(handle: handle, sessionToken: "tok")
            XCTFail("409")
        } catch AuthClientError.http(let status, _, let body) {
            XCTAssertEqual(status, 409)
            XCTAssertTrue(body.contains(handleTakenCode))
        } catch {
            XCTFail("unexpected \(error)")
        }
    }
}

// MARK: - The flows

/// A directory any call to which fails the test: nothing before a session
/// may reach discovery.
private final class ForbiddenDirectory: AccountDirectory {
    private(set) var calls = 0

    func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
        calls += 1
        throw AuthClientError.argument("discovery must not be reached")
    }

    func publishIdentityBundle(
        accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
        leafAuth: Data?, genesisAttest: Data?, sessionToken: String
    ) async throws {
        calls += 1
        throw AuthClientError.argument("discovery must not be reached")
    }
}

private struct NoPersistence: SessionPersistence {
    func hasDeviceKey(accountIdB64: String) -> Bool { false }
    func accountIdentitySeed(accountIdB64: String) -> Data? { nil }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { nil }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {}
    func deleteSessionToken(accountIdB64: String) {}
}

@MainActor
final class LoginByNameFlowTests: XCTestCase {

    private func store(_ marker: PersistedSession? = nil) -> SessionStore {
        let store = SessionStore(defaults: UserDefaults(suiteName: "login-by-name-\(UUID().uuidString)")!)
        if let marker { store.save(marker) }
        return store
    }

    private func repository(_ host: String, _ sessionStore: SessionStore, _ directory: ForbiddenDirectory) -> AuthRepository {
        AuthRepository(
            sessionStore: sessionStore,
            authClientFactory: { NameRouteStub.client(host) },
            persistence: NoPersistence(),
            nowMs: { 1_000_000 },
            accountDirectory: directory
        )
    }

    private func waitUntil(_ condition: () -> Bool, timeout: TimeInterval = 5) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        return condition()
    }

    func testLoginOnAFreshDeviceSendsTheHandleAndNothingElse() async {
        let host = NameRouteStub.scope { _, _, _ in (401, "") }
        let directory = ForbiddenDirectory()
        let sessionStore = store()
        let repo = repository(host, sessionStore, directory)
        do {
            try await repo.login(username: "@Alice", password: Data("password1".utf8))
            XCTFail("the stub refuses")
        } catch AuthClientError.http(let status, let endpoint, _) {
            XCTAssertEqual(status, 401)
            XCTAssertEqual(endpoint, "/login/start")
        } catch {
            XCTFail("unexpected \(error)")
        }
        let requests = NameRouteStub.requests(host)
        XCTAssertEqual(requests.map(\.path), ["/login/start"])
        XCTAssertEqual(requests.first?.json["handle"] as? String, wireHandle("alice"))
        XCTAssertNil(requests.first?.json["account_id"])
        XCTAssertEqual(directory.calls, 0, "no discovery before a session")
        XCTAssertNil(sessionStore.load())
    }

    func testLoginWithACachedAccountStillNamesTheHandle() async {
        let host = NameRouteStub.scope { _, _, _ in (401, "") }
        let repo = repository(
            host, store(PersistedSession(accountIdB64: base64Encode(Data(count: 16)), username: "alice")),
            ForbiddenDirectory())
        _ = try? await repo.login(username: "alice", password: Data("password1".utf8))
        let body = NameRouteStub.requests(host).first?.json
        XCTAssertEqual(body?["handle"] as? String, wireHandle("alice"))
        XCTAssertNil(body?["account_id"])
    }

    func testAnInvalidNameNeverReachesAuth() async {
        let host = NameRouteStub.scope { _, _, _ in (401, "") }
        let repo = repository(host, store(), ForbiddenDirectory())
        do {
            try await repo.login(username: "a", password: Data("password1".utf8))
            XCTFail("too short")
        } catch let violation as UsernameRuleViolation {
            XCTAssertEqual(violation.reason, .tooShort)
        } catch {
            XCTFail("unexpected \(error)")
        }
        XCTAssertTrue(NameRouteStub.requests(host).isEmpty)
    }

    func testRecoverFromWordsOnAFreshDeviceSendsTheHandle() async {
        let host = NameRouteStub.scope { _, _, _ in (401, "") }
        let directory = ForbiddenDirectory()
        let repo = repository(host, store(), directory)
        do {
            try await repo.recover(
                mnemonicPhrase: RecoveryMnemonic.generate().phrase(),
                newPassword: Data("password1".utf8),
                username: "Alice"
            )
            XCTFail("the stub refuses")
        } catch {}
        let requests = NameRouteStub.requests(host)
        XCTAssertEqual(requests.map(\.path), ["/password/reset/start"])
        XCTAssertEqual(requests.first?.json["handle"] as? String, wireHandle("alice"))
        XCTAssertNil(requests.first?.json["account_id"])
        XCTAssertEqual(directory.calls, 0)
    }

    func testRecoverWithTheFieldEmptyUsesTheCachedName() async {
        let host = NameRouteStub.scope { _, _, _ in (401, "") }
        let repo = repository(
            host, store(PersistedSession(accountIdB64: base64Encode(Data(count: 16)), username: "alice")),
            ForbiddenDirectory())
        _ = try? await repo.recover(
            mnemonicPhrase: RecoveryMnemonic.generate().phrase(),
            newPassword: Data("password1".utf8),
            username: nil
        )
        XCTAssertEqual(NameRouteStub.requests(host).first?.json["handle"] as? String, wireHandle("alice"))
    }

    /// A TOTP recovery on a fresh device: the handle goes out, the account id
    /// comes back and is what the marker records, and the handle is re-bound
    /// with the new session.
    func testTotpRecoveryLearnsTheAccountAndRebindsTheHandle() async throws {
        let accountId = Data((0..<16).map { _ in UInt8.random(in: 0...255) })
        let host = NameRouteStub.scope { method, path, _ in
            if path == "/totp/recover" {
                return (200, #"{"account_id_b64":"\#(base64UrlEncodeNoPad(accountId))","session_token":"tok-totp","session_expires_at_ms":9000000}"#)
            }
            if method == "PUT", path == "/auth/login-handle" { return (200, #"{"bound":true}"#) }
            return (503, "")
        }
        let directory = ForbiddenDirectory()
        let sessionStore = store()
        let repo = repository(host, sessionStore, directory)

        try await repo.recoverWithTotp(username: "@Alice", code: "123456")

        XCTAssertEqual(sessionStore.load(),
                       PersistedSession(accountIdB64: base64Encode(accountId), username: "alice"))
        XCTAssertEqual(repo.state, .needsRecoveryPhrase(
            SessionAccount(accountIdB64: base64Encode(accountId), username: "alice")))
        let rebound = await waitUntil {
            NameRouteStub.requests(host).contains { $0.method == "PUT" && $0.path == "/auth/login-handle" }
        }
        XCTAssertTrue(rebound, "a sign-in by name re-binds the handle")
        let requests = NameRouteStub.requests(host)
        XCTAssertEqual(requests.map(\.path), ["/totp/recover", "/auth/login-handle"])
        XCTAssertEqual(requests.first?.json["handle"] as? String, wireHandle("alice"))
        XCTAssertEqual(requests.last?.json["handle"] as? String, wireHandle("alice"))
        XCTAssertEqual(requests.last?.authorization, "Bearer tok-totp")
        XCTAssertEqual(directory.calls, 0)
    }

    func testContactRecoveryNamesTheHandleOnBothStepsAndLearnsTheAccountFromTheFinish() async throws {
        let accountId = Data((0..<16).map { _ in UInt8.random(in: 0...255) })
        let host = NameRouteStub.scope { method, path, _ in
            switch (method, path) {
            case (_, "/recovery-contact/recover/start"): return (200, #"{"sent":true}"#)
            case (_, "/recovery-contact/recover/finish"):
                return (200, #"{"account_id_b64":"\#(base64UrlEncodeNoPad(accountId))","session_token":"tok-c","session_expires_at_ms":9000000}"#)
            case ("PUT", "/auth/login-handle"): return (200, #"{"bound":true}"#)
            default: return (503, "")
            }
        }
        let directory = ForbiddenDirectory()
        let sessionStore = store()
        let repo = repository(host, sessionStore, directory)

        try await repo.recoverWithContactStart(username: "alice", kind: .email, contact: "a@b.co")
        try await repo.recoverWithContactFinish(code: "123456")

        XCTAssertEqual(sessionStore.load()?.accountIdB64, base64Encode(accountId))
        let rebound = await waitUntil { NameRouteStub.requests(host).count == 3 }
        XCTAssertTrue(rebound)
        let requests = NameRouteStub.requests(host)
        XCTAssertEqual(requests.map(\.path),
                       ["/recovery-contact/recover/start", "/recovery-contact/recover/finish", "/auth/login-handle"])
        for request in requests {
            XCTAssertEqual(request.json["handle"] as? String, wireHandle("alice"), request.path)
            XCTAssertNil(request.json["account_id_b64"], request.path)
        }
        XCTAssertEqual(directory.calls, 0)
    }

    func testAnUnknownNameIsTheOrdinaryWrongCodeOutcome() async {
        let host = NameRouteStub.scope { _, _, _ in (401, #"{"error":"invalid"}"#) }
        let repo = repository(host, store(), ForbiddenDirectory())
        do {
            try await repo.recoverWithTotp(username: "nobody", code: "123456")
            XCTFail("401")
        } catch AuthClientError.http(let status, _, _) {
            XCTAssertEqual(status, 401)
        } catch {
            XCTFail("unexpected \(error)")
        }
        XCTAssertEqual(NameRouteStub.requests(host).map(\.path), ["/totp/recover"])
    }
}
