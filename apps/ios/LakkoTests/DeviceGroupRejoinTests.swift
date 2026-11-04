// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0081 decisions 3 and 4 on iOS — a device that lost or fell behind its
/// DeviceGroup rejoins through a sibling; re-founding is a single-device
/// account's; every DeviceGroup snapshot write fails closed and a stale
/// handle's write is refused. Mirrors
/// `apps/web/src/devices/device-group-rejoin.test.ts`,
/// `device-group-snapshot-store.test.ts` and `lifecycle-founding.test.ts`.
@MainActor
final class DeviceGroupRejoinTests: XCTestCase {

    private let sibling = Data(repeating: 0xAA, count: 32)
    private let lost = Data(repeating: 0xBB, count: 32)
    private let other = Data(repeating: 0xCC, count: 32)

    // MARK: Behind

    func testBehindIsADrainedReadStillBehindTheDsWithNoOwnCommitInFlight() {
        XCTAssertTrue(DeviceGroupRejoin.isBehind(localEpoch: 5, drained: true, latestEpoch: 7, commitPending: false))
        XCTAssertFalse(DeviceGroupRejoin.isBehind(localEpoch: 5, drained: true, latestEpoch: 5, commitPending: false))
        XCTAssertFalse(DeviceGroupRejoin.isBehind(localEpoch: 5, drained: false, latestEpoch: 7, commitPending: false),
                       "rows still to read may bring it forward")
        XCTAssertFalse(DeviceGroupRejoin.isBehind(localEpoch: 5, drained: true, latestEpoch: nil, commitPending: false),
                       "an unclaimed journal names no epoch")
        XCTAssertFalse(DeviceGroupRejoin.isBehind(localEpoch: 5, drained: true, latestEpoch: 6, commitPending: true),
                       "its own commit in flight")
    }

    // MARK: A sibling's admission rule

    private func input(_ change: (inout DeviceGroupRejoin.AdmissionInput) -> Void = { _ in }) -> DeviceGroupRejoin.AdmissionInput {
        var i = DeviceGroupRejoin.AdmissionInput(
            knockId: 10, knockAtMs: 1_000_000, key: lost, ownKey: sibling,
            roster: [.init(leaf: 0, key: sibling), .init(leaf: 1, key: lost), .init(leaf: 2, key: other)],
            handled: [], watchingSinceMs: 500_000, lastAddedAtMs: nil, listed: true,
            policyRequiresAttestation: false, nowMs: 1_000_100)
        change(&i)
        return i
    }

