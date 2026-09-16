// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0045 S4b — the committer liveness beacon: the emit gate, the
/// observation store, and the rank-at-verdict-time projection. iOS mirror of
/// the web `chat/committer-beacon.test.ts`.
///
/// The core's own rules (`shouldEmitBeacon`, `actingCommitterVerdict`) are wired
/// in REAL through the FFI wherever a case exercises them, so what is proven is
/// the composition against the core's actual rules rather than against a double
/// that happens to agree with them.
///
/// ## Which of these discriminate (measured by RUNNING the break, quoted
/// verbatim — not predicted)
///
/// Deleting the `if !foreground { return .notForeground }` line from
/// `beaconEmitVerdict` AND the matching early guard in
/// `ChatRepository.emitCommitterBeaconIfDue` (both, because either alone still
/// stops the emit — which is the point of having two):
///   - `testABackgroundedCandidateDoesNotEmit` FAILS with
///     `XCTAssertEqual failed: ("emit") is not equal to ("notForeground") - a
///     beacon means ABLE TO SWEEP, and a backgrounded app cannot: every other
///     candidate would defer to a device that is never going to sweep`;
///   - `testABackgroundedOwnerPublishesNothingAtAll` FAILS with
///     `XCTAssertEqual failed: ("1") is not equal to ("0") - a backgrounded
///     device cannot sweep (C-59), so its beacon would be a lie that costs the
///     group a takeover for as long as it kept sending one` — the same rule
///     proven at the repository, against a real `Group` and a real journal.
///
/// Replacing `projectObservations`' `guard let rank = rankOf(...) else
/// { continue }` with an unconditional `let rank = rankOf(...)`:
///   - `testASenderThatNoLongerRanksIsDroppedAndPruned` FAILS three ways —
///     `("[Lakko.ProjectedObservation(rank: nil, ageMs: 2000),
///     Lakko.ProjectedObservation(rank: Optional(1), ageMs: 1000)]") is not
///     equal to ("[Lakko.ProjectedObservation(rank: Optional(1), ageMs:
///     1000)]")`, then `("["gone", "live"]") is not equal to ("["live"]")`,
///     then `("2") is not equal to ("1") - a demoted sender's observation
///     contributes nothing and must not linger in the store`.
///
/// `testARankIsDerivedAtVerdictTimeNotAtReceipt` is deliberately STRUCTURAL
/// rather than break-driven, and the distinction is worth stating: the store
/// holds only the sender KEY and the DS row time, so a receipt-time rank is not
/// representable at all. There is no line to delete — what the case pins is
/// that the projection asks `rankOf` at the moment the verdict is taken, so a
/// future change that started caching a rank would have to change the store's
/// shape and this signature together.
@MainActor
final class CommitterBeaconTests: XCTestCase {

    private let account = "acct-1"
    private let group = "group-1"

    private func makeStore() -> BeaconObservationStore {
        BeaconObservationStore(
            defaults: UserDefaults(suiteName: "beacon-\(UUID().uuidString)")!
        )
    }

    // MARK: - The emit gate

    /// The one rule the core cannot check, and it is load-bearing rather than
    /// cosmetic: a beacon means *able to sweep*, and on this client a sweep runs
    /// only at the tail of a poll driven by a visible `ChatScreen`. A beacon
    /// from a backgrounded app is a claim it cannot back up.
    func testABackgroundedCandidateDoesNotEmit() {
        let verdict = beaconEmitVerdict(
            ownRank: 0,
            foreground: false,
            lastEmittedAtMs: nil,
            nowMs: 1_000_000,
            throttle: { rank, last, now in
                shouldEmitBeacon(ownRank: rank, lastEmittedAtMs: last, nowMs: now)
            }
        )
        XCTAssertEqual(
            verdict, .notForeground,
            "a beacon means ABLE TO SWEEP, and a backgrounded app cannot: every other "
            + "candidate would defer to a device that is never going to sweep"
        )
    }

