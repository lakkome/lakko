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
        // Only journal PUBLISHES are measured. ADR-0057 ownership statements
        // (`PUT .../push-binding`, `.../registrars`) are fire-and-forget work
        // a repository from an EARLIER test can still have in flight, and they
        // are not what the serialization cases are about.
        guard path.hasSuffix("/messages") else {
            let url = request.url ?? URL(fileURLWithPath: "/")
            let response = HTTPURLResponse(
                url: url, statusCode: 204, httpVersion: "HTTP/1.1", headerFields: [:])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocolDidFinishLoading(self)
            return
        }
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

/// Two membership operations driven CONCURRENTLY at one `ChatRepository`.
///
/// Without a lock the sequence is: op A mutates the shared OpenMLS provider,
/// persists, and suspends at `await publishGroup`; `@MainActor` isolation is
/// released at that suspension, so op B runs its own create + persist and
/// publishes while A's request is still open. Two membership artifacts are then
/// in flight at once, and the DS assigns journal order by ARRIVAL, not by which
/// call started first.
///
/// ## What ADR-0045 changed about this suite, and what it did not
///
/// The vehicle changed. Every group `Group.create` produces carries an ADR-0022
/// role table, so add / remove / role-change no longer eager-merge a commit at
/// all — they publish a **proposal**, and the epoch-advancing commit is the
/// committer's sweep. The consequence the original assertions read (`epoch == 2`
/// after two role changes) is therefore no longer true, and asserting it would
/// be asserting the hazard this arc removed.
///
/// The property under test did NOT change: the region from "mutate the shared
/// provider" to "the artifact is on the wire" must be the sole in-flight section
/// for one group, must be keyed PER group, and must release on the throw path.
/// Those are what the three cases below assert, now over the artifacts the
/// paths actually publish. The epoch-advancing half of the hazard is covered by
/// `SendEpochStraddleTests`, whose oracle replays the recorded journal as a
/// recipient at `max_past_epochs = 0`.
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

    /// A founder identity carrying `groupCount` real MLS groups, each with a
    /// second real member, rehydrated into a repo whose outbox + snapshot
    /// persists genuinely SUCCEED — the membership paths must reach the publish,
    /// which is the region under test.
    ///
    /// The second member is what makes a Remove proposal available: a
    /// one-of-one group has no removable leaf, and ADR-0045 turned the only
    /// other verb (role change) into something that refuses a SECOND concurrent
    /// request by design, so two same-kind operations can no longer be used to
    /// probe the lock.
    private struct Fixture {
        let repo: ChatRepository
        let groupIdHexes: [String]
        let memberLeaves: [UInt32]
        let accountIdB64: String
        let snapshotStore: IdentitySnapshotStore
        let outboxDir: URL
        /// Two distinct valid 32-byte signature keys to promote, so back-to-back
        /// role operations are genuinely different operations rather than a repeat.
        let promotableKeys: [Data]
    }

    private func makeFounderRepo(groupCount: Int) throws -> Fixture {
        let seed = Data(repeating: 0x11, count: 32)
        let founder = try Identity.fromSeed(seed: seed)
        let other = try Identity.fromSeed(seed: Data(repeating: 0x55, count: 32))
        var hexes: [String] = []
        var memberLeaves: [UInt32] = []
        for _ in 0..<groupCount {
            let group = try Group.create(identity: founder, kind: .group)
            // C-100: the two people the role changes below name admin are
            // members too. One commit, so the fixture still leaves the group at
            // epoch 1, and `other` first, so its leaf is the first non-owner.
            _ = try group.addMembersFromWire(
                identity: founder,
                keyPackages: [
                    try other.createKeyPackage(),
                    try Identity.fromSeed(seed: Data(repeating: 0x33, count: 32)).createKeyPackage(),
                    try Identity.fromSeed(seed: Data(repeating: 0x44, count: 32)).createKeyPackage(),
                ]
            )
            hexes.append(DsClient.pseudonymHex(try group.groupId()))
            let ownerKey = try XCTUnwrap(try group.groupRoles()).owner
            memberLeaves.append(
                try XCTUnwrap(
                    try group.members().first { $0.signaturePublicKey != ownerKey }?.leafIndex))
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
            chatClient: Offline.chatClient(),
            dsClient: makeGatedDs(),
            discovery: Offline.discovery(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: outboxDir),
            pushRouting: Offline.pushRouting(),
            beaconStore: BeaconObservationStore(
                defaults: UserDefaults(suiteName: "commit-serialization-b-\(UUID().uuidString)")!),
            pendingRequestStore: PendingRequestStore(
                defaults: UserDefaults(suiteName: "commit-serialization-p-\(UUID().uuidString)")!)
        )
        // Phase 1 fixture: the chats run on the account identity the snapshot
        // was taken under (ADR-0050 device identities are exercised elsewhere).
        repo.deviceIdentityInstaller = { _, _ in nil }
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
                repo.memberCount(groupIdHex: hex), 4,
                "founder group \(hex) must rehydrate for the membership path to reach the publish"
            )
            XCTAssertTrue(
                repo.usesProposalPath(groupIdHex: hex),
                "every `Group.create` seeds an ADR-0022 role table, so ADR-0045 routes "
                + "membership on this group through proposals — if this ever became false "
                + "the cases below would silently be testing the direct-commit path instead"
            )
        }
        let promotableKeys = [
            try Identity.fromSeed(seed: Data(repeating: 0x33, count: 32)).signaturePublicKey(),
            try Identity.fromSeed(seed: Data(repeating: 0x44, count: 32)).signaturePublicKey(),
        ]
        return Fixture(
            repo: repo,
            groupIdHexes: hexes,
            memberLeaves: memberLeaves,
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

    // MARK: - (1) Same group: two membership publishes must NOT be in flight at once

    func testTwoConcurrentMembershipOpsOnOneGroupSerialize() async throws {
        let fx = try makeFounderRepo(groupCount: 1)
        defer { tearDown(fx) }
        let hex = fx.groupIdHexes[0]
        GatedPublishURLProtocol.reset()

        // Two DIFFERENT kinds, dispatched before either is awaited, so they are
        // genuinely concurrent from the repository's point of view — the shape a
        // double tap (or a re-created view model clearing `membershipBusy`)
        // produces. Two ROLE changes would no longer work as a probe: ADR-0045
        // refuses a second role request while one is pending, because RFC 9420
        // permits at most one `GroupContextExtensions` proposal per commit and
        // the sweep would silently discard one of them.
        async let first: MembershipOutcome = fx.repo.setMemberAdmin(
            groupIdHex: hex,
            memberSignaturePubkey: fx.promotableKeys[0],
            makeAdmin: true
        )
        async let second: MembershipOutcome = fx.repo.removeMemberFromGroup(
            groupIdHex: hex,
            leafIndex: fx.memberLeaves[0]
        )
        let (a, b) = try await (first, second)

        // THE ASSERTION: the two publishes were never simultaneously in flight,
        // so the DS cannot have received them out of creation order.
        XCTAssertEqual(
            GatedPublishURLProtocol.maxConcurrent(pathContaining: hex), 1,
            "two membership operations on ONE group must never be in flight at once — "
            + "the DS orders the journal by arrival, and the section from the shared "
            + "OpenMLS provider mutation to the completed publish is a critical section"
        )

        // And the consequence of that, stated directly: the DS saw them in
        // non-decreasing epoch order. Both are proposals, so both are framed at
        // the epoch the group is actually at — which is the point: a proposal
        // advances nothing, so there is no second epoch for the loser to be
        // stranded at.
        let epochs = GatedPublishURLProtocol.arrivalOrder
            .filter { $0.path.contains(hex) }
            .compactMap { epochOfMessage(bytes: $0.body) }
        XCTAssertEqual(epochs.count, 2, "both proposals must have reached the DS")
        XCTAssertEqual(
            epochs, epochs.sorted(),
            "artifacts must arrive at the DS in non-decreasing epoch order, got \(epochs)"
        )

        // Neither lost: serializing must not drop an intent, and both are now
        // waiting for a committer to sweep them.
        for outcome in [a, b] {
            guard case .proposed = outcome else {
                return XCTFail("ADR-0045 routes membership on a role-table group through proposals")
            }
        }
        XCTAssertEqual(
            fx.repo.pendingProposals(groupIdHex: hex).filter({ $0.stored }).count, 2,
            "serializing must not lose a request — both survive for the sweep"
        )
        XCTAssertEqual(
            fx.repo.epoch(groupIdHex: hex), 1,
            "a proposal advances NO epoch: the group is still where the fixture's add left it"
        )
    }

    // MARK: - (2) Different groups: the lock must be per-group, not global

    func testConcurrentMembershipOpsOnDifferentGroupsDoNotBlockEachOther() async throws {
        let fx = try makeFounderRepo(groupCount: 2)
        defer { tearDown(fx) }
        let (hexA, hexB) = (fx.groupIdHexes[0], fx.groupIdHexes[1])
        GatedPublishURLProtocol.reset()

        async let first: MembershipOutcome = fx.repo.setMemberAdmin(
            groupIdHex: hexA,
            memberSignaturePubkey: fx.promotableKeys[0],
            makeAdmin: true
        )
        async let second: MembershipOutcome = fx.repo.setMemberAdmin(
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
            "operations on DIFFERENT groups must run concurrently — the chain is keyed "
            + "per group, and a global lock would stall unrelated conversations"
        )
        XCTAssertEqual(GatedPublishURLProtocol.maxConcurrent(pathContaining: hexA), 1)
        XCTAssertEqual(GatedPublishURLProtocol.maxConcurrent(pathContaining: hexB), 1)
        XCTAssertEqual(fx.repo.pendingProposals(groupIdHex: hexA).count, 1)
        XCTAssertEqual(fx.repo.pendingProposals(groupIdHex: hexB).count, 1)
    }

    // MARK: - (3) A failing section must not wedge the group's chain

    /// The chain waits on a predecessor's completion, not its success. A section
    /// that throws must still release the group so the next one runs — otherwise
    /// one failed membership operation would silently disable membership
    /// management for that conversation for the rest of the session.
    func testAFailedMembershipOpDoesNotWedgeTheChain() async throws {
        let fx = try makeFounderRepo(groupCount: 1)
        defer { tearDown(fx) }
        let hex = fx.groupIdHexes[0]
        GatedPublishURLProtocol.reset()

        // A 3-byte key is not a valid Ed25519 signature key, so
        // `proposeGroupRoles` throws inside the locked section, before any
        // publish.
        do {
            _ = try await fx.repo.setMemberAdmin(
                groupIdHex: hex,
                memberSignaturePubkey: Data([0x01, 0x02, 0x03]),
                makeAdmin: true
            )
            XCTFail("a malformed signature key must fail the role request")
        } catch {
            // expected
        }

        // The next caller must still get through.
        let outcome = try await fx.repo.setMemberAdmin(
            groupIdHex: hex,
            memberSignaturePubkey: fx.promotableKeys[0],
            makeAdmin: true
        )
        guard case .proposed = outcome else {
            return XCTFail("the second request must reach the wire")
        }
        XCTAssertEqual(
            fx.repo.pendingProposals(groupIdHex: hex).filter({ $0.stored }).count, 1,
            "a throwing section must release the chain so the next request still lands"
        )
    }
}
