// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// MARK: - Scripted DS interceptor

/// A `URLProtocol` that separates the two halves of a poll so their ordering
/// against a membership commit is observable.
///
///   * every **POST** (a DS publish) is answered with a valid `PublishResult`
///     but held open for ``publishHoldSeconds``, widening a commit's
///     `await dsClient.publishGroup` suspension into a window a poll can be
///     driven into;
///   * every **GET** (a DS journal fetch) is answered from ``fetchScript``,
///     keyed on the request's `wait` query value, after that entry's own delay
///     — so one test can hold one poll's fetch while another's returns
///     immediately.
///
/// Both halves append to a shared ordered ``log``, which the tests assert on.
/// Order, not wall-clock duration, is the assertion; the holds only exist to
/// give that order a margin far larger than scheduler jitter.
///
/// **The hold must not block `startLoading`.** `URLSession` may invoke
/// `startLoading` on a shared internal queue, so sleeping inline would
/// serialize the requests itself and make every ordering assertion vacuously
/// true regardless of what the code under test does. Responses are therefore
/// scheduled onto a global concurrent queue and `startLoading` returns at once.
final class SplitPollURLProtocol: URLProtocol {

    /// One scripted GET answer.
    struct FetchScript {
        /// How long the fetch is held before the rows are delivered.
        let delay: TimeInterval
        /// DS row ids to return. Payloads are junk on purpose: the apply loop
        /// advances its cursor to `message.id` BEFORE attempting to decrypt, so
        /// an undecryptable row still moves the cursor — which is exactly the
        /// state a cursor-race guard has to reason about, and it needs no real
        /// MLS ciphertext to reproduce.
        let rowIds: [Int64]
    }

    /// Per-request hold on every publish.
    static var publishHoldSeconds: TimeInterval = 0.30
    /// GET answers keyed on the request's `wait` query value.
    static var fetchScript: [Int: FetchScript] = [:]

    private static let lock = NSLock()
    private static var _log: [String] = []
    private static var _publishesEntered = 0
    private static var _inFlight = 0
    private static var _lastEventAt = Date.distantPast
    private static var nextId: Int64 = 1

    static func reset() {
        lock.lock(); defer { lock.unlock() }
        _log.removeAll()
        _publishesEntered = 0
        _inFlight = 0
        _lastEventAt = Date.distantPast
        nextId = 1
        publishHoldSeconds = 0.30
        fetchScript = [:]
    }

    /// Drop everything recorded so far, keeping the scripts — used after a
    /// warm-up poll so the assertions see only the scenario under test.
    static func resetLog() {
        lock.lock(); defer { lock.unlock() }
        _log.removeAll()
    }

    /// Append an event. Also called from the tests themselves, so a task's
    /// completion lands in the same ordered sequence as the DS traffic.
    static func note(_ label: String) {
        lock.lock(); defer { lock.unlock() }
        _log.append(label)
        _lastEventAt = Date()
    }

    static var log: [String] {
        lock.lock(); defer { lock.unlock() }
        return _log
    }

    /// Index of `label` in the log, or `nil` if it never happened.
    static func index(of label: String) -> Int? {
        log.firstIndex(of: label)
    }

