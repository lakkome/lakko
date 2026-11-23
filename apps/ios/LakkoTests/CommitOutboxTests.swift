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
// MARK: - (1) A fail-closed durable record aborts the SWEEP before any publish

/// Drives a REAL founder MLS group through the ADR-0045 committer sweep with
/// the commit outbox pointed at an UNAVAILABLE store.
///
/// ## Why the vehicle moved
///
/// This class used to drive `ChatRepository.setMemberAdmin`, because that was a
/// direct-commit path with an ADR-0040 fail-closed durable record in front of
/// its publish. ADR-0045 turned it into a **proposal**: a proposal advances no
/// epoch and produces nothing to re-publish, so it has no outbox entry, and
/// asserting one would be asserting behaviour that no longer exists. Every group
/// `Group.create` produces carries an ADR-0022 role table, so the same is true
/// of add and remove — the epoch-advancing commit is now the sweep, and the
/// sweep is where the fail-closed record lives.
///
/// The property under test is unchanged and still ADR-0040's: **no epoch
/// advance reaches the wire without a durable, re-publishable record of the
/// commit that caused it**, and a failed record ABORTS with the group rolled
/// back rather than half-applied.
@MainActor
final class CommitOutboxCommitPathAbortTests: XCTestCase {

    /// A DS double that answers a journal FETCH with an empty batch and a
    /// PUBLISH with a valid receipt, counting each separately.
    ///
    /// The sweep needs both: obligation 3's stand-down re-read is a fetch, and
    /// an empty answer is what lets it proceed to stage. Counting them apart is
    /// what makes "the publish was not attempted" assertable at all — a single
    /// request counter would be satisfied by the fetch alone.
    final class SweepDsURLProtocol: URLProtocol {
        private static let lock = NSLock()
        private static var _fetches = 0
        private static var _publishes = 0
        private static var _nextId: Int64 = 1
        /// When true, every publish fails — the "record succeeded, publish did
        /// not" half of the pair.
        static var failPublishes = false

        static func reset() {
            lock.lock(); defer { lock.unlock() }
            _fetches = 0
            _publishes = 0
            _nextId = 1
            failPublishes = false
        }

        static var fetchCount: Int { lock.lock(); defer { lock.unlock() }; return _fetches }
        static var publishCount: Int { lock.lock(); defer { lock.unlock() }; return _publishes }

        override class func canInit(with request: URLRequest) -> Bool { true }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

        override func startLoading() {
            let isPublish = request.httpMethod == "POST"
            Self.lock.lock()
            if isPublish { Self._publishes += 1 } else { Self._fetches += 1 }
            let id = Self._nextId
            if isPublish { Self._nextId += 1 }
            let fail = isPublish && Self.failPublishes
            Self.lock.unlock()

            if fail {
                client?.urlProtocol(self, didFailWithError: URLError(.notConnectedToInternet))
                return
            }
            let json = isPublish
                ? "{\"id\":\(id),\"created_at\":\"2026-08-25T10:00:00.000Z\"}"
                : "{\"messages\":[],\"has_more\":false}"
            let response = HTTPURLResponse(
                url: request.url!,
                statusCode: 200,
                httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"]
            )!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data(json.utf8))
            client?.urlProtocolDidFinishLoading(self)
        }

