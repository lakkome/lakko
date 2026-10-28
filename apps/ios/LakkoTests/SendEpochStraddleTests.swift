// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// The SEND path is serialized against the regions that move the group's epoch.
/// iOS mirror of the web `chat/send-epoch-straddle.test.ts`.
///
/// ## The hazard, stated precisely
///
/// It is NOT "two `createApplicationMessage` calls interleave". They cannot: the
/// UniFFI call is synchronous and `ChatRepository` is `@MainActor`, so no second
/// caller observes the core mid-call. And it is NOT "two application messages
/// are published out of order" — within one epoch MLS orders by generation, not
/// by journal position, so a reordered pair of same-epoch messages both decrypt.
///
/// The hazard is that an application message can be SEALED at one epoch and
/// PUBLISHED on the far side of a commit that changes it. `lakko-mls` never
/// calls `MlsGroupCreateConfig::max_past_epochs`, so it is openmls's default of
/// **0**: a peer keeps NO past-epoch secrets, and a straddling message is not
/// late, it is **permanently undecryptable for every recipient** — dropped
/// silently, with the sender's own optimistic echo still on screen.
///
/// ADR-0045 makes one direction of this a **certainty rather than a race**. The
/// committer sweep STAGES its commit (the group stays at N), publishes it, and
/// then spends a whole DS round-trip reading the journal verdict before merging.
/// `MlsGroup::create_message` gates on `is_active()`, not `is_operational()`, so
/// a pending commit does NOT block a send: for the entire verdict window a send
/// seals at N with the commit already on the wire, and therefore necessarily
/// publishes after it. Nothing about that window is a race.
///
/// ## The oracle
///
/// ``replayAsPeer(_:)`` is the assertion, and it is not a hand-written
/// expectation of call order: a REAL second MLS member — the one this fixture
/// added to the group — replays the recorded journal in DS id order exactly as a
/// recipient does, and the test reports what that recipient never sees. A test
/// that merely pinned an ordering would keep passing if the ordering changed for
/// a good reason; this one only passes when the journal is *applyable*, which is
/// the property that actually matters. `max_past_epochs = 0` is not simulated
/// here: it is whatever `lakko-mls` really configures, so if that ever changed
/// this oracle would change with it.
///
/// ## Which of these discriminate (measured by RUNNING the break, quoted
/// verbatim — not predicted)
///
/// Removing `withCommitLock` from `ChatRepository.sendMessage` and
/// `publishApplicationMessage` (i.e. calling `sealAndPublishLocked` directly):
///   - `testASendDoesNotSealBehindAStagedSweepsPublishedCommit` FAILS with
///     `XCTAssertEqual failed: ("["row 4 (epoch 1)"]") is not equal to ("[]") -
///     a real second member replayed the journal and never saw these rows`;
///   - `testAControlBroadcastDoesNotSealBehindAStagedSweepsPublishedCommit`
///     FAILS identically.
/// Row 4 is the application message: sealed at epoch 1 while the sweep was
/// staged, published after the commit that took the peer to epoch 2, and
/// therefore gone for good — not late.
///
/// Re-keying the chain globally (`let groupIdHex = "GLOBAL"` inside
/// `withCommitLock`):
///   - `testAnUnrelatedGroupsSendIsNotBlocked` FAILS with
///     `failed - DEADLOCK: send on an unrelated group`, while the two cases
///     above keep passing — which is exactly why the per-group case has to
///     exist separately.
@MainActor
final class SendEpochStraddleTests: XCTestCase {

    // MARK: - A DS double that models arrival order and can hold one request

    /// Assigns journal ids at RESPONSE time and records the payloads in that
    /// order, which is what a real DS does: the row exists once the request is
    /// processed, not when it was dispatched. `holdFetches` parks every GET
    /// until released — the sweep's verdict read is a GET, and holding it is how
    /// the staged-sweep window is widened enough to drive a send into it.
    final class JournalURLProtocol: URLProtocol {
        private static let lock = NSLock()
        private static var _journal: [(id: Int64, body: Data)] = []
        private static var _nextId: Int64 = 0
        private static var _fetchCount = 0
        private static var _heldFetchCount = 0
        private static var _holdFromFetch = Int.max
        private static var _failPublishes = false
        /// The host of the current fixture's DS. A repository left over from
        /// an earlier test (its background work, e.g. a push-routing broadcast
        /// that lands after that test finished) talks to its own host, and its
        /// publishes must never reach this test's journal.
        private static var _host = ""

