// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// MARK: - Gated DS publish interceptor

/// A `URLProtocol` that answers every DS publish with a valid `PublishResult`
/// but HOLDS the response open for a fixed window, and records — per endpoint
/// path — how many requests were in flight at once plus the order the payloads
/// arrived in.
///
/// The hold is what makes the ADR-0040 hazard observable. A membership commit
/// eager-merges epoch N -> N+1 and then suspends at `await publishGroup`; the
/// hold widens that suspension into a window a second commit can be driven
/// into. `maxConcurrent` then answers the only question that matters: were two
/// commits for one group ever simultaneously in flight to the DS, where the
/// journal order is decided by arrival rather than by creation?
///
/// **The hold must not block `startLoading`.** `URLSession` may invoke
/// `startLoading` on a shared internal queue, so sleeping inside it would
/// serialize the requests itself and report `maxConcurrent == 1` no matter what
/// the code under test does — a false pass that would make this whole suite
/// vacuous. The response is therefore scheduled onto a global concurrent queue
/// and `startLoading` returns immediately.
final class GatedPublishURLProtocol: URLProtocol {

    /// How long each publish response is held open.
    static let holdSeconds: TimeInterval = 0.30

    private static let lock = NSLock()
    private static var inFlight: [String: Int] = [:]
    private static var _maxConcurrent: [String: Int] = [:]
    private static var globalInFlight = 0
    private static var _globalMaxConcurrent = 0
    private static var _arrivalOrder: [(path: String, body: Data)] = []
    private static var nextId: Int64 = 1

    static func reset() {
        lock.lock(); defer { lock.unlock() }
        inFlight.removeAll()
        _maxConcurrent.removeAll()
        globalInFlight = 0
        _globalMaxConcurrent = 0
        _arrivalOrder.removeAll()
        nextId = 1
    }

    /// Peak simultaneous in-flight publishes for one endpoint path (which
    /// carries the group's hex id, so this is per group).
    static func maxConcurrent(pathContaining needle: String) -> Int {
        lock.lock(); defer { lock.unlock() }
        return _maxConcurrent
            .filter { $0.key.contains(needle) }
            .values
            .max() ?? 0
    }

    /// Peak simultaneous in-flight publishes across ALL endpoints combined.
    ///
    /// Deliberately a separate counter, not `_maxConcurrent.values.max()`: that
    /// reads the largest PER-PATH peak, which stays 1 even when two different
    /// groups overlap perfectly, and would have made the per-group test below
    /// unfalsifiable.
    static var maxConcurrentOverall: Int {
        lock.lock(); defer { lock.unlock() }
        return _globalMaxConcurrent
    }

    /// Payload bytes in the order the DS received them.
    static var arrivalOrder: [(path: String, body: Data)] {
        lock.lock(); defer { lock.unlock() }
        return _arrivalOrder
    }

    private static func noteEnter(path: String, body: Data) -> Int64 {
        lock.lock(); defer { lock.unlock() }
        let now = (inFlight[path] ?? 0) + 1
        inFlight[path] = now
        _maxConcurrent[path] = max(_maxConcurrent[path] ?? 0, now)
        globalInFlight += 1
        _globalMaxConcurrent = max(_globalMaxConcurrent, globalInFlight)
        _arrivalOrder.append((path: path, body: body))
        let id = nextId
        nextId += 1
        return id
    }

    private static func noteExit(path: String) {
        lock.lock(); defer { lock.unlock() }
        inFlight[path] = max((inFlight[path] ?? 1) - 1, 0)
        globalInFlight = max(globalInFlight - 1, 0)
    }

