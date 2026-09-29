// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// A discovery stand-in keyed on the request host. Answers
/// `/discovery/group-invite` per the scope's
/// scripted mode, recording each request's path and `Authorization` header.
final class GroupInviteDiscoveryURLProtocol: URLProtocol {
    enum Mode {
        case ok
        case status(Int)
        case network
        /// A `GET` answers 200 with this SEALED link blob; anything else as `.ok`.
        case capability(Data)
        /// An upload answers 200 with this `blob_id` string.
        case uploadAnswers(String)
    }

    struct Seen {
        let method: String
        let path: String
        let authorization: String?
    }

    private static let lock = NSLock()
    private static var modes: [String: Mode] = [:]
    private static var seen: [String: [Seen]] = [:]

    /// The 8-character ledger id every `.ok` upload answers with (6 bytes).
    static let mintedBlobIdSlug = "AbCd-_12"

    static func freshScope(_ mode: Mode) -> String {
        let scope = "g\(UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()).test"
        lock.lock(); defer { lock.unlock() }
        modes[scope] = mode
        return scope
    }

    static func client(_ scope: String) -> DiscoveryClient {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [GroupInviteDiscoveryURLProtocol.self]
        return DiscoveryClient(
            baseURL: "http://\(scope):9",
            session: URLSession(configuration: config),
            tokens: FakeAnonTokens())
    }

    static func requests(_ scope: String) -> [Seen] {
        lock.lock(); defer { lock.unlock() }
        return seen[scope] ?? []
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url else {
            client?.urlProtocol(self, didFailWithError: URLError(.badURL))
            return
        }
        let scope = url.host?.lowercased() ?? "?"
        Self.lock.lock()
        Self.seen[scope, default: []].append(Seen(
            method: request.httpMethod ?? "GET",
            path: url.path,
            authorization: request.value(forHTTPHeaderField: "Authorization")))
        let mode = Self.modes[scope] ?? .ok
        Self.lock.unlock()

        let status: Int
        let body: Data
        do {
            switch mode {
            case .network:
                client?.urlProtocol(self, didFailWithError: URLError(.notConnectedToInternet))
                return
            case .status(let code):
                status = code
                body = Data("refused".utf8)
            case .capability(let wire) where request.httpMethod == "GET":
                status = 200
                body = Data(#"{"sealed":"\#(wire.base64EncodedString())"}"#.utf8)
            case .uploadAnswers(let slug):
                status = 200
                body = Data(#"{"blob_id":"\#(slug)"}"#.utf8)
            case .ok, .capability:
                status = 200
                body = Data(#"{"blob_id":"\#(Self.mintedBlobIdSlug)"}"#.utf8)
            }
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

/// The group-invite ledger upload and revoke are `require_session` routes on
/// `services/discovery`. They used to go out on the repository's anonymous
/// client, so every upload was answered 401 and the mint silently fell back to
/// the long inline link. They now carry the signed-in session's bearer token,
/// read at call time; with no token at all the mint fails loudly, and only a
/// transport failure still falls back to the inline form.
@MainActor
final class GroupInviteUploadAuthTests: XCTestCase {

    func testTheUploadCarriesTheSessionBearer() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.ok)
        let f = try makeOwnerRepo(discoveryScope: scope, token: "session-token-1")
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }

        let link = try await f.repo.createGroupInviteLink(groupIdHex: f.groupIdHex)

        let upload = try XCTUnwrap(GroupInviteDiscoveryURLProtocol.requests(scope)
            .first { $0.method == "POST" && $0.path == "/discovery/group-invite" })
        XCTAssertEqual(upload.authorization, "Bearer session-token-1")
        XCTAssertTrue(
            link.contains("/g/\(GroupInviteDiscoveryURLProtocol.mintedBlobIdSlug)#"),
            "a successful upload shares the short pointer link, got \(link)")
        // ADR-0028 amendment of 2026-09-27: `/g/<8>#<16>`.
        let (base, fragment) = GroupInviteLink.split(link)
        XCTAssertEqual(base.components(separatedBy: "/g/").last?.count, 8)
        XCTAssertEqual(fragment?.count, 16)
        guard case .secret(let secret) = GroupInviteLink.readFragment(fragment) else {
            return XCTFail("the minted fragment is a link secret, got \(String(describing: fragment))")
        }
        XCTAssertEqual(secret.count, 12)
    }

    /// The token is read when the request is made, not captured at bootstrap:
    /// a re-login to the same account must not keep sending the old token.
    func testTheTokenIsReadAtCallTime() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.ok)
        let f = try makeOwnerRepo(discoveryScope: scope, token: "old")
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }
        f.repo.bearerSessionToken = { "new" }

        _ = try await f.repo.createGroupInviteLink(groupIdHex: f.groupIdHex)

        let upload = try XCTUnwrap(GroupInviteDiscoveryURLProtocol.requests(scope)
            .first { $0.path == "/discovery/group-invite" })
        XCTAssertEqual(upload.authorization, "Bearer new")
    }