    /// Suspend until at least `count` publishes have reached the DS, so a test
    /// can start a poll knowing a commit is genuinely parked mid-publish.
    /// Without this the poll could win the lock first and the test would prove
    /// nothing.
    static func awaitPublishEntered(count: Int, timeout: TimeInterval = 5) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if publishesEntered >= count { return }
            try await Task.sleep(nanoseconds: 2_000_000)
        }
        XCTFail("timed out waiting for \(count) publish(es) to reach the DS")
    }

    /// Locked accessors, kept non-`async` so the `NSLock` is never held across
    /// a suspension point (which Swift 6 rejects outright).
    private static var publishesEntered: Int {
        lock.lock(); defer { lock.unlock() }
        return _publishesEntered
    }

    private static var isQuiet: (busy: Int, idle: TimeInterval) {
        lock.lock(); defer { lock.unlock() }
        return (_inFlight, Date().timeIntervalSince(_lastEventAt))
    }

    /// Suspend until no request has been in flight, and none has started or
    /// finished, for `idleFor`.
    ///
    /// `ChatRepository.bootstrap` fires ADR-0042 push-routing provisioning per
    /// restored group, and that broadcast publishes through the SAME intercepted
    /// client as a membership commit and to the same path. Draining it before
    /// the scenario is what keeps a stray `publish-exit` out of the middle of
    /// the ordering under test — the alternative, matching publishes by body,
    /// would not separate them anyway (both are MLS frames on the group
    /// journal).
    static func awaitQuiescent(
        idleFor: TimeInterval = 0.20,
        timeout: TimeInterval = 8
    ) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            let (busy, idle) = isQuiet
            if busy == 0 && idle >= idleFor { return }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        XCTFail("timed out waiting for the DS interceptor to go quiet")
    }

    private static func noteEnter(_ label: String) {
        lock.lock(); defer { lock.unlock() }
        _log.append(label)
        _inFlight += 1
        _lastEventAt = Date()
    }

    private static func noteExit(_ label: String) {
        lock.lock(); defer { lock.unlock() }
        _log.append(label)
        _inFlight = max(_inFlight - 1, 0)
        _lastEventAt = Date()
    }

    private static func notePublishEnter() -> Int64 {
        noteEnter("publish-enter")
        lock.lock(); defer { lock.unlock() }
        _publishesEntered += 1
        let id = nextId
        nextId += 1
        return id
    }

    private static func waitQueryValue(_ request: URLRequest) -> Int {
        guard let url = request.url,
              let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems,
              let raw = items.first(where: { $0.name == "wait" })?.value,
              let value = Int(raw) else { return 0 }
        return value
    }

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let url = request.url ?? URL(fileURLWithPath: "/")
        let isPublish = (request.httpMethod ?? "GET").uppercased() == "POST"
        if isPublish {
            let id = Self.notePublishEnter()
            respond(after: Self.publishHoldSeconds, url: url, exitLabel: "publish-exit") {
                Data(#"{"id":\#(id),"created_at":"1970-01-01T00:00:00Z"}"#.utf8)
            }
            return
        }
        Self.noteEnter("fetch-enter")
        let script = Self.fetchScript[Self.waitQueryValue(request)]
            ?? FetchScript(delay: 0, rowIds: [])
        respond(after: script.delay, url: url, exitLabel: "fetch-exit") {
            let rows = script.rowIds.map { id in
                #"{"id":\#(id),"payload_b64":"AAEC","created_at":"1970-01-01T00:00:00Z"}"#
            }
            return Data(#"{"messages":[\#(rows.joined(separator: ","))],"has_more":false}"#.utf8)
        }
    }

    private func respond(
        after delay: TimeInterval,
        url: URL,
        exitLabel: String,
        body: @escaping () -> Data
    ) {
        DispatchQueue.global().asyncAfter(deadline: .now() + delay) { [weak self] in
            guard let self else { return }
            Self.noteExit(exitLabel)
            let response = HTTPURLResponse(
                url: url,
                statusCode: 200,
                httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"]
            )!
            self.client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            self.client?.urlProtocol(self, didLoad: body())
            self.client?.urlProtocolDidFinishLoading(self)
        }
    }

    override func stopLoading() {}
}

// MARK: - G-38 residual 1 (local sub-case): poll apply vs local commit

/// The poll APPLIES incoming commits — `processIncomingVerifyingRoles` mutates
/// the shared OpenMLS provider through the same `Group` handle a membership
/// commit is mid-operation on — so it is not a read-only observer and must not
/// run beside a local commit.
///
/// `@MainActor` isolation does not provide that on its own, and the reason is
/// the whole point of these tests: the apply is fully SYNCHRONOUS, but the
/// COMMIT holds its create-through-publish region across
/// `await dsClient.publishGroup`, and isolation is RELEASED at that suspension.
/// A synchronous apply therefore lands squarely inside the window where the
/// group has eager-merged N -> N+1 but the commit is not yet on the wire.
///
/// Scope, stated honestly: this closes the LOCAL interleave. A poll applying a
/// REMOTE commit while a local one is in flight to the DS is a genuine MLS
/// epoch fork no client-side lock can address (it needs a DS-side epoch gate or
/// proposal-based commits) — G-38 residual 1, deliberately untouched.
@MainActor
final class SplitPollSerializationTests: XCTestCase {

    private func makeScriptedDs() -> DsClient {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [SplitPollURLProtocol.self]
        return DsClient(
            baseURL: "http://127.0.0.1:9",
            session: URLSession(configuration: config)
        )
    }

