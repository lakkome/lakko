// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0045 S4a/S4b — the committer sweep's five obligations, driven over the
/// injected ports so the whole orchestration runs with no MLS provider, no
/// filesystem and no network. iOS mirror of the web `chat/sweep.test.ts`.
///
/// ## Which of these discriminate (measured by RUNNING the break, quoted
/// verbatim — not predicted)
///
/// Hoisting `publishWelcomes` from the merge branch to just after
/// `journal.publishGroup`, i.e. publishing the Welcome before the verdict:
///   - `testWelcomeIsPublishedOnlyAfterTheVerdict` FAILS with
///     `XCTAssertEqual failed: ("["fetch", "stage", "record", "publish:commit",
///     "publish:welcome", "fetch", "merge", "ack"]") is not equal to
///     ("["fetch", "stage", "record", "publish:commit", "fetch", "merge",
///     "publish:welcome", "ack"]")`;
///   - `testALostSweepPublishesNoWelcome` FAILS with
///     `XCTAssertEqual failed: ("1") is not equal to ("0") - a lost sweep must
///     publish NO Welcome: the joiner would open into a state no member holds`;
///   - `testALosingSweepAbandonsAndTheProposalsSurvive` and
///     `testARestartFinishesAnOwedVerdict` fail on their sequences too.
///
/// Replacing the `verdict == .lost` branch's `abandonStagedSweep` with
/// `mergeStagedSweep`:
///   - `testALosingSweepAbandonsAndTheProposalsSurvive` FAILS three ways —
///     `("[…, "apply", "merge", "discard"]") is not equal to ("[…, "apply",
///     "abandon", "discard"]")`, then `("0") is not equal to ("1") - abandoning
///     keeps the proposals pending for the winner's next sweep`, then
///     `("7") is not equal to ("6") - the epoch moved because the WINNER's
///     commit was applied, not because ours merged`.
///
/// Moving the `hasStagedSweep()` check in `runCommitterSweep` to AFTER the rank
/// check:
///   - `testADemotedDeviceStillFinishesTheVerdictItOwes` FAILS with
///     `XCTAssertEqual failed: ("notCommitter(rank: nil)") is not equal to
///     ("committed(commitMessageId: 71, welcomeMessageIds: [], addedCount: 0)")`
///     and then `XCTAssertFalse failed - …and that is what lifts the membership
///     freeze`.
@MainActor
final class CommitterSweepTests: XCTestCase {

    // MARK: - Harness

    /// A stand-in group whose epoch moves only where the real one does: a
    /// merged staged sweep advances it, and so does applying a FOREIGN commit
    /// row — which is exactly the signal `sweepVerdict` reads.
    private final class FakeGroup {
        var epoch: UInt64 = 5
        var rank: UInt32? = 0
        var pending: Int = 1
        var staged = false
        var log: [String] = []
        /// Rows whose payload starts with this byte are treated as a foreign
        /// COMMIT and advance the epoch when applied. Anything else is an
        /// application message / proposal / non-MLS envelope and does not.
        static let foreignCommitTag: UInt8 = 0xC0
        /// Rows whose payload starts with this byte make the apply THROW — a
        /// non-MLS Lakko envelope (rendezvous / attribution), a wrong-epoch
        /// frame, or a role-refused commit. Skipped exactly as the poll skips it.
        static let unappliableTag: UInt8 = 0xFF
        /// What the applied rows handed to the timeline, in journal order.
        ///
        /// The real adapter both applies the row and surfaces what it produced;
        /// modelling only the apply half here would make the defect untestable,
        /// because the whole failure was that the second half did not exist.
        var surfaced: [String] = []
        var stageThrows: Error?
        var welcome: Data? = Data([0x11, 0x22])
        var addedKeys: [Data] = [Data(repeating: 0xAA, count: 32)]
        /// Set to make the verdict apply's FAIL-CLOSED snapshot write fail —
        /// the swallowed-filesystem-error shape that used to strand the device.
        var persistAppliedThrows: Error?
    }

    private final class FakeJournal {
        /// Rows the stand-down re-read and the verdict read will return, keyed
        /// by the `after` cursor they answer.
        var rowsAfter: [Int64: [SweepJournalRow]] = [:]
        var fetchThrows: Error?
        var publishThrows: Error?
        /// Set to make the mailbox fan-out fail (a transient DS blip); clear it
        /// again to model the retry after a restart.
        var welcomePublishThrows: Error?
        var nextId: Int64 = 70
        var published: [(kind: String, bytes: Data)] = []
    }

    private final class FakeOutbox {
        var recorded: [SweepDurableEntry] = []
        var owed: SweepDurableEntry?
        /// The durable welcomes-owed marker: an entry whose VERDICT settled and
        /// whose mailbox fan-out is all that remains.
        var owedWelcomes: SweepDurableEntry?
        var recordThrows: Error?
        var markWelcomesOwedThrows: Error?
        var acked: [String] = []
        var discarded: [String] = []
    }

    private struct Harness {
        let group: FakeGroup
        let journal: FakeJournal
        let outbox: FakeOutbox
        let ports: SweepPorts
        /// Ordered record of everything that happened, which is the assertion
        /// for the two ORDERING obligations. Deliberately not a set of "was it
        /// called" flags: obligations 2 and 4 are entirely about sequence.
        var log: [String] { group.log }
    }