        static func reset(host: String = "") {
            lock.lock(); defer { lock.unlock() }
            _host = host
            _journal.removeAll()
            _nextId = 0
            _fetchCount = 0
            _heldFetchCount = 0
            _holdFromFetch = Int.max
            _failPublishes = false
        }

        /// Make every subsequent POST answer 503 and record nothing — a publish
        /// that never reached the wire. Used by the local-apply ordering case:
        /// the timeline must not move for a message no peer will ever see.
        static func failPublishes(_ on: Bool) {
            lock.lock(); _failPublishes = on; lock.unlock()
        }

        static var journal: [(id: Int64, body: Data)] {
            lock.lock(); defer { lock.unlock() }
            return _journal
        }

        /// Park every GET from the `n`-th one onward.
        ///
        /// Indexed rather than a bare flag because the sweep fetches TWICE — the
        /// stand-down re-read, then the verdict read — and only the second one
        /// opens the window under test. Toggling a flag between them would be a
        /// race against the code the test is trying to observe.
        static func holdFetches(fromNumber n: Int) {
            lock.lock(); _holdFromFetch = n; lock.unlock()
        }

        static func releaseFetches() {
            lock.lock(); _holdFromFetch = Int.max; lock.unlock()
        }

        private static var heldFetchCount: Int {
            lock.lock(); defer { lock.unlock() }
            return _heldFetchCount
        }

        /// Wait ASYNCHRONOUSLY for a held GET to arrive.
        ///
        /// It must not block: `ChatRepository` is `@MainActor` and so is this
        /// test, so a synchronous semaphore wait here would hold the main actor
        /// and the sweep could never reach the fetch it is waiting for. That is
        /// not a hypothetical — it is what the first draft of this file did, and
        /// every case timed out.
        static func awaitHeldFetch(timeout: TimeInterval = 5) async -> Bool {
            let deadline = Date().addingTimeInterval(timeout)
            while Date() < deadline {
                if heldFetchCount > 0 { return true }
                try? await Task.sleep(nanoseconds: 20_000_000)
            }
            return false
        }

        override class func canInit(with request: URLRequest) -> Bool { true }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

