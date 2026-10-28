// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// ADR-0054 S3 - the anonymous-token pool, its issuer, the one-retry rule on
// a token route, and the privacy rule every token route keeps: no
// `Authorization`, no cookie, the token header only.

/// A token source that hands out `<context>-<n>` and records what was taken.
final class FakeAnonTokens: AnonTokenSource, @unchecked Sendable {
    private let lock = NSLock()
    private var counters: [AnonTokenContext: Int] = [:]
    private var takenContexts: [AnonTokenContext] = []
    private let failure: AnonTokenUnavailable?

    init(failure: AnonTokenUnavailable? = nil) {
        self.failure = failure
    }

    var taken: [AnonTokenContext] { lock.withLock { takenContexts } }

    func take(_ context: AnonTokenContext) async throws -> AnonToken {
        if let failure { throw failure }
        let n: Int = lock.withLock {
            takenContexts.append(context)
            counters[context, default: 0] += 1
            return counters[context] ?? 0
        }
        return AnonToken(context: context.rawValue, epoch: 0, headerValue: "\(context.rawValue)-\(n)")
    }
}

/// An issuer that answers with tokens at the requested epoch, or with a
/// scripted failure per call.
final class FakeIssuer: AnonTokenIssuing, @unchecked Sendable {
    struct Call: Equatable {
        let context: AnonTokenContext
        let epoch: UInt64
        let count: UInt32
        let sessionToken: String
    }

    private let lock = NSLock()
    private var recorded: [Call] = []
    private var issued = 0
    private var script: [AnonTokenUnavailable?]
    private let tokenEpoch: ((UInt64) -> UInt64)?
    private let hang: Bool

    init(
        script: [AnonTokenUnavailable?] = [],
        tokenEpoch: ((UInt64) -> UInt64)? = nil,
        hang: Bool = false
    ) {
        self.script = script
        self.tokenEpoch = tokenEpoch
        self.hang = hang
    }

    var calls: [Call] { lock.withLock { recorded } }

    /// The issuances asked for `context` (every context's first refill runs
    /// when a session is bound, ADR-0078 section 8).
    func calls(for context: AnonTokenContext) -> [Call] {
        calls.filter { $0.context == context }
    }

    func issue(
        context: AnonTokenContext,
        epoch: UInt64,
        count: UInt32,
        sessionToken: String
    ) async throws -> [AnonToken] {
        let next: AnonTokenUnavailable? = lock.withLock {
            recorded.append(Call(context: context, epoch: epoch, count: count, sessionToken: sessionToken))
            return script.isEmpty ? nil : script.removeFirst()
        }
        if hang { try await Task.sleep(nanoseconds: 60_000_000_000) }
        if let next { throw next }
        let e = tokenEpoch?(epoch) ?? epoch
        let base: Int = lock.withLock {
            defer { issued += Int(count) }
            return issued
        }
        return (0..<Int(count)).map {
            AnonToken(context: context.rawValue, epoch: e, headerValue: "\(context.rawValue)-\(base + $0)")
        }
    }
}

/// Wait (bounded) until `condition` holds.
private func eventually(
    _ condition: () -> Bool,
    timeout: TimeInterval = 5
) async -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        if condition() { return true }
        try? await Task.sleep(nanoseconds: 10_000_000)
    }
    return condition()
}

/// An issuer that refuses the first `failures.count` issuances of ONE
/// context with the scripted failures and answers every other call through
/// `inner` (every context's first refill runs at once when a session is
/// bound, so a script shared by all contexts would hit an arbitrary one).
final class OneContextScriptIssuer: AnonTokenIssuing, @unchecked Sendable {
    let inner = FakeIssuer()
    private let context: AnonTokenContext
    private let lock = NSLock()
    private var failures: [AnonTokenUnavailable]

    init(_ context: AnonTokenContext, failures: [AnonTokenUnavailable]) {
        self.context = context
        self.failures = failures
    }

    func issue(context: AnonTokenContext, epoch: UInt64, count: UInt32, sessionToken: String)
        async throws -> [AnonToken]
    {
        let failure: AnonTokenUnavailable? = lock.withLock {
            guard context == self.context, !failures.isEmpty else { return nil }
            return failures.removeFirst()
        }
        if let failure {
            _ = try? await inner.issue(context: context, epoch: epoch, count: 0, sessionToken: sessionToken)
            throw failure
        }
        return try await inner.issue(context: context, epoch: epoch, count: count, sessionToken: sessionToken)
    }
}

/// A pool whose first scheduled refill of each context runs at once (the
/// random moment drawn is 0); the later ones are 30 minutes out, beyond any
/// test (ADR-0078 section 8).
func makePool(
    _ issuer: AnonTokenIssuing,
    store: AnonTokenStore? = nil,
    waitSeconds: TimeInterval = AnonTokenPool.defaultWaitSeconds
) -> AnonTokenPool {
    AnonTokenPool(issuer: issuer, store: store, waitSeconds: waitSeconds, random: { 0 })
}

/// A pool whose refill schedule never fires by itself: its sleep lasts until
/// the binding cancels it. Since `initialRefillSpread` became zero (2026-10-22)
/// a bound session tops every context up at once, in a background task, and a
/// test that reads the store or a count right after `configure` raced it (seen
/// as 16 tokens where 15 were left, and a removed context written back). A test
/// on this pool refills only by `runScheduledRefills()`, where it says so.
func makeQuietPool(_ issuer: AnonTokenIssuing, store: AnonTokenStore? = nil) -> AnonTokenPool {
    AnonTokenPool(
        issuer: issuer,
        store: store,
        random: { 0 },
        sleep: { _ in try? await Task.sleep(nanoseconds: UInt64.max) }
    )
}

// MARK: - The pool