    private struct StubError: Error {}

    private func makeHarness(
        acting: @escaping (UInt32) async throws -> ActingCommitter = { _ in .act },
        cursor: Int64 = 40
    ) -> Harness {
        let group = FakeGroup()
        let journal = FakeJournal()
        let outbox = FakeOutbox()
        var cursorValue = cursor

        let ports = SweepPorts(
            group: SweepGroupPort(
                groupId: Data([0x01, 0x02]),
                epoch: { group.epoch },
                ownCommitterRank: { group.rank },
                pendingProposalCount: { group.pending },
                hasStagedSweep: { group.staged },
                stagePendingProposals: {
                    if let err = group.stageThrows { throw err }
                    guard group.pending > 0 else { return nil }
                    group.staged = true
                    group.log.append("stage")
                    return StagedSweepArtifacts(
                        commit: Data([0xC0, 0x05]),
                        welcome: group.welcome,
                        addedSignatureKeys: group.welcome == nil ? [] : group.addedKeys
                    )
                },
                mergeStagedSweep: {
                    group.staged = false
                    group.epoch += 1
                    group.pending = 0
                    group.log.append("merge")
                },
                abandonStagedSweep: {
                    group.staged = false
                    group.log.append("abandon")
                },
                applyIncoming: { row in
                    group.log.append("apply")
                    if row.payload.first == FakeGroup.unappliableTag { throw StubError() }
                    // Surfacing happens only AFTER a successful apply, exactly as
                    // in the adapter — the throw above is what keeps a skipped row
                    // from producing an event.
                    group.surfaced.append("row#\(row.id)@\(row.createdAt)")
                    if row.payload.first == FakeGroup.foreignCommitTag { group.epoch += 1 }
                }
            ),
            actingCommitter: acting,
            journal: SweepJournalPort(
                fetch: { after in
                    group.log.append("fetch")
                    if let err = journal.fetchThrows { throw err }
                    return journal.rowsAfter[after] ?? []
                },
                publishGroup: { bytes in
                    group.log.append("publish:commit")
                    if let err = journal.publishThrows { throw err }
                    journal.published.append(("commit", bytes))
                    journal.nextId += 1
                    return journal.nextId
                },
                publishMailbox: { _, bytes in
                    group.log.append("publish:welcome")
                    if let err = journal.welcomePublishThrows { throw err }
                    journal.published.append(("welcome", bytes))
                    journal.nextId += 1
                    return journal.nextId
                }
            ),
            outbox: SweepOutboxPort(
                newEntryId: { "entry-1" },
                record: { entry in
                    group.log.append("record")
                    if let err = outbox.recordThrows { throw err }
                    outbox.recorded.append(entry)
                    outbox.owed = entry
                },
                // The real store's condition, verbatim: a SWEEP entry whose
                // welcomes are NOT yet owed. Once the verdict settles the entry
                // stops being the verdict finder's business.
                findOwed: { outbox.owedWelcomes == nil ? outbox.owed : nil },
                markWelcomesOwed: { id, commitMessageId in
                    group.log.append("welcomes-owed")
                    if let err = outbox.markWelcomesOwedThrows { throw err }
                    guard var entry = outbox.owed, entry.id == id else { return }
                    entry.commitMessageId = commitMessageId
                    outbox.owed = entry
                    outbox.owedWelcomes = entry
                },
                findOwedWelcomes: { outbox.owedWelcomes },
                ack: { id, _, _ in
                    group.log.append("ack")
                    outbox.acked.append(id)
                    outbox.owed = nil
                    outbox.owedWelcomes = nil
                },
                discard: { id in
                    group.log.append("discard")
                    outbox.discarded.append(id)
                    outbox.owed = nil
                    outbox.owedWelcomes = nil
                }
            ),
            cursor: SweepCursorRef(
                read: { cursorValue },
                advance: { cursorValue = $0 }
            ),
            welcomeMailbox: { key in key.prefix(8) },
            wrapWelcome: { Data([0xE0]) + $0 },
            markOwnPublished: { _ in },
            persist: {},
            // The FAIL-CLOSED persist the verdict apply runs before it advances
            // the shared cursor. Distinct from `persist` above because it must
            // THROW.
            persistApplied: {
                if let err = group.persistAppliedThrows { throw err }
                group.log.append("persist-applied")
            }
        )
        return Harness(group: group, journal: journal, outbox: outbox, ports: ports)
    }

    // MARK: - A proposal reaches the committer and is swept

    func testAPendingProposalIsStagedPublishedAndMerged() async throws {
        let h = makeHarness()
        let outcome = try await runCommitterSweep(h.ports)

        guard case .committed(_, let welcomeIds, let addedCount) = outcome else {
            return XCTFail("rank 0 with a pending proposal and an empty journal must sweep, got \(outcome)")
        }
        XCTAssertEqual(addedCount, 1)
        XCTAssertEqual(welcomeIds.count, 1)
        XCTAssertEqual(h.group.epoch, 6, "a merged sweep lands at the epoch the peers reached")
        XCTAssertFalse(h.group.staged, "no verdict may be left owed")
        XCTAssertEqual(h.outbox.acked, ["entry-1"])
    }