    /// `URLProtocol` is handed the request with the body already converted to a
    /// stream in most configurations, so read whichever form is populated.
    private static func bodyBytes(of request: URLRequest) -> Data {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { return Data() }
        stream.open()
        defer { stream.close() }
        var out = Data()
        var buf = [UInt8](repeating: 0, count: 4096)
        while stream.hasBytesAvailable {
            let n = stream.read(&buf, maxLength: buf.count)
            if n <= 0 { break }
            out.append(contentsOf: buf[0..<n])
        }
        return out
    }

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let path = request.url?.path ?? ""
        let body = Self.bodyBytes(of: request)
        let id = Self.noteEnter(path: path, body: body)
        let url = request.url ?? URL(fileURLWithPath: "/")
        // Scheduled, never slept inline — see the type doc.
        DispatchQueue.global().asyncAfter(deadline: .now() + Self.holdSeconds) { [weak self] in
            guard let self else { return }
            Self.noteExit(path: path)
            let json = #"{"id":\#(id),"created_at":"1970-01-01T00:00:00Z"}"#
            let response = HTTPURLResponse(
                url: url,
                statusCode: 200,
                httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"]
            )!
            self.client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            self.client?.urlProtocol(self, didLoad: Data(json.utf8))
            self.client?.urlProtocolDidFinishLoading(self)
        }
    }

    override func stopLoading() {}
}

// MARK: - ADR-0040 (G-35) per-group commit serialization

/// Two membership commits driven CONCURRENTLY at one `ChatRepository`.
///
/// Without a lock the sequence is: op A eager-merges epoch N -> N+1, persists,
/// and suspends at `await publishGroup`; `@MainActor` isolation is released at
/// that suspension, so op B runs its own create + persist and publishes while
/// A's request is still open. Two commits are then in flight at once, and the
/// DS assigns journal order by ARRIVAL, not by which call started first — land
/// them inverted and peers still at N drop the N+1 commit as wrong-epoch, apply
/// the N one, and settle at N+1 while this device sits at N+2. That is the
/// "device-ahead" outcome ADR-0040 exists to prevent, and the pending-commit
/// outbox cannot recover it: both publishes succeeded, so both entries are
/// acked and the restart sweep has nothing to re-publish.
@MainActor
final class CommitSerializationTests: XCTestCase {

    private func makeGatedDs() -> DsClient {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [GatedPublishURLProtocol.self]
        return DsClient(
            baseURL: "http://127.0.0.1:9",
            session: URLSession(configuration: config)
        )
    }

    /// A founder identity carrying `groupCount` real MLS groups, rehydrated into
    /// a repo whose outbox + snapshot persists genuinely SUCCEED — the commit
    /// paths must reach the publish, which is the region under test.
    private struct Fixture {
        let repo: ChatRepository
        let groupIdHexes: [String]
        let accountIdB64: String
        let snapshotStore: IdentitySnapshotStore
        let outboxDir: URL
        /// Two distinct valid 32-byte signature keys to promote, so back-to-back
        /// role commits are genuinely different operations rather than a repeat.
        let promotableKeys: [Data]
    }

