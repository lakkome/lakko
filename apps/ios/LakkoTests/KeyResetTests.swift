// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

// ADR-0065 "Reset account keys": the pure rules, the token provider and the
// signed-out device, driven without a server.

// MARK: - The landing rule (section 4.2)

final class KeyResetLandingTests: XCTestCase {

    private let ours = Data(repeating: 0x0A, count: 32)
    private let theirs = Data(repeating: 0x0B, count: 32)

    private func decide(
        _ observed: KeyResetLanding.Observation, generation: UInt64 = 1, upload: Bool = true
    ) -> KeyResetLanding.Verdict {
        KeyResetLanding.decide(
            observed: observed, derivedSignaturePubkey: ours, ownKeyGeneration: generation, uploads: upload)
    }

    func testAnUnreachableDirectoryLandsWithoutAnUpload() {
        XCTAssertEqual(decide(.unreachable), .land(publish: .skip))
        XCTAssertEqual(decide(.unreachable, upload: false), .land(publish: .skip))
    }

    func testNothingPublishedIsTheFirstPublishOnAnExplicitSignInOnly() {
        XCTAssertEqual(decide(.absent), .land(publish: .firstPublish))
        XCTAssertEqual(decide(.absent, upload: false), .land(publish: .skip))
    }

    func testOurOwnKeyLandsAndRepublishesOnlyOnAnExplicitSignIn() {
        XCTAssertEqual(decide(.present(signaturePubkey: ours, keyGeneration: 1)), .land(publish: .republish))
        XCTAssertEqual(
            decide(.present(signaturePubkey: ours, keyGeneration: 1), upload: false), .land(publish: .skip))
        // A higher published generation with OUR key cannot happen; it is
        // still ours, so it lands.
        XCTAssertEqual(decide(.present(signaturePubkey: ours, keyGeneration: 5)), .land(publish: .republish))
    }

    func testOtherKeysOfAHigherGenerationAreStaleKeys() {
        XCTAssertEqual(decide(.present(signaturePubkey: theirs, keyGeneration: 2), generation: 1), .staleKeys)
        XCTAssertEqual(decide(.present(signaturePubkey: theirs, keyGeneration: 9), generation: 3), .staleKeys)
        XCTAssertEqual(
            decide(.present(signaturePubkey: theirs, keyGeneration: 2), generation: 1, upload: false), .staleKeys)
    }

    func testOtherKeysAtTheSameOrALowerGenerationAreAMismatch() {
        XCTAssertEqual(decide(.present(signaturePubkey: theirs, keyGeneration: 1), generation: 1), .mismatch)
        XCTAssertEqual(decide(.present(signaturePubkey: theirs, keyGeneration: 1), generation: 2), .mismatch)
        XCTAssertEqual(decide(.present(signaturePubkey: nil, keyGeneration: 1), generation: 1), .mismatch)
        XCTAssertEqual(decide(.present(signaturePubkey: nil, keyGeneration: 3), generation: 1), .staleKeys)
    }
}

// MARK: - The token rules (section 3.3)

final class SessionTokenRuleTests: XCTestCase {

    private let now: Int64 = 1_000_000

    func testTheAccessTokenIsUsedWhileMoreThanTwoMinutesRemain() {
        let pair = SessionPair.fixture(accessExpiresAtMs: now + SessionTokenRule.refreshAheadMs + 1)
        XCTAssertEqual(SessionTokenRule.decide(pair: pair, nowMs: now), .useCurrent)
    }

    func testTheAccessTokenIsRefreshedAtTwoMinutesOrLess() {
        let edge = SessionPair.fixture(accessExpiresAtMs: now + SessionTokenRule.refreshAheadMs)
        XCTAssertEqual(SessionTokenRule.decide(pair: edge, nowMs: now), .refreshFirst)
        let expired = SessionPair.fixture(accessExpiresAtMs: now - 1)
        XCTAssertEqual(SessionTokenRule.decide(pair: expired, nowMs: now), .refreshFirst)
    }

    func testWithoutAUsableRefreshCredentialTheTokenIsPresentedAnyway() {
        let none = SessionPair.accessOnly(sessionToken: "tok", sessionExpiresAtMs: now - 1)
        XCTAssertEqual(SessionTokenRule.decide(pair: none, nowMs: now), .noRefreshCredential)
        let expiredRefresh = SessionPair.fixture(accessExpiresAtMs: now - 1, refreshExpiresAtMs: now)
        XCTAssertEqual(SessionTokenRule.decide(pair: expiredRefresh, nowMs: now), .noRefreshCredential)
    }

