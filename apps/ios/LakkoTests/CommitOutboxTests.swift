// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0040 (G-35) — coverage for the iOS durable pending-commit outbox: the
/// store's round-trip + fail-closed durable write, the pure torn-write sweep
/// planner, and the commit-path abort-before-publish gate.
///
/// The bug (recap): the four group-membership commit paths (add / remove /
/// role / approve) published the commit BEFORE the N+1 snapshot was durable +
/// the persist was best-effort. A crash between publish and persist rolled the
/// local snapshot back to N while peers advanced to N+1 -> a silent, permanent
/// single-device membership brick. The fix records the commit in a durable
/// outbox (entry-first, then snapshot, BOTH fail-closed, BEFORE any publish) and
/// re-publishes it on restart; a torn entry-then-snapshot write is reconciled by
/// discarding the orphan (the commit was never published).

// MARK: - (4) Store round-trip + fail-closed durable write

final class CommitOutboxStoreTests: XCTestCase {

    private var tempDir: URL!
    private var store: CommitOutboxStore!

    override func setUpWithError() throws {
        tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("commit-outbox-test-\(UUID().uuidString)")
        store = CommitOutboxStore(rootDir: tempDir)
    }

    override func tearDownWithError() throws {
        if let tempDir = tempDir {
            try? FileManager.default.removeItem(at: tempDir)
        }
    }

    private func entry(
        id: String = "id1",
        account: String = "acct",
        group: String = "g1",
        target: String = "1"
    ) -> PendingCommitEntry {
        PendingCommitEntry(
            id: id,
            accountId: account,
            groupId: group,
            targetEpoch: target,
            commitB64: "AAAA"
        )
    }

    func testRoundTripAndGroupFilter() throws {
        try store.putEntryDurable(accountIdB64: "acct", entry: entry(id: "a", group: "g1"))
        try store.putEntryDurable(accountIdB64: "acct", entry: entry(id: "b", group: "g2"))

        let g1 = store.loadUnacked(accountIdB64: "acct", groupIdB64: "g1")
        XCTAssertEqual(g1.map(\.id), ["a"])

        let all = store.loadUnacked(accountIdB64: "acct").map(\.id).sorted()
        XCTAssertEqual(all, ["a", "b"])
    }

    func testMarkAckedFlipsFlagAndRecordsIdsSoSweepSkipsIt() throws {
        try store.putEntryDurable(accountIdB64: "acct", entry: entry(id: "a", group: "g1"))
        store.markAckedBestEffort(
            accountIdB64: "acct",
            id: "a",
            commitMessageId: 42,
            secondMessageId: 43
        )
        // Un-acked filter no longer returns it.
        XCTAssertEqual(store.loadUnacked(accountIdB64: "acct", groupIdB64: "g1"), [])
        // markAcked on a missing id is a graceful no-op.
        store.markAckedBestEffort(accountIdB64: "acct", id: "does-not-exist")
    }

    func testDeleteEntryRemovesIt() throws {
        try store.putEntryDurable(accountIdB64: "acct", entry: entry(id: "a"))
        store.deleteEntryBestEffort(accountIdB64: "acct", id: "a")
        XCTAssertEqual(store.loadUnacked(accountIdB64: "acct"), [])
    }

    func testDeleteOutboxForAccountPurgesOnlyThatAccount() throws {
        try store.putEntryDurable(accountIdB64: "acct-a", entry: entry(id: "a", account: "acct-a"))
        try store.putEntryDurable(accountIdB64: "acct-b", entry: entry(id: "b", account: "acct-b"))
        store.deleteOutboxForAccount(accountIdB64: "acct-a")
        XCTAssertEqual(store.loadUnacked(accountIdB64: "acct-a"), [])
        XCTAssertEqual(store.loadUnacked(accountIdB64: "acct-b").map(\.id), ["b"])
    }

