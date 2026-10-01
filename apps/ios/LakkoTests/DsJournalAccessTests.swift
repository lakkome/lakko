// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// MARK: - Fakes

/// A group signer that records every request it was asked to sign.
private final class FakeSigner: DsGroupSigner {
    let epochValue: UInt64
    let label: String
    var signed: [(method: String, path: String, query: String, body: Data)] = []

    init(epoch: UInt64, label: String? = nil) {
        epochValue = epoch
        self.label = label ?? "sig-e\(epoch)"
    }

    func epoch() -> UInt64 { epochValue }

    func sign(method: String, path: String, query: String, body: Data) -> String {
        signed.append((method, path, query, body))
        return label
    }

    func verifyingKey() -> Data { Data(repeating: UInt8(truncatingIfNeeded: epochValue), count: 32) }
}

/// A DS stand-in whose answers are scripted per request, recording every
/// request it saw (method, URL, proof header, body).
final class DsAccessURLProtocol: URLProtocol {
    struct Seen {
        let method: String
        let url: URL
        let proof: String?
        let body: Data
        var anonToken: String? = nil
        var authorization: String? = nil
    }

    struct Answer {
        var status: Int
        var body: Data = Data(#"{"messages":[],"has_more":false}"#.utf8)
        var date: String?
    }

    private static let lock = NSLock()
    private static var scripts: [String: [Answer]] = [:]
    private static var seen: [String: [Seen]] = [:]

    static func freshScope() -> String {
        "a\(UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()).test"
    }

    static func client(_ scope: String, tokens: AnonTokenSource = FakeAnonTokens()) -> DsClient {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [DsAccessURLProtocol.self]
        return DsClient(
            baseURL: "http://\(scope):9/native/ds",
            session: URLSession(configuration: config),
            tokens: tokens)
    }

    static func script(_ scope: String, _ answers: [Answer]) {
        lock.lock(); defer { lock.unlock() }
        scripts[scope] = answers
    }

    static func requests(_ scope: String) -> [Seen] {
        lock.lock(); defer { lock.unlock() }
        return seen[scope] ?? []
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url else { return }
        let scope = url.host?.lowercased() ?? "?"
        let answer: Answer
        Self.lock.lock()
        Self.seen[scope, default: []].append(
            Seen(
                method: request.httpMethod ?? "GET",
                url: url,
                proof: request.value(forHTTPHeaderField: "x-lakko-ds-proof"),
                body: Self.bodyData(of: request),
                anonToken: request.value(forHTTPHeaderField: "x-lakko-anon-token"),
                authorization: request.value(forHTTPHeaderField: "Authorization")))
        var queue = Self.scripts[scope] ?? []
        answer = queue.isEmpty ? Answer(status: 200) : queue.removeFirst()
        Self.scripts[scope] = queue
        Self.lock.unlock()
        var headers = ["Content-Type": "application/json"]
        if let date = answer.date { headers["Date"] = date }
        let response = HTTPURLResponse(
            url: url, statusCode: answer.status, httpVersion: "HTTP/1.1", headerFields: headers)!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: answer.body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}

    private static func bodyData(of request: URLRequest) -> Data {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { return Data() }
        stream.open()
        defer { stream.close() }
        var out = Data()
        var buffer = [UInt8](repeating: 0, count: 8192)
        while stream.hasBytesAvailable {
            let read = stream.read(&buffer, maxLength: buffer.count)
            if read <= 0 { break }
            out.append(contentsOf: buffer[0..<read])
        }
        return out
    }
}

private func httpDate(_ date: Date) -> String {
    let formatter = DateFormatter()
    formatter.locale = Locale(identifier: "en_US_POSIX")
    formatter.timeZone = TimeZone(identifier: "GMT")
    formatter.dateFormat = "EEE, dd MMM yyyy HH:mm:ss 'GMT'"
    return formatter.string(from: date)
}

// MARK: - Tests

/// ADR-0053 — the iOS client contract (points 1-10), for the pieces that are
/// pure or can be driven through a stubbed DS: what is signed and with which
/// inputs, the 401 retry order and the clock copy, the previous-epoch memory,
/// the knock-row filter against real MLS bytes, the knock sequence, the
/// register-before-publish order of a direct commit, heal-on-apply, and that
/// every mailbox poller is signed for the mailbox it actually reads.
@MainActor
final class DsJournalAccessTests: XCTestCase {

    // MARK: Point 1 — what a proof signs