    func testALostDeviceWhosePreviousLeafIsStillInTheRosterIsAdmitted() {
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input()), .admit)
    }

    func testADeviceWhoseLeafIsGoneTooIsAdmitted() {
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input {
            $0.roster = [.init(leaf: 0, key: self.sibling), .init(leaf: 2, key: self.other)]
        }), .admit)
    }

    func testARemovedDeviceIsNeverAdmitted() {
        // Deleted from the directory and tombstoned there (ADR-0074): not listed.
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.listed = false }), .notListed)
    }

    func testAKnockIsNotAnsweredTwice() {
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.lastAddedAtMs = 1_000_050 }), .alreadyAdmitted)
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.handled = [10] }), .handled)
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.lastAddedAtMs = 900_000 }), .admit,
                       "a knock after the last addition (the device lost its state again) is new")
    }

    func testAKnockFromBeforeWatchingIsIgnored() {
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.watchingSinceMs = 1_000_001 }), .beforeWatching)
    }

    func testTheLowestOtherLeafAdmitsAtOnceAnyOtherSiblingAfterTheFallback() {
        let notLowest = input { $0.ownKey = self.other }
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(notLowest), .notMineYet)
        var late = notLowest
        late.nowMs = notLowest.knockAtMs + DeviceGroupRejoin.fallbackAdmitAfterMs
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(late), .admit)
    }

    func testTheKnockingKeyIsNeverTheDesignatedAdmitter() {
        let roster: [DeviceGroupRejoin.RosterMember] = [.init(leaf: 0, key: lost), .init(leaf: 1, key: sibling)]
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.roster = roster }), .admit)
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.roster = roster; $0.ownKey = self.lost }), .ownKey)
    }

    func testUnderThePolicyASelfRemovalProofThatCoversTheReplacementAdmits() {
        // ADR-0081 amendment.
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input {
            $0.policyRequiresAttestation = true
            $0.selfRemovalProofCovers = true
        }), .admit)
    }

    func testAKnockWhoseKeyPackageIsAlreadyALeafWasAnswered() {
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.keyPackageAlreadyIn = true }), .alreadyAdmitted)
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input {
            $0.keyPackageAlreadyIn = true
            $0.selfRemovalProofCovers = true
        }), .alreadyAdmitted)
    }

    func testUnderTheAttestedRemovalPolicyAPresentPreviousLeafWithoutACoveringProofWaitsForThePerson() {
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input { $0.policyRequiresAttestation = true }), .needsAttestation)
        XCTAssertEqual(DeviceGroupRejoin.admissionVerdict(input {
            $0.policyRequiresAttestation = true
            $0.roster = [.init(leaf: 0, key: self.sibling)]
        }), .admit)
    }

    func testTheRecordKeepsTheLatestAdditionAndABoundedHandledSet() throws {
        var r = DeviceGroupAdmissionRecord(watchingSinceMs: 5)
        r = r.noteAdded(keyHex: "bb", atMs: 100).noteAdded(keyHex: "bb", atMs: 50)
        XCTAssertEqual(r.lastAddedAtMs["bb"], 100)
        for i in 0..<300 { r = r.noteHandled(Int64(i)) }
        XCTAssertEqual(r.handled.count, 256)
        XCTAssertEqual(r.handled.first, 44)
        XCTAssertEqual(DeviceGroupAdmissionRecord.decode(try r.encoded()), r)
        XCTAssertNil(DeviceGroupAdmissionRecord.decode(Data(#"{"v":2}"#.utf8)))
    }

    // MARK: The rejoining device's run

    @MainActor
    private final class RunModel {
        var now: Int64 = 1_000
        var attempts = 0
        var log: [String] = []
        var state: DeviceGroupRejoinState?
        let welcomeAfter: Int?

        init(welcomeAfter: Int?) { self.welcomeAfter = welcomeAfter }

        var ports: DeviceGroupRejoinPorts {
            DeviceGroupRejoinPorts(
                dropLocalState: { [unowned self] in self.log.append("drop") },
                publishDirectoryKeyPackage: { [unowned self] in self.log.append("directory-kp") },
                loadState: { [unowned self] in self.state },
                saveState: { [unowned self] in self.state = $0 },
                knock: { [unowned self] at in
                    self.log.append("knock")
                    return DeviceGroupRejoinState(rendezvousB64: "r\(at)", joinerSnapshotB64: "s", knockedAtMs: at)
                },
                awaitWelcome: { [unowned self] _, deadline in
                    self.attempts += 1
                    if let after = self.welcomeAfter, self.attempts > after { return Data([1]) }
                    self.now = deadline
                    return nil
                },
                join: { [unowned self] _, _ in self.log.append("join") },
                clearState: { [unowned self] in self.log.append("clear"); self.state = nil },
                now: { [unowned self] in self.now }
            )
        }
    }

    func testTheDeviceDropsItsStatePublishesItsKeyPackageKnocksAndJoins() async throws {
        let m = RunModel(welcomeAfter: 0)
        let outcome = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: true)
        XCTAssertEqual(outcome, .joined)
        XCTAssertEqual(m.log, ["drop", "directory-kp", "knock", "join", "clear"])
    }

    func testItWaitsThenKnocksAgainAfterTheReknockTime() async throws {
        let m = RunModel(welcomeAfter: 1)
        let first = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: true)
        XCTAssertEqual(first, .waiting)
        XCTAssertEqual(m.log, ["drop", "directory-kp", "knock"])
        let second = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: false)
        XCTAssertEqual(second, .joined)
        XCTAssertEqual(m.log.filter { $0 == "knock" }.count, 2)
    }

    func testARestartResumesTheKnockItAlreadyMade() async throws {
        let m = RunModel(welcomeAfter: 0)
        m.state = DeviceGroupRejoinState(rendezvousB64: "kept", joinerSnapshotB64: "s", knockedAtMs: 1_000)
        let outcome = try await DeviceGroupRejoin.rejoinOnce(ports: m.ports, firstPass: false)
        XCTAssertEqual(outcome, .joined)
        XCTAssertEqual(m.log, ["join", "clear"])
    }

    func testTheReknockRuleAndTheStateCodec() throws {
        let s = DeviceGroupRejoinState(rendezvousB64: "r", joinerSnapshotB64: "s", knockedAtMs: 10)
        XCTAssertTrue(DeviceGroupRejoinState.shouldKnockAgain(nil, nowMs: 0))
        XCTAssertFalse(DeviceGroupRejoinState.shouldKnockAgain(s, nowMs: 10 + DeviceGroupRejoin.reknockAfterMs - 1))
        XCTAssertTrue(DeviceGroupRejoinState.shouldKnockAgain(s, nowMs: 10 + DeviceGroupRejoin.reknockAfterMs))
        XCTAssertTrue(DeviceGroupRejoinState.shouldKnockAgain(s, nowMs: 5), "a clock that went back")
        XCTAssertEqual(DeviceGroupRejoinState.decode(try s.encoded()), s)
        XCTAssertNil(DeviceGroupRejoinState.decode(Data("[]".utf8)))
    }

    // MARK: Decision 3 — the store: generation, pending rows, Forget device

    private func store() -> DeviceGroupSnapshotStore {
        DeviceGroupSnapshotStore(directoryName: "lakko-dg-test-\(UUID().uuidString)")
    }

    func testAConditionalWriteLandsOnlyOnTheGenerationItRead() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        let g1 = try s.putSnapshotDurable(accountIdB64: account, blob: Data("epoch N".utf8))
        XCTAssertEqual(s.generation(accountIdB64: account), g1)
        // Another handle made the commit's N+1 current.
        let g2 = try s.putSnapshotDurable(accountIdB64: account, blob: Data("epoch N+1".utf8))
        XCTAssertEqual(g2, g1 + 1)
        // The long-lived handle, still at N, tries to persist a batch.
        XCTAssertThrowsError(try s.putSnapshotDurable(
            accountIdB64: account, blob: Data("epoch N again".utf8), expectedGeneration: g1)) {
            XCTAssertTrue($0 is DeviceGroupSnapshotStore.SnapshotChanged)
        }
        XCTAssertEqual(s.getSnapshot(accountIdB64: account), Data("epoch N+1".utf8))
        // On the current generation it lands.
        XCTAssertNoThrow(try s.putSnapshotDurable(
            accountIdB64: account, blob: Data("epoch N+1, batch".utf8), expectedGeneration: g2))
        s.deleteSnapshot(accountIdB64: account)
    }

    func testAPendingRowNeverReplacesTheSnapshot() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        try s.putSnapshotDurable(accountIdB64: account, blob: Data("epoch N".utf8))
        try s.putPending(accountIdB64: account, kind: .commit, blob: Data("entry".utf8))
        XCTAssertEqual(s.getSnapshot(accountIdB64: account), Data("epoch N".utf8))
        XCTAssertEqual(s.getPending(accountIdB64: account, kind: .commit), Data("entry".utf8))
        s.deleteSnapshot(accountIdB64: account)
    }

    func testARejoinDropsTheSnapshotTheCommitAndTheRecordAndKeepsTheKnock() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        try s.putSnapshotDurable(accountIdB64: account, blob: Data("behind".utf8))
        try s.putPending(accountIdB64: account, kind: .commit, blob: Data("c".utf8))
        try s.putPending(accountIdB64: account, kind: .admissions, blob: Data("a".utf8))
        try s.putPending(accountIdB64: account, kind: .rejoin, blob: Data("r".utf8))
        s.dropStateForRejoin(accountIdB64: account)
        XCTAssertNil(s.getSnapshot(accountIdB64: account))
        XCTAssertNil(s.getPending(accountIdB64: account, kind: .commit))
        XCTAssertNil(s.getPending(accountIdB64: account, kind: .admissions))
        XCTAssertNotNil(s.getPending(accountIdB64: account, kind: .rejoin))
        s.deleteSnapshot(accountIdB64: account)
    }

    func testForgetDeviceRemovesTheOutboxWithTheSnapshot() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        try s.putSnapshotDurable(accountIdB64: account, blob: Data("s".utf8))
        try s.putPending(accountIdB64: account, kind: .commit, blob: Data("c".utf8))
        try s.putPending(accountIdB64: account, kind: .rejoin, blob: Data("r".utf8))
        s.deleteSnapshot(accountIdB64: account)
        XCTAssertNil(s.getSnapshot(accountIdB64: account))
        XCTAssertNil(s.getPending(accountIdB64: account, kind: .commit))
        XCTAssertNil(s.getPending(accountIdB64: account, kind: .rejoin))
    }

    // MARK: Decision 4 — founding is a single-device account's (the real core)

    private let deviceSeed = Data(repeating: 0x41, count: 32)
    private let accountSeed = Data(repeating: 0x42, count: 64)

    func testWithNoStateAndNoFoundingDecisionNothingIsFounded() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        XCTAssertThrowsError(try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: Data(repeating: 7, count: 16), accountIdB64: account,
            deviceIdentitySeed: deviceSeed, accountSeed: accountSeed, snapshotStore: s)) {
            XCTAssertEqual($0 as? DeviceGroupLifecycle.NotJoined, .init(reason: .noState))
        }
        XCTAssertNil(s.getSnapshot(accountIdB64: account), "nothing written")
    }

    func testASingleDeviceAccountFoundsAndTheSnapshotIsWritten() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: Data(repeating: 7, count: 16), accountIdB64: account,
            deviceIdentitySeed: deviceSeed, accountSeed: accountSeed, found: true, snapshotStore: s)
        XCTAssertTrue(ensured.bootstrapped)
        XCTAssertNotNil(s.getSnapshot(accountIdB64: account))
        XCTAssertEqual(ensured.generation, s.generation(accountIdB64: account))
        // Loaded again without a founding decision: the same DeviceGroup.
        let again = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: Data(repeating: 7, count: 16), accountIdB64: account,
            deviceIdentitySeed: deviceSeed, accountSeed: accountSeed, snapshotStore: s)
        XCTAssertFalse(again.bootstrapped)
        XCTAssertEqual(try again.group.groupId(), try ensured.group.groupId())
        s.deleteSnapshot(accountIdB64: account)
    }

    func testAnUnusableSnapshotIsDroppedAndTheRejoinRowStays() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        try s.putSnapshotDurable(accountIdB64: account, blob: Data("not a provider".utf8))
        try s.putPending(accountIdB64: account, kind: .rejoin, blob: Data("r".utf8))
        XCTAssertThrowsError(try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: Data(repeating: 7, count: 16), accountIdB64: account,
            deviceIdentitySeed: deviceSeed, accountSeed: accountSeed, snapshotStore: s))
        XCTAssertNil(s.getSnapshot(accountIdB64: account))
        XCTAssertNotNil(s.getPending(accountIdB64: account, kind: .rejoin))
        s.deleteSnapshot(accountIdB64: account)
    }

    func testAStaleHandlesPersistIsRefused() throws {
        let s = store()
        let account = "acct-\(UUID().uuidString)"
        let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: Data(repeating: 7, count: 16), accountIdB64: account,
            deviceIdentitySeed: deviceSeed, accountSeed: accountSeed, found: true, snapshotStore: s)
        try s.putSnapshotDurable(accountIdB64: account, blob: try ensured.identity.snapshotStorage())
        XCTAssertThrowsError(try DeviceGroupLifecycle.persistSnapshotOrThrow(
            identity: ensured.identity, accountIdB64: account,
            expectedGeneration: ensured.generation, snapshotStore: s)) {
            XCTAssertTrue($0 is DeviceGroupSnapshotStore.SnapshotChanged)
        }
        s.deleteSnapshot(accountIdB64: account)
    }
}

