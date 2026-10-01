// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// The receive path's DURABILITY ORDERING — the journal cursor may LAG the
/// durable MLS state, never LEAD it. iOS mirror of the web
/// `chat/receive-durability.test.ts`.
///
/// ## The defect these pin
///
/// ADR-0045 made a receiver's durable proposal store a PRECONDITION for applying
/// a future by-reference commit: OpenMLS resolves the reference against the
/// receiver's own store, so a peer that lost the proposal fails
/// `InvalidCommit(MissingProposal)` before any Lakko gate runs. That is correct
/// for a proposal the gate REFUSED and fatal for one the device LOST.
///
/// Losing one took no adversary. The receive path's snapshot write was
/// best-effort (`persistSnapshotBestEffort`) and swallowed its failures, the
/// cursor lives inside a different file (the ``MessageLogStore`` envelope) so
/// the two writes can never be atomic, and the cursor advanced regardless. A
/// swallowed filesystem error — or a crash between the two writes — left the
/// device with an MLS state that never saw the proposal and a cursor already
/// past it. The committer's commit then threw, was swallowed by the per-row
/// catch, the cursor advanced past THAT too, and the device sat at epoch N
/// forever while the group moved to N+1. `isActive()` does not catch it: the
/// device is still a member, just epoch-stranded.
///
/// ## What is driven
///
/// Two production surfaces, not a paraphrase of them:
///
///   1. ``ChatClient/applyFetchedMessages(identity:group:groupIdHex:raw:after:)`` against a
///      REAL FFI `Identity` + `Group`, for the "which rows touched durable
///      state" half. It has to be real: the whole point is that a row which
///      THREW may still have written, because `process_message` decrypts and
///      ratchets before either ADR-0022's role gate or ADR-0045's receipt gate
///      can refuse.
///   2. ``applyDurableCursorBatch(gate:touchedMlsState:persist:advance:)`` for
///      the ordering half, driven by a device whose DURABLE state is modelled
///      separately from its in-memory state — so a restart is a real operation
///      rather than an assertion about one.
///
/// ## Measured discrimination — each probe was RUN, failures quoted verbatim
///
/// Probe A, `applyDurableCursorBatch` reverted to the pre-fix shape (swallow the
/// persist, advance unconditionally) — 13 failures across 4 of the 8 tests:
///
///     testALostProposalStrandsTheDeviceForever : XCTAssertEqual failed:
///     ("10") is not equal to ("0") - the cursor must not pass a batch whose
///     persist failed
///     …and: ("0") is not equal to ("1") - the device must still apply the
///     commit after a restart
///
///     testNoInterleavingCanEverStrandTheDevice : XCTAssertFalse failed - a
///     crash before consuming commit(needs: "A") would strand the device
///     (cursor 10)
///
///     testTheGoodPathCostsOnePersistPerBatchAndNoneWhenEmpty : XCTAssertEqual
///     failed: ("1") is not equal to ("0") - an empty batch persists nothing
///
/// `testAPermanentlyUndecryptableRowNeverWedgesThePoll` stays GREEN under the
/// probe, and that is the point of it: the fix must not change that behaviour.
///
/// Probe B, the sticky flag removed (cleared whether or not the write landed):
/// NO test went red, and the file says so rather than inventing one. Holding the
/// cursor is what makes the touching rows be re-fetched, and a row the echo
/// filter did not drop is handed to the core again — so the gate re-marks itself
/// and the flag never changes an outcome under the current call shape. It is
/// kept as defence in depth; see `Chat/DurableCursor.swift`.
///
/// Probe C, `touchedMlsState` set AFTER a successful apply instead of before:
///
///     ReceiveDurabilityTests.swift:102: error:
///     -[LakkoTests.ReceiveDurabilityTests testARefusedRowStillCountsAsTouched]
///     : XCTAssertTrue failed - a row that THREW may still have ratcheted:
///     process_message runs before either gate
///
/// (Probe D, the ADR-0045 verdict apply's own gate, lives in
/// `CommitterSweepTests.swift` and is quoted there.)
final class ReceiveDurabilityTests: XCTestCase {

    // MARK: - Which rows count as touching durable state (real FFI)

