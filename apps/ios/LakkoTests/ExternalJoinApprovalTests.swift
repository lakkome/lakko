// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// MARK: - Scripted DS

/// A `URLProtocol` standing in for the whole Delivery Service: one in-memory
/// group journal that `POST` appends to and `GET` reads from, with the same
/// `after` cursor semantics the real DS has.
///
/// It is deliberately a real journal rather than a canned response, because the
/// behaviour under test runs a committer sweep — which publishes a commit and
/// then reads the journal back to learn whether it won the order. A stub that
/// answered every fetch with the same rows would either loop or report a verdict
/// that has nothing to do with what was published.
final class ScriptedDsURLProtocol: URLProtocol {

    private static let lock = NSLock()
    private static var rows: [(id: Int64, body: Data)] = []
    private static var nextId: Int64 = 1

    static func reset() {
        lock.lock(); defer { lock.unlock() }
        rows.removeAll()
        nextId = 1
    }

    /// Append a row as if a peer had published it. Returns the assigned id.
    @discardableResult
    static func seed(_ body: Data) -> Int64 {
        lock.lock(); defer { lock.unlock() }
        let id = nextId
        nextId += 1
        rows.append((id: id, body: body))
        return id
    }

    static var rowCount: Int {
        lock.lock(); defer { lock.unlock() }
        return rows.count
    }

    private static func append(_ body: Data) -> Int64 {
        seed(body)
    }

    private static func after(_ cursor: Int64) -> [(id: Int64, body: Data)] {
        lock.lock(); defer { lock.unlock() }
        return rows.filter { $0.id > cursor }
    }

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url else {
            client?.urlProtocol(self, didFailWithError: URLError(.badURL))
            return
        }
        let body: Data
        if request.httpMethod == "POST" {
            // `URLProtocol` strips `httpBody` for a body set on the request when
            // it came through `URLSession`; the stream is the reliable source.
            let payload = Self.bodyData(of: request)
            let id = Self.append(payload)
            body = Data(#"{"id":\#(id),"created_at":"1970-01-01T00:00:00Z"}"#.utf8)
        } else {
            let cursor = Self.cursor(from: url)
            let rows = Self.after(cursor).map { row -> String in
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
    }

    private func nowMs() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

    /// Owner A + admin B (the device under test) + member D, plus a stranger C
    /// whose external-join request is waiting, and one pending Remove proposal
    /// from the owner already applied on B.
    private func makeAdminRepo() async throws -> Fixture {
        ScriptedDsURLProtocol.reset()

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
        let roleCommit = try ownerGroup.setGroupRoles(
            identity: owner, owner: ownerKey, admins: [adminKey])
        _ = try adminGroup.processIncomingVerifyingRoles(identity: admin, bytes: roleCommit)

        let groupId = try ownerGroup.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)
        let epoch = try ownerGroup.epoch()
        XCTAssertEqual(
            try adminGroup.epoch(), epoch,
            "the fixture must leave both devices at one epoch, or the seeded rows "
            + "would be dropped as wrong-epoch and the whole suite would pass vacuously")
        XCTAssertEqual(
            try adminGroup.ownCommitterRank(identity: admin), 1,
            "the device under test must be an ADMIN — rank 0 (the owner) short-circuits "
            + "the acting-committer fold to `act` and could never exercise the refusal")

        // Row 1 — the owner's Remove proposal. This is what makes the pending
        // set non-empty on the admin.
        let thirdLeaf = try XCTUnwrap(
            try ownerGroup.members().first { $0.signaturePublicKey == (try third.signaturePublicKey()) }?
                .leafIndex)
        ScriptedDsURLProtocol.seed(
            try ownerGroup.proposeRemoveMember(identity: owner, leafIndex: thirdLeaf))
        // Row 2 — the stranger's external-join request (ADR-0027 / ADR-0028).
        ScriptedDsURLProtocol.seed(
            try stranger.createExternalJoinProposal(groupId: groupId, epoch: epoch))

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
        let ds = DsClient(baseURL: "http://127.0.0.1:9", session: URLSession(configuration: config))
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
        let stoodDown = await repo.sweepIfCommitter(groupIdHex: groupIdHex)
        XCTAssertEqual(
            stoodDown, .deferred(rank: 1, toRank: 0),
            "the fixture must leave a pending proposal on a device that is NOT the acting "
            + "committer; anything else means the state under test was never reached")

        return Fixture(
            repo: repo,
            groupIdHex: groupIdHex,
            groupIdB64: groupIdB64,
            accountIdB64: accountIdB64,
            joinerKey: try XCTUnwrap(pendingJoins.first),
            ownerKeyB64: base64Encode(ownerKey),
            beaconStore: beaconStore,
            snapshotStore: snapshotStore,
            outboxDir: outboxDir
        )
    }

    private func tearDown(_ fx: Fixture) {
        fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64)
        try? FileManager.default.removeItem(at: fx.outboxDir)
        ScriptedDsURLProtocol.reset()
    }