    /// The proof covers the SERVICE path, never the base URL's nginx prefix,
    /// and the query exactly as `URLComponents` puts it on the wire.
    func testSigningInputsAreTheServicePathAndTheWireQuery() throws {
        let target = try DsClient.requestTarget(
            baseURL: "https://test.lakko.me/native/ds",
            endpoint: "/ds/group/ab01/messages",
            queryItems: [
                URLQueryItem(name: "after", value: "5"),
                URLQueryItem(name: "limit", value: "100"),
                URLQueryItem(name: "wait", value: "30"),
            ])
        XCTAssertEqual(target.path, "/ds/group/ab01/messages")
        XCTAssertEqual(target.query, "after=5&limit=100&wait=30")
        XCTAssertEqual(target.url.query(percentEncoded: true), target.query)
        XCTAssertEqual(
            target.url.absoluteString,
            "https://test.lakko.me/native/ds/ds/group/ab01/messages?after=5&limit=100&wait=30")
    }

    /// A value needing escaping is signed in its escaped wire form, byte for
    /// byte what the request carries.
    func testAQueryNeedingEscapingIsSignedAsSent() throws {
        let target = try DsClient.requestTarget(
            baseURL: "http://localhost:8082",
            endpoint: "/ds/group/ab/knocks",
            queryItems: [URLQueryItem(name: "x", value: "a b")])
        XCTAssertEqual(target.query, "x=a%20b")
        XCTAssertEqual(target.url.query(percentEncoded: true), "x=a%20b")
    }

    func testNoQueryIsSignedAsEmpty() throws {
        let target = try DsClient.requestTarget(
            baseURL: "http://localhost:8082", endpoint: "/ds/group/ab/access", queryItems: [])
        XCTAssertEqual(target.query, "")
        XCTAssertNil(target.url.query)
    }

    /// Through the client: a signed read carries the header, and the signer
    /// was handed GET, the service path, the exact query and an empty body.
    func testASignedReadCarriesTheHeaderOverTheExactInputs() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        let ds = DsAccessURLProtocol.client(scope)
        let signer = FakeSigner(epoch: 3)
        _ = try await ds.fetchGroup(
            groupId: Data([0xAB, 0x01]), after: 7, waitSecs: 0, auth: DsAuth(signer: signer))

        let seen = try XCTUnwrap(DsAccessURLProtocol.requests(scope).first)
        XCTAssertEqual(seen.proof, "sig-e3")
        XCTAssertEqual(seen.url.path, "/native/ds/ds/group/ab01/messages")
        let input = try XCTUnwrap(signer.signed.first)
        XCTAssertEqual(input.method, "GET")
        XCTAssertEqual(input.path, "/ds/group/ab01/messages")
        XCTAssertEqual(input.query, seen.url.query(percentEncoded: true))
        XCTAssertEqual(input.query, "after=7&wait=0")
        XCTAssertEqual(input.body, Data())
    }

    /// A signed publish signs the exact body bytes it sends.
    func testASignedPublishSignsTheExactBody() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [
            .init(status: 201, body: Data(#"{"id":4,"created_at":"x"}"#.utf8)),
        ])
        let ds = DsAccessURLProtocol.client(scope)
        let signer = FakeSigner(epoch: 1)
        let payload = Data([1, 2, 3, 4])
        _ = try await ds.publishGroup(groupId: Data([0x0A]), payload: payload, auth: DsAuth(signer: signer))

        let seen = try XCTUnwrap(DsAccessURLProtocol.requests(scope).first)
        XCTAssertEqual(seen.method, "POST")
        XCTAssertEqual(seen.body, payload)
        XCTAssertEqual(signer.signed.first?.body, payload)
        XCTAssertEqual(signer.signed.first?.query, "")
    }

    /// Writing to a mailbox stays unsigned (it is open for writes by design).
    func testAMailboxPublishIsUnsigned() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [
            .init(status: 201, body: Data(#"{"id":1,"created_at":"x"}"#.utf8)),
        ])
        _ = try await DsAccessURLProtocol.client(scope).publishMailbox(
            pseudonym: Data(repeating: 1, count: 32), payload: Data([9]))
        XCTAssertNil(DsAccessURLProtocol.requests(scope).first?.proof)
    }

    // MARK: Point 5 — retry on 401, and the clock copy

    func testTheRetryPlanIsPreviousEpochThenReclaim() {
        XCTAssertEqual(
            DsUnauthorizedRetry.plan(hasPrevious: true, hasReclaim: true),
            [.previousEpoch, .reclaim])
        XCTAssertEqual(DsUnauthorizedRetry.plan(hasPrevious: false, hasReclaim: true), [.reclaim])
        XCTAssertEqual(DsUnauthorizedRetry.plan(hasPrevious: true, hasReclaim: false), [.previousEpoch])
        XCTAssertEqual(DsUnauthorizedRetry.plan(hasPrevious: false, hasReclaim: false), [])
    }

