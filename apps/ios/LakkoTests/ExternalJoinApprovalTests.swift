// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// MARK: - Scripted DS

/// A `URLProtocol` standing in for the whole Delivery Service: an in-memory
/// group journal that `POST` appends to and `GET` reads from, with the same
/// `after` cursor semantics the real DS has.
///
/// It is deliberately a real journal rather than a canned response, because the
/// behaviour under test runs a committer sweep — which publishes a commit and
/// then reads the journal back to learn whether it won the order. A stub that
/// answered every fetch with the same rows would either loop or report a verdict
/// that has nothing to do with what was published.
///
/// ## One journal PER SCOPE, and why that is a correctness property
///
/// A `URLProtocol` subclass is registered on a `URLSessionConfiguration` and
/// instantiated per request, so any state it keeps is necessarily `static` — and
/// therefore process-global, shared by every test in the bundle. That was the
/// whole design until this scoping landed, and it made a straggling publish from
/// one fixture visible in another fixture's row count.
///
/// The straggler is not hypothetical and it is not a test artefact: it is the
/// production ADR-0042 path. `ChatRepository.bootstrap` calls
/// `ensurePushRouting` for every restored group, which is `Task { @MainActor }`
/// fire-and-forget; on a fixture whose device is the group OWNER that reaches
/// the mint branch, composes a push-routing-secret envelope and **publishes it
/// to the group journal** through the very `DsClient` this stub is standing in
/// for. In a SYNCHRONOUS test the main actor never yields inside the test body,
/// so that `Task` cannot run until the test returns — i.e. during a later test,
/// which may be an `async` one asserting an exact `rowCount`. A shared
/// `reset()` cannot fix that: the reset happens first and the row lands after.
///
/// So the journal is keyed on the request's HOST, and each fixture allocates a
/// unique one via ``freshScope()`` and builds its `DsClient` on
/// ``baseURL(_:)``. A straggler then writes into the journal of the fixture
/// that spawned it, where nobody is looking, and cross-test bleed becomes
/// impossible rather than unlikely.
final class ScriptedDsURLProtocol: URLProtocol {

    private struct Journal {
        var rows: [(id: Int64, body: Data)] = []
        var nextId: Int64 = 1
    }

    private static let lock = NSLock()
    private static var journals: [String: Journal] = [:]

    /// A fresh, unique journal scope. Every fixture takes one.
    static func freshScope() -> String {
        "s\(UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()).test"
    }

    /// The `DsClient` base URL that routes to `scope`'s journal.
    static func baseURL(_ scope: String) -> String { "http://\(scope):9" }

    /// Drop every journal. Belt-and-braces hygiene only: scopes are unique, so
    /// no fixture depends on this to be isolated from another.
    static func reset() {
        lock.lock(); defer { lock.unlock() }
        journals.removeAll()
    }

    /// Append a row to `scope` as if a peer had published it. Returns the id.
    @discardableResult
    static func seed(_ body: Data, scope: String) -> Int64 {
        lock.lock(); defer { lock.unlock() }
        var journal = journals[scope] ?? Journal()
        let id = journal.nextId
        journal.nextId += 1
        journal.rows.append((id: id, body: body))
        journals[scope] = journal
        return id
    }

    static func rowCount(scope: String) -> Int {
        lock.lock(); defer { lock.unlock() }
        return journals[scope]?.rows.count ?? 0
    }

    private static func scope(of url: URL) -> String {
        url.host?.lowercased() ?? "?"
    }