    /// Only candidates emit. Three beacons in a fifty-member group, not fifty —
    /// and a plain member's beacon is dropped by every receiver's fold anyway,
    /// so emitting one is pure journal litter and a pure activity leak.
    func testAPlainMemberNeverEmits() {
        let verdict = beaconEmitVerdict(
            ownRank: nil,
            foreground: true,
            lastEmittedAtMs: nil,
            nowMs: 1_000_000,
            throttle: { rank, last, now in
                shouldEmitBeacon(ownRank: rank, lastEmittedAtMs: last, nowMs: now)
            }
        )
        XCTAssertEqual(verdict, .notACandidate)
    }

    /// The FIRST beacon of a session is immediate — the core's rule, reached
    /// through the gate rather than reimplemented beside it.
    func testAForegroundedCandidateEmitsImmediatelyOnItsFirstTick() {
        let verdict = beaconEmitVerdict(
            ownRank: 2,
            foreground: true,
            lastEmittedAtMs: nil,
            nowMs: 1_000_000,
            throttle: { rank, last, now in
                shouldEmitBeacon(ownRank: rank, lastEmittedAtMs: last, nowMs: now)
            }
        )
        XCTAssertEqual(verdict, .emit)
    }

    /// One per interval after that, and the interval is READ OFF THE FFI rather
    /// than hand-copied: the interval and the freshness window are two halves of
    /// one budget, and a local literal is the drift class ADR-0044 exists about.
    func testAnEmitInsideTheIntervalIsThrottled() {
        let interval = committerBeaconIntervalMs()
        let now: UInt64 = 10_000_000
        let throttle: (UInt32?, UInt64?, UInt64) -> Bool = { rank, last, nowMs in
            shouldEmitBeacon(ownRank: rank, lastEmittedAtMs: last, nowMs: nowMs)
        }
        XCTAssertEqual(
            beaconEmitVerdict(
                ownRank: 0, foreground: true,
                lastEmittedAtMs: now - interval / 2, nowMs: now, throttle: throttle),
            .throttled
        )
        XCTAssertEqual(
            beaconEmitVerdict(
                ownRank: 0, foreground: true,
                lastEmittedAtMs: now - interval, nowMs: now, throttle: throttle),
            .emit
        )
    }

    /// The emitter's WAKE interval must sit comfortably inside the core's beacon
    /// interval, or the first beacon after a screen becomes visible waits longer
    /// than the cadence it is supposed to keep.
    func testTheTickIsWellInsideTheCoreInterval() {
        XCTAssertLessThan(
            UInt64(beaconTickSeconds * 1000), committerBeaconIntervalMs(),
            "the tick only bounds how long the FIRST beacon waits; it is not the interval"
        )
    }

    // MARK: - The observation store

    /// The NEWEST timestamp wins — the opposite of the pending-request store,
    /// where the FIRST observation wins. Here recency IS the signal: the whole
    /// question is how long since this candidate last proved it was running, so
    /// an older stamp must never overwrite a newer one (a re-fetched journal
    /// batch would otherwise age a live candidate out and hand its duty away).
    func testTheNewestObservationWins() {
        let store = makeStore()
        store.record(accountIdB64: account, groupIdB64: group, senderKeyB64: "k", atMs: 5_000)
        store.record(accountIdB64: account, groupIdB64: group, senderKeyB64: "k", atMs: 9_000)
        store.record(accountIdB64: account, groupIdB64: group, senderKeyB64: "k", atMs: 7_000)
        XCTAssertEqual(store.load(accountIdB64: account, groupIdB64: group), ["k": 9_000])
    }

    func testObservationsAreScopedPerAccountAndGroup() {
        let store = makeStore()
        store.record(accountIdB64: account, groupIdB64: group, senderKeyB64: "k", atMs: 1)
        XCTAssertTrue(store.load(accountIdB64: "other", groupIdB64: group).isEmpty)
        XCTAssertTrue(store.load(accountIdB64: account, groupIdB64: "other").isEmpty)
    }