        override func stopLoading() {}
    }

    private func makeSweepDs() -> DsClient {
        SweepDsURLProtocol.reset()
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [SweepDsURLProtocol.self]
        return DsClient(baseURL: "http://127.0.0.1:9", session: URLSession(configuration: config))
    }

    private struct Fixture {
        let repo: ChatRepository
        let groupIdHex: String
        let accountIdB64: String
        let snapshotStore: IdentitySnapshotStore
        let promotableKey: Data
    }

    /// A real founder group with one other member, the person the role change
    /// names admin (C-100) (its role table seeded by
    /// `Group.create`, so this device is the ADR-0022 owner and therefore
    /// committer rank 0) rehydrated into a repo whose outbox is `outboxDir`.
    /// `nil` makes every durable write fail closed.
    private func makeFixture(outboxDir: URL?) throws -> Fixture {
        let seed = Data(repeating: 0x11, count: 32)
        let founder = try Identity.fromSeed(seed: seed)
        let group = try Group.create(identity: founder, kind: .group)
        // C-100: the person a role change names admin must be a member.
        _ = try group.addMemberFromWire(
            identity: founder,
            keyPackage: try Identity.fromSeed(seed: Data(repeating: 0x33, count: 32)).createKeyPackage())
        let groupIdHex = DsClient.pseudonymHex(try group.groupId())
        let snapshotBlob = try founder.snapshotStorage()

        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let accountIdB64 = base64Encode(accountIdBytes)

        let snapshotStore = IdentitySnapshotStore()
        let knownGroupsStore = KnownGroupsStore(
            defaults: UserDefaults(suiteName: "commit-outbox-test-\(UUID().uuidString)")!
        )
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshotBlob)
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)

        let repo = ChatRepository(
            chatClient: Offline.chatClient(),
            dsClient: makeSweepDs(),
            discovery: Offline.discovery(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: outboxDir),
            pushRouting: Offline.pushRouting(),
            beaconStore: BeaconObservationStore(
                defaults: UserDefaults(suiteName: "cob-\(UUID().uuidString)")!),
            pendingRequestStore: PendingRequestStore(
                defaults: UserDefaults(suiteName: "cop-\(UUID().uuidString)")!)
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
        XCTAssertEqual(
            repo.memberCount(groupIdHex: groupIdHex), 2,
            "founder group must rehydrate for the sweep to reach the record gate"
        )
        XCTAssertEqual(
            repo.ownCommitterRank(groupIdHex: groupIdHex), 0,
            "the founder is the ADR-0022 owner, hence committer rank 0 — if this were nil the "
            + "sweep would short-circuit as `notCommitter` and every assertion below would be "
            + "vacuously true"
        )
        return Fixture(
            repo: repo,
            groupIdHex: groupIdHex,
            accountIdB64: accountIdB64,
            snapshotStore: snapshotStore,
            promotableKey: try Identity.fromSeed(seed: Data(repeating: 0x33, count: 32))
                .signaturePublicKey()
        )
    }

    /// Publish one role-change PROPOSAL so the sweep has something to stage.
    /// A proposal touches no outbox, so this works even against the unavailable
    /// store the abort cases use.
    private func seedPendingProposal(_ fx: Fixture) async throws {
        let outcome = try await fx.repo.setMemberAdmin(
            groupIdHex: fx.groupIdHex,
            memberSignaturePubkey: fx.promotableKey,
            makeAdmin: true
        )
        guard case .proposed = outcome else {
            return XCTFail("ADR-0045 routes a role change on a role-table group through a proposal")
        }
        XCTAssertEqual(
            fx.repo.pendingProposals(groupIdHex: fx.groupIdHex).filter({ $0.stored }).count, 1,
            "the sweep needs an authorised pending proposal or it short-circuits"
        )
    }

    // MARK: - (1) The record fails -> no publish

    func testDurableRecordFailureAbortsTheSweepBeforePublish() async throws {
        // An UNAVAILABLE outbox: every durable write throws.
        let fx = try makeFixture(outboxDir: nil)
        defer { fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64) }
        try await seedPendingProposal(fx)

        SweepDsURLProtocol.reset()
        let outcome = await fx.repo.sweepIfCommitter(groupIdHex: fx.groupIdHex)

        // THE LOAD-BEARING ASSERTION: the commit never reached the DS.
        XCTAssertEqual(
            SweepDsURLProtocol.publishCount, 0,
            "the sweep commit must NOT be published after a failed durable record — "
            + "ADR-0040's whole point is that no epoch advance reaches the wire without a "
            + "re-publishable record of the commit that caused it"
        )
        XCTAssertGreaterThan(
            SweepDsURLProtocol.fetchCount, 0,
            "the stand-down re-read must still have run, else this test proves nothing about "
            + "the record gate — it would only prove the sweep never started"
        )
        // The repository swallows a sweep failure (a sweep failure is not a user
        // failure), so it is reported rather than thrown — and reported as
        // `unresolved`, not `noVerdictOwed`: something was left unfinished,
        // which is the opposite of there having been nothing to do.
        XCTAssertEqual(outcome, .unresolved)
    }

    // MARK: - (2) The abort ROLLS BACK: no staged sweep, epoch unmoved

    func testDurableRecordFailureLeavesNoStagedSweepAndNoEpochAdvance() async throws {
        let fx = try makeFixture(outboxDir: nil)
        defer { fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64) }
        try await seedPendingProposal(fx)
        let epochBefore = fx.repo.epoch(groupIdHex: fx.groupIdHex)

        SweepDsURLProtocol.reset()
        _ = await fx.repo.sweepIfCommitter(groupIdHex: fx.groupIdHex)

        XCTAssertEqual(
            fx.repo.epoch(groupIdHex: fx.groupIdHex), epochBefore,
            "an aborted sweep must leave the group exactly where it was"
        )
        // The rollback is per-group `abandonStagedSweep`, and it matters that it
        // RAN: a staged sweep is durable and freezes this group's own membership
        // operations until a verdict is reached, so leaving one behind after an
        // abort that published nothing would wedge membership for nothing.
        XCTAssertFalse(
            fx.repo.hasStagedSweep(groupIdHex: fx.groupIdHex),
            "the fail-closed abort must abandon the staged sweep, or membership on this group "
            + "stays frozen across relaunches with no commit on the wire to justify it"
        )
        XCTAssertEqual(
            fx.repo.pendingProposals(groupIdHex: fx.groupIdHex).filter({ $0.stored }).count, 1,
            "abandoning keeps the proposals pending — nothing the user asked for is lost"
        )
    }

    // MARK: - (3) Record succeeds, publish fails -> the verdict stays OWED

    /// The ADR-0045 counterpart of ADR-0040's "persist-success / publish-fail
    /// leaves the group at N+1 for the restart sweep".
    ///
    /// The sweep's answer is deliberately different, and the difference is the
    /// whole point of staging: the group stays at **N**, because a staged sweep
    /// has not merged. What must survive is the durable entry, flagged
    /// `sweepVerdictPending` so ADR-0040's re-publish loop leaves it alone and
    /// `resolveOwedVerdict` owns it instead — re-publishing it as a direct-path
    /// entry would push its Welcome onto the wire before any verdict, which
    /// ADR-0045 obligation 2 forbids.
    func testRecordSucceedsButPublishFailsLeavesTheVerdictOwed() async throws {
        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("commit-outbox-sweep-\(UUID().uuidString)")
        let fx = try makeFixture(outboxDir: outboxDir)
        defer {
            fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64)
            try? FileManager.default.removeItem(at: outboxDir)
        }
        try await seedPendingProposal(fx)
        let epochBefore = fx.repo.epoch(groupIdHex: fx.groupIdHex)

        SweepDsURLProtocol.reset()
        SweepDsURLProtocol.failPublishes = true
        _ = await fx.repo.sweepIfCommitter(groupIdHex: fx.groupIdHex)

        XCTAssertEqual(
            fx.repo.epoch(groupIdHex: fx.groupIdHex), epochBefore,
            "a STAGED sweep has not merged — the group stays at N until the journal verdict"
        )
        XCTAssertTrue(
            fx.repo.hasStagedSweep(groupIdHex: fx.groupIdHex),
            "the sweep must stay staged: its commit may or may not have reached the wire, and "
            + "both merging and abandoning on an unread verdict are wrong"
        )
        let entries = CommitOutboxStore(rootDir: outboxDir)
            .loadUnacked(accountIdB64: fx.accountIdB64)
        XCTAssertEqual(entries.count, 1, "the commit must be durably recorded for the retry")
        XCTAssertEqual(
            entries.first?.sweepVerdictPending, true,
            "a staged-sweep entry must be flagged so ADR-0040's re-publish loop skips it — that "
            + "loop would publish its Welcome before the verdict, which obligation 2 forbids"
        )
        XCTAssertEqual(entries.first?.acked, false)
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