    // MARK: - Obligation 2: the Welcome is published only AFTER the verdict

    func testWelcomeIsPublishedOnlyAfterTheVerdict() async throws {
        let h = makeHarness()
        _ = try await runCommitterSweep(h.ports)

        // The whole assertion is the SEQUENCE. Publishing the Welcome before
        // the verdict would let a joiner open into a state no other member
        // holds if the commit then lost — a three-party fork, strictly worse
        // than the two-party one this mechanism exists to close.
        XCTAssertEqual(
            h.log,
            ["fetch", "stage", "record", "publish:commit", "fetch",
             "merge", "welcomes-owed", "publish:welcome", "ack"],
            "obligation 4 orders record -> publish -> verdict -> merge, and obligation 2 puts "
            + "the Welcome strictly after the merge — with the durable welcomes-owed marker "
            + "in between, so a fan-out that fails is still retryable"
        )
    }

    // MARK: - A losing sweep abandons, and the proposals survive

    func testALosingSweepAbandonsAndTheProposalsSurvive() async throws {
        let h = makeHarness()
        // A foreign COMMIT was ordered below ours, so applying it moves our
        // epoch — which is the only thing `sweepVerdict` reads.
        //
        // The stand-down re-read (before staging) must see an EMPTY journal or
        // the sweep stands down instead of staging; the verdict read (after
        // publishing) is what carries the foreign commit. Both call `fetch(40)`,
        // so the harness swaps the answer between them.
        var call = 0
        let ports = swapFetch(h) { after in
            call += 1
            if call == 1 { return [] }
            return [SweepJournalRow(id: 50, payload: Data([FakeGroup.foreignCommitTag]), createdAt: "t-50")]
        }

        let outcome = try await runCommitterSweep(ports)

        guard case .lost = outcome else {
            return XCTFail("a foreign commit ordered below ours must read as a LOSS, got \(outcome)")
        }
        XCTAssertEqual(
            h.log,
            // `persist-applied` sits between the apply and the abandon: the
            // verdict apply advances the shared cursor over the rows it
            // consumed, so it must durably write them first (see
            // `Chat/DurableCursor.swift`).
            [
                "fetch", "stage", "record", "publish:commit", "fetch", "apply",
                "persist-applied", "abandon", "discard",
            ],
            "a lost sweep abandons and DISCARDS its entry — re-publishing a commit whose "
            + "proposals the winner already swept is what the discard prevents"
        )
        XCTAssertEqual(
            h.group.pending, 1,
            "abandoning keeps the proposals pending for the winner's next sweep"
        )
        XCTAssertEqual(
            h.group.epoch, 6,
            "the epoch moved because the WINNER's commit was applied, not because ours merged"
        )
        XCTAssertFalse(h.group.staged)
    }

    func testALostSweepPublishesNoWelcome() async throws {
        let h = makeHarness()
        var call = 0
        let ports = swapFetch(h) { _ in
            call += 1
            return call == 1
                ? []
                : [SweepJournalRow(id: 50, payload: Data([FakeGroup.foreignCommitTag]), createdAt: "t-50")]
        }
        _ = try await runCommitterSweep(ports)
        XCTAssertEqual(
            h.journal.published.filter({ $0.kind == "welcome" }).count, 0,
            "a lost sweep must publish NO Welcome: the joiner would open into a state no member holds"
        )
    }

    // MARK: - Obligation 1: a restart finishes an owed verdict

    func testARestartFinishesAnOwedVerdict() async throws {
        let h = makeHarness()
        // The shape a relaunch finds: OpenMLS restored a PendingCommit, and the
        // ADR-0040 entry that was written before the publish is still un-acked.
        h.group.staged = true
        h.group.pending = 1
        h.outbox.owed = SweepDurableEntry(
            id: "owed-1",
            commitB64: base64Encode(Data([0xC0, 0x05])),
            welcomeEnvelopeB64: base64Encode(Data([0xE0, 0x11])),
            welcomeMailboxesB64: [base64Encode(Data([0x09]))]
        )

        let outcome = try await resolveOwedVerdict(h.ports)

        guard case .committed = outcome else {
            return XCTFail("an owed verdict with an empty journal below our commit is a WIN, got \(outcome)")
        }
        XCTAssertFalse(
            h.group.staged,
            "the whole point of the restart path is that the freeze is lifted — a staged sweep "
            + "refuses every propose / add / remove on this group until it is adjudicated"
        )
        XCTAssertEqual(
            h.log,
            ["publish:commit", "fetch", "merge", "welcomes-owed", "publish:welcome", "ack"],
            "the re-publish is idempotent (peers at N+1 drop the duplicate as wrong-epoch) and "
            + "NO second durable record is written — the entry already exists"
        )
        XCTAssertTrue(h.outbox.recorded.isEmpty, "a re-publish must not re-record")
    }

