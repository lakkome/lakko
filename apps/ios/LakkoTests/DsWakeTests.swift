// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// MARK: - Stubs

/// A DS stand-in that records every request's method, path and `x-lakko-wake`
/// header, keyed on the request host so fixtures cannot see each other's
/// traffic. A publish is answered with a fresh id, a read with an empty page,
/// an access registration with `204`.
final class WakeRecordingDsURLProtocol: URLProtocol {
    struct Seen {
        let method: String
        let path: String
        let wake: String?
    }

    private static let lock = NSLock()
    private static var seen: [String: [Seen]] = [:]
    private static var nextId: Int64 = 1

    static func freshScope() -> String {
        "w\(UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()).test"
    }

    static func client(_ scope: String) -> DsClient {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [WakeRecordingDsURLProtocol.self]
        return DsClient(baseURL: "http://\(scope):9", session: URLSession(configuration: config))
    }

    static func requests(_ scope: String) -> [Seen] {
        lock.lock(); defer { lock.unlock() }
        return seen[scope] ?? []
    }

    /// The group-journal publishes only.
    static func publishes(_ scope: String) -> [Seen] {
        requests(scope).filter {
            $0.method == "POST" && $0.path.hasPrefix("/ds/group/") && $0.path.hasSuffix("/messages")
        }
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url else {
            client?.urlProtocol(self, didFailWithError: URLError(.badURL))
            return
        }
        let scope = url.host?.lowercased() ?? "?"
        let method = request.httpMethod ?? "GET"
        Self.lock.lock()
        Self.seen[scope, default: []].append(Seen(
            method: method,
            path: url.path,
            wake: request.value(forHTTPHeaderField: DsWake.headerName)))
        let id = Self.nextId
        Self.nextId += 1
        Self.lock.unlock()

        let status: Int
        let body: Data
        if url.path.hasSuffix("/access") {
            status = 204
            body = Data()
        } else if method == "POST" {
            status = 201
            body = Data(#"{"id":\#(id),"created_at":"1970-01-01T00:00:00Z"}"#.utf8)
        } else {
            status = 200
            body = Data(#"{"messages":[],"has_more":false}"#.utf8)
        }
        let response = HTTPURLResponse(
            url: url, statusCode: status, httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

/// A process-wide tripwire for any push-gw request. Registered on the shared
/// URL loading system (what ``PushClient`` uses by default) for the duration of
/// one test; it answers every `/push/` request with a `500` and remembers it.
final class PushGwTripwireURLProtocol: URLProtocol {
    private static let lock = NSLock()
    private static var hits: [String] = []

    static func reset() {
        lock.lock(); defer { lock.unlock() }
        hits.removeAll()
    }

    static func seen() -> [String] {
        lock.lock(); defer { lock.unlock() }
        return hits
    }

    override class func canInit(with request: URLRequest) -> Bool {
        request.url?.path.contains("/push/") ?? false
    }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        Self.lock.lock()
        Self.hits.append(request.url?.path ?? "?")
        Self.lock.unlock()
        let response = HTTPURLResponse(
            url: request.url ?? URL(string: "http://tripwire.test")!,
            statusCode: 500, httpVersion: "HTTP/1.1", headerFields: [:])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data())
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

// MARK: - Tests

/// ADR-0054 decision 1 — the group wake is a header on the signed DS publish.
///
/// The header is `x-lakko-wake: <push_gid base64url, no padding>;<kind>`; the
/// DS refuses a malformed one with `400`, which would fail the publish itself,
/// so the value is built in one place (``DsWake``) and pinned here byte for
/// byte. The send path then carries it on the publish and makes no request to
/// push-gw at all.
@MainActor
final class DsWakeTests: XCTestCase {

    // MARK: The header helper

    /// A fixed id covering every base64url alphabet position that differs from
    /// standard base64 (`+` -> `-`, `/` -> `_`) and a length whose standard
    /// encoding ends in one `=`, which must be stripped.
    func testTheHeaderValueIsUnpaddedBase64UrlThenTheKind() throws {
        var gid = Data((0..<30).map { UInt8($0) })
        gid.append(contentsOf: [0xFB, 0xFF])
        let wake = try XCTUnwrap(DsWake(pushGid: gid, kind: .message))
        XCTAssertEqual(
            wake.headerValue,
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwd-_8;message")
        let call = try XCTUnwrap(DsWake(pushGid: gid, kind: .call))
        XCTAssertEqual(
            call.headerValue,
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwd-_8;call")
    }

    /// 32 bytes is always 43 unpadded characters, and nothing outside the
    /// base64url alphabet ever appears, for any id.
    func testEveryIdEncodesToFortyThreeUrlSafeCharacters() throws {
        let allowed = Set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_")
        for fill: UInt8 in [0x00, 0x3E, 0x3F, 0x7F, 0xF8, 0xFB, 0xFF] {
            let wake = try XCTUnwrap(DsWake(pushGid: Data(repeating: fill, count: 32), kind: .message))
            let parts = wake.headerValue.split(separator: ";", omittingEmptySubsequences: false)
            XCTAssertEqual(parts.count, 2)
            XCTAssertEqual(parts[0].count, 43, "fill \(fill)")
            XCTAssertTrue(parts[0].allSatisfy { allowed.contains($0) }, "fill \(fill)")
            XCTAssertEqual(parts[1], "message")
        }
    }

    /// A wrong-length id can never become a header: the DS would refuse the
    /// whole publish, not only the wake.
    func testAWrongLengthIdIsRefused() {
        XCTAssertNil(DsWake(pushGid: Data(), kind: .message))
        XCTAssertNil(DsWake(pushGid: Data(repeating: 1, count: 31), kind: .message))
        XCTAssertNil(DsWake(pushGid: Data(repeating: 1, count: 33), kind: .call))
    }

    func testTheWireSpellingsAreTheServers() {
        XCTAssertEqual(DsWake.headerName, "x-lakko-wake")
        XCTAssertEqual(DsWake.Kind.message.rawValue, "message")
        XCTAssertEqual(DsWake.Kind.call.rawValue, "call")
    }

    // MARK: The client

    func testAGroupPublishCarriesTheWakeHeaderOnlyWhenAsked() async throws {
        let scope = WakeRecordingDsURLProtocol.freshScope()
        let ds = WakeRecordingDsURLProtocol.client(scope)
        let wake = try XCTUnwrap(DsWake(pushGid: Data(repeating: 0xAB, count: 32), kind: .call))

        _ = try await ds.publishGroup(groupId: Data([0x01, 0x02]), payload: Data([0x09]), wake: wake)
        _ = try await ds.publishGroup(groupId: Data([0x01, 0x02]), payload: Data([0x09]))

        let seen = WakeRecordingDsURLProtocol.publishes(scope)
        XCTAssertEqual(seen.count, 2)
        XCTAssertEqual(seen[0].wake, wake.headerValue)
        XCTAssertNil(seen[1].wake, "no wake asked for, no header")
    }

    /// A mailbox publish has no wake parameter at all.
    func testAMailboxPublishNeverCarriesAWake() async throws {
        let scope = WakeRecordingDsURLProtocol.freshScope()
        let ds = WakeRecordingDsURLProtocol.client(scope)
        _ = try await ds.publishMailbox(pseudonym: Data(repeating: 7, count: 32), payload: Data([1]))
        let seen = try XCTUnwrap(WakeRecordingDsURLProtocol.requests(scope).first)
        XCTAssertNil(seen.wake)
    }

    // MARK: The send path

    /// The whole send path through the repository: a chat message carries a
    /// `message` wake named by the group's `push_gid`, a call offer a `call`
    /// wake, a control envelope none — and nothing reaches push-gw.
    func testTheSendPathWakesThroughThePublishAndNeverCallsPushGw() async throws {
        PushGwTripwireURLProtocol.reset()
        URLProtocol.registerClass(PushGwTripwireURLProtocol.self)
        defer { URLProtocol.unregisterClass(PushGwTripwireURLProtocol.self) }

        let f = try makeOwnerRepo()
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }
        let expected = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: f.secret))

        try await f.repo.sendMessage(groupIdHex: f.groupIdHex, plaintext: "hello")
        try await f.repo.publishApplicationMessage(
            groupIdHex: f.groupIdHex,
            plaintextBytes: try composeTypingEnvelope(isTyping: true))
        try await f.repo.publishApplicationMessage(
            groupIdHex: f.groupIdHex,
            plaintextBytes: Data([0x52, 0x00]),
            wake: .call)

        let wakes = WakeRecordingDsURLProtocol.publishes(f.dsScope).map(\.wake)
        XCTAssertEqual(wakes, [
            try XCTUnwrap(DsWake(pushGid: expected, kind: .message)).headerValue,
            nil,
            try XCTUnwrap(DsWake(pushGid: expected, kind: .call)).headerValue,
        ])
        XCTAssertEqual(
            PushGwTripwireURLProtocol.seen(), [],
            "ADR-0054 — the send path must not make a push-gw request of its own")
    }

    /// ADR-0042 defer: with no routing secret for the group, the answer is no
    /// wake at all, never a wake named by the group id.
    func testAGroupWithNoRoutingSecretGetsNoWake() throws {
        let f = try makeOwnerRepo()
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }
        XCTAssertNotNil(f.repo.pushWake(groupIdHex: f.groupIdHex, kind: .message))
        XCTAssertNil(f.repo.pushWake(
            groupIdHex: DsClient.pseudonymHex(Data(repeating: 0x5A, count: 32)), kind: .message))
    }

    // MARK: - Fixture

    private struct OwnerFixture {
        let repo: ChatRepository
        let groupIdHex: String
        let secret: Data
        let outboxDir: URL
        let dsScope: String
    }

    /// An owner device whose group already holds a distributed routing secret,
    /// so the bootstrap's ADR-0042 provisioning only subscribes (stubbed) and
    /// never mints a second secret under the test.
    private func makeOwnerRepo() throws -> OwnerFixture {
        let dsScope = WakeRecordingDsURLProtocol.freshScope()
        let ownerSeed = Data(repeating: 0x31, count: 32)
        let owner = try Identity.fromSeed(seed: ownerSeed)
        let group = try Group.create(identity: owner)
        let groupIdHex = DsClient.pseudonymHex(try group.groupId())

        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            guard let base = $0.baseAddress else { return }
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, base)
        }
        let accountIdB64 = base64Encode(accountIdBytes)
        let suffix = UUID().uuidString

        let snapshotStore = IdentitySnapshotStore()
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: try owner.snapshotStorage())
        let knownGroupsStore = KnownGroupsStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "ds-wake-k-\(suffix)")))
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("ds-wake-\(suffix)")
        try FileManager.default.createDirectory(at: outboxDir, withIntermediateDirectories: true)

        let routingStore = PushRoutingStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "ds-wake-p-\(suffix)")))
        let secret = Data((0..<32).map { UInt8(0x40 + $0) })
        let pushGid = try XCTUnwrap(PushGroupRouting.derivePushGid(secret: secret))
        routingStore.putRouting(
            accountIdB64: accountIdB64,
            groupIdHex: groupIdHex,
            secret: secret,
            pushGidHex: DsClient.pseudonymHex(pushGid),
            distributed: true,
            mintEpoch: 0)

        let ds = WakeRecordingDsURLProtocol.client(dsScope)
        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            discovery: DiscoveryClient(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: outboxDir),
            pushRouting: PushGroupRouting(
                store: routingStore,
                subscribeUnderPushGid: { _ in },
                unsubscribeUnderPushGid: { _ in }),
            beaconStore: BeaconObservationStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "ds-wake-b-\(suffix)"))),
            pendingRequestStore: PendingRequestStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "ds-wake-r-\(suffix)"))),
            lastObservedOwnerStore: LastObservedOwnerStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "ds-wake-o-\(suffix)"))),
            leaveRequestStore: LeaveRequestStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "ds-wake-l-\(suffix)"))))
        repo.deviceIdentityInstaller = { _, _ in nil }
        repo.bootstrap(
            secrets: IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: Data(repeating: 0x22, count: 32),
                accountIdentitySeed: ownerSeed,
                sessionToken: "t",
                sessionExpiresAtMs: 0))

        return OwnerFixture(
            repo: repo, groupIdHex: groupIdHex, secret: secret,
            outboxDir: outboxDir, dsScope: dsScope)
    }
}