    func testNoSessionTokenFailsLoudlyWithoutARequest() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.ok)
        let f = try makeOwnerRepo(discoveryScope: scope, token: nil)
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }

        do {
            let link = try await f.repo.createGroupInviteLink(groupIdHex: f.groupIdHex)
            XCTFail("expected a refusal, got \(link)")
        } catch {}
        XCTAssertTrue(
            GroupInviteDiscoveryURLProtocol.requests(scope)
                .filter { $0.path.hasPrefix("/discovery/") }.isEmpty,
            "nothing is sent to the ledger without a session")
    }

    /// A server refusal (the 401 seen live) is the server's answer, not a
    /// transport failure: it surfaces instead of producing the inline link.
    func testAServerRefusalSurfacesInsteadOfFallingBack() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.status(401))
        let f = try makeOwnerRepo(discoveryScope: scope, token: "t")
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }

        do {
            let link = try await f.repo.createGroupInviteLink(groupIdHex: f.groupIdHex)
            XCTFail("expected the 401 to surface, got \(link)")
        } catch {}
    }

    /// A censored or offline network still gets a working link.
    func testATransportFailureFallsBackToTheInlineLink() async throws {
        let scope = GroupInviteDiscoveryURLProtocol.freshScope(.network)
        let f = try makeOwnerRepo(discoveryScope: scope, token: "t")
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }

        let link = try await f.repo.createGroupInviteLink(groupIdHex: f.groupIdHex)
        let slug = try XCTUnwrap(GroupInviteLink.split(link).base
            .components(separatedBy: "/g/").last)
        XCTAssertGreaterThan(slug.count, 100, "the inline form carries the whole sealed blob")
        guard case .inline = try GroupInviteUrl.parseGroupInviteSlug(slug) else {
            return XCTFail("the fallback link parses as inline")
        }
        XCTAssertEqual(GroupInviteLink.split(link).fragment?.count, 16,
                       "the link secret still rides the fragment, 16 characters")
    }

    func testTheFallbackRuleAndTheLogSummary() {
        let network = DiscoveryClientError.network(endpoint: "/x", underlying: URLError(.timedOut))
        let refused = DiscoveryClientError.http(status: 401, endpoint: "/x", body: "capability?")
        XCTAssertTrue(ChatRepository.groupInviteUploadFallsBackInline(network))
        XCTAssertFalse(ChatRepository.groupInviteUploadFallsBackInline(refused))
        XCTAssertFalse(ChatRepository.groupInviteUploadFallsBackInline(
            DiscoveryClientError.http(status: 503, endpoint: "/x", body: "")))
        XCTAssertEqual(ChatRepository.discoveryFailureSummary(refused), "http 401")
        XCTAssertEqual(ChatRepository.discoveryFailureSummary(network), "network")
    }

    /// The repository's shared client stays anonymous: the bearer is a copy.
    func testTheBearerIsACopyNotAMutationOfTheSharedClient() {
        let shared = GroupInviteDiscoveryURLProtocol.client(
            GroupInviteDiscoveryURLProtocol.freshScope(.ok))
        let bearer = shared.withSessionToken("t")
        XCTAssertEqual(bearer.sessionToken, "t")
        XCTAssertNil(shared.sessionToken)
        XCTAssertEqual(bearer.baseURL, shared.baseURL)
    }

    // MARK: - Fixture

    private func makeOwnerRepo(discoveryScope: String, token: String?) throws -> GroupInviteRepoFixture {
        try makeGroupInviteRepo(discoveryScope: discoveryScope, token: token)
    }
}

/// A bootstrapped owner repository whose discovery client is a
/// ``GroupInviteDiscoveryURLProtocol`` scope. Shared with
/// `InviteLinkShapeTests`.
struct GroupInviteRepoFixture {
    let repo: ChatRepository
    let groupIdHex: String
    let outboxDir: URL
}

@MainActor
func makeGroupInviteRepo(discoveryScope: String, token: String?) throws -> GroupInviteRepoFixture {
    let dsScope = WakeRecordingDsURLProtocol.freshScope()
    let ownerSeed = Data(repeating: 0x41, count: 32)
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
        defaults: try XCTUnwrap(UserDefaults(suiteName: "gi-auth-k-\(suffix)")))
    knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
    let outboxDir = FileManager.default.temporaryDirectory
        .appendingPathComponent("gi-auth-\(suffix)")
    try FileManager.default.createDirectory(at: outboxDir, withIntermediateDirectories: true)

    let ds = WakeRecordingDsURLProtocol.client(dsScope)
    let repo = ChatRepository(
        chatClient: ChatClient(ds: ds),
        dsClient: ds,
        discovery: GroupInviteDiscoveryURLProtocol.client(discoveryScope),
        snapshotStore: snapshotStore,
        knownGroupsStore: knownGroupsStore,
        messageLogStore: MessageLogStore(),
        commitOutbox: CommitOutboxStore(rootDir: outboxDir),
        pushRouting: PushGroupRouting(
            store: PushRoutingStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "gi-auth-p-\(suffix)"))),
            subscribeUnderPushGid: { _ in },
            unsubscribeUnderPushGid: { _ in }),
        beaconStore: BeaconObservationStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "gi-auth-b-\(suffix)"))),
        pendingRequestStore: PendingRequestStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "gi-auth-r-\(suffix)"))),
        lastObservedOwnerStore: LastObservedOwnerStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "gi-auth-o-\(suffix)"))),
        leaveRequestStore: LeaveRequestStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "gi-auth-l-\(suffix)"))))
    repo.deviceIdentityInstaller = { _, _ in nil }
    repo.bearerSessionToken = { token }
    repo.bootstrap(
        secrets: IdentitySecrets(
            accountId: accountIdBytes,
            exportKey: Data(repeating: 0x22, count: 32),
            accountIdentitySeed: ownerSeed,
            sessionToken: "bootstrap-token",
            sessionExpiresAtMs: 0))
    return GroupInviteRepoFixture(repo: repo, groupIdHex: groupIdHex, outboxDir: outboxDir)
}