    func testSecondArtifactFieldsRoundTrip() throws {
        let e = PendingCommitEntry(
            id: "a",
            accountId: "acct",
            groupId: "g1",
            targetEpoch: "7",
            commitB64: "Y29tbWl0",
            secondArtifactB64: "d2VsY29tZQ==",
            secondArtifactKind: .welcome,
            secondArtifactMailboxB64: "cHNldWRv"
        )
        try store.putEntryDurable(accountIdB64: "acct", entry: e)
        let loaded = store.loadUnacked(accountIdB64: "acct")
        XCTAssertEqual(loaded, [e])
    }

    /// The load-bearing durability guarantee: a durable write that CANNOT land
    /// THROWS (fail-closed), so the commit path aborts before publishing rather
    /// than swallowing the failure. `rootDir: nil` models "no place to write".
    func testPutEntryDurableThrowsWhenStoreUnavailable() {
        let unavailable = CommitOutboxStore(rootDir: nil)
        XCTAssertThrowsError(
            try unavailable.putEntryDurable(accountIdB64: "acct", entry: entry())
        ) { error in
            XCTAssertTrue(
                error is CommitOutboxStoreError,
                "expected a fail-closed CommitOutboxStoreError, got \(error)"
            )
        }
        // putNewDurable (the production entry point) is fail-closed too.
        XCTAssertThrowsError(
            try unavailable.putNewDurable(
                accountIdB64: "acct",
                entry: NewCommitEntry(id: "x", groupId: "g", targetEpoch: "1", commitB64: "AAAA")
            )
        )
    }
}

// MARK: - (2) + (3) Pure sweep planner — republish vs torn-write discard

final class CommitOutboxSweepPlannerTests: XCTestCase {

    private func entry(id: String = "e", target: String) -> PendingCommitEntry {
        PendingCommitEntry(
            id: id,
            accountId: "acct",
            groupId: "g",
            targetEpoch: target,
            commitB64: "AAAA"
        )
    }

    /// (2) An un-acked entry whose group durably REACHED its targetEpoch (the
    /// commit was applied) -> re-publish.
    func testMatchingEpochRepublishes() {
        let e = entry(target: "5")
        XCTAssertEqual(
            ChatRepository.planSweep(currentEpoch: 5, entries: [e]),
            [.republish(e)]
        )
    }

    /// A group AHEAD of targetEpoch (a later commit advanced it further) still
    /// re-publishes — the commit at targetEpoch was durably applied.
    func testAheadEpochRepublishes() {
        let e = entry(target: "5")
        XCTAssertEqual(
            ChatRepository.planSweep(currentEpoch: 9, entries: [e]),
            [.republish(e)]
        )
    }

    /// (3) An un-acked entry whose group did NOT reach targetEpoch (a torn
    /// entry-then-snapshot write) -> DISCARD the orphan (the commit was never
    /// published, so the device stays cleanly at N).
    func testBehindEpochDiscardsTornWrite() {
        let e = entry(target: "5")
        XCTAssertEqual(
            ChatRepository.planSweep(currentEpoch: 4, entries: [e]),
            [.discardTornWrite(e)]
        )
    }

    /// A malformed targetEpoch is treated as unreachable -> discard (a corrupt
    /// entry can never be blindly re-published).
    func testMalformedTargetEpochDiscards() {
        let e = entry(target: "not-a-number")
        XCTAssertEqual(
            ChatRepository.planSweep(currentEpoch: 999, entries: [e]),
            [.discardTornWrite(e)]
        )
    }

    /// A mixed batch is classified per-entry.
    func testMixedBatchClassifiedPerEntry() {
        let applied = entry(id: "a", target: "3")
        let torn = entry(id: "b", target: "4")
        XCTAssertEqual(
            ChatRepository.planSweep(currentEpoch: 3, entries: [applied, torn]),
            [.republish(applied), .discardTornWrite(torn)]
        )
    }
}

// MARK: - (1) A persist failure aborts a commit path BEFORE any publish

/// Drives a REAL founder MLS group through `ChatRepository.setMemberAdmin` (a
/// role-change commit, valid on a single-member founder group whose role table
/// `Group.create` seeds) with the commit outbox pointed at an UNAVAILABLE store,
/// and asserts (a) the path THROWS and (b) ZERO DS requests were attempted — the
/// commit's fail-closed durable record is the gate that stops the publish.
@MainActor
final class CommitOutboxCommitPathAbortTests: XCTestCase {