    func testA401IsReadByItsBody() {
        XCTAssertEqual(
            SessionTokenRule.classify401(body: Data(#"{"error":"key-generation-superseded"}"#.utf8)),
            .keyGenerationSuperseded)
        XCTAssertEqual(
            SessionTokenRule.classify401(body: Data(#"{"error":"refresh-invalid"}"#.utf8)), .refreshInvalid)
        XCTAssertEqual(SessionTokenRule.classify401(body: Data()), .other)
        XCTAssertEqual(SessionTokenRule.classify401(body: Data("invalid credentials".utf8)), .other)
        XCTAssertEqual(SessionTokenRule.classify401(body: Data(#"{"error":"handle-taken"}"#.utf8)), .other)
        XCTAssertEqual(SessionTokenRule.classify401(body: Data(#"["key-generation-superseded"]"#.utf8)), .other)
    }

    func testASupersededGenerationSignsOutAndIsNeverRetried() {
        XCTAssertEqual(
            SessionTokenRule.afterBearer401(meaning: .keyGenerationSuperseded, alreadyRetried: false),
            .signOutSuperseded)
        XCTAssertEqual(
            SessionTokenRule.afterBearer401(meaning: .keyGenerationSuperseded, alreadyRetried: true),
            .signOutSuperseded)
    }

    func testAnyOther401EarnsExactlyOneRefreshAndRetry() {
        XCTAssertEqual(SessionTokenRule.afterBearer401(meaning: .other, alreadyRetried: false), .refreshAndRetry)
        XCTAssertEqual(SessionTokenRule.afterBearer401(meaning: .other, alreadyRetried: true), .report)
        XCTAssertEqual(
            SessionTokenRule.afterBearer401(meaning: .refreshInvalid, alreadyRetried: false), .refreshAndRetry)
        XCTAssertEqual(SessionTokenRule.afterBearer401(meaning: .refreshInvalid, alreadyRetried: true), .report)
    }
}

// MARK: - The token provider

final class SessionTokenProviderTests: XCTestCase {

    private let now: Int64 = 1_000_000

    /// The scripted refresher: `answers` are consumed in order; a refresh
    /// past the script fails as offline.
    private final class Script: @unchecked Sendable {
        private let lock = NSLock()
        private var remaining: [Result<SessionPair, Error>]
        private var calls = 0
        let delayNs: UInt64
        init(_ answers: [Result<SessionPair, Error>], delayNs: UInt64) {
            remaining = answers
            self.delayNs = delayNs
        }
        func next() -> Result<SessionPair, Error>? {
            lock.lock(); defer { lock.unlock() }
            calls += 1
            return remaining.isEmpty ? nil : remaining.removeFirst()
        }
        var count: Int { lock.lock(); defer { lock.unlock() }; return calls }
    }

    /// A provider whose clock and refresher are scripted.
    private func provider(
        pair: SessionPair,
        answers: [Result<SessionPair, Error>],
        delayNs: UInt64 = 0
    ) -> (SessionTokenProvider, () -> Int) {
        let provider = SessionTokenProvider()
        provider.nowMs = { [now] in now }
        let script = Script(answers, delayNs: delayNs)
        provider.refresher = { _ in
            let next = script.next()
            if script.delayNs > 0 { try await Task.sleep(nanoseconds: script.delayNs) }
            guard let next else { throw URLError(.notConnectedToInternet) }
            return try next.get()
        }
        provider.install(pair, accountIdB64: "acct")
        return (provider, { script.count })
    }

    private func superseded() -> Error {
        AuthClientError.http(
            status: 401, endpoint: "/auth/session/refresh", body: #"{"error":"key-generation-superseded"}"#)
    }

    private func invalid() -> Error {
        AuthClientError.http(status: 401, endpoint: "/auth/session/refresh", body: #"{"error":"refresh-invalid"}"#)
    }

    func testALiveTokenIsServedWithoutARefresh() async {
        let (p, calls) = provider(
            pair: .fixture(access: "live", accessExpiresAtMs: now + 600_000), answers: [])
        let outcome = await p.accessToken()
        XCTAssertEqual(outcome, .token("live"))
        XCTAssertEqual(calls(), 0)
    }

    func testAnExpiringTokenIsRefreshedAndTheNewPairInstalled() async {
        let fresh = SessionPair.fixture(access: "fresh", accessExpiresAtMs: now + 900_000, generation: 1)
        let (p, calls) = provider(
            pair: .fixture(access: "stale", accessExpiresAtMs: now + 1_000), answers: [.success(fresh)])
        var announced: [SessionPair] = []
        let announcedLock = NSLock()
        p.onPairRefreshed = { pair, _ in announcedLock.lock(); announced.append(pair); announcedLock.unlock() }
        let outcome = await p.accessToken()
        XCTAssertEqual(outcome, .token("fresh"))
        XCTAssertEqual(calls(), 1)
        XCTAssertEqual(p.current, fresh)
        XCTAssertTrue(p.isKnownAccessToken("stale"), "an earlier token of this session stays recognised")
        XCTAssertTrue(p.isKnownAccessToken("fresh"))
        announcedLock.lock(); defer { announcedLock.unlock() }
        XCTAssertEqual(announced, [fresh])
    }

    func testConcurrentCallersShareOneRefreshInFlight() async {
        let fresh = SessionPair.fixture(access: "fresh", accessExpiresAtMs: now + 900_000)
        let (p, calls) = provider(
            pair: .fixture(access: "stale", accessExpiresAtMs: now - 1),
            answers: [.success(fresh), .success(fresh)], delayNs: 50_000_000)
        async let a = p.accessToken()
        async let b = p.accessToken()
        async let c = p.refresh()
        let (ra, rb, rc) = await (a, b, c)
        XCTAssertEqual(ra, .token("fresh"))
        XCTAssertEqual(rb, .token("fresh"))
        XCTAssertEqual(rc, .refreshed(fresh))
        XCTAssertEqual(calls(), 1, "one refresh in flight at a time")
    }

    func testATransportFailureKeepsTheCurrentToken() async {
        let (p, calls) = provider(pair: .fixture(access: "stale", accessExpiresAtMs: now - 1), answers: [])
        let outcome = await p.accessToken()
        XCTAssertEqual(outcome, .token("stale"), "offline: the caller fails as it does today")
        XCTAssertEqual(calls(), 1)
        XCTAssertFalse(p.isSuperseded)
    }

    func testASupersededGenerationIsReportedOnceAndSticks() async {
        let (p, calls) = provider(
            pair: .fixture(access: "stale", accessExpiresAtMs: now - 1), answers: [.failure(superseded())])
        var fired: [String] = []
        let lock = NSLock()
        p.onSuperseded = { account in lock.lock(); fired.append(account); lock.unlock() }
        let first = await p.accessToken()
        XCTAssertEqual(first, .superseded)
        XCTAssertTrue(p.isSuperseded)
        // No further network call: the session is over.
        let second = await p.accessToken()
        XCTAssertEqual(second, .superseded)
        let third = await p.refresh()
        XCTAssertEqual(third, .superseded)
        XCTAssertEqual(calls(), 1)
        p.noteSuperseded()
        lock.lock(); defer { lock.unlock() }
        XCTAssertEqual(fired, ["acct"], "fired once")
    }

    func testAnInvalidRefreshCredentialIsReportedAndNothingIsWiped() async {
        let (p, _) = provider(
            pair: .fixture(access: "stale", accessExpiresAtMs: now - 1), answers: [.failure(invalid())])
        var fired: [String] = []
        let lock = NSLock()
        p.onRefreshInvalid = { account in lock.lock(); fired.append(account); lock.unlock() }
        let outcome = await p.accessToken()
        XCTAssertEqual(outcome, .invalid)
        XCTAssertFalse(p.isSuperseded)
        lock.lock(); defer { lock.unlock() }
        XCTAssertEqual(fired, ["acct"])
    }

    func testNoRefreshCredentialMeansNoRefresh() async {
        let (p, calls) = provider(
            pair: .accessOnly(sessionToken: "only", sessionExpiresAtMs: now - 1), answers: [])
        let access = await p.accessToken()
        XCTAssertEqual(access, .token("only"))
        let refresh = await p.refresh()
        XCTAssertEqual(refresh, .noCredential)
        XCTAssertEqual(calls(), 0)
    }

    // MARK: The transport hook

    private func request(bearer: String?) -> URLRequest {
        var request = URLRequest(url: URL(string: "https://example.test/x")!)
        if let bearer { request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization") }
        return request
    }

    private func response(_ status: Int, body: String = "") -> (Data, URLResponse) {
        (Data(body.utf8), HTTPURLResponse(
            url: URL(string: "https://example.test/x")!, statusCode: status, httpVersion: nil, headerFields: nil)!)
    }

    func testARequestWithoutASessionBearerPassesThroughUntouched() async throws {
        let (p, _) = provider(pair: .fixture(access: "live", accessExpiresAtMs: now + 600_000), answers: [])
        var seen: [String?] = []
        _ = try await p.perform(request(bearer: nil)) { r in
            seen.append(r.value(forHTTPHeaderField: "Authorization")); return self.response(200)
        }
        _ = try await p.perform(request(bearer: "call-jwt")) { r in
            seen.append(r.value(forHTTPHeaderField: "Authorization")); return self.response(401)
        }
        XCTAssertEqual(seen, [nil, "Bearer call-jwt"])
    }

    func testAStaleSessionBearerIsSwappedForTheCurrentToken() async throws {
        let fresh = SessionPair.fixture(access: "fresh", accessExpiresAtMs: now + 900_000)
        let (p, _) = provider(
            pair: .fixture(access: "stale", accessExpiresAtMs: now + 1_000), answers: [.success(fresh)])
        var seen: [String?] = []
        let (_, resp) = try await p.perform(request(bearer: "stale")) { r in
            seen.append(r.value(forHTTPHeaderField: "Authorization")); return self.response(204)
        }
        XCTAssertEqual(seen, ["Bearer fresh"])
        XCTAssertEqual((resp as? HTTPURLResponse)?.statusCode, 204)
    }

    func testAPlain401OnABearerRouteRefreshesOnceAndRetriesOnce() async throws {
        let fresh = SessionPair.fixture(access: "fresh", accessExpiresAtMs: now + 900_000)
        let (p, calls) = provider(
            pair: .fixture(access: "live", accessExpiresAtMs: now + 600_000), answers: [.success(fresh)])
        var seen: [String?] = []
        let (_, resp) = try await p.perform(request(bearer: "live")) { r in
            seen.append(r.value(forHTTPHeaderField: "Authorization"))
            return seen.count == 1 ? self.response(401) : self.response(200)
        }
        XCTAssertEqual(seen, ["Bearer live", "Bearer fresh"])
        XCTAssertEqual(calls(), 1)
        XCTAssertEqual((resp as? HTTPURLResponse)?.statusCode, 200)

        // A second 401 after the retry is reported, not retried again.
        let (p2, calls2) = provider(
            pair: .fixture(access: "live", accessExpiresAtMs: now + 600_000), answers: [.success(fresh)])
        seen = []
        let (_, again) = try await p2.perform(request(bearer: "live")) { r in
            seen.append(r.value(forHTTPHeaderField: "Authorization")); return self.response(401)
        }
        XCTAssertEqual((again as? HTTPURLResponse)?.statusCode, 401)
        XCTAssertEqual(seen, ["Bearer live", "Bearer fresh"], "one refresh, one retry, then the 401 stands")
        XCTAssertEqual(calls2(), 1)

        // A refresh that fails leaves the 401 as it is, with no retry.
        let (p3, _) = provider(pair: .fixture(access: "live", accessExpiresAtMs: now + 600_000), answers: [])
        seen = []
        let (_, offline) = try await p3.perform(request(bearer: "live")) { r in
            seen.append(r.value(forHTTPHeaderField: "Authorization")); return self.response(401)
        }
        XCTAssertEqual((offline as? HTTPURLResponse)?.statusCode, 401)
        XCTAssertEqual(seen.count, 1)
    }

    func testASupersededBodyOnABearerRouteSignsOutWithoutARetry() async throws {
        let (p, calls) = provider(pair: .fixture(access: "live", accessExpiresAtMs: now + 600_000), answers: [])
        var fired = 0
        let lock = NSLock()
        p.onSuperseded = { _ in lock.lock(); fired += 1; lock.unlock() }
        var sends = 0
        let (_, resp) = try await p.perform(request(bearer: "live")) { _ in
            sends += 1
            return self.response(401, body: #"{"error":"key-generation-superseded"}"#)
        }
        XCTAssertEqual((resp as? HTTPURLResponse)?.statusCode, 401)
        XCTAssertEqual(sends, 1)
        XCTAssertEqual(calls(), 0, "no refresh is attempted")
        XCTAssertTrue(p.isSuperseded)
        lock.lock(); XCTAssertEqual(fired, 1); lock.unlock()

        // Nothing is sent under a superseded session.
        do {
            _ = try await p.perform(request(bearer: "live")) { _ in XCTFail("must not send"); return self.response(200) }
            XCTFail("a superseded session sends nothing")
        } catch {
            XCTAssertTrue(error is SessionSupersededError)
        }
    }
}

// MARK: - The publish a reset owes

final class KeyResetPublishTests: XCTestCase {

    private var record: KeyResetPublishRecord {
        KeyResetPublishRecord(
            attestation: Data(repeating: 1, count: 177), keyGeneration: 2, signedAtMs: 1_000,
            newSignaturePubkey: Data(repeating: 2, count: 32))
    }

    func testEveryStepIsOwedAtFirstWithVerifyAhead() {
        XCTAssertEqual(KeyResetPublish.remaining(record), [.verify, .discovery, .keyPackage, .deviceGroup])
        XCTAssertFalse(KeyResetPublish.isComplete(record))
    }

    func testALandedStepIsNotRepeatedAndVerifyIsNeverRecorded() {
        var r = KeyResetPublish.marking(record, done: .discovery)
        XCTAssertEqual(KeyResetPublish.remaining(r), [.verify, .keyPackage, .deviceGroup])
        r = KeyResetPublish.marking(r, done: .verify)
        XCTAssertEqual(KeyResetPublish.remaining(r), [.verify, .keyPackage, .deviceGroup])
        r = KeyResetPublish.marking(r, done: .deviceGroup)
        XCTAssertEqual(KeyResetPublish.remaining(r), [.verify, .keyPackage])
        r = KeyResetPublish.marking(r, done: .keyPackage)
        XCTAssertEqual(KeyResetPublish.remaining(r), [])
        XCTAssertTrue(KeyResetPublish.isComplete(r))
    }

    func testTheThirtyDayBoundAppliesOnlyWhenAsked() {
        let day: Int64 = 24 * 60 * 60 * 1000
        XCTAssertEqual(
            KeyResetPublish.freshness(signedAtMs: 0, nowMs: 31 * day, requireFresh: true), .signInRequired)
        XCTAssertEqual(KeyResetPublish.freshness(signedAtMs: 0, nowMs: 30 * day, requireFresh: true), .fresh)
        XCTAssertEqual(KeyResetPublish.freshness(signedAtMs: 0, nowMs: 31 * day, requireFresh: false), .fresh)
    }

    func testTheDirectorysAnswersAreReadByTheirCode() {
        func http(_ status: Int, _ body: String) -> Error {
            DiscoveryClientError.http(status: status, endpoint: "/discovery/identity", body: body)
        }
        XCTAssertEqual(
            KeyResetPublish.discoveryOutcome(http(409, #"{"error":"key-generation-not-newer"}"#)), .done)
        XCTAssertEqual(
            KeyResetPublish.discoveryOutcome(http(401, #"{"error":"key-generation-superseded"}"#)), .superseded)
        XCTAssertEqual(
            KeyResetPublish.discoveryOutcome(http(409, #"{"error":"identity-change-needs-attestation"}"#)), .retry)
        XCTAssertEqual(KeyResetPublish.discoveryOutcome(http(503, #"{"error":"key-reset-disabled"}"#)), .retry)
        XCTAssertEqual(KeyResetPublish.discoveryOutcome(URLError(.notConnectedToInternet)), .retry)

        XCTAssertEqual(IdentityUploadRefusal.classify(status: 400, body: #"{"error":"key-reset-attestation-invalid"}"#), .attestationInvalid)
        XCTAssertEqual(
            IdentityUploadRefusal.classify(status: 429, body: #"{"error":"identity-changed-too-recently","retry_after_secs":90}"#),
            .changedTooRecently(retryAfterSecs: 90))
        XCTAssertEqual(IdentityUploadRefusal.classify(status: 503, body: #"{"error":"key-reset-disabled"}"#), .keyResetDisabled)
        XCTAssertEqual(IdentityUploadRefusal.classify(status: 409, body: "conflict"), .other)
        XCTAssertNil(IdentityUploadRefusal.of(URLError(.timedOut)))
    }

    func testTheRecordRoundTripsThroughItsStore() {
        let defaults = UserDefaults(suiteName: "key-reset-\(UUID().uuidString)")!
        let markers = RecoveryMarkers(defaults: defaults)
        XCTAssertNil(markers.publishOwed(accountIdB64: "a"))
        XCTAssertFalse(markers.isPublishOwed(accountIdB64: "a"))
        var r = record
        r.discoveryDone = true
        markers.markPublishOwed(r, accountIdB64: "a")
        XCTAssertEqual(markers.publishOwed(accountIdB64: "a"), r)
        XCTAssertNil(markers.publishOwed(accountIdB64: "b"))
        markers.clear(accountIdB64: "a")
        XCTAssertNil(markers.publishOwed(accountIdB64: "a"))
        // An unreadable record reads as nothing owed and is dropped.
        defaults.set(Data("garbage".utf8), forKey: RecoveryMarkers.publishOwedKey(accountIdB64: "c"))
        XCTAssertNil(markers.publishOwed(accountIdB64: "c"))
        XCTAssertNil(defaults.data(forKey: RecoveryMarkers.publishOwedKey(accountIdB64: "c")))
    }

    func testTheAttestedResetBodyAddsTheOneFieldBesideTheStatement() throws {
        let plain = DsStatementRequest(
            path: "/ds/group/ab/reset", bodyJson: #"{"statement_b64":"c3RtdA=="}"#)
        // ADR-0066 section 4.6 — the DS refuses the attestation without the
        // root endorsement, so the two fields are only ever attached together.
        let attested = try AttestedResetBody.attach(
            attestation: Data([1, 2, 3]), endorsement: Data([9, 8, 7]), to: plain)
        XCTAssertEqual(attested.path, plain.path)
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(attested.bodyJson.utf8)) as? [String: Any])
        XCTAssertEqual(json["statement_b64"] as? String, "c3RtdA==")
        XCTAssertEqual(json["key_reset_attest_b64"] as? String, Data([1, 2, 3]).base64EncodedString())
        XCTAssertEqual(json["root_endorsement_b64"] as? String, Data([9, 8, 7]).base64EncodedString())
        XCTAssertEqual(json.count, 3)
        XCTAssertThrowsError(
            try AttestedResetBody.attach(
                attestation: Data(), endorsement: Data([1]), to: DsStatementRequest(path: "/x", bodyJson: "[]")))
        // An empty endorsement is refused here rather than sent: the DS would
        // answer 400.
        XCTAssertThrowsError(
            try AttestedResetBody.attach(attestation: Data([1]), endorsement: Data(), to: plain))
    }

    func testTheDsRefusalsName503KeyResetDisabled() {
        XCTAssertEqual(
            DsStatementError.from(status: 503, body: #"{"error":"key-reset-disabled"}"#), .keyResetDisabled)
        XCTAssertEqual(
            DsStatementError.from(status: 503, body: #"{"error":"push-binding-disabled"}"#), .pushBindingDisabled)
    }

    func testTheConfirmationWordIsTheSameLatinLettersTrimmed() {
        XCTAssertTrue(KeyResetConfirmation.matches("RESET"))
        XCTAssertTrue(KeyResetConfirmation.matches("  RESET\n"))
        XCTAssertFalse(KeyResetConfirmation.matches("reset"))
        XCTAssertFalse(KeyResetConfirmation.matches(""))
        XCTAssertFalse(KeyResetConfirmation.matches("RESET1"))
    }
}

// MARK: - The signed-out device (section 3.5)

@MainActor
final class KeysSignOutTests: XCTestCase {

    private final class Persistence: SessionPersistence {
        var seed: Data? = Data(repeating: 0x5A, count: 64)
        var token: PersistedSessionToken?
        private(set) var deleted: [String] = []
        func hasDeviceKey(accountIdB64: String) -> Bool { true }
        func accountIdentitySeed(accountIdB64: String) -> Data? { seed }
        func exportKey(accountIdB64: String) -> Data? { nil }
        func sessionToken(accountIdB64: String) -> PersistedSessionToken? { token }
        func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws { self.token = token }
        func deleteSessionToken(accountIdB64: String) { deleted.append(accountIdB64) }
    }

    /// Discovery, scripted: what it publishes for the account.
    private final class Directory: AccountDirectory, DeviceRestoreDirectory {
        var published: (key: Data, generation: UInt64)?
        var fetchError: Error?
        private(set) var uploads = 0
        func fetchIdentityBundle(accountId: Data, sessionToken: String?) async throws -> IdentityFetchResponse? {
            if let fetchError { throw fetchError }
            guard let published else { return nil }
            return IdentityFetchResponse(
                signaturePubkey: base64Encode(published.key), x25519Pubkey: "", keyGeneration: published.generation)
        }
        func lookupAccount(username: String, accountId: Data, sessionToken: String) async throws -> Data? { nil }
        func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
            RegisterResult(normalisedUsername: username)
        }
        func publishIdentityBundle(
            accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
            leafAuth: Data?, genesisAttest: Data?, keyResetAttest: Data?, sessionToken: String
        ) async throws {
            uploads += 1
        }
    }

    private final class WipeLog {
        private(set) var accounts: [String] = []
        func note(_ account: String) { accounts.append(account) }
    }

    private let accountId = Data((0..<16).map { UInt8($0) })
    private var accountIdB64: String { base64Encode(accountId) }
    private let seed = Data(repeating: 0x5A, count: 64)
    private var suites: [String] = []

    override func tearDown() async throws {
        for suite in suites { UserDefaults().removePersistentDomain(forName: suite) }
        try await super.tearDown()
    }

    private func defaults(_ name: String) -> UserDefaults {
        let suite = "\(name)-\(UUID().uuidString)"
        suites.append(suite)
        return UserDefaults(suiteName: suite)!
    }

    private struct Harness {
        let repository: AuthRepository
        let tokens: SessionTokenProvider
        let directory: Directory
        let persistence: Persistence
        let wiped: WipeLog
        let store: SessionStore
        let recalls: InMemoryRecallRecordStore
    }

    private func restoredSession(
        refresh: Result<SessionPair, Error>? = nil,
        directory: Directory = Directory(),
        holdsKeys: Bool = true
    ) -> Harness {
        let store = SessionStore(defaults: defaults("keys-signout"))
        store.save(PersistedSession(accountIdB64: accountIdB64, username: "alice"))
        let persistence = Persistence()
        persistence.token = PersistedSessionToken(pair: .fixture(access: "tok-old", accessExpiresAtMs: 2_000_000))
        let tokens = SessionTokenProvider()
        tokens.nowMs = { 1_000_000 }
        let wiped = WipeLog()
        let recalls = InMemoryRecallRecordStore()
        if !holdsKeys { persistence.seed = nil }
        let repository = AuthRepository(
            sessionStore: store,
            authClientFactory: { NameRouteStub.client(NameRouteStub.scope { _, _, _ in (503, "") }) },
            persistence: persistence,
            nowMs: { 1_000_000 },
            accountDirectory: directory,
            loginHandleBinds: LoginHandleBindLedger(),
            restoreDirectory: directory,
            markers: RecoveryMarkers(defaults: defaults("keys-signout-markers")),
            tokenProvider: tokens,
            wipeAfterKeyReset: { account in wiped.note(account) },
            recallRecords: recalls,
            recallOperations: .unreachable
        )
        if let refresh {
            tokens.refresher = { _ in try refresh.get() }
        }
        return Harness(
            repository: repository, tokens: tokens, directory: directory, persistence: persistence,
            wiped: wiped, store: store, recalls: recalls)
    }

    private func waitUntil(_ condition: @MainActor () -> Bool, timeout: TimeInterval = 5) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 20_000_000)
        }
        return condition()
    }

    /// A device that never held the account's keys (signed in, keys not
    /// restored): the existing signed-out screen and the full wipe.
    func testARefreshRefusedAsSupersededShowsTheScreenAndWipesByItself() async {
        let h = restoredSession(
            refresh: .failure(AuthClientError.http(
                status: 401, endpoint: "/auth/session/refresh", body: #"{"error":"key-generation-superseded"}"#)),
            holdsKeys: false)
        // Without the keys the launch state is the sign-in screen; the
        // superseded signal is delivered for the account of the record.
        h.repository.handleKeysSuperseded(accountIdB64: accountIdB64)
        let shown = await waitUntil { h.repository.keysSignOut != nil }
        XCTAssertTrue(shown)
        XCTAssertEqual(h.repository.keysSignOutUsername, "alice")
        let wiped = await waitUntil { h.repository.keysSignOut == .removed }
        XCTAssertTrue(wiped)
        XCTAssertEqual(h.wiped.accounts, [accountIdB64], "the full wipe ran by itself, once")
        XCTAssertNil(h.repository.recall, "no keys, so nothing to take the account back with")
        XCTAssertNil(h.recalls.record)
        XCTAssertNil(h.tokens.current, "the superseded pair is gone")
        XCTAssertEqual(h.directory.uploads, 0, "nothing is published first")
        // "Sign in" dismisses the screen; the session behind it is over.
        h.repository.dismissKeysSignOut()
        XCTAssertNil(h.repository.keysSignOut)
    }

    /// ADR-0066 section 1.1: a device that holds the keys learns of the
    /// reset at launch, deletes what the account kept, and asks.
    func testARefreshRefusedAsSupersededOnADeviceWithKeysAsksWasItYou() async {
        let h = restoredSession(refresh: .failure(AuthClientError.http(
            status: 401, endpoint: "/auth/session/refresh", body: #"{"error":"key-generation-superseded"}"#)))
        XCTAssertEqual(h.repository.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
        let shown = await waitUntil { h.repository.recall != nil }
        XCTAssertTrue(shown, "the silent restore refreshes and learns of the reset at launch")
        XCTAssertEqual(h.repository.recall?.username, "alice")
        XCTAssertNil(h.repository.keysSignOut)
        let wiped = await waitUntil { h.wiped.accounts == [self.accountIdB64] }
        XCTAssertTrue(wiped, "the staged wipe ran by itself, once")
        XCTAssertNil(h.tokens.current, "the superseded pair is gone")
        XCTAssertEqual(h.directory.uploads, 0, "nothing is published first")
    }

    func testAPublishedHigherGenerationWithOtherKeysIsStaleKeys() async {
        let directory = Directory()
        directory.published = (Data(repeating: 0xBB, count: 32), 2)
        let h = restoredSession(directory: directory)
        let shown = await waitUntil { h.repository.recall != nil }
        XCTAssertTrue(shown, "the landing rule's stale keys lead to the same staged wipe")
        let wiped = await waitUntil { h.wiped.accounts == [accountIdB64] }
        XCTAssertTrue(wiped)
        XCTAssertNotNil(h.recalls.record)
        XCTAssertEqual(h.directory.uploads, 0, "the old bundle is never uploaded")
    }

    func testAPublishedOtherKeyAtTheSameGenerationIsAMismatchWipedOnTheTap() async {
        let directory = Directory()
        directory.published = (Data(repeating: 0xBB, count: 32), 1)
        let h = restoredSession(directory: directory)
        let shown = await waitUntil { h.repository.keysSignOut == .mismatch }
        XCTAssertTrue(shown)
        try? await Task.sleep(nanoseconds: 100_000_000)
        XCTAssertTrue(h.wiped.accounts.isEmpty, "the anomaly wipes on the tap only")
        XCTAssertEqual(h.directory.uploads, 0)
        h.repository.signOutAfterKeysMismatch()
        let wiped = await waitUntil { h.repository.keysSignOut == .removed }
        XCTAssertTrue(wiped)
        XCTAssertEqual(h.wiped.accounts, [accountIdB64])
    }

    func testOurOwnPublishedKeyLandsAndASilentRestoreUploadsNothing() async {
        let directory = Directory()
        directory.published = (try! Identity.fromSeed(seed: seed).signaturePublicKey(), 1)
        let h = restoredSession(directory: directory)
        try? await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertNil(h.repository.keysSignOut)
        XCTAssertEqual(h.directory.uploads, 0, "a silent restore does not re-publish an unchanged key")
        XCTAssertEqual(h.repository.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
    }

    func testAnUnreachableDirectoryLandsTheSessionAndWipesNothing() async {
        let directory = Directory()
        directory.fetchError = URLError(.notConnectedToInternet)
        let h = restoredSession(directory: directory)
        try? await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertNil(h.repository.keysSignOut)
        XCTAssertTrue(h.wiped.accounts.isEmpty)
        XCTAssertEqual(h.repository.state, .session(SessionAccount(accountIdB64: accountIdB64, username: "alice")))
    }

    func testARefreshRefusedAsInvalidSignsOutWithoutAWipe() async {
        let h = restoredSession(refresh: .failure(AuthClientError.http(
            status: 401, endpoint: "/auth/session/refresh", body: #"{"error":"refresh-invalid"}"#)))
        let out = await waitUntil { h.repository.state == .anon(cachedUsername: "alice") }
        XCTAssertTrue(out)
        XCTAssertTrue(h.repository.sessionExpiredNotice)
        XCTAssertNil(h.repository.keysSignOut)
        XCTAssertTrue(h.wiped.accounts.isEmpty, "the keys are still this account's")
        XCTAssertEqual(h.persistence.deleted, [accountIdB64], "the dead pair is dropped")
        h.repository.clearSessionExpiredNotice()
        XCTAssertFalse(h.repository.sessionExpiredNotice)
    }

    func testARefreshedPairReplacesTheLiveSecretsAndIsPersisted() async {
        let fresh = SessionPair.fixture(access: "tok-new", accessExpiresAtMs: 3_000_000, generation: 1)
        let h = restoredSession(refresh: .success(fresh))
        let swapped = await waitUntil { h.repository.identitySecrets?.session == fresh }
        XCTAssertTrue(swapped)
        XCTAssertEqual(h.persistence.token?.sessionToken, "tok-new")
        XCTAssertTrue(h.tokens.isKnownAccessToken("tok-old"))
        XCTAssertNil(h.repository.keysSignOut)
    }
}