    // MARK: - (1) The drain: an acting committer approves through a pending set

    /// THE DISCRIMINATING CASE. With the owner's beacon aged out, this admin is
    /// the acting committer, so the drain clears the pending set and the approve
    /// goes through. Without the drain the core refuses and this throws.
    func testApproveDrainsThePendingSetWhenThisDeviceIsTheActingCommitter() async throws {
        let fx = try await makeAdminRepo()
        defer { tearDown(fx) }

        // The owner went quiet. Nothing else about the group changes — same
        // pending proposal, same waiting request, same roster.
        fx.beaconStore.clearForAccount(accountIdB64: fx.accountIdB64)

        let commitId: Int64
        do {
            commitId = try await fx.repo.approveExternalJoin(
                groupIdHex: fx.groupIdHex,
                proposalRef: fx.joinerKey
            )
        } catch {
            // Reported here rather than left to propagate so the discrimination
            // is legible: without the drain this is the core's own refusal.
            XCTFail("the approve must go through once the drain has cleared the "
                    + "pending set, but it threw: \(error)")
            return
        }

        XCTAssertGreaterThan(commitId, 0, "the approve must have reached the DS")
        XCTAssertTrue(
            fx.repo.pendingExternalJoins(groupIdHex: fx.groupIdHex).isEmpty,
            "an approved request must leave the pending list")
    }

    // MARK: - (2) The surviving refusal reads as human copy

    /// The drain is not always able to clear the set: this admin is standing
    /// down behind the owner's live beacon, so nothing drains and the approve
    /// still cannot commit. What the admin must see is a sentence, not the
    /// core's diagnostic.
    func testApproveRefusesInHumanCopyWhenTheDrainCannotClearThePendingSet() async throws {
        let fx = try await makeAdminRepo()
        defer { tearDown(fx) }

        // The owner is still beaconing — re-recorded because the fixture's own
        // verification sweep is the last thing to have read it, and a stale
        // observation here would silently turn this into case (1).
        fx.beaconStore.record(
            accountIdB64: fx.accountIdB64,
            groupIdB64: fx.groupIdB64,
            senderKeyB64: fx.ownerKeyB64,
            atMs: nowMs()
        )

        do {
            _ = try await fx.repo.approveExternalJoin(
                groupIdHex: fx.groupIdHex,
                proposalRef: fx.joinerKey
            )
            XCTFail("the approve must not succeed while this device cannot commit")
            return
        } catch let error as ChatError {
            guard case .membershipChangesPending = error else {
                XCTFail("expected the typed pending-changes refusal, got \(error)")
                return
            }
            let shown = error.localizedDescription
            XCTAssertEqual(shown, ChatError.membershipChangesPendingCopy)
            // The point of the whole case: none of the core's vocabulary reaches
            // the screen.
            for leak in [
                "commit_pending_proposals",
                "stage_pending_proposals",
                "membership proposal(s) are pending",
                "ADR-0045",
            ] {
                XCTAssertFalse(
                    shown.contains(leak),
                    "user-facing copy must not contain the developer string '\(leak)'")
            }
        }
    }
}