final class AnonTokenPoolTests: XCTestCase {

    func testContextNamesAreTheCoresInWireOrder() {
        XCTAssertEqual(AnonTokenContext.allCases.map(\.rawValue), anonTokenContexts())
        // The twelve names as literals too, so a rename on either side shows up
        // here as a named difference (source of truth:
        // `crates/lakko-anon-token/src/context.rs`, `TokenContext::ALL`).
        XCTAssertEqual(anonTokenContexts(), [
            "discovery-lookup", "discovery-register", "invite-consume", "ds-knock",
            "push-register", "channel-subscribe", "directory-search",
            "vault-claim", "blob-upload", "blob-read", "mailbox-write", "call-join",
        ])
        XCTAssertEqual(AnonTokenRoute.headerName, "x-lakko-anon-token")
    }

    func testTheFirstScheduledRefillDrawsADayOfUseAndATakeWaitsForIt() async throws {
        let issuer = FakeIssuer()
        let pool = makePool(issuer)
        pool.configure(accountKey: "A", sessionToken: "s1")

        let token = try await pool.take(.discoveryLookup)

        XCTAssertEqual(token.context, "discovery-lookup")
        let call = try XCTUnwrap(issuer.calls(for: .discoveryLookup).first)
        XCTAssertEqual(Int(call.count), AnonTokenContext.discoveryLookup.poolTarget)
        XCTAssertEqual(call.sessionToken, "s1")
        XCTAssertEqual(call.epoch, pool.currentEpoch())
        XCTAssertEqual(pool.count(.discoveryLookup), AnonTokenContext.discoveryLookup.poolTarget - 1, "the token taken leaves the pool")
    }

    /// ADR-0078 section 8: the token pool refills on a timer, not on spend.
    func testThePoolRefillsOnATimerNotOnSpend() async throws {
        let issuer = FakeIssuer()
        let pool = makePool(issuer, waitSeconds: 0.3)
        pool.configure(accountKey: "A", sessionToken: "s1")
        _ = try await pool.take(.dsKnock)
        while pool.takeIfAvailable(.dsKnock) != nil {}
        try await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertEqual(issuer.calls(for: .dsKnock).count, 1, "spending the bucket empty draws nothing")
        do {
            _ = try await pool.take(.dsKnock)
            XCTFail("an empty bucket must not draw at the moment of use")
        } catch let error as AnonTokenUnavailable {
            XCTAssertEqual(error, .timedOut)
        }
        XCTAssertEqual(issuer.calls(for: .dsKnock).count, 1)
        // The schedule's next refill tops it up to its target in one issuance.
        await pool.runScheduledRefills()
        XCTAssertEqual(issuer.calls(for: .dsKnock).map(\.count), [8, 8])
        XCTAssertEqual(pool.count(.dsKnock), 8)
    }

    func testEachContextHasItsPoolTarget() async throws {
        let expected: [AnonTokenContext: UInt32] = [
            .discoveryLookup: 32, .pushRegister: 8, .inviteConsume: 4, .dsKnock: 8, .discoveryRegister: 2,
            .channelSubscribe: 4, .directorySearch: 4,
            .vaultClaim: 8, .blobUpload: 4, .blobRead: 4,
            .mailboxWrite: 4,
            .callJoin: 4,
        ]
        XCTAssertEqual(Set(expected.keys), Set(AnonTokenContext.allCases), "every context has a pinned policy")
        let issuer = FakeIssuer()
        let pool = makePool(issuer)
        pool.configure(accountKey: "A", sessionToken: "s1")
        for (context, target) in expected {
            _ = try await pool.take(context)
            XCTAssertEqual(issuer.calls(for: context).first?.count, target, context.rawValue)
            XCTAssertEqual(context.poolTarget, Int(target), context.rawValue)
        }
    }

    func testTokensAreNeverReusedWithinAContext() async throws {
        let pool = makePool(FakeIssuer())
        pool.configure(accountKey: "A", sessionToken: "s1")
        var seen = Set<String>()
        for _ in 0..<16 {
            let token = try await pool.take(.discoveryLookup)
            XCTAssertTrue(seen.insert(token.headerValue).inserted, "reused \(token.headerValue)")
        }
    }

    func testLogoutClearsThePoolAndAnAccountSwitchDoesToo() async throws {
        let issuer = FakeIssuer()
        let pool = makePool(issuer)
        pool.configure(accountKey: "A", sessionToken: "s1")
        _ = try await pool.take(.dsKnock)
        XCTAssertEqual(pool.count(.dsKnock), 7)

        pool.configure(accountKey: "A", sessionToken: "s1-refreshed")
        XCTAssertEqual(pool.count(.dsKnock), 7, "a fresh session for the same account keeps its tokens")

        pool.configure(accountKey: "B", sessionToken: "s2")
        XCTAssertEqual(pool.count(.dsKnock), 0, "account switch")
        _ = try await pool.take(.dsKnock)
        XCTAssertEqual(issuer.calls(for: .dsKnock).last?.sessionToken, "s2")

        pool.clear()
        XCTAssertEqual(pool.count(.dsKnock), 0, "logout")
        XCTAssertNil(pool.takeIfAvailable(.dsKnock))
        do {
            _ = try await pool.take(.dsKnock)
            XCTFail("no session, no token")
        } catch let error as AnonTokenUnavailable {
            XCTAssertEqual(error, .noSession)
        }
    }

    func testTakeIfAvailableNeverStartsARefill() async throws {
        let issuer = FakeIssuer()
        // No session: no schedule runs.
        let pool = makePool(issuer)
        pool.configure(accountKey: "A", sessionToken: nil)
        XCTAssertNil(pool.takeIfAvailable(.pushRegister))
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertTrue(issuer.calls.isEmpty)
    }