    private func makeFounderRepo(groupCount: Int) throws -> Fixture {
        let seed = Data(repeating: 0x11, count: 32)
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
            defaults: UserDefaults(suiteName: "commit-serialization-\(UUID().uuidString)")!
        )
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshotBlob)
        for hex in hexes { knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: hex) }

        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("commit-serialization-\(UUID().uuidString)")

        let repo = ChatRepository(
            chatClient: ChatClient(),
            dsClient: makeGatedDs(),
            discovery: DiscoveryClient(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: outboxDir)
        )
        repo.bootstrap(
            secrets: IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: Data(repeating: 0x22, count: 32),
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
            try Identity.fromSeed(seed: Data(repeating: 0x33, count: 32)).signaturePublicKey(),
            try Identity.fromSeed(seed: Data(repeating: 0x44, count: 32)).signaturePublicKey(),
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

    // MARK: - (1) Same group: two commits must NOT be in flight at once

    func testTwoConcurrentCommitsOnOneGroupSerialize() async throws {
        let fx = try makeFounderRepo(groupCount: 1)
        defer { tearDown(fx) }
        let hex = fx.groupIdHexes[0]
        GatedPublishURLProtocol.reset()

        // Both ops are dispatched before either is awaited, so they are genuinely
        // concurrent from the repository's point of view — exactly the shape a
        // double tap (or a re-created view model clearing `membershipBusy`)
        // produces.
        async let first: Int64 = fx.repo.setMemberAdmin(
            groupIdHex: hex,
            memberSignaturePubkey: fx.promotableKeys[0],
            makeAdmin: true
        )
        async let second: Int64 = fx.repo.setMemberAdmin(
            groupIdHex: hex,
            memberSignaturePubkey: fx.promotableKeys[1],
            makeAdmin: true
        )
        _ = try await (first, second)

        // THE ASSERTION: the two commits were never simultaneously in flight, so
        // the DS cannot have received them out of creation order.
        XCTAssertEqual(
            GatedPublishURLProtocol.maxConcurrent(pathContaining: hex), 1,
            "two membership commits on ONE group must never be in flight at once — "
            + "the DS orders the journal by arrival, so an overlap lets commit(N+1 -> N+2) "
            + "land before commit(N -> N+1) and strand the group at 'device ahead'"
        )

        // And the consequence of that, stated directly: the DS saw the commits in
        // strictly increasing epoch order.
        let epochs = GatedPublishURLProtocol.arrivalOrder
            .filter { $0.path.contains(hex) }
            .compactMap { epochOfMessage(bytes: $0.body) }
        XCTAssertEqual(epochs.count, 2, "both commits must have reached the DS")
        XCTAssertEqual(
            epochs, epochs.sorted(),
            "commits must arrive at the DS in increasing epoch order, got \(epochs)"
        )

        // Both landed, and the group advanced exactly twice (0 -> 1 -> 2).
        XCTAssertEqual(
            fx.repo.epoch(groupIdHex: hex), 2,
            "serializing must not lose a commit — both operations still apply"
        )
    }

    // MARK: - (2) Different groups: the lock must be per-group, not global

    func testConcurrentCommitsOnDifferentGroupsDoNotBlockEachOther() async throws {
        let fx = try makeFounderRepo(groupCount: 2)
        defer { tearDown(fx) }
        let (hexA, hexB) = (fx.groupIdHexes[0], fx.groupIdHexes[1])
        GatedPublishURLProtocol.reset()

        async let first: Int64 = fx.repo.setMemberAdmin(
            groupIdHex: hexA,
            memberSignaturePubkey: fx.promotableKeys[0],
            makeAdmin: true
        )
        async let second: Int64 = fx.repo.setMemberAdmin(
            groupIdHex: hexB,
            memberSignaturePubkey: fx.promotableKeys[0],
            makeAdmin: true
        )
        _ = try await (first, second)

        // THE ASSERTION: two DIFFERENT conversations overlap. A global mutex
        // would pass test (1) while silently making every group wait behind every
        // other group's network round-trip; only this catches that.
        XCTAssertEqual(
            GatedPublishURLProtocol.maxConcurrentOverall, 2,
            "commits on DIFFERENT groups must run concurrently — the chain is keyed "
            + "per group, and a global lock would stall unrelated conversations"
        )
        XCTAssertEqual(GatedPublishURLProtocol.maxConcurrent(pathContaining: hexA), 1)
        XCTAssertEqual(GatedPublishURLProtocol.maxConcurrent(pathContaining: hexB), 1)
        XCTAssertEqual(fx.repo.epoch(groupIdHex: hexA), 1)
        XCTAssertEqual(fx.repo.epoch(groupIdHex: hexB), 1)
    }

    // MARK: - (3) A failing section must not wedge the group's chain

    /// The chain waits on a predecessor's completion, not its success. A section
    /// that throws must still release the group so the next one runs — otherwise
    /// one failed membership operation would silently disable membership
    /// management for that conversation for the rest of the session.
    func testAFailedCommitDoesNotWedgeTheChain() async throws {
        let fx = try makeFounderRepo(groupCount: 1)
        defer { tearDown(fx) }
        let hex = fx.groupIdHexes[0]
        GatedPublishURLProtocol.reset()

        // A 3-byte key is not a valid Ed25519 signature key, so `setGroupRoles`
        // throws inside the locked section, before any publish.
        do {
            _ = try await fx.repo.setMemberAdmin(
                groupIdHex: hex,
                memberSignaturePubkey: Data([0x01, 0x02, 0x03]),
                makeAdmin: true
            )
            XCTFail("a malformed signature key must fail the role commit")
        } catch {
            // expected
        }

        // The next caller must still get through.
        _ = try await fx.repo.setMemberAdmin(
            groupIdHex: hex,
            memberSignaturePubkey: fx.promotableKeys[0],
            makeAdmin: true
        )
        XCTAssertEqual(
            fx.repo.epoch(groupIdHex: hex), 1,
            "a throwing section must release the chain so the next commit still applies"
        )
    }
}