/// A publish stopped by a handle that fell behind the disk is sealed once
/// more on the reloaded handle, so the message is not lost to that race.
@MainActor
final class StaleHandlePublishTests: XCTestCase {

    private struct Other: Error {}

    func testAStaleHandleIsRetriedOnce() async throws {
        var calls = 0
        let id: Int64 = try await DeviceGroupSyncRepository.retryingOnceOnStaleHandle {
            calls += 1
            if calls == 1 { throw DeviceGroupSnapshotStore.SnapshotChanged() }
            return 7
        }
        XCTAssertEqual(id, 7)
        XCTAssertEqual(calls, 2)
    }

    func testASecondStaleHandleIsTheCallers() async {
        var calls = 0
        do {
            let _: Int64 = try await DeviceGroupSyncRepository.retryingOnceOnStaleHandle {
                calls += 1
                throw DeviceGroupSnapshotStore.SnapshotChanged()
            }
            XCTFail("must throw")
        } catch {
            XCTAssertTrue(error is DeviceGroupSnapshotStore.SnapshotChanged)
        }
        XCTAssertEqual(calls, 2)
    }

    func testAnyOtherErrorIsNotRetried() async {
        var calls = 0
        do {
            let _: Int64 = try await DeviceGroupSyncRepository.retryingOnceOnStaleHandle {
                calls += 1
                throw Other()
            }
            XCTFail("must throw")
        } catch {
            XCTAssertTrue(error is Other)
        }
        XCTAssertEqual(calls, 1)
    }
}
