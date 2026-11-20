// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
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

    // MARK: - Retry of recorded commits (2026-11-20)

    private func entry(_ n: Int, channel: String) -> PublisherCommitEntry {
        PublisherCommitEntry(
            id: String(format: "%032x", n),
            kind: .rotation,
            channelIdHex: channel,
            groupIdHex: approve.groupIdHex,
            targetEpoch: 5,
            serverTargetEpoch: 8,
            commitB64: Data("rotation".utf8).base64EncodedString(),
            done: [.journal])
    }

    private let refusal = ChannelsApiError(
        status: 401, code: "owner-proof-required", detail: "this route needs a valid channel owner proof")

    func testARefusalIsTheOwnerProofOrADeletedChannelAndNothingElse() {
        XCTAssertEqual(PublisherCommitFailure.of(refusal), .refused)
        XCTAssertEqual(PublisherCommitFailure.of(
            ChannelsApiError(status: 410, code: "channel-deleted", detail: "")), .refused)
        XCTAssertEqual(PublisherCommitFailure.of(
            ChannelsApiError(status: 503, code: "internal", detail: "")), .transient)
        XCTAssertEqual(PublisherCommitFailure.of(
            ChannelsApiError(status: 409, code: "stale-target-epoch", detail: "")), .transient)
        XCTAssertEqual(PublisherCommitFailure.of(URLError(.notConnectedToInternet)), .transient)
        XCTAssertEqual(PublisherCommitFailure.logLabel(refusal), "401 owner-proof-required")
    }

    /// The storm seen on an iPhone: 172 entries of one channel, each refused
    /// `401`, tried back to back. Now one request, then none until a reset.
    func testAPileOfRefusedEntriesCostsOneRequestAndIsHeldUntilTheNextStart() async {
        var now: Int64 = 1_000_000
        let retry = PublisherCommitRetry(clock: { now })
        let channel = String(repeating: "11", count: 16)
        let entries = (0..<172).map { entry($0, channel: channel) }
        var calls = 0
        await retry.replay(entries, accountIdB64: "acct") { _ in
            calls += 1
            throw self.refusal
        }
        XCTAssertEqual(calls, 1)
        XCTAssertEqual(retry.hold(accountIdB64: "acct", channelIdHex: channel)?.refused, true)
        // Held for the rest of the process, however long it runs.
        now += 24 * 3_600_000
        await retry.replay(entries, accountIdB64: "acct") { _ in calls += 1 }
        XCTAssertEqual(calls, 1)
        XCTAssertFalse(retry.mayAttempt(accountIdB64: "acct", channelIdHex: channel))
        // The next start (or a new binding) tries once more.
        retry.reset()
        await retry.replay(entries, accountIdB64: "acct") { _ in calls += 1 }
        XCTAssertEqual(calls, 1 + entries.count)
    }

    func testARefusedChannelDoesNotHoldAnotherChannel() async {
        let retry = PublisherCommitRetry(clock: { 0 })
        let refused = String(repeating: "11", count: 16)
        let other = String(repeating: "33", count: 16)
        let entries = [entry(1, channel: refused), entry(2, channel: other), entry(3, channel: refused)]
        var ran: [String] = []
        await retry.replay(entries, accountIdB64: "acct") { e in
            ran.append(e.id)
            if e.channelIdHex == refused { throw self.refusal }
        }
        XCTAssertEqual(ran, [entries[0].id, entries[1].id])
        XCTAssertTrue(retry.mayAttempt(accountIdB64: "acct", channelIdHex: other))
    }

    func testATransientFailureWaitsThirtySecondsDoublingToTenMinutes() async {
        var now: Int64 = 5_000_000
        let retry = PublisherCommitRetry(clock: { now })
        let channel = String(repeating: "11", count: 16)
        let entries = [entry(1, channel: channel), entry(2, channel: channel)]
        var calls = 0
        let fail: (PublisherCommitEntry) async throws -> Void = { _ in
            calls += 1
            throw URLError(.timedOut)
        }
        await retry.replay(entries, accountIdB64: "acct", run: fail)
        XCTAssertEqual(calls, 1)
        now += 29_999
        await retry.replay(entries, accountIdB64: "acct", run: fail)
        XCTAssertEqual(calls, 1)
        now += 1
        await retry.replay(entries, accountIdB64: "acct", run: fail)
        XCTAssertEqual(calls, 2)
        // Second consecutive failure: 60 s.
        now += 59_999
        XCTAssertFalse(retry.mayAttempt(accountIdB64: "acct", channelIdHex: channel))
        now += 1
        XCTAssertTrue(retry.mayAttempt(accountIdB64: "acct", channelIdHex: channel))
        // Never more than 10 minutes.
        let many = PublisherCommitHold(refused: false, failures: 40, failedAtMs: 0)
        XCTAssertEqual(many.notBeforeMs, 600_000)
        // A run that finishes clears the hold.
        await retry.replay(entries, accountIdB64: "acct") { _ in calls += 1 }
        XCTAssertNil(retry.hold(accountIdB64: "acct", channelIdHex: channel))
        XCTAssertEqual(calls, 4)
    }

    func testNoRotationIsStagedWhileARecordedCommitOfTheChannelIsOwed() {
        let channel = String(repeating: "11", count: 16)
        let owed = entry(1, channel: channel)
        XCTAssertTrue(ChannelPublisherLoops.hasOwedCommit(channelId: channel.uppercased(), entries: [owed]))
        XCTAssertFalse(ChannelPublisherLoops.hasOwedCommit(
            channelId: String(repeating: "33", count: 16), entries: [owed]))
        let finished = owed.withStepDone(.reseal)
        XCTAssertFalse(ChannelPublisherLoops.hasOwedCommit(channelId: channel, entries: [finished]))
        // An approve owing only its Welcome does not hold a rotation.
        let welcomeOnly = approve.withStepDone(.journal).withStepDone(.reseal)
        XCTAssertFalse(ChannelPublisherLoops.hasOwedCommit(channelId: approve.channelIdHex, entries: [welcomeOnly]))
        // A staged commit not yet answered by the journal does.
        XCTAssertTrue(ChannelPublisherLoops.hasOwedCommit(channelId: approve.channelIdHex, entries: [approve]))
        XCTAssertFalse(ChannelPublisherLoops.hasOwedCommit(channelId: channel, entries: []))
    }

    // MARK: - Round 2 (2026-11-20): journal step, owner key repair, epoch mapping

    /// A staged commit freezes the group's membership: its journal step is
    /// sent every pass, whatever the channel's hold, and an unanswered one
    /// sets no hold.
    func testAnEntryOwingItsJournalStepIsNeverHeld() async {
        let retry = PublisherCommitRetry(clock: { 0 })
        let channel = approve.channelIdHex
        retry.noteFailure(accountIdB64: "acct", channelIdHex: channel, error: refusal)
        XCTAssertFalse(retry.mayAttempt(accountIdB64: "acct", channelIdHex: channel))
        var calls = 0
        for _ in 0..<3 {
            await retry.replay([approve], accountIdB64: "acct", reread: { _ in self.approve }) { _ in
                calls += 1
                throw URLError(.timedOut)
            }
        }
        XCTAssertEqual(calls, 3, "sent again every pass")
        XCTAssertEqual(retry.hold(accountIdB64: "acct", channelIdHex: channel)?.failures, 1, "no hold added")
        // The journal step landed, then the reseal was refused: that holds.
        let merged = approve.withStepDone(.journal)
        let fresh = PublisherCommitRetry(clock: { 0 })
        await fresh.replay([approve], accountIdB64: "acct", reread: { _ in merged }) { _ in throw self.refusal }
        XCTAssertEqual(fresh.hold(accountIdB64: "acct", channelIdHex: channel)?.refused, true)
    }

    func testTheOwnerKeyRepairTriesThePredecessorsNewestFirst() {
        let a = Data(repeating: 1, count: 64), b = Data(repeating: 2, count: 64), c = Data(repeating: 3, count: 64)
        XCTAssertEqual(ChannelOwnerRepair.candidates(predecessorsOldestFirst: [a, b, b, c], currentSeed: c), [b, a])
    }

    /// The service holds the OLDEST predecessor's key: the newer one is not
    /// the owner (401 both ways), the oldest one hands over.
    func testTheOwnerKeyRepairHandsTheChannelToTheCurrentKey() async throws {
        let oldest = Data(repeating: 1, count: 64), newer = Data(repeating: 2, count: 64)
        let current = Data(repeating: 3, count: 64)
        var owner = try Identity.fromSeed(seed: oldest).signaturePublicKey()
        let currentKey = try Identity.fromSeed(seed: current).signaturePublicKey()
        var puts = 0
        let outcome = await ChannelOwnerRepair.repair(predecessors: [oldest, newer], currentSeed: current) { signing, naming in
            puts += 1
            guard signing.signaturePublicKey() == owner else { throw self.refusal }
            owner = naming.signaturePublicKey()
        }
        XCTAssertEqual(outcome, .owned)
        XCTAssertEqual(owner, currentKey)
        XCTAssertEqual(puts, 3, "newer (refused), newer's probe by the current key (refused), oldest")
        // No key of the account owns it: not the owner, nothing moved.
        let stranger = await ChannelOwnerRepair.repair(predecessors: [oldest], currentSeed: current) { _, _ in
            throw self.refusal
        }
        XCTAssertEqual(stranger, .notOwner)
        // Offline: no verdict, tried again.
        let offline = await ChannelOwnerRepair.repair(predecessors: [oldest], currentSeed: current) { _, _ in
            throw URLError(.notConnectedToInternet)
        }
        XCTAssertEqual(offline, .retry)
    }

    func testTheEpochMappingIsReanchoredWhenTheGroupRanAhead() {
        // Normal: at most one commit ahead of the service.
        XCTAssertNil(ChannelDeviceAdmission.reanchoredOffset(mls: 5, offset: nil, serviceEpoch: 5))
        XCTAssertNil(ChannelDeviceAdmission.reanchoredOffset(mls: 6, offset: nil, serviceEpoch: 5))
        XCTAssertNil(ChannelDeviceAdmission.reanchoredOffset(mls: 2, offset: 8, serviceEpoch: 9))
        // 154 commits with no reseal: the current epoch maps to the next one.
        XCTAssertEqual(ChannelDeviceAdmission.reanchoredOffset(mls: 160, offset: nil, serviceEpoch: 6), -153)
        XCTAssertEqual(ChannelDeviceAdmission.channelEpoch(mls: 160, offset: -153), 7)
        // A landed reseal anchors exactly.
        XCTAssertEqual(ChannelDeviceAdmission.landedOffset(mls: 160, landedEpoch: 7), -153)
        XCTAssertNil(ChannelDeviceAdmission.landedOffset(mls: 7, landedEpoch: 7))
    }

    /// After the anchor the right seed is kept at the landed epoch and the
    /// seeds filed above it under the broken mapping are gone, so the next
    /// rotation keeps its outgoing seed where readers expect it.
    func testAnAnchorKeepsTheLandedSeedAndDropsSeedsFiledAbove() {
        let channels = ChannelsUiState()
        let id = String(repeating: "a1", count: 16)
        channels.addChannel(ChannelHandle(id: id, localName: "News", localDescription: "", addedAtMs: 1))
        let first = Data(repeating: 1, count: 32), current = Data(repeating: 9, count: 32)
        channels.setKeys(channelId: id, keys: ChannelPublisherKeys(
            seed: current, signingKeySeed: Data(repeating: 2, count: 32), seedEpoch: 160))
        channels.setSubscribedSeed(channelId: id, epoch: 6, seed: Data(repeating: 6, count: 32))
        channels.setSubscribedSeed(channelId: id, epoch: 7, seed: first)
        channels.setSubscribedSeed(channelId: id, epoch: 90, seed: Data(repeating: 4, count: 32))
        ChannelDeviceAdmission.anchor(channelId: id, state: channels, mls: 160, epoch: 7, seed: current)
        XCTAssertEqual(channels.extras(id).epochOffset, -153)
        XCTAssertEqual(channels.subscribedSeeds(channelId: id)[7], current)
        XCTAssertEqual(channels.subscribedSeeds(channelId: id)[6], Data(repeating: 6, count: 32))
        XCTAssertNil(channels.subscribedSeeds(channelId: id)[90])
        // The composer moves down to the anchored epoch.
        channels.notePublisherSeedAt(channelId: id, epoch: 7, seed: current)
        XCTAssertEqual(channels.keysByChannel[id]?.seedEpoch, 7)
        XCTAssertNil(channels.subscribedSeeds(channelId: id)[160])
    }

    /// 2026-11-20 (ADR-0074) — after a rotation every client signs owner
    /// proofs with the current key and co-publisher proofs with the lineage
    /// key (the one the appointed binding names), whichever the caller passed.
    func testPublisherProofsUseTheCurrentKeyForTheOwnerAndTheLineageKeyForACoPublisher() throws {
        let savedOwner = ChannelPublisherIdentity.ownerAccountSeed
        defer {
            ChannelPublisherIdentity.noteOwnerSeed(savedOwner)
            ChannelPublisherIdentity.noteCurrentSeed(nil)
        }
        let lineageSeed = Data(repeating: 4, count: 64), currentSeed = Data(repeating: 5, count: 64)
        let lineage = try Identity.fromSeed(seed: lineageSeed), current = try Identity.fromSeed(seed: currentSeed)
        let stranger = try Identity.fromSeed(seed: Data(repeating: 6, count: 64))
        ChannelPublisherIdentity.noteSessionSeeds(accountIdB64: "acct", current: currentSeed, owner: lineageSeed)
        for passed in [lineage, current] {
            XCTAssertEqual(ChannelPublisherIdentity.proofIdentity(role: .owner, passed: passed).signaturePublicKey(),
                           current.signaturePublicKey())
            XCTAssertEqual(ChannelPublisherIdentity.proofIdentity(role: .copublisher, passed: passed).signaturePublicKey(),
                           lineage.signaturePublicKey())
        }
        XCTAssertEqual(ChannelPublisherIdentity.proofIdentity(role: .copublisher, passed: stranger).signaturePublicKey(),
                       stranger.signaturePublicKey(), "another account's identity is used as passed")
        // No rotation: the one identity.
        ChannelPublisherIdentity.noteSessionSeeds(accountIdB64: "acct", current: lineageSeed, owner: lineageSeed)
        XCTAssertEqual(ChannelPublisherIdentity.proofIdentity(role: .owner, passed: lineage).signaturePublicKey(),
                       lineage.signaturePublicKey())
    }

    /// 2026-11-20 — every owner request (create, listing, appointments,
    /// delete, the subscriber list) takes its signer from the one mapping:
    /// after a rotation the current key, whichever of the account's
    /// identities was passed; only the handover signs by an exact key.
    func testEveryOwnerSignerIsTheCurrentKeysAfterARotation() throws {
        let savedOwner = ChannelPublisherIdentity.ownerAccountSeed
        defer {
            ChannelPublisherIdentity.noteOwnerSeed(savedOwner)
            ChannelPublisherIdentity.noteCurrentSeed(nil)
        }
        let lineageSeed = Data(repeating: 7, count: 64), currentSeed = Data(repeating: 8, count: 64)
        let lineage = try Identity.fromSeed(seed: lineageSeed), current = try Identity.fromSeed(seed: currentSeed)
        let channel = String(repeating: "c3", count: 16)
        ChannelPublisherIdentity.noteSessionSeeds(accountIdB64: "acct", current: currentSeed, owner: lineageSeed)
        let expected = try ChannelsClient.ownerSignerExact(accountIdentity: current, channelIdHex: channel).verifyingKey()
        XCTAssertEqual(try ChannelsClient.ownerSigner(accountIdentity: lineage, channelIdHex: channel).verifyingKey(), expected)
        XCTAssertEqual(try ChannelsClient.ownerSigner(accountIdentity: current, channelIdHex: channel).verifyingKey(), expected)
        XCTAssertNotEqual(
            try ChannelsClient.ownerSignerExact(accountIdentity: lineage, channelIdHex: channel).verifyingKey(), expected,
            "the handover still signs by the old key")
    }

    /// 2026-11-20 — an owner-signed request as the first channel call of a
    /// session: nothing noted the seeds yet, and the mapping still signs by
    /// the current key, deriving the seeds from the stored account seed and
    /// its own predecessors.
    func testTheFirstOwnerRequestOfASessionIsMappedFromTheStoredSeeds() throws {
        let savedOwner = ChannelPublisherIdentity.ownerAccountSeed
        defer {
            ChannelPublisherIdentity.noteOwnerSeed(savedOwner)
            ChannelPublisherIdentity.noteCurrentSeed(nil)
        }
        ChannelPublisherIdentity.noteOwnerSeed(nil)
        ChannelPublisherIdentity.noteCurrentSeed(nil)
        let lineageSeed = Data(repeating: 11, count: 64), currentSeed = Data(repeating: 12, count: 64)
        var reads = 0
        let seeds = ChannelPublisherIdentity.sessionSeeds(
            boundAccountIdB64: "acct",
            storedCurrentSeed: { _ in reads += 1; return currentSeed },
            lineageSeed: { _, _ in lineageSeed },
            storedBindingKey: { _ in nil })
        XCTAssertEqual(seeds?.current, currentSeed)
        XCTAssertEqual(seeds?.owner, lineageSeed)
        // Noted now: later requests read no store.
        _ = ChannelPublisherIdentity.sessionSeeds(
            boundAccountIdB64: "acct", storedCurrentSeed: { _ in reads += 1; return nil },
            lineageSeed: { _, _ in lineageSeed }, storedBindingKey: { _ in nil })
        XCTAssertEqual(reads, 1)
        let lineage = try Identity.fromSeed(seed: lineageSeed), current = try Identity.fromSeed(seed: currentSeed)
        let channel = String(repeating: "c4", count: 16)
        XCTAssertEqual(
            try ChannelsClient.ownerSigner(accountIdentity: lineage, channelIdHex: channel).verifyingKey(),
            try ChannelsClient.ownerSignerExact(accountIdentity: current, channelIdHex: channel).verifyingKey())
        // Another account bound later is not served the first one's seeds.
        XCTAssertNil(ChannelPublisherIdentity.sessionSeeds(
            boundAccountIdB64: "other", storedCurrentSeed: { _ in nil }, lineageSeed: { _, _ in nil },
            storedBindingKey: { _ in nil }))
    }

    /// 2026-11-20 — a device whose binding names the CURRENT key, first proof
    /// of a session (nothing noted): the stored binding decides the
    /// co-publisher key, and owner proofs take the current key; with the
    /// binding unreadable, owner proofs still take the current key and the
    /// binding is read again on the next proof.
    func testTheFirstProofReadsTheStoredBindingAndOwnerProofsTakeTheCurrentKey() throws {
        let savedOwner = ChannelPublisherIdentity.ownerAccountSeed
        defer {
            ChannelPublisherIdentity.noteOwnerSeed(savedOwner)
            ChannelPublisherIdentity.noteCurrentSeed(nil)
        }
        let lineageSeed = Data(repeating: 21, count: 64), currentSeed = Data(repeating: 22, count: 64)
        let lineage = try Identity.fromSeed(seed: lineageSeed), current = try Identity.fromSeed(seed: currentSeed)

        // The binding names the current key.
        ChannelPublisherIdentity.noteOwnerSeed(nil)
        ChannelPublisherIdentity.noteCurrentSeed(nil)
        let bound = try XCTUnwrap(ChannelPublisherIdentity.sessionSeeds(
            boundAccountIdB64: "acct", storedCurrentSeed: { _ in currentSeed },
            lineageSeed: { _, _ in lineageSeed }, storedBindingKey: { _ in current.signaturePublicKey() }))
        XCTAssertEqual(bound.owner, currentSeed)
        for passed in [lineage, current] {
            XCTAssertEqual(ChannelPublisherIdentity.proofIdentity(role: .owner, passed: passed, seeds: bound)
                .signaturePublicKey(), current.signaturePublicKey())
            XCTAssertEqual(ChannelPublisherIdentity.proofIdentity(role: .copublisher, passed: passed, seeds: bound)
                .signaturePublicKey(), current.signaturePublicKey())
        }

        // The binding cannot be read now.
        ChannelPublisherIdentity.noteOwnerSeed(nil)
        ChannelPublisherIdentity.noteCurrentSeed(nil)
        var bindingReads = 0
        let unread = try XCTUnwrap(ChannelPublisherIdentity.sessionSeeds(
            boundAccountIdB64: "acct", storedCurrentSeed: { _ in currentSeed },
            lineageSeed: { _, _ in lineageSeed }, storedBindingKey: { _ in bindingReads += 1; return nil }))
        XCTAssertEqual(ChannelPublisherIdentity.proofIdentity(role: .owner, passed: lineage, seeds: unread)
            .signaturePublicKey(), current.signaturePublicKey(), "owner proofs take the current key")
        XCTAssertEqual(ChannelPublisherIdentity.proofIdentity(role: .copublisher, passed: current, seeds: unread)
            .signaturePublicKey(), lineage.signaturePublicKey())
        // Read again on the next proof, and decided once it reads.
        let later = try XCTUnwrap(ChannelPublisherIdentity.sessionSeeds(
            boundAccountIdB64: "acct", storedCurrentSeed: { _ in nil },
            lineageSeed: { _, _ in lineageSeed },
            storedBindingKey: { _ in bindingReads += 1; return current.signaturePublicKey() }))
        XCTAssertEqual(bindingReads, 2)
        XCTAssertEqual(later.owner, currentSeed)
    }
}