        override func startLoading() {
            let isPublish = request.httpMethod == "POST"
            // ADR-0057 statements (a registration, a probe, a registrar set, a
            // push binding) are PUTs: answered 204 and never counted as a
            // journal read, so they cannot take the verdict read's place in
            // `holdFetches(fromNumber:)`. A stale fixture's request is
            // answered the same way and recorded nowhere.
            Self.lock.lock()
            let stale = !Self._host.isEmpty && request.url?.host != Self._host
            Self.lock.unlock()
            if stale || request.httpMethod == "PUT" {
                let response = HTTPURLResponse(
                    url: request.url!, statusCode: 204, httpVersion: "HTTP/1.1", headerFields: [:])!
                client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                client?.urlProtocolDidFinishLoading(self)
                return
            }
            // ADR-0081 — a commit reaches the journal through
            // `POST .../commit` as `{"statement_b64", "row_b64"}`: the DS
            // appends the ROW (and registers its epoch) in one step, so the
            // journal records the row, not the request body. A body that is
            // not such a request is answered 400 and records nothing.
            var body = Self.bodyData(of: request) ?? Data()
            if isPublish, request.url?.lastPathComponent == "commit" {
                guard let row = Self.commitRow(of: body) else {
                    let response = HTTPURLResponse(
                        url: request.url!, statusCode: 400, httpVersion: "HTTP/1.1",
                        headerFields: ["Content-Type": "text/plain"])!
                    client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                    client?.urlProtocol(self, didLoad: Data("body must carry row_b64".utf8))
                    client?.urlProtocolDidFinishLoading(self)
                    return
                }
                body = row
            }
            // Never park on `startLoading`'s own queue: `URLSession` may invoke
            // it on a shared internal queue, so blocking there would serialize
            // the requests itself and make the whole suite vacuous.
            DispatchQueue.global().async { [weak self] in
                guard let self else { return }
                let json: String
                if isPublish {
                    Self.lock.lock()
                    let refuse = Self._failPublishes
                    var id: Int64 = 0
                    if !refuse {
                        Self._nextId += 1
                        id = Self._nextId
                        Self._journal.append((id, body))
                    }
                    Self.lock.unlock()
                    if refuse {
                        let response = HTTPURLResponse(
                            url: self.request.url!,
                            statusCode: 503,
                            httpVersion: "HTTP/1.1",
                            headerFields: ["Content-Type": "text/plain"]
                        )!
                        self.client?.urlProtocol(
                            self, didReceive: response, cacheStoragePolicy: .notAllowed)
                        self.client?.urlProtocol(self, didLoad: Data("down".utf8))
                        self.client?.urlProtocolDidFinishLoading(self)
                        return
                    }
                    json = "{\"id\":\(id),\"created_at\":\"2026-08-25T10:00:00.000Z\"}"
                } else {
                    Self.lock.lock()
                    Self._fetchCount += 1
                    let hold = Self._fetchCount >= Self._holdFromFetch
                    if hold { Self._heldFetchCount += 1 }
                    Self.lock.unlock()
                    if hold {
                        let deadline = Date().addingTimeInterval(10)
                        while Date() < deadline {
                            Self.lock.lock()
                            let stillHeld = Self._holdFromFetch != Int.max
                            Self.lock.unlock()
                            if !stillHeld { break }
                            Thread.sleep(forTimeInterval: 0.01)
                        }
                        Self.lock.lock(); Self._heldFetchCount -= 1; Self.lock.unlock()
                    }
                    // Always an empty batch: the journal holds only this device's
                    // own rows, and re-feeding them to the sweep would exercise
                    // the echo filter rather than the ordering property.
                    json = "{\"messages\":[],\"has_more\":false}"
                }
                let response = HTTPURLResponse(
                    url: self.request.url!,
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

        /// The commit row of a `POST .../commit` body (ADR-0081).
        private static func commitRow(of body: Data) -> Data? {
            guard let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
                  json["statement_b64"] is String,
                  let rowB64 = json["row_b64"] as? String,
                  let row = Data(base64Encoded: rowB64), !row.isEmpty
            else { return nil }
            return row
        }

        /// `URLProtocol` strips `httpBody` when the request was built from a
        /// stream, so fall back to draining `httpBodyStream`.
        private static func bodyData(of request: URLRequest) -> Data? {
            if let body = request.httpBody { return body }
            guard let stream = request.httpBodyStream else { return nil }
            stream.open()
            defer { stream.close() }
            var data = Data()
            let size = 4096
            var buffer = [UInt8](repeating: 0, count: size)
            while stream.hasBytesAvailable {
                let read = stream.read(&buffer, maxLength: size)
                if read <= 0 { break }
                data.append(contentsOf: buffer[0..<read])
            }
            return data
        }
    }

    // MARK: - Fixture

    private struct Fixture {
        let repo: ChatRepository
        let groupIdHex: String
        let accountIdB64: String
        let snapshotStore: IdentitySnapshotStore
        let outboxDir: URL
        /// A REAL second member of the same group, and the oracle: it replays
        /// the recorded journal exactly as a recipient does.
        let peer: Group
        let peerIdentity: Identity
        let promotableKey: Data
    }

    private func makeFixture() throws -> Fixture {
        let seed = Data(repeating: 0x11, count: 32)
        let founder = try Identity.fromSeed(seed: seed)
        let other = try Identity.fromSeed(seed: Data(repeating: 0x55, count: 32))
        let group = try Group.create(identity: founder, kind: .group)
        // The founder is one-of-one before this, so the commit has no audience
        // and is dropped — exactly what `startGroupWith` does.
        let added = try group.addMemberFromWire(
            identity: founder, keyPackage: try other.createKeyPackage())
        let peer = try Group.joinFromWelcome(welcome: added.welcome, identity: other)
        let groupIdHex = DsClient.pseudonymHex(try group.groupId())
        XCTAssertEqual(
            try peer.epoch(), try group.epoch(),
            "the peer must start at the same epoch or every replay below is meaningless"
        )
        let snapshotBlob = try founder.snapshotStorage()

        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let accountIdB64 = base64Encode(accountIdBytes)

        let snapshotStore = IdentitySnapshotStore()
        let knownGroupsStore = KnownGroupsStore(
            defaults: UserDefaults(suiteName: "straddle-\(UUID().uuidString)")!)
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshotBlob)
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)

        let host = "fx-\(UUID().uuidString.lowercased()).invalid"
        JournalURLProtocol.reset(host: host)
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [JournalURLProtocol.self]
        let session = URLSession(configuration: config)
        let ds = DsClient(baseURL: "http://\(host)", session: session)

        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("straddle-\(UUID().uuidString)")

        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            discovery: Offline.discovery(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: outboxDir),
            pushRouting: Offline.pushRouting(),
            beaconStore: BeaconObservationStore(
                defaults: UserDefaults(suiteName: "straddle-b-\(UUID().uuidString)")!),
            pendingRequestStore: PendingRequestStore(
                defaults: UserDefaults(suiteName: "straddle-p-\(UUID().uuidString)")!)
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
        XCTAssertEqual(repo.memberCount(groupIdHex: groupIdHex), 2)
        XCTAssertEqual(
            repo.ownCommitterRank(groupIdHex: groupIdHex), 0,
            "the founder is the ADR-0022 owner, hence committer rank 0 — otherwise the sweep "
            + "short-circuits and no staged window is ever opened"
        )
        return Fixture(
            repo: repo,
            groupIdHex: groupIdHex,
            accountIdB64: accountIdB64,
            snapshotStore: snapshotStore,
            outboxDir: outboxDir,
            peer: peer,
            peerIdentity: other,
            promotableKey: try Identity.fromSeed(seed: Data(repeating: 0x33, count: 32))
                .signaturePublicKey()
        )
    }