    func testAUsedUpAllowanceSurfacesAndIsRemembered() async throws {
        let issuer = OneContextScriptIssuer(.discoveryLookup, failures: [.quotaExhausted(retryAfterSecs: 3600)])
        let pool = makePool(issuer)
        pool.configure(accountKey: "A", sessionToken: "s1")
        do {
            _ = try await pool.take(.discoveryLookup)
            XCTFail("expected the allowance refusal")
        } catch let error as AnonTokenUnavailable {
            XCTAssertEqual(error, .quotaExhausted(retryAfterSecs: 3600))
            XCTAssertEqual(error.errorDescription, AnonTokenCopy.allowanceUsed)
            XCTAssertEqual(AnonTokenCopy.searchMessage(for: error), AnonTokenCopy.searchAllowanceUsed)
        }
        do {
            _ = try await pool.take(.discoveryLookup)
            XCTFail("still exhausted")
        } catch let AnonTokenUnavailable.quotaExhausted(retry) {
            XCTAssertNotNil(retry)
        }
        await pool.runScheduledRefills()
        XCTAssertEqual(
            issuer.inner.calls(for: .discoveryLookup).count, 1,
            "no second issuance while the allowance is used up")
    }

    func testEpochNotIssuableRebuildsOnceForTheIssuersEpoch() async throws {
        let probe = makePool(FakeIssuer())
        let serverEpoch = probe.currentEpoch() + 1
        let issuer = OneContextScriptIssuer(.dsKnock, failures: [.epochNotIssuable(currentEpoch: serverEpoch)])
        let skewed = makePool(issuer)
        skewed.configure(accountKey: "A", sessionToken: "s1")

        let token = try await skewed.take(.dsKnock)

        XCTAssertEqual(issuer.inner.calls(for: .dsKnock).map(\.epoch), [serverEpoch - 1, serverEpoch])
        XCTAssertEqual(token.epoch, serverEpoch)
        XCTAssertEqual(skewed.currentEpoch(), serverEpoch, "the server's epoch corrects the local one")
    }

    func testDeadTokensAreDropped() async throws {
        // The issuer hands back tokens two epochs behind: never live.
        let issuer = FakeIssuer(tokenEpoch: { $0 &- 2 })
        let pool = makePool(issuer, waitSeconds: 0.3)
        pool.configure(accountKey: "A", sessionToken: "s1")
        do {
            _ = try await pool.take(.dsKnock)
            XCTFail("a dead token must not be spent")
        } catch let error as AnonTokenUnavailable {
            XCTAssertEqual(error, .timedOut)
        }
    }

    func testAWaitThatRunsOutFailsWithTheNetworkCopy() async throws {
        let pool = makePool(FakeIssuer(hang: true), waitSeconds: 0.2)
        pool.configure(accountKey: "A", sessionToken: "s1")
        let started = Date()
        do {
            _ = try await pool.take(.inviteConsume)
            XCTFail("expected a timeout")
        } catch let error as AnonTokenUnavailable {
            XCTAssertEqual(error, .timedOut)
            XCTAssertEqual(error.errorDescription, AnonTokenCopy.networkFailure)
        }
        XCTAssertLessThan(Date().timeIntervalSince(started), 5)
        pool.clear()
    }

    func testDisabledIssuanceFailsWithTheNetworkCopy() async throws {
        let pool = makePool(OneContextScriptIssuer(.pushRegister, failures: [.disabled]))
        pool.configure(accountKey: "A", sessionToken: "s1")
        do {
            _ = try await pool.take(.pushRegister)
            XCTFail("expected disabled")
        } catch let error as AnonTokenUnavailable {
            XCTAssertEqual(error, .disabled)
            XCTAssertEqual(error.errorDescription, AnonTokenCopy.networkFailure)
        }
    }

    func testAPrepaidTokenIsSpentFirstThenTheFallback() async throws {
        let prepaid = AnonToken(context: "push-register", epoch: 1, headerValue: "prepaid")
        let fallback = FakeAnonTokens()
        let source = PrepaidAnonToken(prepaid, then: fallback)
        let first = try await source.take(.pushRegister)
        let second = try await source.take(.pushRegister)
        XCTAssertEqual(first.headerValue, "prepaid")
        XCTAssertEqual(second.headerValue, "push-register-1")
    }
}

// MARK: - Persistence between launches (ADR-0054 S5)

/// An in-memory ``AnonTokenStore``: a new pool over the same store object is
/// what a relaunch looks like to the pool.
final class InMemoryAnonTokenStore: AnonTokenStore, @unchecked Sendable {
    private let lock = NSLock()
    private var rows: [String: [AnonTokenContext: [AnonToken]]] = [:]

    var snapshot: [String: [AnonTokenContext: [AnonToken]]] { lock.withLock { rows } }

    func put(_ accountKey: String, _ context: AnonTokenContext, _ tokens: [AnonToken]) {
        save(accountKey: accountKey, context: context, tokens: tokens)
    }

    func load(accountKey: String) -> [AnonTokenContext: [AnonToken]] {
        lock.withLock { rows[accountKey] ?? [:] }
    }

    func save(accountKey: String, context: AnonTokenContext, tokens: [AnonToken]) {
        lock.withLock {
            var account = rows[accountKey] ?? [:]
            account[context] = tokens.isEmpty ? nil : tokens
            rows[accountKey] = account.isEmpty ? nil : account
        }
    }

    func delete(accountKey: String) {
        lock.withLock { rows[accountKey] = nil }
    }
}

final class AnonTokenPersistenceTests: XCTestCase {