    func testClearForAccountDropsEveryGroup() {
        let store = makeStore()
        store.record(accountIdB64: account, groupIdB64: "g1", senderKeyB64: "k", atMs: 1)
        store.record(accountIdB64: account, groupIdB64: "g2", senderKeyB64: "k", atMs: 1)
        store.clearForAccount(accountIdB64: account)
        XCTAssertTrue(store.load(accountIdB64: account, groupIdB64: "g1").isEmpty)
        XCTAssertTrue(store.load(accountIdB64: account, groupIdB64: "g2").isEmpty)
    }

    // MARK: - Rank at verdict time, never at receipt

    /// A rank is a POSITION in the role table's canonical order, not an
    /// identity. Promoting, demoting or removing any admin renumbers every admin
    /// that sorts after it, so a rank captured when a beacon arrived and
    /// compared later against this device's own rank — which is necessarily
    /// current — is not merely stale: it is a position in a DIFFERENT ordering.
    func testARankIsDerivedAtVerdictTimeNotAtReceipt() {
        // The observation stores only the KEY and the row time. The rank comes
        // from the CURRENT table, which has renumbered since the beacon landed.
        let observations: [String: Int64] = ["peer": 4_000]
        let (projected, _) = projectObservations(
            observations,
            rankOf: { key in key == "peer" ? 3 : nil },  // its rank TODAY
            nowMs: 5_000
        )
        XCTAssertEqual(projected, [ProjectedObservation(rank: 3, ageMs: 1_000)])
    }

    /// A demoted / removed sender contributes nothing and must not linger: the
    /// store is bounded by the candidate count only because the projection
    /// prunes what no longer ranks.
    func testASenderThatNoLongerRanksIsDroppedAndPruned() {
        let store = makeStore()
        store.record(accountIdB64: account, groupIdB64: group, senderKeyB64: "gone", atMs: 1_000)
        store.record(accountIdB64: account, groupIdB64: group, senderKeyB64: "live", atMs: 2_000)

        let (projected, liveKeys) = projectObservations(
            store.load(accountIdB64: account, groupIdB64: group),
            rankOf: { $0 == "live" ? 1 : nil },
            nowMs: 3_000
        )
        XCTAssertEqual(projected, [ProjectedObservation(rank: 1, ageMs: 1_000)])
        XCTAssertEqual(liveKeys, ["live"])

        store.prune(accountIdB64: account, groupIdB64: group, liveKeys: liveKeys)
        XCTAssertEqual(
            store.load(accountIdB64: account, groupIdB64: group).count, 1,
            "a demoted sender's observation contributes nothing and must not linger in the store"
        )
    }

    /// A row that appears to come from the FUTURE is clamped to age 0, never
    /// wrapped. `ageMs` is a `UInt64` on the ABI, so a negative age would not
    /// merely be wrong: it would land ABOVE every freshness window forever and
    /// silently read as infinitely stale.
    func testAFutureRowIsClampedToZeroRatherThanWrapped() {
        let (projected, _) = projectObservations(
            ["k": 9_000], rankOf: { _ in 1 }, nowMs: 5_000)
        XCTAssertEqual(projected, [ProjectedObservation(rank: 1, ageMs: 0)])
    }

    // MARK: - The composed decision, over the core's REAL fold

    /// Hearing nothing means ACT. Silence is the answer that nobody better
    /// placed is running, not a failure to elect — and it is the single most
    /// likely thing to get wrong.
    func testHearingNothingMeansAct() {
        let store = makeStore()
        let verdict = decideActingCommitter(
            store: store,
            accountIdB64: account,
            groupIdB64: group,
            ownRank: 2,
            rankOf: { _ in nil },
            nowMs: 1_000,
            freshnessWindowMs: committerBeaconFreshnessWindowMs(),
            fold: coreActingCommitterFold
        )
        XCTAssertEqual(verdict, .act)
    }