    private func tearDown(_ fx: Fixture) {
        fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64)
        try? FileManager.default.removeItem(at: fx.outboxDir)
    }

    /// Replay the recorded journal in DS id order the way a recipient does, and
    /// return a label for every row that recipient never sees.
    ///
    /// This is a REAL `Group` at whatever `max_past_epochs` `lakko-mls` really
    /// configures — nothing about the drop is simulated. A commit applies only
    /// when it was built on the epoch the peer is currently at; an application
    /// message decrypts only at exactly that epoch, because no past secrets are
    /// retained and nothing buffers a future one.
    private func replayAsPeer(_ fx: Fixture) -> [String] {
        var dropped: [String] = []
        for (index, row) in JournalURLProtocol.journal.enumerated() {
            do {
                _ = try fx.peer.processIncomingVerifyingRoles(
                    identity: fx.peerIdentity, bytes: row.body)
            } catch {
                let epoch = epochOfMessage(bytes: row.body).map(String.init) ?? "?"
                dropped.append("row \(index + 1) (epoch \(epoch))")
            }
        }
        return dropped
    }

    private func settle() async {
        try? await Task.sleep(nanoseconds: 120_000_000)
    }

    /// Fail loudly instead of hanging the suite when a lock deadlocks.
    private func within<T: Sendable>(
        _ seconds: Double, _ what: String, _ work: @escaping @Sendable () async -> T
    ) async throws -> T {
        try await withThrowingTaskGroup(of: T?.self) { group in
            group.addTask { await work() }
            group.addTask {
                try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
                return nil
            }
            guard let first = try await group.next() else {
                throw NSError(domain: "straddle", code: 1)
            }
            group.cancelAll()
            guard let value = first else {
                XCTFail("DEADLOCK: \(what)")
                throw NSError(domain: "straddle", code: 2)
            }
            return value
        }
    }

    // MARK: - The staged-sweep window

