// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// G-50 — the durable record of a PublisherGroup commit: the steps run in
/// order and each is recorded as it lands; a failure leaves the entry with
/// what landed and the next run resumes there; a restart drops an entry whose
/// group never reached its target epoch; a reseal already on the channels
/// service is not sent twice; a rotation's epoch registration stays best
/// effort while an approve's is required. Mirrors
/// `apps/web/src/channels/publisher-commit-outbox.test.ts`.
@MainActor
final class PublisherCommitOutboxTests: XCTestCase {

    private struct Failure: Error {}

    private let approve = PublisherCommitEntry(
        id: String(repeating: "0a", count: 16),
        kind: .approve,
        channelIdHex: String(repeating: "11", count: 16),
        groupIdHex: String(repeating: "22", count: 32),
        targetEpoch: 5,
        serverTargetEpoch: 8,
        commitB64: Data("commit".utf8).base64EncodedString(),
        welcomeB64: Data("welcome".utf8).base64EncodedString(),
        joinerKeyB64: Data(repeating: 0x6A, count: 32).base64EncodedString(),
        proofEpoch: 4,
        proofSeedB64: Data(repeating: 0x73, count: 32).base64EncodedString())

    private var rotation: PublisherCommitEntry {
        PublisherCommitEntry(
            id: String(repeating: "0b", count: 16),
            kind: .rotation,
            channelIdHex: approve.channelIdHex,
            groupIdHex: approve.groupIdHex,
            targetEpoch: 5,
            serverTargetEpoch: 8,
            commitB64: Data("rotation".utf8).base64EncodedString())
    }

    private final class Recorder {
        var calls: [String] = []
        var saved: [PublisherCommitEntry] = []
        var finished: PublisherCommitEntry?
        var proofs: [UInt64?] = []
    }

    private func ports(
        _ r: Recorder, failAt: String? = nil, serverEpoch: UInt32 = 7, lost: Bool = false,
        merged: Bool = false
    ) -> PublisherCommitPorts {
        func step(_ name: String) throws {
            r.calls.append(name)
            if name == failAt { throw Failure() }
        }
        return PublisherCommitPorts(
            commitToJournal: { commit, proof in
                r.proofs.append(proof?.epoch)
                try step("commit:\(String(decoding: commit, as: UTF8.self))")
            },
            isLost: { _ in lost },
            alreadyMerged: { merged },
            adjudicate: { accepted in try step(accepted ? "merge" : "abandon") },
            deliverWelcome: { welcome, _ in try step("welcome:\(String(decoding: welcome, as: UTF8.self))") },
            serverEpoch: {
                try step("server-epoch")
                return serverEpoch
            },
            reseal: { try step("reseal:\($0)") },
            appoint: { _ in try step("appoint") },
            save: { r.saved.append($0) },
            finish: { r.finished = $0 })
    }

    func testAnApproveRunsTheJournalTheWelcomeTheResealAndTheAppointmentInOrder() async throws {
        let r = Recorder()
        try await PublisherCommitRunner.run(approve, ports: ports(r))
        XCTAssertEqual(r.calls, [
            "commit:commit", "merge", "server-epoch", "reseal:8", "welcome:welcome", "appoint",
        ])
        XCTAssertEqual(r.saved.map(\.done), [
            [.journal], [.journal, .reseal], [.journal, .reseal, .welcome],
            [.journal, .reseal, .welcome, .appoint],
        ])
        XCTAssertEqual(r.finished?.id, approve.id)
    }

    /// ADR-0090 section 3 — a rotation's staged commit goes to the DS, is
    /// merged once taken, and only then the reseal.
    func testARotationCommitsMergesThenReseals() async throws {
        let r = Recorder()
        try await PublisherCommitRunner.run(rotation, ports: ports(r))
        XCTAssertEqual(r.calls, ["commit:rotation", "merge", "server-epoch", "reseal:8"])
    }

