// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// ADR-0054 S4 - auth without proof of work: the limit copy, and the one
// remaining heavy proof (signup) being cancellable.

final class AuthRateLimitTests: XCTestCase {

    private func http429(_ body: String, endpoint: String = "/login/start") -> AuthClientError {
        .http(status: 429, endpoint: endpoint, body: body)
    }

    func testEveryLimitCodeMapsToItsCopy() {
        let cases: [(String, String)] = [
            (#"{"error":"too-many-attempts","retry_after_secs":30}"#,
             "Too many attempts. Wait a moment, then try again."),
            (#"{"error":"too-many-attempts","retry_after_secs":60}"#,
             "Too many attempts. Wait a moment, then try again."),
            (#"{"error":"too-many-attempts","retry_after_secs":61}"#,
             "Too many attempts. Try again in 2 minutes."),
            (#"{"error":"too-many-attempts","retry_after_secs":900}"#,
             "Too many attempts. Try again in 15 minutes."),
            (#"{"error":"too-many-attempts","retry_after_secs":901}"#,
             "Too many attempts. Try again in 16 minutes."),
            (#"{"error":"device-request-limited","retry_after_secs":600}"#,
             "Too many requests to your other devices. Try again in a few minutes."),
            (#"{"error":"signup-rate-limited","retry_after_secs":3600}"#,
             "Too many new accounts from this network right now. Try again later."),
        ]
        for (body, copy) in cases {
            XCTAssertEqual(AuthRateLimit.copy(for: http429(body)), copy, body)
        }
    }

    func testAnUnparseableLimitStillReadsAsTooManyAttempts() {
        XCTAssertEqual(AuthRateLimit.copy(for: http429("slow down")), AuthRateLimit.tooManyAttemptsCopy)
    }