    /// A DsClient whose requests are intercepted + counted by
    /// ``RecordingURLProtocol`` (defined in `ChatSendNonceReuseTests`), so any
    /// publish attempt is directly observable.
    private func makeRecordingDs() -> DsClient {
        RecordingURLProtocol.reset()
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [RecordingURLProtocol.self]
        let session = URLSession(configuration: config)
        return DsClient(baseURL: "http://127.0.0.1:9", session: session)
    }

    func testPersistFailureAbortsRoleCommitBeforePublish() async throws {
        // 1. Build a real founder group + its identity from a seed. `Group.create`
        //    seeds the owner role table (ADR-0022), so `setMemberAdmin` can craft
        //    a real `setGroupRoles` commit on this single-member group.
        let seed = Data(repeating: 0x11, count: 32)
        let founder = try Identity.fromSeed(seed: seed)
        let group = try Group.create(identity: founder)
        let groupId = try group.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)
        let snapshotBlob = try founder.snapshotStorage()

        // 2. A random account id keeps the shared IdentitySnapshotStore /
        //    KnownGroupsStore rows isolated across runs.
        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes { _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!) }
        let accountIdB64 = base64Encode(accountIdBytes)

        let snapshotStore = IdentitySnapshotStore()
        let knownGroupsStore = KnownGroupsStore(
            defaults: UserDefaults(suiteName: "commit-outbox-test-\(UUID().uuidString)")!
        )
        defer { snapshotStore.deleteSnapshot(accountIdB64: accountIdB64) }

        // 3. Pre-seed the restore path: the snapshot + the known-group id, so
        //    `bootstrap` rehydrates the founder group into the repo.
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshotBlob)
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)

        // 4. Repo with the recording DS + an UNAVAILABLE outbox (fail-closed on
        //    every durable write).
        let repo = ChatRepository(
            chatClient: ChatClient(),
            dsClient: makeRecordingDs(),
            discovery: DiscoveryClient(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: nil)
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

        // Guard: the founder group must have rehydrated, else the abort below
        // would be for the wrong reason ("no live Group handle").
        XCTAssertEqual(
            repo.memberCount(groupIdHex: groupIdHex), 1,
            "founder group must rehydrate for the commit path to reach the persist gate"
        )

        // 5. A DIFFERENT valid 32-byte signature key to promote to admin.
        let otherKey = try Identity.fromSeed(seed: Data(repeating: 0x33, count: 32))
            .signaturePublicKey()

        // Only count requests from here on — bootstrap issues none on the
        // recording DS, but reset defensively.
        RecordingURLProtocol.reset()

        do {
            _ = try await repo.setMemberAdmin(
                groupIdHex: groupIdHex,
                memberSignaturePubkey: otherKey,
                makeAdmin: true
            )
            XCTFail("setMemberAdmin must throw when the outbox persist fails")
        } catch {
            XCTAssertTrue(
                error is CommitOutboxStoreError,
                "abort must be attributed to the fail-closed outbox persist, got \(error)"
            )
        }

        // The load-bearing assertion: the commit never reached the DS.
        XCTAssertEqual(
            RecordingURLProtocol.startedRequestCount, 0,
            "the DS publish must NOT be attempted after a failed outbox persist"
        )
    }

    // MARK: - (F1) The abort RESETS the in-memory group to epoch N

    /// The pieces an F1 reset test asserts against — a real single-member founder
    /// group (its role table seeded by `Group.create`, so `setMemberAdmin` crafts a
    /// real `setGroupRoles` commit) rehydrated into a repo with an injectable
    /// commit outbox + recording DS.
    private struct Fixture {
        let repo: ChatRepository
        let groupIdHex: String
        let accountIdB64: String
        let otherKey: Data
        let snapshotStore: IdentitySnapshotStore
    }

    private func makeFounderRepo(
        outbox: CommitOutboxStore,
        discovery: DiscoveryClient = DiscoveryClient()
    ) throws -> Fixture {
        let seed = Data(repeating: 0x11, count: 32)
        let founder = try Identity.fromSeed(seed: seed)
        let group = try Group.create(identity: founder)
        let groupId = try group.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)
        let snapshotBlob = try founder.snapshotStorage()

        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let accountIdB64 = base64Encode(accountIdBytes)

        let snapshotStore = IdentitySnapshotStore()
        let knownGroupsStore = KnownGroupsStore(
            defaults: UserDefaults(suiteName: "commit-outbox-f1-\(UUID().uuidString)")!
        )
        // Pre-seed the restore path so `bootstrap` rehydrates the founder group
        // (at epoch 0) — this durable snapshot is what a reset rolls back TO.
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshotBlob)
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)

        let repo = ChatRepository(
            chatClient: ChatClient(),
            dsClient: makeRecordingDs(),
            discovery: discovery,
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: outbox
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
        XCTAssertEqual(
            repo.memberCount(groupIdHex: groupIdHex), 1,
            "founder group must rehydrate for the commit path to reach the persist gate"
        )
        let otherKey = try Identity.fromSeed(seed: Data(repeating: 0x33, count: 32))
            .signaturePublicKey()
        return Fixture(
            repo: repo,
            groupIdHex: groupIdHex,
            accountIdB64: accountIdB64,
            otherKey: otherKey,
            snapshotStore: snapshotStore
        )
    }

    /// F1 — after a commit persist-ABORT, the in-memory group is RESET to the
    /// durable snapshot (epoch N=0), NOT left at the eager-merged N+1. Otherwise a
    /// second same-session commit would build on the un-published N+1 and publish
    /// commit(N+1 -> N+2), which peers still at N drop as wrong-epoch — stranding
    /// the whole group (the "device-ahead" outcome worse than the brick).
    func testPersistAbortResetsInMemoryGroupToEpochN() async throws {
        // `rootDir: nil` -> `putNewDurable` throws on the entry write, so the
        // FAIL-CLOSED persist aborts the commit AFTER the eager merge.
        let fx = try makeFounderRepo(outbox: CommitOutboxStore(rootDir: nil))
        defer { fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64) }

        // Pre-condition: a freshly created + rehydrated founder group is at epoch 0.
        XCTAssertEqual(fx.repo.epoch(groupIdHex: fx.groupIdHex), 0)

        do {
            _ = try await fx.repo.setMemberAdmin(
                groupIdHex: fx.groupIdHex,
                memberSignaturePubkey: fx.otherKey,
                makeAdmin: true
            )
            XCTFail("setMemberAdmin must throw when the outbox persist fails")
        } catch {
            XCTAssertTrue(
                error is CommitOutboxStoreError,
                "abort must be attributed to the fail-closed outbox persist, got \(error)"
            )
        }

        // THE F1 ASSERTION: the eager merge advanced the in-memory group to epoch
        // 1, but the abort ROLLED IT BACK to the durable N (0).
        XCTAssertEqual(
            fx.repo.epoch(groupIdHex: fx.groupIdHex), 0,
            "in-memory group must reset to N (0) after a persist-abort, not stay at the un-published N+1 (1)"
        )

        // A SECOND commit attempt in the SAME session still builds on N: it aborts
        // the same way and the group stays at 0 — it never reaches the stranding
        // N+1 -> N+2 that the bug produced.
        do {
            _ = try await fx.repo.setMemberAdmin(
                groupIdHex: fx.groupIdHex,
                memberSignaturePubkey: fx.otherKey,
                makeAdmin: true
            )
            XCTFail("second setMemberAdmin must also throw")
        } catch {
            XCTAssertTrue(error is CommitOutboxStoreError)
        }
        XCTAssertEqual(
            fx.repo.epoch(groupIdHex: fx.groupIdHex), 0,
            "a second same-session commit must still build on N (0), never on a stranded N+1"
        )
    }

    /// F1 — the reset fires ONLY on the persist-abort. When the persist SUCCEEDS
    /// (durable N+1 + a re-publishable outbox entry) and only the PUBLISH fails,
    /// the in-memory group correctly STAYS at N+1 — the restart sweep re-publishes
    /// the recorded commit, and rolling back here would strand the group instead.
    func testPersistSuccessButPublishFailureKeepsGroupAtEpochNPlus1() async throws {
        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("commit-outbox-f1-\(UUID().uuidString)")
        let outbox = CommitOutboxStore(rootDir: outboxDir)
        let fx = try makeFounderRepo(outbox: outbox)
        defer {
            fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64)
            try? FileManager.default.removeItem(at: outboxDir)
        }

        XCTAssertEqual(fx.repo.epoch(groupIdHex: fx.groupIdHex), 0)

        // The recording DS points at 127.0.0.1:9 (connection refused): the persist
        // SUCCEEDS, then `publishGroup` throws.
        do {
            _ = try await fx.repo.setMemberAdmin(
                groupIdHex: fx.groupIdHex,
                memberSignaturePubkey: fx.otherKey,
                makeAdmin: true
            )
            XCTFail("setMemberAdmin must throw on the DS publish failure")
        } catch {
            XCTAssertFalse(
                error is CommitOutboxStoreError,
                "the failure must be the DS publish, not the outbox persist, got \(error)"
            )
        }

        // The group STAYS at N+1 (1) — the reset must NOT fire on a publish failure
        // (the commit is durably recorded; the sweep re-publishes it on restart).
        XCTAssertEqual(
            fx.repo.epoch(groupIdHex: fx.groupIdHex), 1,
            "a persist-success / publish-fail must leave the group at N+1 for the restart sweep"
        )

        // And the durable outbox holds exactly one un-acked entry at target epoch 1,
        // ready for the idempotent restart re-publish.
        let entries = outbox.loadUnacked(accountIdB64: fx.accountIdB64)
        XCTAssertEqual(entries.count, 1, "the persisted-but-unpublished commit must be recorded for re-publish")
        XCTAssertEqual(entries.first?.targetEpoch, "1")
        XCTAssertEqual(entries.first?.acked, false)
    }

    // MARK: - (F1 residual) An ARTIFACT-compute throw after the merge also resets

    /// F1 pre-persist residual — on the two-artifact add path the SECOND artifact
    /// (the Welcome mailbox pseudonym) is computed AFTER the core
    /// `addMemberFromWire` eager-merges epoch N -> N+1 but BEFORE any publish. If
    /// that compute throws it must ALSO roll the in-memory group back to N — the
    /// same "device-ahead" hazard as a persist-abort, just a rarer path. Before
    /// the widened reset guard this throw aborted WITHOUT resetting, leaving the
    /// group at the un-published N+1.
    ///
    /// This drives a REAL add: a stub discovery feeds a valid KeyPackage (so
    /// `fetchKeyPackage` succeeds and `addMemberFromWire` eager-merges to epoch 1),
    /// then a MALFORMED `recipientSignaturePubkey` (3 bytes, not 32) makes
    /// `IdentityPseudonym.fromIdentity` throw — the second-artifact compute, NOT
    /// the persist (a WORKING outbox is supplied, and its remaining-empty state
    /// proves the persist was never reached). The group must reset to N (0).
    func testAddPathArtifactComputeThrowAfterMergeResetsToEpochN() async throws {
        // A real member identity + its KeyPackage — what `addMemberFromWire`
        // consumes to eager-merge the founder group to epoch 1.
        let member = try Identity.fromSeed(seed: Data(repeating: 0x55, count: 32))
        let keyPackageBytes = try member.createKeyPackage()

        // Stub discovery: answer `GET /discovery/keypackage/...` with a 200 whose
        // body carries the base64 KeyPackage, so `fetchKeyPackage` returns it.
        StubKeyPackageURLProtocol.responseBody = try JSONEncoder().encode(
            KeyPackageFetchResponse(keypackage: base64Encode(keyPackageBytes))
        )
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StubKeyPackageURLProtocol.self]
        let discovery = DiscoveryClient(
            baseURL: "http://127.0.0.1:9",
            session: URLSession(configuration: config),
            antiCensorship: .disarmed
        )

        // A WORKING outbox (temp dir): if the persist ran it would SUCCEED (leaving
        // an entry). The throw must come earlier, at the pseudonym compute, so the
        // outbox stays empty.
        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("commit-outbox-f1-artifact-\(UUID().uuidString)")
        let outbox = CommitOutboxStore(rootDir: outboxDir)
        let fx = try makeFounderRepo(outbox: outbox, discovery: discovery)
        defer {
            fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64)
            try? FileManager.default.removeItem(at: outboxDir)
        }

        // Pre-condition: the rehydrated founder group is at epoch 0 (= N).
        XCTAssertEqual(fx.repo.epoch(groupIdHex: fx.groupIdHex), 0)

        // A valid 16-byte recipient account id (so `fetchKeyPackage`'s length gate
        // passes) but a MALFORMED 3-byte signature pubkey (so the pseudonym
        // derivation — the second artifact — throws AFTER the eager merge).
        let recipientAccountId = Data(repeating: 0x66, count: 16)
        let malformedPubkey = Data([0x01, 0x02, 0x03])

        do {
            _ = try await fx.repo.addMemberToGroup(
                groupIdHex: fx.groupIdHex,
                recipientAccountId: recipientAccountId,
                recipientSignaturePubkey: malformedPubkey
            )
            XCTFail("addMemberToGroup must throw when the second-artifact pseudonym compute fails")
        } catch {
            // The failure is the artifact compute, NOT the outbox persist.
            XCTAssertFalse(
                error is CommitOutboxStoreError,
                "the failure must be the pseudonym compute, not the outbox persist, got \(error)"
            )
        }

        // THE RESIDUAL ASSERTION: the eager merge advanced the in-memory group to
        // epoch 1, but the artifact-compute throw ROLLED IT BACK to the durable N (0).
        XCTAssertEqual(
            fx.repo.epoch(groupIdHex: fx.groupIdHex), 0,
            "in-memory group must reset to N (0) after an artifact-compute throw, not stay at the un-published N+1 (1)"
        )

        // The persist was never reached — the outbox holds no entry (confirming the
        // throw was the second-artifact compute, strictly before the persist).
        XCTAssertEqual(
            outbox.loadUnacked(accountIdB64: fx.accountIdB64).count, 0,
            "the outbox must be empty — the throw preceded the persist, so nothing was recorded"
        )

        // And a SECOND same-session commit still builds on N (0): with the malformed
        // pubkey it throws the same way and the group never reaches the stranding
        // N+1 -> N+2.
        do {
            _ = try await fx.repo.addMemberToGroup(
                groupIdHex: fx.groupIdHex,
                recipientAccountId: recipientAccountId,
                recipientSignaturePubkey: malformedPubkey
            )
            XCTFail("second addMemberToGroup must also throw")
        } catch {
            XCTAssertFalse(error is CommitOutboxStoreError)
        }
        XCTAssertEqual(
            fx.repo.epoch(groupIdHex: fx.groupIdHex), 0,
            "a second same-session commit must still build on N (0), never on a stranded N+1"
        )
    }
}

/// Answers every request with a fixed 200 + a JSON body set in
/// ``responseBody`` — used to feed `DiscoveryClient.fetchKeyPackage` a real
/// KeyPackage so the add path reaches its eager merge (after which a malformed
/// recipient signature pubkey makes the second-artifact compute throw).
final class StubKeyPackageURLProtocol: URLProtocol {

    private static let lock = NSLock()
    private static var _responseBody = Data()

    static var responseBody: Data {
        get { lock.lock(); defer { lock.unlock() }; return _responseBody }
        set { lock.lock(); _responseBody = newValue; lock.unlock() }
    }

    override class func canInit(with request: URLRequest) -> Bool { true }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url,
              let response = HTTPURLResponse(
                url: url,
                statusCode: 200,
                httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"]
              )
        else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: StubKeyPackageURLProtocol.responseBody)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}