    /// ADR-0087 phase B — a removal commits, revokes the appointment, then
    /// reseals to the seed the removed person cannot derive; a failed
    /// revocation does not hold the entry.
    func testARemovalCommitsRevokesTheAppointmentAndReseals() async throws {
        let removal = PublisherCommitEntry(
            id: String(repeating: "0c", count: 16), kind: .removal,
            channelIdHex: approve.channelIdHex, groupIdHex: approve.groupIdHex,
            targetEpoch: 5, serverTargetEpoch: 8,
            commitB64: Data("removal".utf8).base64EncodedString(),
            joinerKeyB64: Data(repeating: 0x6B, count: 32).base64EncodedString())
        XCTAssertTrue(removal.isWellFormed)
        let r = Recorder()
        var p = ports(r)
        var revoked: [Data] = []
        p.unappoint = { key in
            revoked.append(key)
            throw Failure()
        }
        try await PublisherCommitRunner.run(removal, ports: p)
        XCTAssertEqual(r.calls, ["commit:removal", "merge", "server-epoch", "reseal:8"])
        XCTAssertEqual(revoked, [Data(repeating: 0x6B, count: 32)])
        XCTAssertEqual(r.saved.last?.done, [.journal, .unappoint, .reseal])
        XCTAssertEqual(r.finished?.id, removal.id)
    }

    func testAFailedStepLeavesWhatLandedAndTheNextRunResumesThere() async throws {
        let first = Recorder()
        do {
            try await PublisherCommitRunner.run(approve, ports: ports(first, failAt: "reseal:8"))
            XCTFail("the reseal failure must surface")
        } catch {}
        XCTAssertNil(first.finished)
        let recorded = try XCTUnwrap(first.saved.last)
        XCTAssertEqual(recorded.remainingSteps, [.reseal, .welcome, .appoint])
        let second = Recorder()
        try await PublisherCommitRunner.run(recorded, ports: ports(second))
        XCTAssertEqual(second.calls, ["server-epoch", "reseal:8", "welcome:welcome", "appoint"])
        XCTAssertEqual(second.finished?.done, [.journal, .reseal, .welcome, .appoint])
    }

    func testAResealTheServiceAlreadyHoldsIsNotSentAgain() async throws {
        let r = Recorder()
        var entry = rotation
        entry.done = [.journal]
        try await PublisherCommitRunner.run(entry, ports: ports(r, serverEpoch: 8))
        XCTAssertEqual(r.calls, ["server-epoch"])
        XCTAssertEqual(r.finished?.done, [.journal, .reseal])
    }

    /// ADR-0081 — the journal step is proven under the recorded seed of the
    /// epoch the commit left, so a replay after a restart can still send a
    /// commit that never reached the DS.
    func testTheJournalStepIsProvenUnderTheRecordedEpoch() async throws {
        let r = Recorder()
        try await PublisherCommitRunner.run(approve, ports: ports(r))
        XCTAssertEqual(r.proofs, [4])
    }

    /// ADR-0090 — a replay after the merge whose `done` mark did not land
    /// sends nothing again and goes on with the steps.
    func testAnAlreadyMergedCommitIsNotSentAgain() async throws {
        let r = Recorder()
        try await PublisherCommitRunner.run(rotation, ports: ports(r, merged: true))
        XCTAssertEqual(r.calls, ["server-epoch", "reseal:8"])
        XCTAssertEqual(r.finished?.done, [.journal, .reseal])
    }

    /// ADR-0090 — an unanswered rotation commit stays staged and owed: no
    /// merge, no reseal; the next run sends it again (C-90 proposed: no seed
    /// reaches subscribers from a commit the journal did not take).
    func testAnUnansweredRotationCommitResealsNothingAndStaysOwed() async throws {
        let r = Recorder()
        do {
            try await PublisherCommitRunner.run(rotation, ports: ports(r, failAt: "commit:rotation"))
            XCTFail("the unanswered commit must surface")
        } catch {}
        XCTAssertEqual(r.calls, ["commit:rotation"])
        XCTAssertNil(r.finished, "kept, owed")
        let again = Recorder()
        try await PublisherCommitRunner.run(rotation, ports: ports(again, serverEpoch: 7))
        XCTAssertEqual(again.calls, ["commit:rotation", "merge", "server-epoch", "reseal:8"])
    }

    /// ADR-0090 — a rotation lost to another owner device's commit (`409`)
    /// is abandoned and reseals nothing.
    func testALostRotationIsAbandonedAndResealsNothing() async throws {
        let r = Recorder()
        try await PublisherCommitRunner.run(
            rotation, ports: ports(r, failAt: "commit:rotation", lost: true))
        XCTAssertEqual(r.calls, ["commit:rotation", "abandon"])
        XCTAssertEqual(r.finished?.journalLost, true)
        XCTAssertEqual(r.finished?.done, [])
    }