    /// A FRESH beacon from a strictly better-placed candidate stands this device
    /// down; the same beacon past the freshness window does not. The window is
    /// the core's, read off the FFI.
    func testAStaleBeaconIsIgnoredWhereAFreshOneDefers() {
        let store = makeStore()
        let window = committerBeaconFreshnessWindowMs()
        let now: Int64 = 10_000_000

        // Rank 0 (the owner) beaconed one second ago: strictly better placed and
        // inside the window.
        store.record(
            accountIdB64: account, groupIdB64: group,
            senderKeyB64: "owner", atMs: now - 1_000)
        XCTAssertEqual(
            decideActingCommitter(
                store: store, accountIdB64: account, groupIdB64: group,
                ownRank: 1, rankOf: { $0 == "owner" ? 0 : nil }, nowMs: now,
                freshnessWindowMs: window, fold: coreActingCommitterFold),
            .defer(toRank: 0)
        )

        // The same candidate, now silent for longer than the window: its beacon
        // ages out and this device takes over on its own, with no handover
        // protocol and nobody to ask.
        let stale = makeStore()
        stale.record(
            accountIdB64: account, groupIdB64: group,
            senderKeyB64: "owner", atMs: now - Int64(window) - 1)
        XCTAssertEqual(
            decideActingCommitter(
                store: stale, accountIdB64: account, groupIdB64: group,
                ownRank: 1, rankOf: { $0 == "owner" ? 0 : nil }, nowMs: now,
                freshnessWindowMs: window, fold: coreActingCommitterFold),
            .act
        )
    }

    /// A device's own echoed-back beacon must not deadlock it: only a STRICTLY
    /// lower rank defers.
    func testAnEqualRankDoesNotDefer() {
        let store = makeStore()
        store.record(
            accountIdB64: account, groupIdB64: group, senderKeyB64: "self", atMs: 9_999_000)
        XCTAssertEqual(
            decideActingCommitter(
                store: store, accountIdB64: account, groupIdB64: group,
                ownRank: 1, rankOf: { $0 == "self" ? 1 : nil }, nowMs: 10_000_000,
                freshnessWindowMs: committerBeaconFreshnessWindowMs(),
                fold: coreActingCommitterFold),
            .act
        )
    }

    /// The freshness window is the interval plus one lost beacon plus the worst
    /// case poll — pinned here so a client that hand-copied a number instead of
    /// reading the FFI would show up as a failure rather than as a silent
    /// disagreement about who is live.
    func testTheFreshnessWindowIsTheDocumentedConstruction() {
        XCTAssertEqual(
            committerBeaconFreshnessWindowMs(),
            2 * committerBeaconIntervalMs() + committerBeaconWorstCasePollMs()
        )
    }

    // MARK: - The repository wiring (a real Group, a real journal)

    /// The pure gate above says what SHOULD happen; this says the repository
    /// actually consults it and actually publishes. Without a case at this
    /// level the emitter could be wired to nothing and every test above would
    /// still pass.
    private struct EmitFixture {
        let repo: ChatRepository
        let groupIdHex: String
        let accountIdB64: String
        let snapshotStore: IdentitySnapshotStore
    }