    /// Obligation 1 runs BEFORE the rank check, and that ordering is the whole
    /// point rather than a tidiness choice.
    ///
    /// S4b made a demoted committer reachable: an admin can be voted out while
    /// holding a staged sweep. A staged sweep is DURABLE and freezes this
    /// group's membership operations across relaunches, so gating the
    /// adjudication behind the rank check would leave such a device frozen
    /// permanently with nothing that could ever unwedge it.
    func testADemotedDeviceStillFinishesTheVerdictItOwes() async throws {
        let h = makeHarness()
        h.group.staged = true
        h.group.rank = nil  // demoted out of the committer set entirely
        h.outbox.owed = SweepDurableEntry(id: "owed-1", commitB64: base64Encode(Data([0xC0, 0x05])))

        let outcome = try await runCommitterSweep(h.ports)

        XCTAssertEqual(
            outcome,
            .committed(commitMessageId: 71, welcomeMessageIds: [], addedCount: 0),
            "a device that is no longer a candidate must still adjudicate the sweep it staged"
        )
        XCTAssertFalse(h.group.staged, "…and that is what lifts the membership freeze")
    }

    /// A staged sweep with NO durable record provably never reached the wire —
    /// ADR-0040 records strictly before it publishes — so abandoning is both
    /// correct and what unwedges the frozen membership operations.
    func testAStagedSweepWithNoRecordIsAbandonedAsAnOrphan() async throws {
        let h = makeHarness()
        h.group.staged = true
        h.outbox.owed = nil

        let outcome = try await resolveOwedVerdict(h.ports)

        XCTAssertEqual(outcome, .abandonedOrphan)
        XCTAssertEqual(h.log, ["abandon"])
        XCTAssertFalse(h.group.staged)
    }

    // MARK: - Obligation 3: the stand-down re-read

    func testAnyUnappliedRowStandsTheSweepDownWithoutStaging() async throws {
        let h = makeHarness()
        h.journal.rowsAfter[40] = [SweepJournalRow(id: 41, payload: Data([0xA0]), createdAt: "t-41")]

        let outcome = try await runCommitterSweep(h.ports)

        XCTAssertEqual(outcome, .stoodDown(unappliedRows: 1))
        XCTAssertEqual(h.log, ["fetch"], "nothing may be staged, recorded or published")
        XCTAssertFalse(h.group.staged)
    }

    /// The rule errs deliberately in the conservative direction: ANY unapplied
    /// row stands the sweep down, not only a Commit — there is no
    /// `contentTypeOfMessage` peek, so a row that might be a commit is treated
    /// as one, and standing down is free.
    func testStandDownRuleIsAnyRowNotOnlyACommit() {
        XCTAssertFalse(shouldStandDown(unappliedRowCount: 0))
        XCTAssertTrue(shouldStandDown(unappliedRowCount: 1))
    }

    // MARK: - Obligation 4: fail-closed on the durable record

    func testAFailedDurableRecordAbortsWithoutPublishingAndRollsBack() async throws {
        let h = makeHarness()
        h.outbox.recordThrows = StubError()

        do {
            _ = try await runCommitterSweep(h.ports)
            XCTFail("a failed durable record must ABORT the sweep")
        } catch is StubError {
            // expected
        }
        XCTAssertEqual(
            h.log, ["fetch", "stage", "record", "abandon"],
            "nothing was published, so the group must look as though the sweep never happened"
        )
        XCTAssertFalse(h.group.staged)
        XCTAssertEqual(h.journal.published.count, 0)
    }

    // MARK: - S4b: the acting-committer verdict gates the stage

    func testACandidateThatHearsABetterPlacedBeaconStandsDown() async throws {
        let h = makeHarness(acting: { _ in .defer(toRank: 0) })
        h.group.rank = 2

        let outcome = try await runCommitterSweep(h.ports)

        XCTAssertEqual(outcome, .deferred(rank: 2, toRank: 0))
        XCTAssertEqual(h.log, [], "a deferral must not even re-read the journal")
    }

    /// A fold this device could not evaluate is a DEFER, not an act: sweeping on
    /// no evidence about who else is running is the failure the election exists
    /// to avoid.
    func testAnUnreadableActingVerdictFailsClosedToADeferral() async throws {
        let h = makeHarness(acting: { _ in throw StubError() })
        h.group.rank = 1

        let outcome = try await runCommitterSweep(h.ports)

        XCTAssertEqual(
            outcome, .deferred(rank: 1, toRank: nil),
            "and it names no successor — `defer(toRank:)` carries a non-optional rank on the "
            + "FFI, so synthesising one here would have to name this device, which is a lie"
        )
        XCTAssertFalse(h.group.staged)
    }

    func testAPlainMemberNeverSweeps() async throws {
        let h = makeHarness()
        h.group.rank = nil

        let outcome = try await runCommitterSweep(h.ports)

        XCTAssertEqual(outcome, .notCommitter(rank: nil))
        XCTAssertEqual(h.log, [])
    }

    func testNothingPendingShortCircuitsBeforeTheNetwork() async throws {
        let h = makeHarness()
        h.group.pending = 0

        let outcome = try await runCommitterSweep(h.ports)

        XCTAssertEqual(outcome, .nothingPending)
        XCTAssertEqual(
            h.log, [],
            "the poll tail calls this on every group on every tick — it must cost one "
            + "synchronous check when there is nothing to do"
        )
    }