    func testALostApproveIsAbandonedWithNoWelcomeNoAppointmentNoReseal() async throws {
        let r = Recorder()
        try await PublisherCommitRunner.run(
            approve, ports: ports(r, failAt: "commit:commit", lost: true))
        XCTAssertEqual(r.calls, ["commit:commit", "abandon"])
        XCTAssertEqual(r.finished?.journalLost, true)
    }

    func testAnUnansweredCommitStopsAnApproveBeforeTheWelcome() async throws {
        let r = Recorder()
        do {
            try await PublisherCommitRunner.run(approve, ports: ports(r, failAt: "commit:commit"))
            XCTFail("the commit failure must surface")
        } catch {}
        XCTAssertEqual(r.calls, ["commit:commit"])
        XCTAssertTrue(r.saved.isEmpty)
        XCTAssertNil(r.finished)
    }

    func testAFailedAppointmentNeverHoldsTheEntry() async throws {
        let r = Recorder()
        var entry = approve
        entry.done = [.journal, .reseal, .welcome]
        try await PublisherCommitRunner.run(entry, ports: ports(r, failAt: "appoint"))
        XCTAssertNotNil(r.finished)
    }

    func testARestartDropsAnEntryBelowItsTargetOrWithoutAGroupAndReplaysTheRest() {
        XCTAssertEqual(PublisherCommitEntry.replayVerdict(approve, restoredEpoch: 4), .discard)
        XCTAssertEqual(PublisherCommitEntry.replayVerdict(approve, restoredEpoch: nil), .discard)
        XCTAssertEqual(PublisherCommitEntry.replayVerdict(approve, restoredEpoch: 5), .replay)
        XCTAssertEqual(PublisherCommitEntry.replayVerdict(approve, restoredEpoch: 6), .replay)
    }

    /// ADR-0090 — a group still holding the staged commit at N: the owed
    /// verdict, replayed (the journal step first); an abandoned entry is
    /// dropped.
    func testAStagedCommitIsReplayedAndALostOneDropped() {
        XCTAssertEqual(PublisherCommitEntry.replayVerdict(approve, restoredEpoch: 4, staged: true), .replay)
        XCTAssertEqual(PublisherCommitEntry.replayVerdict(approve, restoredEpoch: 4, staged: false), .discard)
        var lost = approve
        lost.journalLost = true
        XCTAssertEqual(PublisherCommitEntry.replayVerdict(lost, restoredEpoch: 5), .discard)
    }

    /// ADR-0090 — an admission entry runs the journal, the Welcome to the
    /// account, then the reseal.
    func testADevicesEntryCommitsWelcomesAndReseals() async throws {
        let devices = PublisherCommitEntry(
            id: String(repeating: "0d", count: 16), kind: .devices,
            channelIdHex: approve.channelIdHex, groupIdHex: approve.groupIdHex,
            targetEpoch: 5, serverTargetEpoch: 8,
            commitB64: Data("devices".utf8).base64EncodedString(),
            welcomeB64: Data("welcome".utf8).base64EncodedString(),
            joinerKeyB64: Data(repeating: 0x6B, count: 32).base64EncodedString())
        XCTAssertTrue(devices.isWellFormed)
        let r = Recorder()
        try await PublisherCommitRunner.run(devices, ports: ports(r))
        XCTAssertEqual(r.calls, ["commit:devices", "merge", "server-epoch", "reseal:8", "welcome:welcome"])
    }

    func testAPersistedEntryRoundTripsAndAMalformedOneIsRefused() throws {
        var entry = approve
        entry.done = [.journal]
        let back = try JSONDecoder().decode(
            PublisherCommitEntry.self, from: try JSONEncoder().encode(entry))
        XCTAssertEqual(back, entry)
        XCTAssertTrue(back.isWellFormed)
        XCTAssertTrue(rotation.isWellFormed)
        let badGroup = PublisherCommitEntry(
            id: approve.id, kind: .rotation, channelIdHex: approve.channelIdHex,
            groupIdHex: "zz", targetEpoch: 1, serverTargetEpoch: 1)
        XCTAssertFalse(badGroup.isWellFormed)
        // An approve needs its commit, its Welcome and the joiner.
        let noCommit = PublisherCommitEntry(
            id: approve.id, kind: .approve, channelIdHex: approve.channelIdHex,
            groupIdHex: approve.groupIdHex, targetEpoch: 1, serverTargetEpoch: 1,
            welcomeB64: approve.welcomeB64, joinerKeyB64: approve.joinerKeyB64)
        XCTAssertFalse(noCommit.isWellFormed)
    }
}