    func testThePoolSurvivesARelaunch() async throws {
        let store = InMemoryAnonTokenStore()
        let issuer = FakeIssuer()
        let first = makeQuietPool(issuer, store: store)
        first.configure(accountKey: "A", sessionToken: "s1")
        await first.runScheduledRefills()
        let spent = try await first.take(.discoveryLookup)
        XCTAssertEqual(store.snapshot["A"]?[.discoveryLookup]?.count, AnonTokenContext.discoveryLookup.poolTarget - 1, "every change is written")
        first.clear()

        // A relaunch: a fresh pool over the same storage, bound to the same account.
        let second = makeQuietPool(issuer, store: store)
        second.configure(accountKey: "A", sessionToken: "s2")
        XCTAssertEqual(second.count(.discoveryLookup), AnonTokenContext.discoveryLookup.poolTarget - 1)
        let next = try await second.take(.discoveryLookup)
        XCTAssertNotEqual(next.headerValue, spent.headerValue, "a spent token never comes back")
    }

    func testDeadEpochTokensAreDroppedOnLoad() {
        let store = InMemoryAnonTokenStore()
        let probe = AnonTokenPool(issuer: FakeIssuer())
        let current = probe.currentEpoch()
        store.put("A", .dsKnock, [
            AnonToken(context: "ds-knock", epoch: current &- 2, headerValue: "dead"),
            AnonToken(context: "ds-knock", epoch: current &- 1, headerValue: "previous"),
            AnonToken(context: "ds-knock", epoch: current, headerValue: "current"),
        ])
        store.put("A", .pushRegister, [AnonToken(context: "push-register", epoch: current &- 5, headerValue: "old")])

        let pool = makeQuietPool(FakeIssuer(), store: store)
        pool.configure(accountKey: "A", sessionToken: "s1")

        XCTAssertEqual(pool.count(.dsKnock), 2)
        XCTAssertEqual(store.snapshot["A"]?[.dsKnock]?.map(\.headerValue), ["previous", "current"])
        XCTAssertNil(store.snapshot["A"]?[.pushRegister], "an all-dead context is removed from storage")
        XCTAssertEqual(pool.takeIfAvailable(.dsKnock)?.headerValue, "previous")
    }

    func testLogoutAndAnAccountSwitchKeepEveryAccountsTokens() async throws {
        let store = InMemoryAnonTokenStore()
        let issuer = FakeIssuer()
        let pool = makeQuietPool(issuer, store: store)
        pool.configure(accountKey: "A", sessionToken: "s1")
        await pool.runScheduledRefills()
        _ = try await pool.take(.discoveryLookup)
        let aStored = try XCTUnwrap(store.snapshot["A"]?[.discoveryLookup])

        pool.configure(accountKey: "B", sessionToken: "s2")
        XCTAssertEqual(store.snapshot["A"]?[.discoveryLookup], aStored, "account switch keeps A's")
        XCTAssertEqual(pool.count(.discoveryLookup), 0, "B never sees A's tokens")
        await pool.runScheduledRefills()
        let bToken = try await pool.take(.discoveryLookup)
        XCTAssertFalse(aStored.map(\.headerValue).contains(bToken.headerValue))
        XCTAssertEqual(issuer.calls(for: .discoveryLookup).last?.sessionToken, "s2")
        let bStored = try XCTUnwrap(store.snapshot["B"]?[.discoveryLookup])

        pool.clear()
        XCTAssertEqual(pool.count(.discoveryLookup), 0, "logout drops memory")
        XCTAssertEqual(store.snapshot["A"]?[.discoveryLookup], aStored, "logout keeps A's")
        XCTAssertEqual(store.snapshot["B"]?[.discoveryLookup], bStored, "logout keeps B's")

        // The next sign-in of A loads A's own, and nothing of B's.
        pool.configure(accountKey: "A", sessionToken: "s3")
        XCTAssertEqual(pool.count(.discoveryLookup), aStored.count)
        let next = try await pool.take(.discoveryLookup)
        XCTAssertEqual(next.headerValue, aStored.first?.headerValue)
        XCTAssertFalse(bStored.map(\.headerValue).contains(next.headerValue))
    }

    func testBindingToAnAccountLeavesAnotherAccountsTokensAndNeverYieldsThem() {
        let store = InMemoryAnonTokenStore()
        let epoch = AnonTokenPool(issuer: FakeIssuer()).currentEpoch()
        store.put("OTHER", .dsKnock, [AnonToken(context: "ds-knock", epoch: epoch, headerValue: "x")])
        let pool = makeQuietPool(FakeIssuer(), store: store)
        pool.configure(accountKey: "A", sessionToken: "s1")
        XCTAssertNotNil(store.snapshot["OTHER"])
        XCTAssertNil(pool.takeIfAvailable(.dsKnock))
    }

    func testForgetDeletesOnlyThatAccountAndNothingIsWrittenBack() async throws {
        let store = InMemoryAnonTokenStore()
        let pool = makePool(FakeIssuer(), store: store)
        pool.configure(accountKey: "B", sessionToken: "s0")
        _ = try await pool.take(.dsKnock)
        pool.configure(accountKey: "A", sessionToken: "s1")
        _ = try await pool.take(.pushRegister)
        let bStored = store.snapshot["B"]
        XCTAssertNotNil(store.snapshot["A"])

        // Forget device for A while A is still bound (the wipe runs before
        // the session ends).
        pool.forget(accountKey: "A")
        XCTAssertNil(store.snapshot["A"])
        // The session's push unregister still spends one from memory, and
        // that spend does not put A's rows back.
        XCTAssertNotNil(pool.takeIfAvailable(.pushRegister))
        XCTAssertNil(store.snapshot["A"], "no write-back after forget")
        pool.clear()
        XCTAssertNil(store.snapshot["A"])
        XCTAssertEqual(store.snapshot["B"], bStored, "another account's tokens stay")

        // A new sign-in of A starts empty and is persisted again.
        pool.configure(accountKey: "A", sessionToken: "s2")
        _ = try await pool.take(.pushRegister)
        XCTAssertNotNil(store.snapshot["A"])
    }