    func testARefusedReadIsRetriedUnderThePreviousEpoch() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [.init(status: 401), .init(status: 200)])
        let current = FakeSigner(epoch: 5)
        let previous = FakeSigner(epoch: 4)
        var reclaimed = false
        _ = try await DsAccessURLProtocol.client(scope).fetchGroup(
            groupId: Data([1]), after: 0, waitSecs: 0,
            auth: DsAuth(signer: current, previous: previous, reclaim: {
                reclaimed = true
                return current
            }))
        XCTAssertEqual(DsAccessURLProtocol.requests(scope).map(\.proof), ["sig-e5", "sig-e4"])
        XCTAssertFalse(reclaimed, "a success under the previous epoch needs no claim")
    }

    func testAfterThePreviousEpochTheClaimOnOpenIsTheLastRetry() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [.init(status: 401), .init(status: 401), .init(status: 200)])
        let fresh = FakeSigner(epoch: 5, label: "after-claim")
        _ = try await DsAccessURLProtocol.client(scope).fetchGroup(
            groupId: Data([1]), after: 0, waitSecs: 0,
            auth: DsAuth(
                signer: FakeSigner(epoch: 5), previous: FakeSigner(epoch: 4),
                reclaim: { fresh }))
        XCTAssertEqual(
            DsAccessURLProtocol.requests(scope).map(\.proof), ["sig-e5", "sig-e4", "after-claim"])
    }

    /// Two refusals and a server clock five minutes off: the copy is the clock.
    func testAFinalRefusalWithASkewedClockSaysSo() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        let skewed = httpDate(Date().addingTimeInterval(300))
        DsAccessURLProtocol.script(scope, [
            .init(status: 401, date: skewed), .init(status: 401, date: skewed),
        ])
        do {
            _ = try await DsAccessURLProtocol.client(scope).fetchGroup(
                groupId: Data([1]), after: 0, waitSecs: 0,
                auth: DsAuth(signer: FakeSigner(epoch: 1), reclaim: { FakeSigner(epoch: 1) }))
            XCTFail("a refused read must throw")
        } catch {
            guard let dsError = error as? DsClientError, case .clockSkew = dsError else {
                return XCTFail("expected the clock error, got \(error)")
            }
            XCTAssertEqual(error.localizedDescription, "Check this device's date and time.")
            XCTAssertEqual(
                ChatError.wrapped("DS publish failed", underlying: error).errorDescription,
                "Check this device's date and time.")
        }
    }

    /// A refusal with the clocks in agreement is the ordinary delivery error.
    func testAFinalRefusalWithAGoodClockIsTheOrdinaryError() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [.init(status: 401, date: httpDate(Date()))])
        do {
            _ = try await DsAccessURLProtocol.client(scope).fetchGroup(
                groupId: Data([1]), after: 0, waitSecs: 0, auth: DsAuth(signer: FakeSigner(epoch: 1)))
            XCTFail("a refused read must throw")
        } catch {
            guard let dsError = error as? DsClientError, case .http(let status, _, _) = dsError else {
                return XCTFail("expected the ordinary error, got \(error)")
            }
            XCTAssertEqual(status, 401)
        }
    }

    func testClockSkewTolerance() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        XCTAssertFalse(DsClockSkew.isSkewed(serverDateHeader: httpDate(now.addingTimeInterval(59)), now: now))
        XCTAssertTrue(DsClockSkew.isSkewed(serverDateHeader: httpDate(now.addingTimeInterval(61)), now: now))
        XCTAssertTrue(DsClockSkew.isSkewed(serverDateHeader: httpDate(now.addingTimeInterval(-3600)), now: now))
        XCTAssertFalse(DsClockSkew.isSkewed(serverDateHeader: nil, now: now))
        XCTAssertFalse(DsClockSkew.isSkewed(serverDateHeader: "not a date", now: now))
    }

    // MARK: Points 2 + 5 — the previous-epoch memory

    func testTheMemoryKeepsTheEpochBeforeTheCurrentOne() {
        let memory = DsSignerMemory()
        let e3 = FakeSigner(epoch: 3), e4 = FakeSigner(epoch: 4), e5 = FakeSigner(epoch: 5)
        XCTAssertNil(memory.observe(journal: "g", signer: e3))
        XCTAssertTrue(memory.observe(journal: "g", signer: e4) === e3)
        XCTAssertTrue(memory.observe(journal: "g", signer: FakeSigner(epoch: 4)) === e3,
                      "re-deriving the same epoch keeps the previous")
        XCTAssertEqual(
            memory.observe(journal: "g", signer: e5)?.epoch(), 4,
            "moving on makes the epoch-4 signer the previous one")
        XCTAssertNil(memory.previousSigner(journal: "other", before: 5))
    }

    /// A group that went BACK (a reset to the durable snapshot) has no
    /// "previous" at or above its epoch.
    func testAGroupThatWentBackDropsAPreviousThatIsNotBefore() {
        let memory = DsSignerMemory()
        memory.observe(journal: "g", signer: FakeSigner(epoch: 4))
        memory.observe(journal: "g", signer: FakeSigner(epoch: 5))
        XCTAssertNil(memory.observe(journal: "g", signer: FakeSigner(epoch: 4)))
    }

    // MARK: Point 3 — register before publish (direct commit)

    func testADirectCommitRegistersTheNextEpochBeforeRecordAndPublish() async throws {
        var log: [String] = []
        var epoch: UInt64 = 6
        var registered: UInt64?
        var publishedUnder: UInt64?
        let result = try await DirectCommitSteps(
            currentSigner: { log.append("signer"); return FakeSigner(epoch: epoch) },
            create: { () -> String in log.append("create"); epoch += 1; return "commit" },
            register: { next in
                log.append("register"); registered = next.epoch()
            },
            prepare: { created in log.append("record"); return created },
            rollback: { log.append("rollback") },
            publish: { prepared, signerN in
                log.append("publish"); publishedUnder = signerN.epoch(); return prepared
            }
        ).run()
        XCTAssertEqual(result, "commit")
        XCTAssertEqual(log, ["signer", "create", "signer", "register", "record", "publish"])
        XCTAssertEqual(registered, 7, "the epoch the commit moves to")
        XCTAssertEqual(publishedUnder, 6, "the commit POST is signed under N")
    }

    func testAFailedRegistrationRollsBackAndPublishesNothing() async throws {
        var log: [String] = []
        struct Refused: Error {}
        do {
            _ = try await DirectCommitSteps(
                currentSigner: { FakeSigner(epoch: 1) },
                create: { () -> Int in log.append("create"); return 1 },
                register: { _ in log.append("register"); throw Refused() },
                prepare: { created in log.append("record"); return created },
                rollback: { log.append("rollback") },
                publish: { _, _ in log.append("publish") }
            ).run()
            XCTFail("a failed registration must abort")
        } catch is Refused {}
        XCTAssertEqual(log, ["create", "register", "rollback"])
    }

    func testAFailedRecordAfterRegistrationRollsBackToo() async throws {
        var log: [String] = []
        struct Disk: Error {}
        do {
            _ = try await DirectCommitSteps(
                currentSigner: { FakeSigner(epoch: 1) },
                create: { () -> Int in 1 },
                register: { _ in log.append("register") },
                prepare: { (_: Int) -> Int in log.append("record"); throw Disk() },
                rollback: { log.append("rollback") },
                publish: { _, _ in log.append("publish") }
            ).run()
            XCTFail("a failed record must abort")
        } catch is Disk {}
        XCTAssertEqual(log, ["register", "record", "rollback"])
    }

    // MARK: Point 4 — heal on apply: gone under ADR-0057 (see
    // `JournalOwnershipTests`, a plain member registers nothing).

    // MARK: Point 6 — the knock sequence

    func testTheJoinerProbesThenKnocksWithAttributionAndProposal() async throws {
        var log: [String] = []
        var knocked: [[Data]] = []
        let epoch = try await DsKnockJoin(
            knock: { payloads in
                log.append("knock:\(payloads.count)"); knocked.append(payloads); return 7
            },
            createProposal: { e in log.append("proposal@\(e)"); return Data([0xB0]) },
            attribution: { log.append("attribution"); return Data([0xA7]) }
        ).run()
        XCTAssertEqual(epoch, 7)
        XCTAssertEqual(log, ["knock:0", "proposal@7", "attribution", "knock:2"])
        XCTAssertEqual(knocked.last, [Data([0xA7]), Data([0xB0])], "attribution first, then the proposal")
    }

    func testAnUnclaimedGroupIsNotKnockedWithAGuessedEpoch() async throws {
        var proposed = false
        do {
            _ = try await DsKnockJoin(
                knock: { _ in nil },
                createProposal: { _ in proposed = true; return Data() },
                attribution: { nil }
            ).run()
            XCTFail("no epoch means no proposal")
        } catch DsKnockJoinError.groupNotClaimed {}
        XCTAssertFalse(proposed)
    }

    func testAMissingAttributionStillSendsTheProposal() async throws {
        var last: [Data] = []
        _ = try await DsKnockJoin(
            knock: { payloads in last = payloads; return 2 },
            createProposal: { _ in Data([0xB0]) },
            attribution: { nil }
        ).run()
        XCTAssertEqual(last, [Data([0xB0])])
    }

    /// ADR-0054 S3: the knock spends one `ds-knock` token in its header,
    /// carries no proof of work, no Authorization and no journal proof.
    func testTheKnockWireCarriesATokenAndBase64Payloads() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [
            .init(status: 201, body: Data(#"{"ids":[10,11],"epoch":4}"#.utf8)),
        ])
        let tokens = FakeAnonTokens()
        let result = try await DsAccessURLProtocol.client(scope, tokens: tokens).knock(
            groupId: Data([0xEE]),
            payloads: [Data([1]), Data([2, 3])])
        XCTAssertEqual(result, KnockResult(ids: [10, 11], epoch: 4))
        let seen = try XCTUnwrap(DsAccessURLProtocol.requests(scope).first)
        XCTAssertEqual(seen.url.path, "/native/ds/ds/group/ee/knock")
        XCTAssertNil(seen.proof, "a knock is unauthenticated")
        XCTAssertNil(seen.authorization, "a token route never carries a session")
        XCTAssertEqual(seen.anonToken, "ds-knock-1")
        XCTAssertEqual(tokens.taken, [.dsKnock])
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: seen.body) as? [String: Any])
        XCTAssertEqual(json["payloads_b64"] as? [String], ["AQ==", "AgM="])
        XCTAssertNil(json["pow"], "no proof of work any more")
    }

    /// A spent token is dropped and the knock retried ONCE with another.
    func testASpentTokenIsReplacedOnce() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [
            .init(status: 401, body: Data(#"{"error":"anon-token-spent"}"#.utf8)),
            .init(status: 201, body: Data(#"{"ids":[],"epoch":5}"#.utf8)),
        ])
        let result = try await DsAccessURLProtocol.client(scope).knock(
            groupId: Data([0xEE]), payloads: [])
        XCTAssertEqual(result.epoch, 5)
        XCTAssertEqual(
            DsAccessURLProtocol.requests(scope).map(\.anonToken), ["ds-knock-1", "ds-knock-2"])
    }

    /// A second refusal surfaces as a token refusal, never retried again.
    func testASecondTokenRefusalSurfaces() async throws {
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [
            .init(status: 401, body: Data(#"{"error":"anon-token-expired"}"#.utf8)),
            .init(status: 401, body: Data(#"{"error":"anon-token-invalid"}"#.utf8)),
        ])
        do {
            _ = try await DsAccessURLProtocol.client(scope).knock(groupId: Data([0xEE]), payloads: [])
            XCTFail("two refusals must fail")
        } catch let error as AnonTokenUnavailable {
            XCTAssertEqual(error, .refused(code: "anon-token-invalid"))
            XCTAssertEqual(JoinRequestFailure.classify(error), .generic)
        }
        XCTAssertEqual(DsAccessURLProtocol.requests(scope).count, 2)
    }

    /// A used-up allowance reads as the allowance copy on the join screen.
    func testAUsedUpKnockAllowanceShowsTheAllowanceCopy() {
        let failure = JoinRequestFailure.classify(AnonTokenUnavailable.quotaExhausted(retryAfterSecs: 100))
        XCTAssertEqual(failure, .allowanceUsed)
        XCTAssertEqual(failure.message, "You have used today's allowance for this. Try again tomorrow.")
        XCTAssertFalse(failure.offersRetry)
    }

    // MARK: The knock poll floor

    func testTheKnockPollFloorIsTenSecondsPerGroup() {
        let throttle = KnockPollThrottle()
        let t0 = Date(timeIntervalSince1970: 1_800_000_000)
        XCTAssertEqual(KnockPollThrottle.floorSeconds, 10)
        XCTAssertTrue(throttle.take("a", now: t0))
        XCTAssertFalse(throttle.take("a", now: t0.addingTimeInterval(0.5)))
        XCTAssertFalse(throttle.take("a", now: t0.addingTimeInterval(9.9)))
        XCTAssertTrue(throttle.take("b", now: t0.addingTimeInterval(1)), "the floor is per group")
        XCTAssertTrue(throttle.take("a", now: t0.addingTimeInterval(10)))
        XCTAssertFalse(throttle.take("a", now: t0.addingTimeInterval(15)))
    }

    /// Twice a second for 43 s (what was measured live) is at most five reads.
    func testTheFloorBoundsASteadyPoll() {
        let throttle = KnockPollThrottle()
        let t0 = Date(timeIntervalSince1970: 1_800_000_000)
        let reads = (0..<86).filter { throttle.take("g", now: t0.addingTimeInterval(Double($0) * 0.5)) }
        XCTAssertEqual(reads.count, 5)
    }

    /// A clock that went backwards must not silence the knocks.
    func testABackwardsClockCountsAsDue() {
        let throttle = KnockPollThrottle()
        let t0 = Date(timeIntervalSince1970: 1_800_000_000)
        XCTAssertTrue(throttle.take("a", now: t0))
        XCTAssertTrue(throttle.take("a", now: t0.addingTimeInterval(-60)))
    }

    func testReleaseResetsTheFloor() {
        let throttle = KnockPollThrottle()
        let t0 = Date()
        XCTAssertTrue(throttle.take("a", now: t0))
        throttle.reset()
        XCTAssertTrue(throttle.take("a", now: t0))
    }

    // MARK: The members sheet's attribution read

    private let keyA = Data(repeating: 0xA1, count: 32)
    private let keyB = Data(repeating: 0xB2, count: 32)

    private func decide(
        _ pending: Set<Data>, _ fetchedFor: Set<Data>?, inFlight: Bool = false,
        allow: Bool = true, asked: inout Int
    ) -> JoinAttributionRefresh.Decision {
        var count = 0
        let decision = JoinAttributionRefresh.decide(
            pending: pending, fetchedFor: fetchedFor, inFlight: inFlight,
            allowRead: { count += 1; return allow })
        asked += count
        return decision
    }

    func testANewPendingSetIsReadOnce() {
        var asked = 0
        XCTAssertEqual(decide([keyA], nil, asked: &asked), .fetch)
        XCTAssertEqual(decide([keyA, keyB], [keyA], asked: &asked), .fetch, "a new joiner")
        XCTAssertEqual(decide([keyB], [keyA, keyB], asked: &asked), .fetch, "one left the set")
        XCTAssertEqual(asked, 3)
    }

    /// The same pending set keeps the map and does not even spend the floor.
    func testAnUnchangedPendingSetKeepsTheMapWithoutARead() {
        var asked = 0
        XCTAssertEqual(decide([keyA, keyB], [keyB, keyA], asked: &asked), .keep)
        XCTAssertEqual(asked, 0)
    }

    /// No pending joiner empties the map with no read; once empty, it stays
    /// that way without re-publishing.
    func testAnEmptyPendingSetClearsWithoutARead() {
        var asked = 0
        XCTAssertEqual(decide([], [keyA], asked: &asked), .clear)
        XCTAssertEqual(decide([], nil, asked: &asked), .clear)
        XCTAssertEqual(decide([], [], asked: &asked), .keep)
        XCTAssertEqual(asked, 0)
    }

    /// A changed set inside the floor waits (keeping the map); the next
    /// refresh after the floor reads.
    func testAChangedSetInsideTheFloorWaits() {
        var asked = 0
        XCTAssertEqual(decide([keyA, keyB], [keyA], allow: false, asked: &asked), .keep)
        XCTAssertEqual(decide([keyA, keyB], [keyA], allow: true, asked: &asked), .fetch)
        XCTAssertEqual(asked, 2)
    }

    /// A read already running is not doubled.
    func testAReadInFlightIsNotDoubled() {
        var asked = 0
        XCTAssertEqual(decide([keyA], nil, inFlight: true, asked: &asked), .keep)
        XCTAssertEqual(asked, 0)
    }

    /// End to end with the real floor: an open sheet refreshing twice a second
    /// for 60 s with an unchanged pending set reads once; a new joiner arriving
    /// mid-way costs one more read, no sooner than the floor allows.
    func testAnOpenSheetReadsAtMostOncePerFloorAndOnlyOnChange() {
        let throttle = KnockPollThrottle()
        let t0 = Date(timeIntervalSince1970: 1_800_000_000)
        var fetchedFor: Set<Data>?
        var reads: [Double] = []
        for tick in 0..<120 {
            let seconds = Double(tick) * 0.5
            let pending: Set<Data> = seconds < 3 ? [keyA] : [keyA, keyB]
            let decision = JoinAttributionRefresh.decide(
                pending: pending, fetchedFor: fetchedFor, inFlight: false,
                allowRead: { throttle.take("g", now: t0.addingTimeInterval(seconds)) })
            if decision == .fetch {
                reads.append(seconds)
                fetchedFor = pending
            }
        }
        XCTAssertEqual(reads, [0, 10], "the first read, then the change once the floor allows")
    }

    // MARK: The join screen's failure copy

    func testJoinFailureCopyIsExact() {
        XCTAssertEqual(joinSucceededMessage, "You joined the group.")
        XCTAssertEqual(joinOpenChat, "Open chat")
        XCTAssertEqual(joinTryAgain, "Try again")
        XCTAssertEqual(
            JoinRequestFailure.generic.message,
            "Your request didn't go through. Check your connection and try again.")
        XCTAssertEqual(
            JoinRequestFailure.notTakingRequests.message,
            "This group isn't taking requests yet. Try again later.")
        XCTAssertEqual(JoinRequestFailure.clockSkew.message, "Check this device's date and time.")
        XCTAssertTrue(JoinRequestFailure.generic.offersRetry)
        XCTAssertFalse(JoinRequestFailure.notTakingRequests.offersRetry)
        XCTAssertFalse(JoinRequestFailure.clockSkew.offersRetry)
    }

    func testJoinFailureClassification() {
        XCTAssertEqual(JoinRequestFailure.classify(DsKnockJoinError.groupNotClaimed), .notTakingRequests)
        XCTAssertEqual(
            JoinRequestFailure.classify(DsClientError.clockSkew(endpoint: "/ds/x")), .clockSkew)
        XCTAssertEqual(
            JoinRequestFailure.classify(
                ChatError.wrapped("DS knock failed", underlying: DsClientError.clockSkew(endpoint: "/x"))),
            .clockSkew, "a wrapped clock error is still the clock")
        XCTAssertEqual(
            JoinRequestFailure.classify(
                ChatError.wrapped("x", underlying: DsClientError.network(endpoint: "/x", underlying: URLError(.notConnectedToInternet)))),
            .generic)
        XCTAssertEqual(JoinRequestFailure.classify(ChatError.argument("anything")), .generic)
    }

    // MARK: The pending join request label

    private func resolved(_ rename: String?) -> ResolvedDisplayName {
        resolveDisplayName(
            signatureKey: Data(repeating: 1, count: 32), localRename: rename,
            profile: nil, verification: nil)
    }

    /// A contact is stored under their ACCOUNT key, so the account key names
    /// them — even though the leaf key alone would read "Member".
    func testAPendingJoinFromAContactIsNamedThroughTheAccountKey() {
        XCTAssertEqual(
            ChatViewModel.pendingJoinLabel(accountKeyName: resolved("Anna"), leafName: "Member"),
            "Anna")
    }

    /// A stranger keeps the label the row always had.
    func testAPendingJoinFromAStrangerKeepsTheExistingLabel() {
        XCTAssertEqual(
            ChatViewModel.pendingJoinLabel(accountKeyName: resolved(nil), leafName: "Member"),
            "Member")
        XCTAssertEqual(
            ChatViewModel.pendingJoinLabel(accountKeyName: nil, leafName: nil), "Someone")
    }

    /// The core's account-key read against real MLS bytes: a joiner whose leaf
    /// carries no device binding is named by its own key.
    func testThePendingJoinAccountKeyOfAnUnboundJoinerIsItsOwnKey() throws {
        let owner = try identity(12)
        let group = try Group.create(identity: owner, kind: .group)
        let joiner = try identity(13)
        let proposal = try joiner.createExternalJoinProposal(
            groupId: try group.groupId(), epoch: try group.epoch())
        _ = try group.processKnock(identity: owner, bytes: proposal)
        XCTAssertEqual(
            try group.pendingExternalJoinAccountKey(joinerSignatureKey: joiner.signaturePublicKey()),
            joiner.signaturePublicKey())
        XCTAssertNil(
            try? group.pendingExternalJoinAccountKey(joinerSignatureKey: Data(repeating: 7, count: 32)),
            "no request, no key (nil or a refusal)")
    }

    // MARK: Point 7 — the core knock gate (`processKnock`), against real MLS bytes

    private func identity(_ byte: UInt8) throws -> Identity {
        try Identity.fromSeed(seed: Data(repeating: byte, count: 64))
    }

    /// A stranger's real external-join proposal passes the core's knock gate
    /// and surfaces exactly as the ordinary receive path would surface it.
    func testProcessKnockAcceptsARealExternalJoinProposal() throws {
        let owner = try identity(1)
        let group = try Group.create(identity: owner, kind: .group)
        let joiner = try identity(2)
        let proposal = try joiner.createExternalJoinProposal(
            groupId: try group.groupId(), epoch: try group.epoch())

        let processed = try group.processKnock(identity: owner, bytes: proposal)
        guard case .externalJoinProposal(let key, let retained) = processed else {
            return XCTFail("a stranger's proposal must surface as an external join, got \(processed)")
        }
        XCTAssertEqual(key, joiner.signaturePublicKey())
        XCTAssertTrue(retained)
        XCTAssertEqual(try group.pendingExternalJoins(), [joiner.signaturePublicKey()])
    }

    /// A proposal crafted for ANOTHER group is refused and retains nothing.
    func testProcessKnockRefusesAProposalForAnotherGroup() throws {
        let owner = try identity(3)
        let group = try Group.create(identity: owner, kind: .group)
        let other = try Group.create(identity: try identity(4), kind: .group)
        let joiner = try identity(5)
        let proposal = try joiner.createExternalJoinProposal(
            groupId: try other.groupId(), epoch: try other.epoch())

        XCTAssertThrowsError(try group.processKnock(identity: owner, bytes: proposal))
        XCTAssertEqual(try group.pendingExternalJoins(), [])
    }

    /// A member's application message replayed into the knock queue is refused
    /// by the gate and never applied: the same bytes still decrypt through the
    /// ordinary receive path afterwards, which they could not if the gate had
    /// consumed them.
    func testProcessKnockRefusesAMembersApplicationMessageWithoutApplyingIt() throws {
        let alice = try identity(6)
        let bob = try identity(7)
        let aliceGroup = try Group.create(identity: alice, kind: .group)
        let bobPackage = try bob.createKeyPackage()
        let added = try aliceGroup.addMemberFromWire(identity: alice, keyPackage: bobPackage)
        let bobGroup = try Group.joinFromWelcome(welcome: added.welcome, identity: bob)
        let message = try aliceGroup.createApplicationMessage(
            identity: alice, plaintext: Data("hi".utf8))

        XCTAssertThrowsError(try bobGroup.processKnock(identity: bob, bytes: message))
        let processed = try bobGroup.processIncomingVerifyingRoles(identity: bob, bytes: message)
        guard case .application(let data, _, _) = processed else {
            return XCTFail("the refused row must still be unconsumed, got \(processed)")
        }
        XCTAssertEqual(data, Data("hi".utf8))
    }

    /// A member's commit replayed into the knock queue is refused and does not
    /// move the reader's epoch.
    func testProcessKnockRefusesAMembersCommitWithoutApplyingIt() throws {
        let alice = try identity(8)
        let bob = try identity(9)
        let carol = try identity(10)
        let aliceGroup = try Group.create(identity: alice, kind: .group)
        let added = try aliceGroup.addMemberFromWire(
            identity: alice, keyPackage: try bob.createKeyPackage())
        let bobGroup = try Group.joinFromWelcome(welcome: added.welcome, identity: bob)
        let epochBefore = try bobGroup.epoch()
        let commit = try aliceGroup.addMemberFromWire(
            identity: alice, keyPackage: try carol.createKeyPackage()).commit

        XCTAssertThrowsError(try bobGroup.processKnock(identity: bob, bytes: commit))
        XCTAssertEqual(try bobGroup.epoch(), epochBefore, "a refused commit is not applied")
    }

    /// Non-MLS bytes (an attribution, garbage) are refused rather than crashing
    /// the knock poll.
    func testProcessKnockRefusesNonMlsBytes() throws {
        let owner = try identity(11)
        let group = try Group.create(identity: owner, kind: .group)
        for bytes in [Data(), Data([0, 1, 0, 1]), Data("attribution".utf8)] {
            XCTAssertThrowsError(try group.processKnock(identity: owner, bytes: bytes))
        }
    }

    // MARK: Point 1 — every mailbox poller is signed for the mailbox it reads

    /// The account and the device mailbox: the pseudonym the signer is named
    /// after IS the pseudonym the poller fetches, and the fetch is signed.
    func testEachMailboxPollerIsSignedForTheMailboxItFetches() async throws {
        for byte: UInt8 in [4, 5] {
            let id = try identity(byte)
            let signer = dsMailboxSigner(identity: id)
            let expected = try IdentityPseudonym.fromIdentity(pubkey: id.signaturePublicKey()).asBytes()
            XCTAssertEqual(signer.mailboxPseudonym(), expected)

            let scope = DsAccessURLProtocol.freshScope()
            let client = ContactsClient(ds: DsAccessURLProtocol.client(scope))
            _ = try await client.pollStrangerInbox(
                mySignaturePubkey: id.signaturePublicKey(),
                myX25519Secret: id.x25519SecretKey(),
                mailboxSigner: signer)
            let seen = try XCTUnwrap(DsAccessURLProtocol.requests(scope).first)
            XCTAssertEqual(
                seen.url.path, "/native/ds/ds/mailbox/\(DsClient.pseudonymHex(expected))/messages")
            XCTAssertNotNil(seen.proof, "a claimed mailbox is read with a proof")
        }
    }

    /// The mailbox claim body names the key the mailbox is named after.
    func testTheMailboxClaimGoesToTheSignersOwnMailbox() async throws {
        let id = try identity(6)
        let signer = dsMailboxSigner(identity: id)
        let scope = DsAccessURLProtocol.freshScope()
        DsAccessURLProtocol.script(scope, [.init(status: 204, body: Data())])
        try await DsAccessURLProtocol.client(scope).claimMailbox(
            pseudonym: signer.mailboxPseudonym(), bodyJson: signer.mailboxClaimBodyJson())
        let seen = try XCTUnwrap(DsAccessURLProtocol.requests(scope).first)
        XCTAssertEqual(seen.method, "PUT")
        XCTAssertEqual(
            seen.url.path,
            "/native/ds/ds/mailbox/\(DsClient.pseudonymHex(signer.mailboxPseudonym()))/access")
        XCTAssertNil(seen.proof, "a mailbox claim is unsigned")
        XCTAssertEqual(seen.body, Data(signer.mailboxClaimBodyJson().utf8))
    }

    /// The real FFI signer's header names its epoch, so the server can pick the
    /// registered key it verifies against.
    func testTheRealGroupSignerProvesItsEpoch() throws {
        let owner = try identity(7)
        let group = try Group.create(identity: owner, kind: .group)
        let signer = try group.dsAccessSigner(identity: owner)
        XCTAssertEqual(signer.epoch(), try group.epoch())
        let header = signer.sign(method: "GET", path: "/ds/group/ab/messages", query: "", body: Data())
        XCTAssertTrue(header.hasPrefix("v1.\(signer.epoch())."), header)
        XCTAssertNil(try group.dsStagedAccessSigner(identity: owner), "nothing staged")
    }
}