    /// Seed a pending proposal, then open the staged-sweep window by holding the
    /// sweep's verdict fetch, and drive `send` into it.
    private func driveIntoStagedSweepWindow(
        _ fx: Fixture,
        _ send: @escaping @MainActor () async -> Void
    ) async throws {
        let outcome = try await fx.repo.setMemberAdmin(
            groupIdHex: fx.groupIdHex,
            memberSignaturePubkey: fx.promotableKey,
            makeAdmin: true
        )
        guard case .proposed = outcome else {
            return XCTFail("ADR-0045 routes a role change through a proposal")
        }

        // From here the sweep will: re-read the journal (stand-down), stage,
        // record, publish the commit, then read the VERDICT. Parking that second
        // fetch leaves the commit already on the wire with the group still at N
        // — the window in which a send's loss is a certainty, not a race.
        JournalURLProtocol.holdFetches(fromNumber: 2)
        let hex = fx.groupIdHex
        let repo = fx.repo
        async let swept: SweepOutcome = repo.sweepIfCommitter(groupIdHex: hex)
        let arrived = await JournalURLProtocol.awaitHeldFetch()
        XCTAssertTrue(arrived, "the sweep's verdict read must park, or the window never opens")

        // The staged window is open: commit published, group still at N.
        let sendTask = Task { @MainActor in await send() }
        await settle()
        JournalURLProtocol.releaseFetches()
        let outcome2 = await swept
        guard case .committed = outcome2 else {
            return XCTFail("the sweep must win an empty journal, got \(outcome2)")
        }
        _ = try await within(5, "send queued behind a staged sweep") { await sendTask.value }
    }

    func testASendDoesNotSealBehindAStagedSweepsPublishedCommit() async throws {
        let fx = try makeFixture()
        defer { tearDown(fx) }
        let hex = fx.groupIdHex
        let repo = fx.repo

        try await driveIntoStagedSweepWindow(fx) {
            try? await repo.sendMessage(groupIdHex: hex, plaintext: "hello")
        }

        XCTAssertEqual(
            replayAsPeer(fx), [],
            "a real second member replayed the journal and never saw these rows"
        )
    }

    func testAControlBroadcastDoesNotSealBehindAStagedSweepsPublishedCommit() async throws {
        let fx = try makeFixture()
        defer { tearDown(fx) }
        let hex = fx.groupIdHex
        let repo = fx.repo

        // Same code path, the lower-severity payload: a routing / beacon control
        // envelope. It matters because `emitCommitterBeaconIfDue` fires one every
        // interval on every candidate's open chat screen, so it is by far the
        // most frequent thing that could land in a staged window.
        try await driveIntoStagedSweepWindow(fx) {
            try? await repo.publishApplicationMessage(
                groupIdHex: hex, plaintextBytes: composeCommitterBeacon())
        }

        XCTAssertEqual(
            replayAsPeer(fx), [],
            "a real second member replayed the journal and never saw these rows"
        )
    }

    // MARK: - ADR-0023 message ops (edit / delete / reaction)

    /// The three ADR-0023 ops are application messages like any other, so they
    /// carry the straddle hazard verbatim — and they reached the DS through a
    /// bare `ChatClient.sendMessage`, outside the chain, until this suite grew
    /// these cases. Nothing above covered them: the two cases before this MARK
    /// drive `ChatRepository.sendMessage` and `publishApplicationMessage`, which
    /// were already serialized, so the drift was invisible.
    func testAnEditDoesNotSealBehindAStagedSweepsPublishedCommit() async throws {
        let fx = try makeFixture()
        defer { tearDown(fx) }
        let hex = fx.groupIdHex
        let repo = fx.repo

        try await driveIntoStagedSweepWindow(fx) {
            try? await repo.editMessage(groupIdHex: hex, target: 1, newText: "fixed")
        }

        XCTAssertEqual(
            replayAsPeer(fx), [],
            "a real second member replayed the journal and never saw these rows"
        )
    }

    /// Delete is the sharpest of the three: the sender's local tombstone is
    /// applied on the publish's success path, so a lost op leaves the user
    /// looking at "message deleted" while every peer still renders the message.
    func testADeleteDoesNotSealBehindAStagedSweepsPublishedCommit() async throws {
        let fx = try makeFixture()
        defer { tearDown(fx) }
        let hex = fx.groupIdHex
        let repo = fx.repo

        try await driveIntoStagedSweepWindow(fx) {
            try? await repo.deleteMessage(groupIdHex: hex, target: 1)
        }

        XCTAssertEqual(
            replayAsPeer(fx), [],
            "a real second member replayed the journal and never saw these rows"
        )
    }