    private static func after(
        _ cursor: Int64, scope: String
    ) -> [(id: Int64, body: Data)] {
        lock.lock(); defer { lock.unlock() }
        return (journals[scope]?.rows ?? []).filter { $0.id > cursor }
    }

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url else {
            client?.urlProtocol(self, didFailWithError: URLError(.badURL))
            return
        }
        let scope = Self.scope(of: url)
        // ADR-0053 — the access registration and the knock queue are separate
        // DS routes; a registration is accepted (204, nothing stored) and the
        // knock queue is empty, so neither leaks into the journal this stub
        // models. The proof header is not checked here: these fixtures
        // exercise membership, not the server's gate.
        if url.path.hasSuffix("/access") {
            let response = HTTPURLResponse(
                url: url, statusCode: 204, httpVersion: "HTTP/1.1", headerFields: [:])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocolDidFinishLoading(self)
            return
        }
        let body: Data
        if url.path.hasSuffix("/knocks") {
            body = Data(#"{"messages":[],"has_more":false}"#.utf8)
        } else if request.httpMethod == "POST" {
            // `URLProtocol` strips `httpBody` for a body set on the request when
            // it came through `URLSession`; the stream is the reliable source.
            let payload = Self.bodyData(of: request)
            let id = Self.seed(payload, scope: scope)
            body = Data(#"{"id":\#(id),"created_at":"1970-01-01T00:00:00Z"}"#.utf8)
        } else {
            let cursor = Self.cursor(from: url)
            let rows = Self.after(cursor, scope: scope).map { row -> String in
                let b64 = row.body.base64EncodedString()
                return #"{"id":\#(row.id),"payload_b64":"\#(b64)","created_at":"1970-01-01T00:00:00Z"}"#
            }
            body = Data(#"{"messages":[\#(rows.joined(separator: ","))],"has_more":false}"#.utf8)
        }
        let response = HTTPURLResponse(
            url: url,
            statusCode: 200,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"]
        )!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}

    private static func bodyData(of request: URLRequest) -> Data {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { return Data() }
        stream.open()
        defer { stream.close() }
        var out = Data()
        let size = 8192
        var buffer = [UInt8](repeating: 0, count: size)
        while stream.hasBytesAvailable {
            let read = stream.read(&buffer, maxLength: size)
            if read <= 0 { break }
            out.append(contentsOf: buffer[0..<read])
        }
        return out
    }

    private static func cursor(from url: URL) -> Int64 {
        guard let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems,
              let raw = items.first(where: { $0.name == "after" })?.value,
              let value = Int64(raw) else { return 0 }
        return value
    }
}

// MARK: - Tests

/// ADR-0045 / ADR-0027 (G-3) — the external-join `Approve` button, after
/// ADR-0045 gave every role-tabled group a pending-proposal set.
///
/// ## What broke
///
/// `Group::approve_external_join` opens with `refuse_if_proposals_pending`,
/// which is a bare count of ANY pending proposal of ANY kind. Under ADR-0045 a
/// non-empty pending set is the ORDINARY state between any member's request and
/// the next committer sweep — so an admin tapping `Approve` hit a hard failure
/// whenever anything at all was waiting, and what reached the screen was the
/// core's own diagnostic naming `commit_pending_proposals` /
/// `stage_pending_proposals`, two Rust entry points no user can call.
///
/// ## The fixture, and why it has the shape it has
///
/// The device under test is an **admin**, not the owner, because that is the
/// only vantage point from which both halves of the fix are reachable:
///
/// * the owner is committer rank 0 and the core's fold short-circuits it to
///   `act`, so an owner can never be made to stand down and the refusal path
///   would be untestable from there;
/// * an admin stands down while the owner beacons and acts once that beacon
///   ages out — so the SAME fixture produces both outcomes by changing nothing
///   but the beacon store.
///
/// The pending proposal is the OWNER'S (a Remove of a third member), delivered
/// through the journal like any peer's. It deliberately is not this device's own
/// proposal: a locally published proposal leaves its journal row unapplied, and
/// the sweep's stand-down re-read then refuses to stage on principle — a real
/// behaviour, but one that would mask the property under test.
@MainActor
final class ExternalJoinApprovalTests: XCTestCase {

    private struct Fixture {
        let repo: ChatRepository
        let groupIdHex: String
        let groupIdB64: String
        let accountIdB64: String
        /// The joiner's leaf signature key — the `proposalRef` handle.
        let joinerKey: Data
        /// The owner's leaf signature key — whose beacon the admin defers to.
        let ownerKeyB64: String
        let beaconStore: BeaconObservationStore
        let snapshotStore: IdentitySnapshotStore
        let outboxDir: URL
        /// This fixture's private journal on the scripted DS — see the scoping
        /// note on `ScriptedDsURLProtocol`.
        let dsScope: String
    }

    private func nowMs() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

    /// Owner A + admin B (the device under test) + member D, plus a stranger C
    /// whose external-join request is waiting, and one pending Remove proposal
    /// from the owner already applied on B.
    private func makeMemberRepo() async throws -> Fixture {
        try await makeRepo(promoteToAdmin: false)
    }

    private func makeAdminRepo() async throws -> Fixture {
        try await makeRepo(promoteToAdmin: true)
    }

    private func makeRepo(promoteToAdmin: Bool) async throws -> Fixture {
        let dsScope = ScriptedDsURLProtocol.freshScope()

        let ownerSeed = Data(repeating: 0x11, count: 32)
        let adminSeed = Data(repeating: 0x55, count: 32)
        let owner = try Identity.fromSeed(seed: ownerSeed)
        let admin = try Identity.fromSeed(seed: adminSeed)
        let third = try Identity.fromSeed(seed: Data(repeating: 0x77, count: 32))
        let stranger = try Identity.fromSeed(seed: Data(repeating: 0x99, count: 32))

        // Owner founds the group; `Group.create` seeds the ADR-0022 role table,
        // which is what routes this group through ADR-0045's proposal path.
        let ownerGroup = try Group.create(identity: owner)
        let addAdmin = try ownerGroup.addMemberFromWire(
            identity: owner, keyPackage: try admin.createKeyPackage())
        let adminGroup = try Group.joinFromWelcome(welcome: addAdmin.welcome, identity: admin)
        let addThird = try ownerGroup.addMemberFromWire(
            identity: owner, keyPackage: try third.createKeyPackage())
        _ = try adminGroup.processIncomingVerifyingRoles(identity: admin, bytes: addThird.commit)

        // Promote B to admin — committer rank 1, so it stands down behind the
        // owner's beacon and acts without one.
        let ownerKey = try owner.signaturePublicKey()
        let adminKey = try admin.signaturePublicKey()
        if promoteToAdmin {
            let roleCommit = try ownerGroup.setGroupRoles(
                identity: owner, owner: ownerKey, admins: [adminKey])
            _ = try adminGroup.processIncomingVerifyingRoles(identity: admin, bytes: roleCommit)
        }

        let groupId = try ownerGroup.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)
        let epoch = try ownerGroup.epoch()
        XCTAssertEqual(
            try adminGroup.epoch(), epoch,
            "the fixture must leave both devices at one epoch, or the seeded rows "
            + "would be dropped as wrong-epoch and the whole suite would pass vacuously")
        XCTAssertEqual(
            try adminGroup.ownCommitterRank(identity: admin), promoteToAdmin ? 1 : nil,
            "the device under test must be an ADMIN — rank 0 (the owner) short-circuits "
            + "the acting-committer fold to `act` — or, for the refusal case, a plain member")

        // Row 1 — the owner's Remove proposal. This is what makes the pending
        // set non-empty on the admin.
        let thirdLeaf = try XCTUnwrap(
            try ownerGroup.members().first { $0.signaturePublicKey == (try third.signaturePublicKey()) }?
                .leafIndex)
        ScriptedDsURLProtocol.seed(
            try ownerGroup.proposeRemoveMember(identity: owner, leafIndex: thirdLeaf),
            scope: dsScope)
        // Row 2 — the stranger's external-join request (ADR-0027 / ADR-0028).
        ScriptedDsURLProtocol.seed(
            try stranger.createExternalJoinProposal(groupId: groupId, epoch: epoch),
            scope: dsScope)

        // Rehydrate the admin's provider into a real repository.
        let snapshotBlob = try admin.snapshotStorage()
        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let accountIdB64 = base64Encode(accountIdBytes)

        let snapshotStore = IdentitySnapshotStore()
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshotBlob)
        let knownGroupsStore = KnownGroupsStore(
            defaults: UserDefaults(suiteName: "ext-join-approve-k-\(UUID().uuidString)")!)
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let beaconStore = BeaconObservationStore(
            defaults: UserDefaults(suiteName: "ext-join-approve-b-\(UUID().uuidString)")!)
        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("ext-join-approve-\(UUID().uuidString)")

        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [ScriptedDsURLProtocol.self]
        // ONE scripted DS behind both seams. `ChatRepository` publishes through
        // its own `dsClient` but polls through `ChatClient`'s, so a repo whose
        // `ChatClient` kept the default would read a real socket.
        let ds = DsClient(
            baseURL: ScriptedDsURLProtocol.baseURL(dsScope),
            session: URLSession(configuration: config))
        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            discovery: DiscoveryClient(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: outboxDir),
            pushRouting: PushGroupRouting(),
            beaconStore: beaconStore,
            pendingRequestStore: PendingRequestStore(
                defaults: UserDefaults(suiteName: "ext-join-approve-p-\(UUID().uuidString)")!)
        )
        // Phase 1 fixture: the chats run on the account identity the snapshot
        // was taken under (ADR-0050 device identities are exercised elsewhere).
        repo.deviceIdentityInstaller = { _, _ in nil }
        repo.bootstrap(
            secrets: IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: Data(repeating: 0x22, count: 32),
                accountIdentitySeed: adminSeed,
                sessionToken: "t",
                sessionExpiresAtMs: 0
            )
        )
        XCTAssertTrue(
            repo.usesProposalPath(groupIdHex: groupIdHex),
            "the group must route membership through ADR-0045 proposals, or none of this applies")

        // The owner is beaconing, so the poll's own tail sweep stands down and
        // leaves the proposal pending — which is exactly the state an admin is
        // in whenever the owner is around.
        let groupIdB64 = base64Encode(groupId)
        beaconStore.record(
            accountIdB64: accountIdB64,
            groupIdB64: groupIdB64,
            senderKeyB64: base64Encode(ownerKey),
            atMs: nowMs()
        )
        try await repo.pollGroup(groupIdHex: groupIdHex)

        let pendingJoins = repo.pendingExternalJoins(groupIdHex: groupIdHex)
        XCTAssertEqual(
            pendingJoins.count, 1,
            "the external-join request must be retained, or `Approve` would fail for a "
            + "reason that has nothing to do with the pending set")
        // Proves BOTH halves of the precondition in one assertion: the sweep got
        // past its `nothingPending` short-circuit (so a proposal really is
        // pending) AND this device is standing down rather than acting.
        if promoteToAdmin {
            let stoodDown = await repo.sweepIfCommitter(groupIdHex: groupIdHex)
            XCTAssertEqual(
                stoodDown, .deferred(rank: 1, toRank: 0),
                "the fixture must leave a pending proposal on a device that is NOT the acting "
                + "committer; anything else means the state under test was never reached")
        }

        return Fixture(
            repo: repo,
            groupIdHex: groupIdHex,
            groupIdB64: groupIdB64,
            accountIdB64: accountIdB64,
            joinerKey: try XCTUnwrap(pendingJoins.first),
            ownerKeyB64: base64Encode(ownerKey),
            beaconStore: beaconStore,
            snapshotStore: snapshotStore,
            outboxDir: outboxDir,
            dsScope: dsScope
        )
    }

    private func tearDown(_ fx: Fixture) {
        fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64)
        try? FileManager.default.removeItem(at: fx.outboxDir)
        ScriptedDsURLProtocol.reset()
    }

    // MARK: - (1) ADR-0045 S7 — the approve is a PROPOSAL

    /// THE HEADLINE. The approve publishes exactly ONE artifact — an ordinary
    /// admin-authored `Add` proposal — advances no epoch, and records nothing
    /// in the ADR-0040 outbox, because a proposal makes nothing durable that a
    /// crash could strand.
    ///
    /// The fixture is unchanged from the pre-S7 suite and that is the point:
    /// this device is an ADMIN standing down behind the owner's live beacon,
    /// with a pending Remove proposal it cannot sweep. Before S7 that state
    /// made `Approve` fail outright; now it is simply irrelevant.
    func testApprovingAnExternalJoinPublishesOneProposalAndAdvancesNoEpoch() async throws {
        let fx = try await makeAdminRepo()
        defer { tearDown(fx) }

        let epochBefore = try XCTUnwrap(fx.repo.epoch(groupIdHex: fx.groupIdHex))
        let rowsBefore = ScriptedDsURLProtocol.rowCount(scope: fx.dsScope)

        let proposalId = try await fx.repo.approveExternalJoin(
            groupIdHex: fx.groupIdHex,
            proposalRef: fx.joinerKey
        )

        XCTAssertGreaterThan(proposalId, 0, "the approve must have reached the DS")
        XCTAssertEqual(
            ScriptedDsURLProtocol.rowCount(scope: fx.dsScope) - rowsBefore, 1,
            "S7 publishes ONE artifact: a proposal. The pre-S7 path published a commit AND a "
            + "WelcomeRendezvous; the rendezvous belongs to the sweep now")
        XCTAssertEqual(
            fx.repo.epoch(groupIdHex: fx.groupIdHex), epochBefore,
            "a proposal advances no epoch — that is the whole of S7")
        XCTAssertTrue(
            CommitOutboxStore(rootDir: fx.outboxDir).loadUnacked(
                accountIdB64: fx.accountIdB64, groupIdB64: fx.groupIdB64).isEmpty,
            "a proposal is not a commit, so it leaves the ADR-0040 outbox entirely")
        XCTAssertTrue(
            fx.repo.pendingExternalJoins(groupIdHex: fx.groupIdHex).isEmpty,
            "an approved request must leave the pending list")
    }

    // MARK: - (2) The pending set no longer blocks an approve

    /// The pre-check that gated `Approve` on an empty pending set is gone with
    /// the direct commit it guarded. Under ADR-0045 a pending proposal is the
    /// NORMAL state, so an admin deferring to a live owner could essentially
    /// never approve.
    ///
    /// This device is standing down behind the owner's beacon and holds a
    /// pending Remove, so nothing it can do would drain the set — and the
    /// approve must succeed anyway, without sweeping.
    func testApproveComposesWithAPendingProposalAndNeverSweeps() async throws {
        let fx = try await makeAdminRepo()
        defer { tearDown(fx) }

        // The owner is still beaconing — re-recorded because the fixture's own
        // verification sweep is the last thing to have read it.
        fx.beaconStore.record(
            accountIdB64: fx.accountIdB64,
            groupIdB64: fx.groupIdB64,
            senderKeyB64: fx.ownerKeyB64,
            atMs: nowMs()
        )
        let epochBefore = try XCTUnwrap(fx.repo.epoch(groupIdHex: fx.groupIdHex))

        _ = try await fx.repo.approveExternalJoin(
            groupIdHex: fx.groupIdHex,
            proposalRef: fx.joinerKey
        )

        XCTAssertEqual(
            fx.repo.epoch(groupIdHex: fx.groupIdHex), epochBefore,
            "the approve must not have swept anything — it is a proposal, not a drain")
        let stillDeferring = await fx.repo.sweepIfCommitter(groupIdHex: fx.groupIdHex)
        XCTAssertEqual(
            stillDeferring, .deferred(rank: 1, toRank: 0),
            "and this device must still be standing down, with its own approval pending too")
    }

    // MARK: - (3) A refused approve leaves the request pending

    /// The core keeps the retained request on a refusal so the admin can
    /// retry; the client must not turn that into a publish or a silent drop.
    /// A plain member's approve is the reachable refusal.
    func testARefusedApprovePublishesNothingAndLeavesTheRequestPending() async throws {
        let fx = try await makeMemberRepo()
        defer { tearDown(fx) }

        let rowsBefore = ScriptedDsURLProtocol.rowCount(scope: fx.dsScope)
        let epochBefore = try XCTUnwrap(fx.repo.epoch(groupIdHex: fx.groupIdHex))

        do {
            _ = try await fx.repo.approveExternalJoin(
                groupIdHex: fx.groupIdHex,
                proposalRef: fx.joinerKey
            )
            XCTFail("a plain member must not be able to approve")
            return
        } catch {}

        XCTAssertEqual(
            ScriptedDsURLProtocol.rowCount(scope: fx.dsScope), rowsBefore,
            "a refused approve must publish nothing")
        XCTAssertEqual(fx.repo.epoch(groupIdHex: fx.groupIdHex), epochBefore)
        XCTAssertEqual(
            fx.repo.pendingExternalJoins(groupIdHex: fx.groupIdHex).count, 1,
            "the request must survive so the admin can retry — discarding it would lose a "
            + "stranger's request nobody can get back")
    }

}