    func testAWaitingTakeIsNeverServedFromTheNextAccount() async throws {
        let store = InMemoryAnonTokenStore()
        let epoch = AnonTokenPool(issuer: FakeIssuer()).currentEpoch()
        store.put("B", .dsKnock, [AnonToken(context: "ds-knock", epoch: epoch, headerValue: "b-token")])
        let pool = makePool(FakeIssuer(hang: true), store: store, waitSeconds: 0.5)
        pool.configure(accountKey: "A", sessionToken: "s1")

        let pending = Task { try await pool.take(.dsKnock) }
        try await Task.sleep(nanoseconds: 100_000_000)
        pool.configure(accountKey: "B", sessionToken: "s2")
        XCTAssertEqual(pool.count(.dsKnock), 1)

        do {
            let token = try await pending.value
            XCTFail("a request started under A got \(token.headerValue)")
        } catch let error as AnonTokenUnavailable {
            XCTAssertEqual(error, .noSession)
        }
        XCTAssertEqual(pool.count(.dsKnock), 1, "B's token is still B's")
    }

    func testALaunchThatLandsLoggedOutKeepsTheStore() {
        // A session that expired while the app was closed: the next sign-in
        // of the same account may still spend what is left.
        let store = InMemoryAnonTokenStore()
        let epoch = AnonTokenPool(issuer: FakeIssuer()).currentEpoch()
        store.put("A", .dsKnock, [AnonToken(context: "ds-knock", epoch: epoch, headerValue: "x")])
        let pool = AnonTokenPool(issuer: FakeIssuer(), store: store)
        pool.configure(accountKey: nil, sessionToken: nil)
        XCTAssertNotNil(store.snapshot["A"])
    }

    /// The Keychain store round-trips, is per (account, context), and deletes.
    func testTheKeychainStoreRoundTrips() {
        let store = KeychainAnonTokenStore(service: "me.lakko.anon-tokens.test.\(UUID().uuidString)")
        defer {
            store.delete(accountKey: "QQ+/==")
            store.delete(accountKey: "other")
        }
        let tokens = [
            AnonToken(context: "ds-knock", epoch: 9, headerValue: "h1"),
            AnonToken(context: "ds-knock", epoch: 10, headerValue: "h2"),
        ]
        store.save(accountKey: "QQ+/==", context: .dsKnock, tokens: tokens)
        store.save(accountKey: "other", context: .pushRegister,
                   tokens: [AnonToken(context: "push-register", epoch: 9, headerValue: "p")])
        XCTAssertEqual(store.load(accountKey: "QQ+/==")[.dsKnock], tokens)
        XCTAssertNil(store.load(accountKey: "QQ+/==")[.pushRegister])

        store.save(accountKey: "QQ+/==", context: .dsKnock, tokens: [tokens[1]])
        XCTAssertEqual(store.load(accountKey: "QQ+/==")[.dsKnock], [tokens[1]])

        store.delete(accountKey: "other")
        XCTAssertTrue(store.load(accountKey: "other").isEmpty)
        XCTAssertEqual(store.load(accountKey: "QQ+/==")[.dsKnock], [tokens[1]])

        store.save(accountKey: "QQ+/==", context: .dsKnock, tokens: [])
        XCTAssertTrue(store.load(accountKey: "QQ+/==").isEmpty)
    }
}

// MARK: - One request on a token route

final class AnonTokenRouteTests: XCTestCase {

    private func response(_ status: Int) -> HTTPURLResponse {
        HTTPURLResponse(
            url: URL(string: "https://x.test/r")!, statusCode: status, httpVersion: nil, headerFields: nil)!
    }