    func testAReactionDoesNotSealBehindAStagedSweepsPublishedCommit() async throws {
        let fx = try makeFixture()
        defer { tearDown(fx) }
        let hex = fx.groupIdHex
        let repo = fx.repo

        try await driveIntoStagedSweepWindow(fx) {
            try? await repo.reactToMessage(groupIdHex: hex, target: 1, emoji: "👍")
        }

        XCTAssertEqual(
            replayAsPeer(fx), [],
            "a real second member replayed the journal and never saw these rows"
        )
    }

    /// The local apply's ORDERING, which serializing the op does not by itself
    /// establish: it must stay strictly on the publish's success path.
    ///
    /// This case does not discriminate the lock — it passed before the ops were
    /// serialized, and it is here so that moving the apply inside the region
    /// cannot silently move it ahead of the publish. Measured by running the
    /// inversion, not predicted: applying the tombstone before
    /// `sealAndPublishLocked` fails it.
    ///
    /// The oracle has to be read AFTER a later successful op, and that is not
    /// incidental. `ChatRepositoryState.threads` holds VALUE copies rebuilt by
    /// `buildThreadList()`, and the op's `state` rebuild sits on its success
    /// path — so straight after the failed delete the published snapshot is
    /// simply stale and reports `deleted == false` no matter what the record
    /// says. That is not the property under test: the record's mutation is real
    /// and permanent, and the UI shows it at the very next rebuild (every poll
    /// does one). The second send is what forces that rebuild, so the assertion
    /// reads what the user actually ends up looking at. Reading `state`
    /// immediately instead makes this case vacuous — measured, not assumed:
    /// it passed under the inversion until the second send was added.
    func testADeleteIsNotAppliedLocallyWhenThePublishNeverLanded() async throws {
        let fx = try makeFixture()
        defer { tearDown(fx) }
        let hex = fx.groupIdHex
        let repo = fx.repo

        try await repo.sendMessage(groupIdHex: hex, plaintext: "keep me")
        guard let target = repo.state.threads
            .first(where: { $0.groupIdHex == hex })?.messages.last?.messageId
        else {
            return XCTFail("the send must have appended a row for the delete to target")
        }

        JournalURLProtocol.failPublishes(true)
        do {
            try await repo.deleteMessage(groupIdHex: hex, target: target)
            XCTFail("a delete whose DS publish 503s must throw, not report success")
        } catch {
            // expected — the publish failed, so the op is not on the wire
        }
        JournalURLProtocol.failPublishes(false)

        // Force the thread-list rebuild the failed op skipped, so the assertion
        // below reads the record rather than a pre-delete snapshot of it.
        try await repo.sendMessage(groupIdHex: hex, plaintext: "still here")

        let row = repo.state.threads
            .first(where: { $0.groupIdHex == hex })?
            .messages.first(where: { $0.messageId == target })
        XCTAssertEqual(
            row?.deleted, false,
            "the user is shown a tombstone for a message that never reached the wire, "
            + "and every peer still renders it"
        )
    }

    // MARK: - Per-group keying

    /// Guards the per-group keying: a global lock would deadlock this, because
    /// the busy group's sweep is parked on a held fetch for the whole window.
    func testAnUnrelatedGroupsSendIsNotBlocked() async throws {
        let fx = try makeFixture()
        defer { tearDown(fx) }
        // A second, unrelated group on the same identity + repo.
        let hex = fx.groupIdHex
        let repo = fx.repo

        _ = try await repo.setMemberAdmin(
            groupIdHex: hex, memberSignaturePubkey: fx.promotableKey, makeAdmin: true)
        JournalURLProtocol.holdFetches(fromNumber: 2)
        async let swept: SweepOutcome = repo.sweepIfCommitter(groupIdHex: hex)
        let parked = await JournalURLProtocol.awaitHeldFetch()
        XCTAssertTrue(parked, "the sweep's verdict read must park for this group")

        // A send on a group id the repo does not hold fails its own guard
        // immediately — the point is that it is not BLOCKED behind the busy
        // group's chain, which a global lock would do.
        let elsewhere = String(repeating: "ab", count: 32)
        _ = try await within(3, "send on an unrelated group") {
            try? await repo.publishApplicationMessage(
                groupIdHex: elsewhere, plaintextBytes: Data([0x01]))
            return true
        }

        JournalURLProtocol.releaseFetches()
        _ = await swept
    }
}