    // MARK: - A group with no role table keeps direct commits

    /// ADR-0045 routes membership through proposals only where there IS a role
    /// table: a group without one has no owner, therefore no rank 0, therefore
    /// no committer, and every peer's receipt gate would drop a proposal
    /// outright — so the propose path would silently discard everything.
    ///
    /// This is the one branch no live `Group` can produce (`Group.create` has
    /// seeded a role table since ADR-0022), which is exactly why the rule is
    /// factored out over an injected read: the table-less answer is otherwise
    /// unreachable and would go untested.
    func testAGroupWithNoRoleTableTakesTheDirectCommitRoute() {
        XCTAssertFalse(
            ChatRepository.usesProposalPath { nil },
            "no role table -> direct commits, unchanged"
        )
        XCTAssertTrue(
            ChatRepository.usesProposalPath { RoleTable(owner: Data(repeating: 0x01, count: 32), admins: []) },
            "a role table -> the proposal path"
        )
        XCTAssertFalse(
            ChatRepository.usesProposalPath { throw StubError() },
            "a TORN read must read as 'no role table', which keeps the direct path working "
            + "rather than routing into a proposal path whose gate cannot resolve"
        )
    }

    // MARK: - The pure verdict rule

    func testOnlyAnEpochAdvanceReadsAsALoss() {
        XCTAssertEqual(sweepVerdict(epochBefore: 5, epochAfter: 5), .won)
        XCTAssertEqual(sweepVerdict(epochBefore: 5, epochAfter: 6), .lost)
    }

    // MARK: - The verdict apply must not swallow the rows it consumes

    /// The defect this block pins.
    ///
    /// `adjudicate` applies every row the DS ordered below our own commit, and
    /// the cursor then advances past them, so the poll never fetches them again.
    /// When one of those rows is an ordinary chat message from another member,
    /// applying it correctly advances the ratchet — and, before this, threw the
    /// plaintext away. The message was permanently lost from the timeline,
    /// silently. This file's own `adjudicate` comment used to record it as a
    /// KNOWN CONSEQUENCE.
    ///
    /// The window is narrow but real and reachable without any fault: the
    /// stand-down check proves the journal empty, and between that check and the
    /// publish another member can send. Anything the DS orders below our commit
    /// lands in exactly this loop.
    ///
    /// What is assertable HERE is that the sweep hands the port every applied row
    /// WHOLE — DS id and DS timestamp included, which is what the shared
    /// `ChatClient.toEvent` needs — in journal order, only below our own commit,
    /// and only after a successful apply. That the real adapter then surfaces it
    /// runs against a live `Group`, a DS client and the message log, so the double
    /// models the adapter's apply-and-surface contract rather than only its apply
    /// half.
    func testAChatMessageBelowOurOwnCommitIsSurfacedNotSwallowed() async throws {
        let h = makeHarness()
        // An ordinary application message another member sent in the window
        // between the stand-down re-read and our publish, so the DS orders it
        // BELOW our commit. It applies cleanly and moves NO epoch, so our own
        // sweep still WINS — losing is a separate case below.
        var call = 0
        let ports = swapFetch(h) { _ in
            call += 1
            return call == 1
                ? []
                : [SweepJournalRow(id: 50, payload: Data([0xA0]), createdAt: "t-50")]
        }

        let outcome = try await runCommitterSweep(ports)

        // The verdict is unaffected — an application message moves no epoch.
        guard case .committed = outcome else {
            return XCTFail("an application message moves no epoch, so the sweep WON, got \(outcome)")
        }
        // …and the message reached the timeline, carrying the DS row id and the
        // DS row timestamp, which is what the shared event constructor needs.
        XCTAssertEqual(h.group.surfaced, ["row#50@t-50"])
    }

    /// Every applied row, in journal order — nothing reordered, nothing coalesced.
    func testEveryAppliedRowIsSurfacedInJournalOrder() async throws {
        let h = makeHarness()
        var call = 0
        let ports = swapFetch(h) { _ in
            call += 1
            return call == 1 ? [] : [
                SweepJournalRow(id: 50, payload: Data([0xA0]), createdAt: "t-50"),
                SweepJournalRow(id: 51, payload: Data([0xA1]), createdAt: "t-51"),
                SweepJournalRow(id: 52, payload: Data([0xA2]), createdAt: "t-52"),
            ]
        }

        _ = try await runCommitterSweep(ports)

        XCTAssertEqual(h.group.surfaced, ["row#50@t-50", "row#51@t-51", "row#52@t-52"])
    }

    /// A row whose apply THROWS surfaces nothing but still advances the cursor
    /// over it, byte-for-byte as before. A skipped row was never decrypted, so
    /// there is no plaintext to surface and inventing one would be a lie.
    func testARowWhoseApplyThrowsSurfacesNothingAndStillAdvancesTheCursor() async throws {
        var cursorValue: Int64 = 40
        let h = makeHarness()
        var call = 0
        var ports = swapFetch(h) { _ in
            call += 1
            return call == 1 ? [] : [
                SweepJournalRow(id: 50, payload: Data([FakeGroup.unappliableTag]), createdAt: "t-50"),
                SweepJournalRow(id: 51, payload: Data([0xA1]), createdAt: "t-51"),
            ]
        }
        ports.cursor = SweepCursorRef(read: { cursorValue }, advance: { cursorValue = $0 })

        _ = try await runCommitterSweep(ports)

        XCTAssertEqual(h.group.surfaced, ["row#51@t-51"])
        XCTAssertEqual(
            cursorValue, 51,
            "the cursor stops at the last row this run consumed, skipped rows included"
        )
    }

