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
        joinerKeyB64: Data(repeating: 0x6A, count: 32).base64EncodedString())

    private var rotation: PublisherCommitEntry {
        PublisherCommitEntry(
            id: String(repeating: "0b", count: 16),
            kind: .rotation,
            channelIdHex: approve.channelIdHex,
            groupIdHex: approve.groupIdHex,
            targetEpoch: 5,
            serverTargetEpoch: 8)
    }

    private final class Recorder {
        var calls: [String] = []
        var saved: [PublisherCommitEntry] = []
        var finished: PublisherCommitEntry?
    }

    private func ports(_ r: Recorder, failAt: String? = nil, serverEpoch: UInt32 = 7) -> PublisherCommitPorts {
        func step(_ name: String) throws {
            r.calls.append(name)
            if name == failAt { throw Failure() }
        }
        return PublisherCommitPorts(
            registerEpoch: { try step("register") },
            publishCommit: { try step("publish:\(String(decoding: $0, as: UTF8.self))") },
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
            "register", "publish:commit", "welcome:welcome", "server-epoch", "reseal:8", "appoint",
        ])
        XCTAssertEqual(r.saved.map(\.done), [
            [.journal], [.journal, .welcome], [.journal, .welcome, .reseal],
            [.journal, .welcome, .reseal, .appoint],
        ])
        XCTAssertEqual(r.finished?.id, approve.id)
    }

    func testARotationRegistersAndResealsAndPublishesNothing() async throws {
        let r = Recorder()
        try await PublisherCommitRunner.run(rotation, ports: ports(r))
        XCTAssertEqual(r.calls, ["register", "server-epoch", "reseal:8"])
    }

    func testAFailedStepLeavesWhatLandedAndTheNextRunResumesThere() async throws {
        let first = Recorder()
        do {
            try await PublisherCommitRunner.run(approve, ports: ports(first, failAt: "reseal:8"))
            XCTFail("the reseal failure must surface")
        } catch {}
        XCTAssertNil(first.finished)
        let recorded = try XCTUnwrap(first.saved.last)
        XCTAssertEqual(recorded.remainingSteps, [.reseal, .appoint])
        let second = Recorder()
        try await PublisherCommitRunner.run(recorded, ports: ports(second))
        XCTAssertEqual(second.calls, ["server-epoch", "reseal:8", "appoint"])
        XCTAssertEqual(second.finished?.done, [.journal, .welcome, .reseal, .appoint])
    }

    func testAResealTheServiceAlreadyHoldsIsNotSentAgain() async throws {
        let r = Recorder()
        var entry = rotation
        entry.done = [.journal]
        try await PublisherCommitRunner.run(entry, ports: ports(r, serverEpoch: 8))
        XCTAssertEqual(r.calls, ["server-epoch"])
        XCTAssertEqual(r.finished?.done, [.journal, .reseal])
    }

    func testARefusedRegistrationDoesNotHoldARotation() async throws {
        let r = Recorder()
        try await PublisherCommitRunner.run(rotation, ports: ports(r, failAt: "register"))
        XCTAssertEqual(r.calls, ["register", "server-epoch", "reseal:8"])
        XCTAssertEqual(r.finished?.done, [.journal, .reseal])
    }

    func testARefusedRegistrationStopsAnApproveBeforeItsCommitIsPublished() async throws {
        let r = Recorder()
        do {
            try await PublisherCommitRunner.run(approve, ports: ports(r, failAt: "register"))
            XCTFail("the registration failure must surface")
        } catch {}
        XCTAssertEqual(r.calls, ["register"])
        XCTAssertTrue(r.saved.isEmpty)
        XCTAssertNil(r.finished)
    }

    func testAFailedAppointmentNeverHoldsTheEntry() async throws {
        let r = Recorder()
        var entry = approve
        entry.done = [.journal, .welcome, .reseal]
        try await PublisherCommitRunner.run(entry, ports: ports(r, failAt: "appoint"))
        XCTAssertNotNil(r.finished)
    }

    func testARestartDropsAnEntryBelowItsTargetOrWithoutAGroupAndReplaysTheRest() {
        XCTAssertEqual(PublisherCommitEntry.replayVerdict(approve, restoredEpoch: 4), .discard)
        XCTAssertEqual(PublisherCommitEntry.replayVerdict(approve, restoredEpoch: nil), .discard)
        XCTAssertEqual(PublisherCommitEntry.replayVerdict(approve, restoredEpoch: 5), .replay)
        XCTAssertEqual(PublisherCommitEntry.replayVerdict(approve, restoredEpoch: 6), .replay)
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