    private struct Fixture {
        let repo: ChatRepository
        let groupIdHexes: [String]
        let accountIdB64: String
        let snapshotStore: IdentitySnapshotStore
        let outboxDir: URL
        let promotableKeys: [Data]
    }

    private func makeFounderRepo(groupCount: Int) throws -> Fixture {
        let seed = Data(repeating: 0x51, count: 32)
        let founder = try Identity.fromSeed(seed: seed)
        var hexes: [String] = []
        for _ in 0..<groupCount {
            let group = try Group.create(identity: founder)
            hexes.append(DsClient.pseudonymHex(try group.groupId()))
        }
        let snapshotBlob = try founder.snapshotStorage()

        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let accountIdB64 = base64Encode(accountIdBytes)

        let snapshotStore = IdentitySnapshotStore()
        let knownGroupsStore = KnownGroupsStore(
            defaults: UserDefaults(suiteName: "split-poll-\(UUID().uuidString)")!
        )
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshotBlob)
        for hex in hexes { knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: hex) }

        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("split-poll-\(UUID().uuidString)")

        // BOTH DS clients must be intercepted. `ChatClient` owns its own
        // `DsClient` and the repository owns another; the poll's FETCH goes
        // through the ChatClient's while every publish goes through the
        // repository's, so default-constructing either one silently sends that
        // half to a real socket and the ordering under test never happens.
        let ds = makeScriptedDs()
        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            discovery: DiscoveryClient(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: outboxDir)
        )
        repo.bootstrap(
            secrets: IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: Data(repeating: 0x52, count: 32),
                accountIdentitySeed: seed,
                sessionToken: "t",
                sessionExpiresAtMs: 0
            )
        )
        for hex in hexes {
            XCTAssertEqual(
                repo.memberCount(groupIdHex: hex), 1,
                "founder group \(hex) must rehydrate for the commit path to reach the publish"
            )
        }
        let promotableKeys = [
            try Identity.fromSeed(seed: Data(repeating: 0x53, count: 32)).signaturePublicKey(),
            try Identity.fromSeed(seed: Data(repeating: 0x54, count: 32)).signaturePublicKey(),
        ]
        return Fixture(
            repo: repo,
            groupIdHexes: hexes,
            accountIdB64: accountIdB64,
            snapshotStore: snapshotStore,
            outboxDir: outboxDir,
            promotableKeys: promotableKeys
        )
    }

    private func tearDown(_ fx: Fixture) {
        fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64)
        try? FileManager.default.removeItem(at: fx.outboxDir)
    }

    /// One throwaway poll per group so the ADR-0040 outbox sweep — which runs
    /// once per group per session, publishes on its own, and is deliberately
    /// OUTSIDE the lock — is already spent before the scenario under test. It
    /// would otherwise re-publish the scenario's own un-acked commit and put a
    /// second POST in the middle of the log.
    private func warmUp(_ fx: Fixture, groupIdHex: String, waitSecs: Int) async {
        try? await fx.repo.pollGroup(groupIdHex: groupIdHex, waitSecs: waitSecs)
    }

    /// Everything a scenario needs settled before it starts: the once-per-group
    /// outbox sweep spent, the bootstrap push-routing broadcast drained, and the
    /// log cleared so only the scenario's own traffic is in it.
    private func settle(_ fx: Fixture, groupIdHexes: [String], warmUpWait: Int = 0) async {
        for hex in groupIdHexes {
            await warmUp(fx, groupIdHex: hex, waitSecs: warmUpWait)
        }
        try? await SplitPollURLProtocol.awaitQuiescent()
        SplitPollURLProtocol.resetLog()
    }

    private func cursor(_ fx: Fixture, groupIdHex: String) -> Int64? {
        fx.repo.state.threads.first { $0.groupIdHex == groupIdHex }?.cursor
    }

    // MARK: - (1) Same group: the apply must wait for an in-flight commit

    func testPollApplyWaitsForAnInFlightCommitOnTheSameGroup() async throws {
        let fx = try makeFounderRepo(groupCount: 1)
        defer { tearDown(fx) }
        let hex = fx.groupIdHexes[0]
        SplitPollURLProtocol.reset()
        // Empty batch: the apply still runs in full, and an empty batch keeps
        // the cursor where it is, so this test measures ordering only.
        SplitPollURLProtocol.fetchScript = [0: .init(delay: 0, rowIds: [])]
        await settle(fx, groupIdHexes: [hex])

        let commitTask = Task { @MainActor in
            _ = try? await fx.repo.setMemberAdmin(
                groupIdHex: hex,
                memberSignaturePubkey: fx.promotableKeys[0],
                makeAdmin: true
            )
            SplitPollURLProtocol.note("commit-done")
        }
        // Only start polling once the commit is genuinely parked in its publish
        // — otherwise the poll could take the lock first and the ordering below
        // would hold for the wrong reason.
        try await SplitPollURLProtocol.awaitPublishEntered(count: 1)
        let pollTask = Task { @MainActor in
            try? await fx.repo.pollGroup(groupIdHex: hex)
            SplitPollURLProtocol.note("poll-done")
        }
        await commitTask.value
        await pollTask.value

        let log = SplitPollURLProtocol.log
        guard let publishExit = SplitPollURLProtocol.index(of: "publish-exit"),
              let pollDone = SplitPollURLProtocol.index(of: "poll-done") else {
            return XCTFail("both the publish and the poll must complete, log: \(log)")
        }
        // THE ASSERTION. The apply is synchronous, so unlocked it would finish
        // the instant its fetch returns — well inside the held publish window.
        // Landing after `publish-exit` is only possible if it waited for the
        // commit's chain.
        XCTAssertLessThan(
            publishExit, pollDone,
            "the poll's MLS apply must not run while a local commit is mid-publish on the "
            + "same group — the commit has eager-merged N -> N+1 but is not yet on the wire, "
            + "and the apply mutates the same OpenMLS provider through the same Group handle. "
            + "log: \(log)"
        )
        // The fetch, by contrast, must have completed inside that window: it is
        // deliberately OUTSIDE the lock.
        guard let fetchExit = SplitPollURLProtocol.index(of: "fetch-exit") else {
            return XCTFail("the poll must have fetched, log: \(log)")
        }
        XCTAssertLessThan(
            fetchExit, publishExit,
            "the DS fetch must run outside the lock — serializing a 30 s long poll behind a "
            + "commit publish is G-37's defect at a per-group blast radius. log: \(log)"
        )
        XCTAssertEqual(
            fx.repo.epoch(groupIdHex: hex), 1,
            "serializing must not lose the commit"
        )
    }

    // MARK: - (2) Different groups: the poll must not block on another group

    func testPollOnAnotherGroupIsNotBlockedByACommit() async throws {
        let fx = try makeFounderRepo(groupCount: 2)
        defer { tearDown(fx) }
        let (hexA, hexB) = (fx.groupIdHexes[0], fx.groupIdHexes[1])
        SplitPollURLProtocol.reset()
        SplitPollURLProtocol.fetchScript = [0: .init(delay: 0, rowIds: [])]
        await settle(fx, groupIdHexes: [hexA, hexB])

        let commitTask = Task { @MainActor in
            _ = try? await fx.repo.setMemberAdmin(
                groupIdHex: hexA,
                memberSignaturePubkey: fx.promotableKeys[0],
                makeAdmin: true
            )
            SplitPollURLProtocol.note("commit-done")
        }
        try await SplitPollURLProtocol.awaitPublishEntered(count: 1)
        let pollTask = Task { @MainActor in
            try? await fx.repo.pollGroup(groupIdHex: hexB)
            SplitPollURLProtocol.note("poll-done")
        }
        await commitTask.value
        await pollTask.value

        let log = SplitPollURLProtocol.log
        guard let publishExit = SplitPollURLProtocol.index(of: "publish-exit"),
              let pollDone = SplitPollURLProtocol.index(of: "poll-done") else {
            return XCTFail("both the publish and the poll must complete, log: \(log)")
        }
        // THE ASSERTION, and the exact inverse of test (1). A single global lock
        // would pass test (1) while silently making every conversation wait
        // behind every other conversation's DS round-trip; only this catches it.
        XCTAssertLessThan(
            pollDone, publishExit,
            "a poll of group B must complete while group A's commit is still in flight — "
            + "the chain is keyed per group. log: \(log)"
        )
    }

    // MARK: - (3) The fetch must stay outside the lock

    /// The inverse pairing of test (1): a poll parked on its DS fetch must not
    /// hold the group's chain, or a 30 s long poll on an open chat screen would
    /// stall every send and membership commit on that conversation — which is
    /// precisely the defect G-37 was about, re-created at per-group scope.
    func testACommitIsNotBlockedByAPollParkedOnItsFetch() async throws {
        let fx = try makeFounderRepo(groupCount: 1)
        defer { tearDown(fx) }
        let hex = fx.groupIdHexes[0]
        SplitPollURLProtocol.reset()
        SplitPollURLProtocol.publishHoldSeconds = 0.05
        SplitPollURLProtocol.fetchScript = [
            0: .init(delay: 0, rowIds: []),      // warm-up
            7: .init(delay: 0.60, rowIds: []),   // the parked long poll
        ]
        await settle(fx, groupIdHexes: [hex])

        let pollTask = Task { @MainActor in
            try? await fx.repo.pollGroup(groupIdHex: hex, waitSecs: 7)
            SplitPollURLProtocol.note("poll-done")
        }
        // Let the poll get as far as its fetch before the commit starts.
        try await Task.sleep(nanoseconds: 100_000_000)
        let commitTask = Task { @MainActor in
            _ = try? await fx.repo.setMemberAdmin(
                groupIdHex: hex,
                memberSignaturePubkey: fx.promotableKeys[0],
                makeAdmin: true
            )
            SplitPollURLProtocol.note("commit-done")
        }
        await commitTask.value
        await pollTask.value

        let log = SplitPollURLProtocol.log
        guard let commitDone = SplitPollURLProtocol.index(of: "commit-done"),
              let fetchExit = SplitPollURLProtocol.index(of: "fetch-exit") else {
            return XCTFail("both operations must complete, log: \(log)")
        }
        XCTAssertLessThan(
            commitDone, fetchExit,
            "a membership commit must complete while a long poll is still parked on its DS "
            + "fetch — only the apply half takes the lock. log: \(log)"
        )
    }

    // MARK: - (4) A stale batch must be dropped, never rewind the cursor

    /// Two polls of one group overlap; the slower one comes back holding rows
    /// that start behind the live cursor. Applying it would re-feed consumed
    /// rows to OpenMLS AND wind the record's cursor BACKWARD to that stale
    /// batch's end, so the next poll would re-download everything in between.
    func testAStaleBatchIsDroppedInsteadOfRewindingTheCursor() async throws {
        let fx = try makeFounderRepo(groupCount: 1)
        defer { tearDown(fx) }
        let hex = fx.groupIdHexes[0]
        SplitPollURLProtocol.reset()
        SplitPollURLProtocol.fetchScript = [
            1: .init(delay: 0, rowIds: []),          // warm-up, leaves cursor 0
            0: .init(delay: 0, rowIds: [1, 2, 3]),   // the fast poll: cursor -> 3
            5: .init(delay: 0.30, rowIds: [1]),      // the slow poll: stale, ends at 1
        ]
        await settle(fx, groupIdHexes: [hex], warmUpWait: 1)
        XCTAssertEqual(cursor(fx, groupIdHex: hex), 0, "warm-up must leave the cursor at 0")

        // Both read cursor 0 before either applies. The slow one is dispatched
        // first so it is genuinely the one holding the stale batch.
        let slow = Task { @MainActor in
            try? await fx.repo.pollGroup(groupIdHex: hex, waitSecs: 5)
        }
        try await Task.sleep(nanoseconds: 30_000_000)
        let fast = Task { @MainActor in
            try? await fx.repo.pollGroup(groupIdHex: hex, waitSecs: 0)
        }
        await fast.value
        XCTAssertEqual(
            cursor(fx, groupIdHex: hex), 3,
            "the fast poll must have applied its batch and advanced the cursor to 3"
        )
        await slow.value

        // THE ASSERTION: the stale batch was dropped whole. Applied, it would
        // have set the cursor to its own last row (1) and lost rows 2 and 3 from
        // this device's view until something else re-fetched them.
        XCTAssertEqual(
            cursor(fx, groupIdHex: hex), 3,
            "a batch fetched at a cursor that has since moved must be DROPPED, not applied — "
            + "applying it rewinds the cursor and re-feeds consumed rows to OpenMLS"
        )
    }
}