    /// Rows AT or ABOVE our own commit are not part of the question: nothing is
    /// applied, nothing is surfaced, and the cursor is untouched — the poll
    /// re-fetches them and drops our own through the echo filter.
    func testRowsAtOrAboveOurOwnCommitSurfaceNothing() async throws {
        var cursorValue: Int64 = 40
        let h = makeHarness()
        var call = 0
        // Our commit takes DS id 71 (the harness starts `nextId` at 70 and
        // pre-increments), so both of these are ordered at or above it.
        var ports = swapFetch(h) { _ in
            call += 1
            return call == 1 ? [] : [
                SweepJournalRow(id: 71, payload: Data([0xA0]), createdAt: "t-71"),
                SweepJournalRow(id: 72, payload: Data([0xA1]), createdAt: "t-72"),
            ]
        }
        ports.cursor = SweepCursorRef(read: { cursorValue }, advance: { cursorValue = $0 })

        _ = try await runCommitterSweep(ports)

        XCTAssertEqual(h.group.surfaced, [])
        XCTAssertEqual(cursorValue, 40, "the cursor must not move past our own commit")
    }

    /// The verdict apply advances the SAME cursor the poll drives, over rows the
    /// poll will never fetch again — so it is subject to the same ordering rule,
    /// and for the same ADR-0045 reason: a PROPOSAL applied here but never
    /// written to disk, with the cursor already past it, makes the commit that
    /// carries it by reference permanently unappliable. The device sits at epoch
    /// N while the group moves on, silently.
    ///
    /// MEASURED — with the `gate.commit(...)` wrapper in `adjudicate` reverted
    /// to the bare `ports.cursor.advance(lastApplied)` it replaced:
    ///
    ///     testTheVerdictHoldsTheCursorWhenItsSnapshotWriteFails, XCTAssertEqual
    ///     failed: ("51") is not equal to ("40") - the cursor must not pass rows
    ///     whose snapshot write did not land
    func testTheVerdictHoldsTheCursorWhenItsSnapshotWriteFails() async throws {
        var cursorValue: Int64 = 40
        let h = makeHarness()
        h.group.persistAppliedThrows = StubError()
        var call = 0
        var ports = swapFetch(h) { _ in
            call += 1
            return call == 1 ? [] : [
                SweepJournalRow(id: 50, payload: Data([0xA0]), createdAt: "t-50"),
                SweepJournalRow(id: 51, payload: Data([0xA1]), createdAt: "t-51"),
            ]
        }
        ports.cursor = SweepCursorRef(read: { cursorValue }, advance: { cursorValue = $0 })

        let outcome = try await runCommitterSweep(ports)

        // The verdict is still read — the epoch comparison is in memory and
        // unaffected — so a storage failure never turns a won sweep into a lost
        // one.
        if case .committed = outcome {} else {
            XCTFail("a storage failure must not change the verdict, got \(outcome)")
        }
        // The rows still reach the timeline: they were decrypted, they are real.
        XCTAssertEqual(h.group.surfaced, ["row#50@t-50", "row#51@t-51"])
        XCTAssertEqual(
            cursorValue, 40,
            "the cursor must not pass rows whose snapshot write did not land"
        )
    }

    /// ...and the write comes FIRST on the happy path. Order, not merely
    /// presence: `persist-applied` sits between the last apply and the merge.
    func testTheVerdictAdvancesTheCursorOnlyAfterTheSnapshotWriteLands() async throws {
        var cursorValue: Int64 = 40
        let h = makeHarness()
        var call = 0
        var ports = swapFetch(h) { _ in
            call += 1
            return call == 1 ? [] : [
                SweepJournalRow(id: 50, payload: Data([0xA0]), createdAt: "t-50"),
            ]
        }
        ports.cursor = SweepCursorRef(read: { cursorValue }, advance: { cursorValue = $0 })

        _ = try await runCommitterSweep(ports)

        XCTAssertEqual(cursorValue, 50)
        let applied = try XCTUnwrap(
            h.log.firstIndex(of: "apply"), "the apply must have happened: \(h.log)"
        )
        let persisted = try XCTUnwrap(
            h.log.firstIndex(of: "persist-applied"),
            "the durable write must have happened: \(h.log)"
        )
        XCTAssertTrue(
            persisted > applied,
            "the durable write must precede the cursor advance: \(h.log)"
        )
    }

    /// A verdict that applied NOTHING neither persists nor touches the cursor.
    func testAVerdictThatAppliedNothingPersistsNothing() async throws {
        let h = makeHarness()
        _ = try await runCommitterSweep(h.ports)
        XCTAssertFalse(h.log.contains("persist-applied"))
    }