    func testTheLimiterBeingDownIsTheGenericFailure() {
        let err = AuthClientError.http(
            status: 503, endpoint: "/login/start", body: #"{"error":"rate-limiter-unavailable"}"#)
        XCTAssertEqual(AuthRateLimit.copy(for: err), AuthRateLimit.unavailableCopy)
    }

    func testOtherErrorsAreNotLimits() {
        XCTAssertNil(AuthRateLimit.copy(for: .http(status: 401, endpoint: "/login/start", body: "")))
        XCTAssertNil(AuthRateLimit.copy(for: .http(status: 503, endpoint: "/totp/recover", body: "disabled")))
        XCTAssertNil(AuthRateLimit.copy(for: .argument("x")))
        XCTAssertNil(AuthRateLimit.copy(for: .network(endpoint: "/login/start", underlying: URLError(.timedOut))))
    }

    func testTheRefusalCarriesTheRetryAfter() {
        let refusal = AuthRateLimit.refusal(in: http429(#"{"error":"too-many-attempts","retry_after_secs":120}"#))
        XCTAssertEqual(refusal, AuthRateLimit.Refusal(code: "too-many-attempts", retryAfterSecs: 120))
    }
}

/// Records every auth request; `/pow/challenge` answers per the scope.
final class SignupAuthRecorder: URLProtocol {
    private static let lock = NSLock()
    private static var paths: [String: [String]] = [:]
    private static var challengeStatus: [String: Int] = [:]

    static func scope(challengeStatus status: Int = 200) -> String {
        let host = "s\(UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()).test"
        lock.withLock { challengeStatus[host] = status }
        return host
    }

    static func requests(_ host: String) -> [String] {
        lock.withLock { paths[host] ?? [] }
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url else { return }
        let host = url.host?.lowercased() ?? "?"
        let challenge: Int = Self.lock.withLock {
            Self.paths[host, default: []].append(url.path)
            return Self.challengeStatus[host] ?? 200
        }
        let status: Int
        let body: String
        if url.path == "/pow/challenge" {
            status = challenge
            body = challenge == 200
                ? #"{"seed":"AAAAAAAAAAAAAAAAAAAAAA==","effort_bits":8,"expires_in":300}"#
                : #"{"error":"signup-rate-limited","retry_after_secs":3600}"#
        } else {
            status = 503
            body = ""
        }
        let response = HTTPURLResponse(
            url: url, statusCode: status, httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}

    static func client(_ host: String, solve: @escaping AuthClient.PowSolver) -> AuthClient {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [SignupAuthRecorder.self]
        return AuthClient(baseURL: "http://\(host):9", session: URLSession(configuration: config), solve: solve)
    }
}

/// A solver that never finishes on its own: only cancellation ends it.
private let blockingSolver: AuthClient.PowSolver = { _, _ in
    try await Task.sleep(nanoseconds: 120_000_000_000)
    return PowProofPayload(seed: "s", nonce: "n", solution: "x")
}

final class SignupCancelTests: XCTestCase {

    func testCancellingTheSolveStopsBeforeSignupStart() async throws {
        let host = SignupAuthRecorder.scope()
        let client = SignupAuthRecorder.client(host, solve: blockingSolver)
        let task = Task { try await client.signup(password: Data("password1".utf8)) }
        let solving = await waitUntil { SignupAuthRecorder.requests(host) == ["/pow/challenge"] }
        XCTAssertTrue(solving)

        task.cancel()
        do {
            _ = try await task.value
            XCTFail("a cancelled signup must not complete")
        } catch is CancellationError {
        }
        try await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertEqual(SignupAuthRecorder.requests(host), ["/pow/challenge"],
                       "no /signup/start after a cancel: nothing on the server")
    }

    /// The real solver: the caller is released at once on cancel, even though
    /// the blocking core solve keeps its background thread until it is done.
    func testTheRealSolverReleasesACancelledCaller() async throws {
        let seed = Data(repeating: 0, count: 16).base64EncodedString()
        let task = Task { try await solvePow(seedB64: seed, effortBits: 12) }
        try await Task.sleep(nanoseconds: 50_000_000)
        let cancelledAt = Date()
        task.cancel()
        do {
            _ = try await task.value
            XCTFail("a 12-bit solve does not finish in 50 ms")
        } catch is CancellationError {
        }
        XCTAssertLessThan(Date().timeIntervalSince(cancelledAt), 1)
    }

    /// ADR-0054 S5: cancelling stops the chunk loop itself, not only the
    /// caller. No chunk starts after the cancel beyond the one in flight.
    func testCancelStopsTheChunkLoop() async throws {
        let counter = ChunkCounter()
        let task = Task {
            try await solvePow(seedB64: "seed", effortBits: 30, chunk: { _, _, _ in
                counter.increment()
                Thread.sleep(forTimeInterval: 0.01)
                return nil
            })
        }
        let running = await waitUntil { counter.value >= 3 }
        XCTAssertTrue(running)

        task.cancel()
        do {
            _ = try await task.value
            XCTFail("a cancelled solve must not return a proof")
        } catch is CancellationError {
        }
        let atCancel = counter.value
        try await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertLessThanOrEqual(counter.value, atCancel + 1, "the loop stopped")
        let settled = counter.value
        try await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertEqual(counter.value, settled, "no chunk runs after the cancel")
    }

    func testTheLoopReturnsTheFirstProofAndPassesTheChunkSize() async throws {
        let counter = ChunkCounter()
        let proof = try await solvePow(seedB64: "seed", effortBits: 8, chunk: { seed, bits, attempts in
            XCTAssertEqual(seed, "seed")
            XCTAssertEqual(bits, 8)
            XCTAssertGreaterThanOrEqual(attempts, 1)
            counter.increment()
            return counter.value == 3 ? PowProofPayload(seed: "s", nonce: "n", solution: "x") : nil
        })
        XCTAssertEqual(proof, PowProofPayload(seed: "s", nonce: "n", solution: "x"))
        XCTAssertEqual(counter.value, 3)
    }

    func testABadSeedFailsInsteadOfLooping() async {
        let seed = Data(repeating: 0, count: 3).base64EncodedString()
        do {
            _ = try await solvePow(seedB64: seed, effortBits: 8)
            XCTFail("a 3-byte seed is refused")
        } catch let error as FfiError {
            guard case .Pow = error else { return XCTFail("expected Pow, got \(error)") }
        } catch {
            XCTFail("expected FfiError.Pow, got \(error)")
        }
    }

    func testTheRealChunkedSolverFindsAProof() async throws {
        let seed = Data(repeating: 7, count: 16).base64EncodedString()
        let proof = try await solvePow(seedB64: seed, effortBits: 1)
        XCTAssertEqual(proof.seed, seed)
        XCTAssertFalse(proof.solution.isEmpty)
    }

    func testTheChunkSizeTargetsATenthOfASecond() {
        var sizer = PowChunkSizer()
        XCTAssertEqual(sizer.nextAttempts, 1, "the first chunk is one attempt")
        // 50 ms per attempt: two attempts fit 100 ms.
        sizer.record(attempts: 1, elapsedNanos: 50_000_000)
        XCTAssertEqual(sizer.nextAttempts, 2)
        // 1 ms per attempt: 100 would fit, but growth is capped at 4x.
        sizer.record(attempts: 2, elapsedNanos: 2_000_000)
        XCTAssertEqual(sizer.nextAttempts, 8)
        sizer.record(attempts: 8, elapsedNanos: 8_000_000)
        XCTAssertEqual(sizer.nextAttempts, 32)
        // An attempt slower than the target still runs one at a time.
        sizer.record(attempts: 32, elapsedNanos: 32 * 300_000_000)
        XCTAssertEqual(sizer.nextAttempts, 1)
        // Never past the ceiling, never zero.
        var fast = PowChunkSizer()
        for _ in 0..<20 { fast.record(attempts: fast.nextAttempts, elapsedNanos: 0) }
        XCTAssertEqual(fast.nextAttempts, PowChunkSizer.maxAttempts)
    }

    func testAnAlreadyCancelledSolveNeverStarts() async throws {
        let task = Task {
            try await Task.sleep(nanoseconds: 10_000_000)
            return try await solvePow(seedB64: Data(count: 16).base64EncodedString(), effortBits: 8)
        }
        task.cancel()
        do {
            _ = try await task.value
            XCTFail("cancelled")
        } catch is CancellationError {
        }
    }

    func testASignupRateLimitShowsItsCopy() async throws {
        let host = SignupAuthRecorder.scope(challengeStatus: 429)
        let client = SignupAuthRecorder.client(host, solve: blockingSolver)
        do {
            _ = try await client.signup(password: Data("password1".utf8))
            XCTFail("expected the limit")
        } catch let err as AuthClientError {
            XCTAssertEqual(AuthRateLimit.copy(for: err), AuthRateLimit.signupCopy)
            XCTAssertEqual(AuthRateLimit.refusal(in: err)?.code, AuthRateLimit.signupRateLimited)
        }
    }

    /// The screen: Cancel returns to the filled-in form with no message.
    @MainActor
    func testSignupViewModelCancelReturnsToTheFilledForm() async throws {
        let host = SignupAuthRecorder.scope()
        let repository = AuthRepository(
            sessionStore: SessionStore(defaults: UserDefaults(suiteName: "signup-cancel-\(UUID().uuidString)")!),
            authClientFactory: { SignupAuthRecorder.client(host, solve: blockingSolver) },
            persistence: NullPersistence(),
            nowMs: { 1_000_000 },
            accountDirectory: UnusedDirectory(),
            restoreDirectory: OfflineRestoreDirectory(),
            earlierKeysInputs: skipEarlierKeysGate
        )
        let viewModel = SignupViewModel(repository: repository)
        viewModel.username = "alice_1"
        viewModel.password = "password1"
        viewModel.confirm = "password1"

        viewModel.onSubmit()
        XCTAssertTrue(viewModel.busy)
        XCTAssertTrue(viewModel.cancellable)
        XCTAssertEqual(SignupViewModel.workingTitle, "Creating your account...")
        XCTAssertEqual(
            SignupViewModel.workingNote,
            "This takes a little while, once. You will not wait like this again.")
        let solving = await waitUntil { SignupAuthRecorder.requests(host) == ["/pow/challenge"] }
        XCTAssertTrue(solving)

        viewModel.onCancel()
        XCTAssertFalse(viewModel.busy)
        XCTAssertNil(viewModel.error)
        XCTAssertEqual(viewModel.username, "alice_1")
        XCTAssertEqual(viewModel.password, "password1", "the form keeps its contents")

        try await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertFalse(viewModel.busy, "the cancelled attempt does not come back")
        XCTAssertNil(viewModel.error)
        XCTAssertEqual(SignupAuthRecorder.requests(host), ["/pow/challenge"])
    }
}

@MainActor
private func waitUntil(_ condition: () -> Bool, timeout: TimeInterval = 5) async -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if condition() { return true }
        try? await Task.sleep(nanoseconds: 10_000_000)
    }
    return condition()
}

private final class ChunkCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    var value: Int { lock.withLock { count } }
    func increment() { lock.withLock { count += 1 } }
}

private struct NullPersistence: SessionPersistence {
    func hasDeviceKey(accountIdB64: String) -> Bool { false }
    func accountIdentitySeed(accountIdB64: String) -> Data? { nil }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { nil }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {}
    func deleteSessionToken(accountIdB64: String) {}
}

private struct UnusedDirectory: AccountDirectory {
    func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
        throw AuthClientError.argument("unused")
    }
    func publishIdentityBundle(
        accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
        leafAuth: Data?, genesisAttest: Data?, keyResetAttest: Data?, sessionToken: String
    ) async throws {
        throw AuthClientError.argument("unused")
    }
}