    private func makeEmitFixture(owner: Bool) async throws -> EmitFixture {
        let seed = Data(repeating: 0x11, count: 32)
        let founder = try Identity.fromSeed(seed: seed)
        let group = try Group.create(identity: founder)
        let groupIdHex = DsClient.pseudonymHex(try group.groupId())
        if !owner {
            // Hand ownership to a key no local leaf holds so this device drops
            // out of the committer set entirely — `ownCommitterRank` goes nil,
            // which is the "plain member" case an emitter must never claim.
            let stranger = try Identity.fromSeed(seed: Data(repeating: 0x77, count: 32))
            _ = try group.setGroupRoles(
                identity: founder,
                owner: try founder.signaturePublicKey(),
                admins: [try stranger.signaturePublicKey()])
        }
        let snapshotBlob = try founder.snapshotStorage()

        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let accountIdB64 = base64Encode(accountIdBytes)
        let snapshotStore = IdentitySnapshotStore()
        let knownGroups = KnownGroupsStore(
            defaults: UserDefaults(suiteName: "emit-\(UUID().uuidString)")!)
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshotBlob)
        knownGroups.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)

        // A predecessor case's in-flight response can still be settling on a
        // global queue, so let it land BEFORE the reset rather than after it.
        try await Task.sleep(nanoseconds: 60_000_000)
        SendEpochStraddleTests.JournalURLProtocol.reset()
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [SendEpochStraddleTests.JournalURLProtocol.self]
        let ds = DsClient(
            baseURL: "http://127.0.0.1:9", session: URLSession(configuration: config))

        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            discovery: DiscoveryClient(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroups,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(
                rootDir: FileManager.default.temporaryDirectory
                    .appendingPathComponent("emit-\(UUID().uuidString)")),
            pushRouting: PushGroupRouting(),
            beaconStore: makeStore(),
            pendingRequestStore: PendingRequestStore(
                defaults: UserDefaults(suiteName: "emit-p-\(UUID().uuidString)")!)
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
                sessionExpiresAtMs: 0))
        return EmitFixture(
            repo: repo, groupIdHex: groupIdHex, accountIdB64: accountIdB64,
            snapshotStore: snapshotStore)
    }

    func testTheOwnerEmitsARealBeaconOntoTheJournalWhenForegrounded() async throws {
        let fx = try await makeEmitFixture(owner: true)
        defer { fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64) }
        XCTAssertEqual(fx.repo.ownCommitterRank(groupIdHex: fx.groupIdHex), 0)

        let verdict = await fx.repo.emitCommitterBeaconIfDue(
            groupIdHex: fx.groupIdHex, foreground: true)

        XCTAssertEqual(verdict, .emit)
        let rows = SendEpochStraddleTests.JournalURLProtocol.journal
        XCTAssertEqual(
            rows.count, 1,
            "exactly one journal row, got: "
            + rows.map { "id=\($0.id) epoch=\(String(describing: epochOfMessage(bytes: $0.body)))" }
                .joined(separator: ", "))
        // The row is an MLS application message, so the beacon bytes are inside
        // it — but the epoch is readable pre-decrypt, which is enough to prove a
        // real framed message reached the journal rather than raw envelope bytes.
        XCTAssertNotNil(
            epochOfMessage(bytes: rows[0].body),
            "the beacon must ride an E2EE MLS application message, not go out in the clear"
        )
    }

    func testABackgroundedOwnerPublishesNothingAtAll() async throws {
        let fx = try await makeEmitFixture(owner: true)
        defer { fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64) }

        let verdict = await fx.repo.emitCommitterBeaconIfDue(
            groupIdHex: fx.groupIdHex, foreground: false)

        XCTAssertEqual(verdict, .notForeground)
        XCTAssertEqual(
            SendEpochStraddleTests.JournalURLProtocol.journal.count, 0,
            "a backgrounded device cannot sweep (C-59), so its beacon would be a lie that "
            + "costs the group a takeover for as long as it kept sending one"
        )
    }

    func testAFollowUpTickInsideTheIntervalPublishesNothingFurther() async throws {
        let fx = try await makeEmitFixture(owner: true)
        defer { fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64) }

        _ = await fx.repo.emitCommitterBeaconIfDue(groupIdHex: fx.groupIdHex, foreground: true)
        let second = await fx.repo.emitCommitterBeaconIfDue(
            groupIdHex: fx.groupIdHex, foreground: true)

        XCTAssertEqual(second, .throttled)
        XCTAssertEqual(
            SendEpochStraddleTests.JournalURLProtocol.journal.count, 1,
            "the emitter WAKES far more often than the beacon interval; the core's throttle is "
            + "what keeps that from becoming one journal row per tick"
        )
    }
}