    /// A LOSING sweep still surfaces the rows it applied.
    ///
    /// Losing the ORDER says nothing about the rows below our commit: they were
    /// applied, the ratchet moved and the cursor advanced past them either way, so
    /// the only question left is whether the user gets to read them. On web this
    /// needed a deliberate `finally`; here surfacing happens inline, per row,
    /// inside the apply — which is what makes the losing path automatic rather
    /// than a second thing to remember.
    func testALosingSweepStillSurfacesTheRowsItApplied() async throws {
        let h = makeHarness()
        var call = 0
        let ports = swapFetch(h) { _ in
            call += 1
            return call == 1 ? [] : [
                SweepJournalRow(id: 50, payload: Data([0xA0]), createdAt: "t-50"),
                // The winner's commit, ordered below ours — this is what makes us lose.
                SweepJournalRow(
                    id: 51, payload: Data([FakeGroup.foreignCommitTag]), createdAt: "t-51"),
            ]
        }

        let outcome = try await runCommitterSweep(ports)

        guard case .lost = outcome else {
            return XCTFail("a foreign commit ordered below ours must read as a LOSS, got \(outcome)")
        }
        XCTAssertEqual(h.group.surfaced, ["row#50@t-50", "row#51@t-51"])
    }

    /// The restart / owed-verdict path consumes rows exactly as a fresh sweep
    /// does, so it has to surface them too. Reached through
    /// ``resolveOwedVerdict(_:)``, the OTHER entry point into `adjudicate`.
    func testTheRestartPathSurfacesTheRowsItsOwedVerdictApplied() async throws {
        let h = makeHarness()
        h.group.staged = true
        h.outbox.owed = SweepDurableEntry(
            id: "entry-owed",
            commitB64: Data([0xC0, 0x05]).base64EncodedString(),
            welcomeEnvelopeB64: nil,
            welcomeMailboxesB64: []
        )
        let ports = swapFetch(h) { _ in
            [SweepJournalRow(id: 50, payload: Data([0xA0]), createdAt: "t-50")]
        }

        let outcome = try await resolveOwedVerdict(ports)

        guard case .committed = outcome else {
            return XCTFail("an application message moves no epoch, so the owed verdict WON, got \(outcome)")
        }
        XCTAssertEqual(h.group.surfaced, ["row#50@t-50"])
    }

    // MARK: - A Welcome that failed AFTER the merge is still owed

    /// Drive the harness into the exact shape the finding describes: the commit
    /// won, the merge and the persist both landed, and only the mailbox fan-out
    /// failed. Aborts the calling test if the fan-out somehow did not throw.
    private func ghostMemberState(
        _ h: Harness,
        file: StaticString = #filePath,
        line: UInt = #line
    ) async -> Bool {
        h.journal.welcomePublishThrows = StubError()
        do {
            _ = try await runCommitterSweep(h.ports)
            XCTFail("the Welcome fan-out was supposed to fail", file: file, line: line)
            return false
        } catch {
            return true
        }
    }

    func testAWelcomeThatFailedAfterTheMergeIsRepublished() async throws {
        // THE headline. Before the fix nothing retried: the merge had already
        // cleared `hasStagedSweep()` AND emptied the pending set, which are the
        // two signals `runCommitterSweep` and `resolveOwedVerdict` branch on, so
        // both reported an idle group while the added member sat in every peer's
        // roster, addressed by every later message, unable to ever join.
        let h = makeHarness()
        guard await ghostMemberState(h) else { return }
        XCTAssertEqual(h.group.epoch, 6, "the commit won and merged — that must stand")
        XCTAssertEqual(h.journal.published.filter { $0.kind == "welcome" }.count, 0)

        // The DS comes back; the next sweep runs.
        h.journal.welcomePublishThrows = nil
        let outcome = try await runCommitterSweep(h.ports)

        guard case .welcomesRepublished(let ids, let addedCount) = outcome else {
            return XCTFail("a settled verdict still owing its Welcome must retry it, got \(outcome)")
        }
        XCTAssertEqual(ids.count, 1)
        XCTAssertEqual(addedCount, 1)
        XCTAssertEqual(h.journal.published.filter { $0.kind == "welcome" }.count, 1)
        XCTAssertEqual(h.outbox.acked, ["entry-1"], "acked, so nothing is owed any more")
    }

    func testTheRestartSeamFinishesAnOwedWelcomeWithoutReadjudicating() async throws {
        let h = makeHarness()
        guard await ghostMemberState(h) else { return }
        h.journal.welcomePublishThrows = nil
        let before = h.log.count

        let outcome = try await resolveOwedWelcomes(h.ports)

        guard case .welcomesRepublished = outcome else {
            return XCTFail("the restart seam must finish an owed fan-out, got \(outcome)")
        }
        // The verdict is already known — the retry must not stage, publish a
        // second commit, or apply another row to re-read the journal order.
        XCTAssertEqual(Array(h.log.dropFirst(before)), ["publish:welcome", "ack"])
        XCTAssertEqual(h.journal.published.filter { $0.kind == "commit" }.count, 1)
    }