    func testEveryRetryableRefusalTakesAnotherTokenOnce() async throws {
        for code in AnonTokenRoute.retryOnceCodes.sorted() {
            let tokens = FakeAnonTokens()
            var sent: [String] = []
            let (_, http) = try await AnonTokenRoute.send(context: .dsKnock, tokens: tokens) { header in
                sent.append(header)
                if sent.count == 1 {
                    return (Data(#"{"error":"\#(code)"}"#.utf8), self.response(401))
                }
                return (Data("{}".utf8), self.response(200))
            }
            XCTAssertEqual(http.statusCode, 200, code)
            XCTAssertEqual(sent, ["ds-knock-1", "ds-knock-2"], code)
        }
    }

    func testASecondRefusalThrowsAndIsNeverASignOut() async throws {
        var attempts = 0
        do {
            _ = try await AnonTokenRoute.send(context: .inviteConsume, tokens: FakeAnonTokens()) { _ in
                attempts += 1
                return (Data(#"{"error":"anon-token-spent"}"#.utf8), self.response(401))
            }
            XCTFail("expected a refusal")
        } catch let error as AnonTokenUnavailable {
            XCTAssertEqual(error, .refused(code: "anon-token-spent"))
        }
        XCTAssertEqual(attempts, 2)
    }

    func testTokenRequiredIsNotRetried() async throws {
        var attempts = 0
        do {
            _ = try await AnonTokenRoute.send(context: .pushRegister, tokens: FakeAnonTokens()) { _ in
                attempts += 1
                return (Data(#"{"error":"anon-token-required"}"#.utf8), self.response(401))
            }
            XCTFail("expected a refusal")
        } catch let error as AnonTokenUnavailable {
            XCTAssertEqual(error, .refused(code: "anon-token-required"))
        }
        XCTAssertEqual(attempts, 1)
    }

    func testDisabledRouteFailsWithTheNetworkCopy() async throws {
        do {
            _ = try await AnonTokenRoute.send(context: .discoveryLookup, tokens: FakeAnonTokens()) { _ in
                (Data(#"{"error":"anon-tokens-disabled"}"#.utf8), self.response(503))
            }
            XCTFail("expected disabled")
        } catch let error as AnonTokenUnavailable {
            XCTAssertEqual(error, .disabled)
            XCTAssertEqual(error.localizedDescription, AnonTokenCopy.networkFailure)
        }
    }

    func testAnyOtherStatusIsTheCallersAndSpendsOneToken() async throws {
        let tokens = FakeAnonTokens()
        let (_, http) = try await AnonTokenRoute.send(context: .inviteConsume, tokens: tokens) { _ in
            (Data(#"{"code":"rate-limited"}"#.utf8), self.response(429))
        }
        XCTAssertEqual(http.statusCode, 429)
        XCTAssertEqual(tokens.taken, [.inviteConsume])
    }
}

// MARK: - A recording HTTP stand-in

/// Records every request per host and answers from a per-host handler.
final class AnonRouteRecorder: URLProtocol {
    struct Seen {
        let method: String
        let path: String
        let authorization: String?
        let anonToken: String?
        let cookie: String?
        let body: Data
    }

    typealias Handler = (URLRequest, String) -> (Int, String)

    private static let lock = NSLock()
    private static var handlers: [String: Handler] = [:]
    private static var seen: [String: [Seen]] = [:]

    static func scope(_ handler: @escaping Handler) -> String {
        let host = "r\(UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()).test"
        lock.withLock { handlers[host] = handler }
        return host
    }

    static func session() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [AnonRouteRecorder.self]
        return URLSession(configuration: config)
    }

    static func requests(_ host: String) -> [Seen] {
        lock.withLock { seen[host] ?? [] }
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url else { return }
        let host = url.host?.lowercased() ?? "?"
        let requestBody = Self.body(of: request)
        let handler: Handler? = Self.lock.withLock {
            Self.seen[host, default: []].append(Seen(
                method: request.httpMethod ?? "GET",
                path: url.path,
                authorization: request.value(forHTTPHeaderField: "Authorization"),
                anonToken: request.value(forHTTPHeaderField: "x-lakko-anon-token"),
                cookie: request.value(forHTTPHeaderField: "Cookie"),
                body: requestBody))
            return Self.handlers[host]
        }
        let (status, body) = handler?(request, String(decoding: requestBody, as: UTF8.self)) ?? (404, "")
        let response = HTTPURLResponse(
            url: url, statusCode: status, httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}

    private static func body(of request: URLRequest) -> Data {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { return Data() }
        stream.open()
        defer { stream.close() }
        var out = Data()
        var buffer = [UInt8](repeating: 0, count: 4096)
        while stream.hasBytesAvailable {
            let read = stream.read(&buffer, maxLength: buffer.count)
            if read <= 0 { break }
            out.append(contentsOf: buffer[0..<read])
        }
        return out
    }
}

// MARK: - The privacy rule on every token route

final class AnonTokenRoutePrivacyTests: XCTestCase {

    private let consumeOK = #"{"used_count":1,"max_uses":5,"already_consumed":false}"#

    /// Discovery: the OPRF step, the channel-name register and the three
    /// consumes each spend one token of their context and carry NO session,
    /// even from a client that holds one.
    func testDiscoveryTokenRoutesCarryATokenAndNoSession() async throws {
        let host = AnonRouteRecorder.scope { request, body in
            switch request.url?.path ?? "" {
            case "/discovery/oprf": return (200, #"{"evaluated":"AA=="}"#)
            case "/discovery/channel/register": return (200, #"{"ok":true}"#)
            default: return (200, self.consumeOK)
            }
        }
        let tokens = FakeAnonTokens()
        let client = DiscoveryClient(
            baseURL: "http://\(host):9",
            session: AnonRouteRecorder.session(),
            sessionToken: "session-token",
            antiCensorship: .disarmed,
            tokens: tokens)
        let blobId = Data(repeating: 7, count: 6)
        let joiner = Data(repeating: 1, count: 16)
        let cap = Data([1, 2, 3])

        _ = try await client.oprfEvaluate(OprfRequest(blinded: "AA=="))
        _ = try await client.channelRegister(ChannelRegisterRequest(oprfHash: "AA==", channelId: "AA=="))
        _ = try await client.consumeInvite(blobId: blobId, capabilityBytes: cap, joinerAccountId: joiner)
        _ = try await client.consumeCallInvite(blobId: blobId, capabilityBytes: cap, joinerAccountId: joiner)
        _ = try await client.consumeGroupInvite(blobId: blobId, capabilityBytes: cap, joinerAccountId: joiner)

        let seen = AnonRouteRecorder.requests(host)
        XCTAssertEqual(seen.map(\.path), [
            "/discovery/oprf",
            "/discovery/channel/register",
            "/discovery/invite/BwcHBwcH/consume",
            "/discovery/call-invite/BwcHBwcH/consume",
            "/discovery/group-invite/BwcHBwcH/consume",
        ])
        for request in seen {
            XCTAssertNil(request.authorization, "\(request.path) carried a session")
            XCTAssertNil(request.cookie, "\(request.path) carried a cookie")
            XCTAssertNotNil(request.anonToken, "\(request.path) carried no token")
            let json = try JSONSerialization.jsonObject(with: request.body) as? [String: Any]
            XCTAssertNil(json?["pow"], "\(request.path) still carries a proof of work")
        }
        XCTAssertEqual(seen.map(\.anonToken), [
            "discovery-lookup-1", "discovery-register-1",
            "invite-consume-1", "invite-consume-2", "invite-consume-3",
        ])
        XCTAssertEqual(tokens.taken, [
            .discoveryLookup, .discoveryRegister, .inviteConsume, .inviteConsume, .inviteConsume,
        ])
    }

    /// The token-free lookups carry no session and no token either.
    func testTheTokenFreeLookupsCarryNoSession() async throws {
        let host = AnonRouteRecorder.scope { _, _ in (404, "") }
        let tokens = FakeAnonTokens()
        let client = DiscoveryClient(
            baseURL: "http://\(host):9",
            session: AnonRouteRecorder.session(),
            sessionToken: "session-token",
            antiCensorship: .disarmed,
            tokens: tokens)
        _ = try await client.lookupRaw(LookupRequest(oprfHash: "AA=="))
        _ = try await client.channelLookupRaw(ChannelLookupRequest(oprfHash: "AA=="))
        for request in AnonRouteRecorder.requests(host) {
            XCTAssertNil(request.authorization, request.path)
            XCTAssertNil(request.anonToken, request.path)
        }
        XCTAssertTrue(tokens.taken.isEmpty)
    }

    /// push-gw (ADR-0078): both subscription routes spend a `push-register`
    /// token, no session, and carry no device pseudonym.
    func testPushRoutesCarryATokenAndNoSession() async throws {
        let host = AnonRouteRecorder.scope { request, body in
            switch request.url?.path ?? "" {
            case "/push/subscribe": return (200, #"{"ok":true}"#)
            default: return (200, #"{"ok":true,"removed":true}"#)
            }
        }
        let tokens = FakeAnonTokens()
        let client = PushClient(baseURL: "http://\(host):9", session: AnonRouteRecorder.session(), tokens: tokens)
        let subId = Data(repeating: 2, count: 16)
        let group = Data(repeating: 3, count: 32)

        try await client.subscribe(subId: subId, pushGid: group, wakePk: group, sealed: Data([9]))
        _ = try await client.unsubscribe(subId: subId)

        let seen = AnonRouteRecorder.requests(host)
        XCTAssertEqual(seen.map(\.path), ["/push/subscribe", "/push/unsubscribe"])
        for request in seen {
            XCTAssertNil(request.authorization, request.path)
            XCTAssertNil(request.cookie, request.path)
            let json = try JSONSerialization.jsonObject(with: request.body) as? [String: Any]
            XCTAssertNil(json?["pow"], request.path)
            XCTAssertNil(json?["device_pseudonym"], request.path)
        }
        XCTAssertEqual(seen.map(\.anonToken), ["push-register-1", "push-register-2"])
    }

    /// Logout's unsubscribe spends the token taken before the pool was cleared.
    func testThePushUnsubscribeSpendsThePrepaidToken() async throws {
        let host = AnonRouteRecorder.scope { _, _ in (200, #"{"ok":true,"removed":true}"#) }
        let client = PushClient(
            baseURL: "http://\(host):9", session: AnonRouteRecorder.session(),
            tokens: FakeAnonTokens(failure: .noSession))
        _ = try await client.unsubscribe(
            subId: Data(repeating: 2, count: 16),
            prepaidToken: AnonToken(context: "push-register", epoch: 1, headerValue: "prepaid"))
        XCTAssertEqual(AnonRouteRecorder.requests(host).map(\.anonToken), ["prepaid"])
    }

    /// The production default for an anonymous route is its own session,
    /// distinct from the shared one authenticated calls use.
    func testTheAnonymousSessionIsItsOwnPool() {
        XCTAssertFalse(AnonymousHTTP.session === URLSession.shared)
        XCTAssertTrue(AnonymousHTTP.session(pairedWith: .shared) === AnonymousHTTP.session)
        XCTAssertNil(AnonymousHTTP.session.configuration.httpCookieStorage)
        XCTAssertFalse(AnonymousHTTP.session.configuration.httpShouldSetCookies)
    }
}

// MARK: - The issuer

final class AuthAnonTokenIssuerTests: XCTestCase {

    private func issueBody(publicKey: String, epoch: UInt64, context: String = "discovery-lookup") -> String {
        #"{"context":"\#(context)","epoch":\#(epoch),"evaluations":["e1","e2"],"proof":"p","public_key":"\#(publicKey)","remaining":10,"epoch_ends_at":1}"#
    }

    /// The load-bearing rule: the proof is checked against the key from the
    /// anonymous key route, never against `public_key` in the issuance body.
    func testTheBatchIsVerifiedAgainstTheKeyRouteNotTheIssuanceResponse() async throws {
        let host = AnonRouteRecorder.scope { request, body in
            let path = request.url?.path ?? ""
            if path == "/auth/tokens" {
                return (200, self.issueBody(publicKey: "KEY-FROM-ISSUANCE", epoch: 42))
            }
            if path == "/auth/tokens/key/discovery-lookup/42" {
                return (200, #"{"context":"discovery-lookup","epoch":42,"public_key":"KEY-FROM-ROUTE"}"#)
            }
            return (404, "")
        }
        let usedKey = LockedBox<String?>(nil)
        let issuer = AuthAnonTokenIssuer(
            baseURL: "http://\(host):9",
            session: AnonRouteRecorder.session(),
            finalize: { _, evaluations, proof, key in
                usedKey.set(key)
                XCTAssertEqual(evaluations, ["e1", "e2"])
                XCTAssertEqual(proof, "p")
                return [
                    AnonToken(context: "discovery-lookup", epoch: 42, headerValue: "t1"),
                    AnonToken(context: "discovery-lookup", epoch: 42, headerValue: "t2"),
                ]
            })

        let tokens = try await issuer.issue(
            context: .discoveryLookup, epoch: 42, count: 2, sessionToken: "session")

        XCTAssertEqual(tokens.map(\.headerValue), ["t1", "t2"])
        XCTAssertEqual(usedKey.get(), "KEY-FROM-ROUTE")
        let seen = AnonRouteRecorder.requests(host)
        XCTAssertEqual(seen.map(\.path), ["/auth/tokens", "/auth/tokens/key/discovery-lookup/42"])
        XCTAssertEqual(seen[0].authorization, "Bearer session", "issuance is drawn with the session")
        XCTAssertNil(seen[1].authorization, "the key route is anonymous")
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: seen[0].body) as? [String: Any])
        XCTAssertEqual(body["context"] as? String, "discovery-lookup")
        XCTAssertEqual((body["epoch"] as? NSNumber)?.uint64Value, 42)
        XCTAssertEqual((body["blinded_elements"] as? [String])?.count, 2)

        // Cached: a second issuance does not fetch the key again.
        _ = try await issuer.issue(context: .discoveryLookup, epoch: 42, count: 2, sessionToken: "session")
        XCTAssertEqual(
            AnonRouteRecorder.requests(host).filter { $0.path.hasPrefix("/auth/tokens/key") }.count, 1)
    }

    /// A proof that does not verify under the route's key discards the whole
    /// batch - with the real core `finalize`.
    func testAMismatchingKeyDiscardsTheBatch() async throws {
        let host = AnonRouteRecorder.scope { request, body in
            if request.url?.path == "/auth/tokens" {
                let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
                let epoch = (json?["epoch"] as? NSNumber)?.uint64Value ?? 0
                let evaluations = (json?["blinded_elements"] as? [String]) ?? []
                let list = evaluations.map { "\"\($0)\"" }.joined(separator: ",")
                return (200, #"{"context":"push-register","epoch":\#(epoch),"evaluations":[\#(list)],"proof":"AAAA","public_key":"x","remaining":1,"epoch_ends_at":1}"#)
            }
            let epoch = request.url?.lastPathComponent ?? "0"
            // Some valid-looking but wrong key: the proof cannot verify.
            return (200, #"{"context":"push-register","epoch":\#(epoch),"public_key":"4gBQhS9Rqh_lg3-Og2ZxDzXvjSUm5fEo5PP2QA8lA0k"}"#)
        }
        let issuer = AuthAnonTokenIssuer(baseURL: "http://\(host):9", session: AnonRouteRecorder.session())
        let epoch = try anonTokenEpochAt(
            unixSecs: UInt64(Date().timeIntervalSince1970), windowSecs: anonTokenDefaultWindowSecs())
        do {
            _ = try await issuer.issue(context: .pushRegister, epoch: epoch, count: 2, sessionToken: "s")
            XCTFail("an unverifiable batch must not yield tokens")
        } catch AnonTokenUnavailable.verificationFailed {
        }

        // And the pool keeps nothing of it.
        let pool = makePool(issuer, waitSeconds: 2)
        pool.configure(accountKey: "A", sessionToken: "s")
        do {
            _ = try await pool.take(.pushRegister)
            XCTFail("expected the discard")
        } catch AnonTokenUnavailable.verificationFailed {
        }
        XCTAssertEqual(pool.count(.pushRegister), 0)
    }

    func testIssuerRefusalsMapToTheirOutcomes() async throws {
        let cases: [(Int, String, AnonTokenUnavailable)] = [
            (429, #"{"error":"anon-token-quota-exhausted","remaining":0,"retry_after_secs":321}"#,
             .quotaExhausted(retryAfterSecs: 321)),
            (400, #"{"error":"epoch-not-issuable","current_epoch":77}"#, .epochNotIssuable(currentEpoch: 77)),
            (503, #"{"error":"anon-tokens-disabled"}"#, .disabled),
        ]
        for (status, body, expected) in cases {
            let host = AnonRouteRecorder.scope { _, _ in (status, body) }
            let issuer = AuthAnonTokenIssuer(baseURL: "http://\(host):9", session: AnonRouteRecorder.session())
            do {
                _ = try await issuer.issue(context: .dsKnock, epoch: 1, count: 8, sessionToken: "s")
                XCTFail("\(status) must fail")
            } catch let error as AnonTokenUnavailable {
                XCTAssertEqual(error, expected)
            }
        }
    }

    func testAPartialAllowanceDrawsWhatIsLeft() async throws {
        let counts = LockedBox<[Int]>([])
        let host = AnonRouteRecorder.scope { request, body in
            if request.url?.path == "/auth/tokens" {
                let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
                let n = (json?["blinded_elements"] as? [String])?.count ?? 0
                counts.set(counts.get() + [n])
                if n > 3 {
                    return (429, #"{"error":"anon-token-quota-exhausted","remaining":3,"retry_after_secs":9}"#)
                }
                return (200, #"{"context":"ds-knock","epoch":5,"evaluations":["a","b","c"],"proof":"p","public_key":"k","remaining":0,"epoch_ends_at":1}"#)
            }
            return (200, #"{"context":"ds-knock","epoch":5,"public_key":"k"}"#)
        }
        let issuer = AuthAnonTokenIssuer(
            baseURL: "http://\(host):9", session: AnonRouteRecorder.session(),
            finalize: { _, evaluations, _, _ in
                evaluations.map { AnonToken(context: "ds-knock", epoch: 5, headerValue: $0) }
            })
        let tokens = try await issuer.issue(context: .dsKnock, epoch: 5, count: 8, sessionToken: "s")
        XCTAssertEqual(tokens.count, 3)
        XCTAssertEqual(counts.get(), [8, 3])
    }
}

/// A value shared with a `@Sendable` closure in a test.
final class LockedBox<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: T
    init(_ value: T) { self.value = value }
    func get() -> T { lock.withLock { value } }
    func set(_ newValue: T) { lock.withLock { value = newValue } }
}