    private func realGroup() throws -> (Identity, Group) {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x71, count: 32))
        let group = try Group.create(identity: identity, kind: .group)
        return (identity, group)
    }

    /// The journal these batches are applied from. The own-echo filter is keyed
    /// on `(journal, id)`, so every apply has to name one.
    private func journalHex(_ group: Group) throws -> String {
        DsClient.pseudonymHex(try group.groupId())
    }

    private func fetchResult(_ rows: [(Int64, Data)]) -> FetchResult {
        FetchResult(
            messages: rows.map {
                MailboxMessage(id: $0.0, payload: $0.1, createdAt: "t-\($0.0)")
            },
            hasMore: false
        )
    }

    /// A row the core cannot apply STILL counts as having touched durable state.
    ///
    /// This is the one that is easy to get backwards, and getting it backwards
    /// is silent: `process_incoming_verifying_roles` calls `process_message`
    /// FIRST — which decrypts and ratchets — and only then can ADR-0022's role
    /// gate refuse a commit or ADR-0045's receipt gate drop a proposal. A flag
    /// set after a successful return would leave exactly those rows unprotected.
    func testARefusedRowStillCountsAsTouched() throws {
        let (identity, group) = try realGroup()
        let client = ChatClient()
        let result = client.applyFetchedMessages(
            identity: identity,
            group: group,
            groupIdHex: try journalHex(group),
            raw: fetchResult([(10, Data([0xFF, 0xFF, 0xFF]))]),
            after: 0
        )
        XCTAssertTrue(result.events.isEmpty, "an unapplicable row surfaces nothing")
        XCTAssertTrue(
            result.touchedMlsState,
            "a row that THREW may still have ratcheted: process_message runs before either gate"
        )
        XCTAssertEqual(result.cursor, 10, "and it still advances the cursor — a bad row must never wedge the poll")
    }

    func testAnEmptyBatchTouchesNothing() throws {
        let (identity, group) = try realGroup()
        let client = ChatClient()
        let result = client.applyFetchedMessages(
            identity: identity, group: group, groupIdHex: try journalHex(group),
            raw: fetchResult([]), after: 7
        )
        XCTAssertFalse(result.touchedMlsState)
        XCTAssertEqual(result.cursor, 7, "an empty batch must not rewind the cursor")
    }

    func testAnAllEchoBatchTouchesNothing() throws {
        let (identity, group) = try realGroup()
        let client = ChatClient()
        let journal = try journalHex(group)
        client.markOwnPublishedCommit(groupIdHex: journal, messageId: 10)
        let result = client.applyFetchedMessages(
            identity: identity,
            group: group,
            groupIdHex: journal,
            raw: fetchResult([(10, Data([0xFF]))]),
            after: 0
        )
        XCTAssertFalse(
            result.touchedMlsState,
            "an echo row never reaches the core, so it cannot have mutated anything"
        )
        XCTAssertEqual(result.cursor, 10)
    }

    // MARK: - The ordering (a device with separate durable + in-memory state)

    /// A device whose durable MLS snapshot is modelled apart from its live
    /// state, reproducing the one OpenMLS behaviour the defect turns on: a
    /// commit that references a proposal THROWS unless that proposal is stored.
    private final class Device {
        private(set) var durableProposals: Set<String> = []
        private(set) var durableEpoch = 0
        private var memProposals: Set<String> = []
        private(set) var memEpoch = 0
        private(set) var cursor: Int64 = 0
        private(set) var persistCalls = 0
        var persistFails = false
        let gate = DurableCursorGate()

        struct PersistFailed: Error {}

        /// One journal row.
        enum Row {
            case proposal(String)
            case commit(needs: String)
            /// A row nothing can ever apply — corrupt frame, wrong-epoch
            /// duplicate, non-MLS envelope.
            case junk
            /// One of OUR OWN published rows: dropped by the echo filter BEFORE
            /// it reaches the core, so it can have mutated nothing.
            case echo
        }

        private var journal: [(id: Int64, row: Row)] = []
        private var nextId: Int64 = 9

        @discardableResult
        func push(_ row: Row) -> Int64 {
            nextId += 1
            journal.append((nextId, row))
            return nextId
        }

        func rows(after: Int64) -> [(id: Int64, row: Row)] {
            journal.filter { $0.id > after }
        }

        /// The production sequence: apply the batch in memory, then run the ONE
        /// ordering function the repository runs.
        @discardableResult
        func poll() -> CursorVerdict {
            let batch = rows(after: cursor)
            var touched = false
            var last = cursor
            for entry in batch {
                last = entry.id
                if case .echo = entry.row { continue }
                touched = true
                switch entry.row {
                case .echo:
                    continue
                case .junk:
                    continue
                case .proposal(let ref):
                    // `lakko-mls` de-duplicates by `ProposalRef` before storing,
                    // so a re-delivered proposal is idempotent.
                    memProposals.insert(ref)
                case .commit(let needs):
                    guard memProposals.contains(needs) else { continue }
                    memEpoch += 1
                    memProposals.removeAll()
                }
            }
            return applyDurableCursorBatch(
                gate: gate,
                touchedMlsState: touched,
                persist: { [self] in
                    persistCalls += 1
                    if persistFails { throw PersistFailed() }
                    durableProposals = memProposals
                    durableEpoch = memEpoch
                },
                advance: { [self] in cursor = last }
            )
        }

        /// Discard everything in memory and reload from disk.
        func restart() {
            memProposals = durableProposals
            memEpoch = durableEpoch
        }

        /// Would a crash RIGHT NOW leave the device unable to apply a later
        /// commit? Replays the journal from the durable cursor against the
        /// durable proposal set — exactly what the next launch would do.
        var strandedAfterCrash: Bool {
            var held = durableProposals
            for entry in rows(after: cursor) {
                switch entry.row {
                case .proposal(let ref): held.insert(ref)
                case .commit(let needs):
                    if !held.contains(needs) { return true }
                    held.removeAll()
                case .junk, .echo: continue
                }
            }
            return false
        }
    }

    /// THE HEADLINE. Everything else in this file is a guard around it.
    func testALostProposalStrandsTheDeviceForever() {
        let d = Device()
        let proposalId = d.push(.proposal("P"))

        // The proposal arrives while the snapshot store is broken.
        d.persistFails = true
        XCTAssertEqual(d.poll(), .held)
        XCTAssertEqual(proposalId, 10)
        XCTAssertEqual(
            d.cursor, 0,
            "the cursor must not pass a batch whose persist failed"
        )
        XCTAssertTrue(d.durableProposals.isEmpty)

        // Meanwhile the elected committer sweeps it into a commit that carries
        // it BY REFERENCE — the artifact this device can only apply if it kept
        // the proposal.
        d.push(.commit(needs: "P"))

        // The storage failure clears, and the app is relaunched.
        d.persistFails = false
        d.restart()
        XCTAssertEqual(d.memEpoch, 0)

        // The poll resumes from the DURABLE cursor, which is where the durable
        // MLS state is, so the proposal is re-delivered ahead of the commit.
        XCTAssertEqual(d.poll(), .advanced)
        XCTAssertEqual(
            d.memEpoch, 1,
            "the device must still apply the commit after a restart"
        )
        XCTAssertEqual(d.durableEpoch, 1)
        XCTAssertEqual(d.cursor, 11)
    }

    /// A held batch is RE-FETCHED, and the cursor only catches up once a write
    /// lands — through however many failures it takes.
    ///
    /// This is also the reason the sticky flag has no test of its own: holding
    /// the cursor is exactly what makes the touching rows come back, and a row
    /// the echo filter did not drop is handed to the core again, so the gate is
    /// re-marked on its own. The flag is defence in depth against a future
    /// caller that advances from somewhere else; see `Chat/DurableCursor.swift`.
    func testAHeldBatchIsRefetchedUntilAWriteLands() {
        let d = Device()
        d.push(.proposal("P"))
        d.persistFails = true
        XCTAssertEqual(d.poll(), .held)
        XCTAssertEqual(d.cursor, 0)

        // A second attempt, still broken, now with a further row behind it.
        d.push(.echo)
        XCTAssertEqual(d.poll(), .held)
        XCTAssertEqual(
            d.cursor, 0,
            "the cursor must not creep forward while the snapshot is behind"
        )
        XCTAssertEqual(d.persistCalls, 2, "and every attempt retries the write")

        // The store recovers: the next batch persists and the cursor catches up
        // in one step, over everything held back.
        d.persistFails = false
        d.push(.junk)
        XCTAssertEqual(d.poll(), .advanced)
        XCTAssertEqual(d.cursor, 12)
        XCTAssertEqual(d.durableProposals, ["P"])
    }

    func testAPermanentlyUndecryptableRowNeverWedgesThePoll() {
        // The trap the ordering rule could have set. Only a PERSIST failure
        // holds the cursor; a row that can never apply is skipped exactly as
        // before, so the poll cannot loop on it forever.
        let d = Device()
        d.push(.junk)
        d.push(.junk)
        XCTAssertEqual(d.poll(), .advanced)
        XCTAssertEqual(d.cursor, 11)
        XCTAssertEqual(d.poll(), .advanced)
        XCTAssertEqual(d.cursor, 11)
    }

    func testTheGoodPathCostsOnePersistPerBatchAndNoneWhenEmpty() {
        let d = Device()
        XCTAssertEqual(d.poll(), .advanced)
        XCTAssertEqual(d.persistCalls, 0, "an empty batch persists nothing")

        d.push(.proposal("P"))
        d.push(.commit(needs: "P"))
        XCTAssertEqual(d.poll(), .advanced)
        XCTAssertEqual(d.persistCalls, 1)
        XCTAssertEqual(d.cursor, 11)
    }

    func testNoInterleavingCanEverStrandTheDevice() {
        let d = Device()
        // The failure pattern is EXPLICIT, not alternating. Alternating happens
        // to let a successful write land between a proposal and the commit that
        // references it — and a successful write captures the still-live
        // in-memory state, repairing the gap before the replay can see it. The
        // hazard needs a commit arriving while the durable snapshot is behind,
        // so each entry below is (row, does this batch's write fail).
        let seq: [(Device.Row, Bool)] = [
            (.proposal("A"), true),
            (.commit(needs: "A"), true),
            (.junk, false),
            (.proposal("B"), true),
            (.echo, true),
            (.commit(needs: "B"), false),
            (.proposal("C"), true),
            (.commit(needs: "C"), true),
        ]
        for (row, failWrite) in seq {
            d.push(row)
            d.persistFails = failWrite
            // Checked BEFORE the row is consumed, which is the only moment it
            // can be caught: once a commit has been polled — applied or thrown
            // — it is no longer above the cursor and the replay has nothing to
            // fail on.
            XCTAssertFalse(
                d.strandedAfterCrash,
                "a crash before consuming \(row) would strand the device (cursor \(d.cursor))"
            )
            d.poll()
        }
    }
}