    func testAFailedWelcomeDoesNotRollTheMergedCommitBack() async throws {
        // The commit won the order and peers are at N+1. Undoing the merge here
        // would strand this device below the group.
        let h = makeHarness()
        guard await ghostMemberState(h) else { return }

        XCTAssertEqual(h.group.epoch, 6)
        XCTAssertFalse(h.group.staged)
        XCTAssertTrue(h.log.contains("merge"))
        XCTAssertFalse(h.log.contains("abandon"))
        XCTAssertNotNil(h.outbox.owedWelcomes, "the marker is what makes the retry possible")
        XCTAssertTrue(h.outbox.acked.isEmpty, "an un-published Welcome must never ack")
    }

    func testTheOwedWelcomeMarkerIsWrittenAfterTheMergeAndBeforeTheFanOut() async throws {
        // The ordering that keeps obligation 2 intact: the marker IS the durable
        // statement "the verdict said WON", so recording it any earlier would
        // let a pre-verdict restart publish a Welcome for a commit that then
        // lost — the three-party fork.
        let h = makeHarness()
        _ = try await runCommitterSweep(h.ports)

        XCTAssertEqual(
            h.log,
            ["fetch", "stage", "record", "publish:commit", "fetch",
             "merge", "welcomes-owed", "publish:welcome", "ack"]
        )
    }

    func testAPreVerdictRestartNeverPublishesTheWelcome() async throws {
        // A staged sweep owes a VERDICT, not a Welcome. `resolveOwedWelcomes`
        // must refuse it outright rather than read the entry's mailboxes, or the
        // obligation-2 ordering collapses on the restart path.
        let h = makeHarness()
        h.group.staged = true
        h.outbox.owed = SweepDurableEntry(
            id: "entry-owed",
            commitB64: Data([0xC0, 0x05]).base64EncodedString(),
            welcomeEnvelopeB64: Data([0xE0, 0x11]).base64EncodedString(),
            welcomeMailboxesB64: [Data(repeating: 0xAA, count: 8).base64EncodedString()]
        )
        // Belt to that braces: even with the marker wrongly present, a staged
        // sweep wins and the Welcome waits.
        h.outbox.owedWelcomes = h.outbox.owed

        let outcome = try await resolveOwedWelcomes(h.ports)

        XCTAssertEqual(outcome, .noWelcomesOwed)
        XCTAssertEqual(h.journal.published.count, 0)
        XCTAssertTrue(h.group.staged)
    }

    func testADuplicateWelcomeRepublishIsANoOp() async throws {
        // The first fan-out may actually have landed and only the ack failed, so
        // a retry can put a second identical Welcome in the joiner's mailbox.
        // That is inert: `StagedWelcome::into_group` deletes the joiner's
        // KeyPackage bundle (openmls-0.8.1 `group/mls_group/creation.rs`), so the
        // second copy cannot be staged and every client's mailbox handler catches
        // and skips it. On THIS side the ack clears the marker, so nothing
        // re-publishes a third time.
        let h = makeHarness()
        guard await ghostMemberState(h) else { return }
        h.journal.welcomePublishThrows = nil

        _ = try await resolveOwedWelcomes(h.ports)
        let again = try await resolveOwedWelcomes(h.ports)

        XCTAssertEqual(again, .noWelcomesOwed)
        XCTAssertEqual(h.journal.published.filter { $0.kind == "welcome" }.count, 1)
    }

    func testASweepWithNoAddOwesNoWelcome() async throws {
        // A remove / role sweep has no Welcome, so it must not pay for a marker
        // write and must leave nothing behind for a restart to chase.
        let h = makeHarness()
        h.group.welcome = nil
        _ = try await runCommitterSweep(h.ports)

        XCTAssertFalse(h.log.contains("welcomes-owed"))
        XCTAssertNil(h.outbox.owedWelcomes)
        let outcome = try await resolveOwedWelcomes(h.ports)
        XCTAssertEqual(outcome, .noWelcomesOwed)
    }

    func testAFailedMarkerWriteStillPublishesTheWelcomeNow() async throws {
        // The marker only buys a RETRY. The verdict is already in, so refusing
        // to publish because the bookkeeping write failed would MANUFACTURE the
        // ghost member this whole branch exists to prevent.
        let h = makeHarness()
        h.outbox.markWelcomesOwedThrows = StubError()

        let outcome = try await runCommitterSweep(h.ports)

        guard case .committed = outcome else {
            return XCTFail("a failed marker write must not abort the fan-out, got \(outcome)")
        }
        XCTAssertEqual(h.journal.published.filter { $0.kind == "welcome" }.count, 1)
    }

    // MARK: - Helpers

    /// Rebuild `ports` with a caller-supplied fetch, so a case can answer the
    /// stand-down re-read and the verdict read differently — they are the same
    /// call with the same cursor, and the whole point of the verdict is that the
    /// journal may have moved between them.
    private func swapFetch(
        _ h: Harness,
        _ fetch: @escaping (Int64) async throws -> [SweepJournalRow]
    ) -> SweepPorts {
        var ports = h.ports
        let inner = ports.journal
        ports.journal = SweepJournalPort(
            fetch: { after in
                h.group.log.append("fetch")
                return try await fetch(after)
            },
            publishGroup: inner.publishGroup,
            publishMailbox: inner.publishMailbox
        )
        return ports
    }
}
