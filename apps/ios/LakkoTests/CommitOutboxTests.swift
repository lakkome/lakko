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
}
