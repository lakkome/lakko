// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0058 — the synced state that replaced `DeviceGroupSyncStateCodec`:
/// persistence round trip through the fold, merge, growth detection and the
/// persisted pending flag that drives the pending line.
@MainActor
final class SyncedStateSupportTests: XCTestCase {

    private let gid = Data(repeating: 0xaa, count: 32)
    private let folderId = Data(repeating: 3, count: 16)

    private func flagsPayload(hlc: UInt64, pinned: Bool) -> ParsedDeviceGroupPayload {
        ParsedDeviceGroupPayload(
            hlc: hlc, senderLeaf: 1,
            body: .flags(
                groupId: gid, pinned: pinned, muted: false, archived: false,
                unreadOverride: false))
    }

    private func folderChanges(hlc: UInt64, name: String) -> FolderChangeSet {
        FolderChangeSet(
            metas: [FolderMetaRegister(
                hlc: hlc, folderId: folderId, name: name,
                includeKinds: ChatFolders.Include.groups, excludeFlags: 0)],
            members: [], pins: [], removed: [], order: nil)
    }

    // MARK: - The fold, persisted

    func testStateRoundTripsThroughBytes() throws {
        let s = SyncedState()
        XCTAssertTrue(try s.applyPayload(payload: flagsPayload(hlc: 10, pinned: true)))
        XCTAssertTrue(try s.applyFolderChanges(changes: folderChanges(hlc: 11, name: "Work")))
        let back = try SyncedState.fromBytes(bytes: try s.toBytes())
        XCTAssertEqual(try back.flags(groupId: gid)?.pinned, true)
        XCTAssertEqual(try back.folders().map(\.meta.name), ["Work"])
        XCTAssertEqual(try back.maxHlc(), 11)
    }

    func testOlderWriteLosesAndAnOldCodecBlobIsRefused() throws {
        let s = SyncedState()
        _ = try s.applyPayload(payload: flagsPayload(hlc: 20, pinned: true))
        XCTAssertFalse(try s.applyPayload(payload: flagsPayload(hlc: 19, pinned: false)))
        XCTAssertEqual(try s.flags(groupId: gid)?.pinned, true)
        // The JSON the removed codec wrote is not SyncedState bytes; the store
        // treats it as nothing stored.
        XCTAssertThrowsError(try SyncedState.fromBytes(bytes: Data(#"{"v":1,"d":{}}"#.utf8)))
    }

    func testMergeKeepsTheNewerRegisterEitherWay() throws {
        let a = SyncedState()
        _ = try a.applyPayload(payload: flagsPayload(hlc: 30, pinned: true))
        _ = try a.applyFolderChanges(changes: folderChanges(hlc: 5, name: "Old"))
        let b = SyncedState()
        _ = try b.applyPayload(payload: flagsPayload(hlc: 10, pinned: false))
        _ = try b.applyFolderChanges(changes: folderChanges(hlc: 50, name: "New"))
        XCTAssertTrue(SyncedStateMerge.merge(a, into: b))
        XCTAssertEqual(try b.flags(groupId: gid)?.pinned, true)
        XCTAssertEqual(try b.folders().map(\.meta.name), ["New"])
        XCTAssertFalse(SyncedStateMerge.merge(a, into: b), "a second merge changes nothing")
    }

    // MARK: - Growth (section 5.1)

    private func leaf(_ index: UInt32, _ key: UInt8) -> DeviceLeaf {
        DeviceLeaf(leafIndex: index, signaturePublicKey: Data(repeating: key, count: 32))
    }

    func testGrowthReportsThePriorLeaves() {
        let before = [leaf(0, 1), leaf(2, 2)]
        let after = before + [leaf(1, 3)]
        XCTAssertEqual(DeviceGroupGrowth.priorLeavesIfGrew(before: before, after: after), [0, 2])
        XCTAssertNil(DeviceGroupGrowth.priorLeavesIfGrew(before: after, after: before))
        XCTAssertNil(DeviceGroupGrowth.priorLeavesIfGrew(before: before, after: before))
    }

    func testAReusedLeafIndexIsStillGrowth() {
        // A removal and an add in one commit: same indexes, new device.
        let before = [leaf(0, 1), leaf(1, 2)]
        let after = [leaf(0, 1), leaf(1, 9)]
        XCTAssertEqual(DeviceGroupGrowth.priorLeavesIfGrew(before: before, after: after), [0, 1])
    }

    func testOnlyTheLowestPriorLeafResends() {
        let before = [leaf(0, 1), leaf(2, 2)]
        let after = before + [leaf(1, 3)]
        XCTAssertTrue(DeviceGroupGrowth.shouldResend(ownLeaf: 0, before: before, after: after))
        XCTAssertFalse(DeviceGroupGrowth.shouldResend(ownLeaf: 2, before: before, after: after))
        // The newcomer is not a prior member.
        XCTAssertFalse(DeviceGroupGrowth.shouldResend(ownLeaf: 1, before: before, after: after))
        XCTAssertFalse(DeviceGroupGrowth.shouldResend(ownLeaf: 0, before: before, after: before))
        XCTAssertLessThanOrEqual(DeviceGroupGrowth.randomDelayNanoseconds(), 30_000_000_000)
    }

    // MARK: - Pending sync (the pending line)

    private func defaults() -> UserDefaults {
        let name = "SyncedStateSupportTests.\(UUID().uuidString)"
        let d = UserDefaults(suiteName: name)!
        d.removePersistentDomain(forName: name)
        return d
    }

    func testAFailureIsPendingAcrossARelaunch() {
        let d = defaults()
        let tracker = PendingSyncTracker(accountIdB64: "acct", defaults: d)
        XCTAssertFalse(tracker.isPending)
        tracker.markFailed()
        XCTAssertTrue(tracker.isPending)
        // A new process reads the same flag.
        XCTAssertTrue(PendingSyncTracker(accountIdB64: "acct", defaults: d).isPending)
        XCTAssertFalse(PendingSyncTracker(accountIdB64: "other", defaults: d).isPending)
    }

    func testACleanFlushClearsTheFlag() {
        let d = defaults()
        let tracker = PendingSyncTracker(accountIdB64: "acct", defaults: d)
        tracker.markFailed()
        let token = tracker.beginFlush()
        XCTAssertTrue(tracker.finishFlush(token))
        XCTAssertFalse(tracker.isPending)
        XCTAssertFalse(PendingSyncTracker(accountIdB64: "acct", defaults: d).isPending)
    }

    func testAFailureDuringTheFlushKeepsTheFlag() {
        let tracker = PendingSyncTracker(accountIdB64: "acct", defaults: defaults())
        tracker.markFailed()
        let token = tracker.beginFlush()
        tracker.markFailed()
        XCTAssertFalse(tracker.finishFlush(token))
        XCTAssertTrue(tracker.isPending)
        // The next flush with nothing failing clears it.
        XCTAssertTrue(tracker.finishFlush(tracker.beginFlush()))
        XCTAssertFalse(tracker.isPending)
    }

    func testAFlushAfterARelaunchClearsAFlagSetBeforeIt() {
        let d = defaults()
        PendingSyncTracker(accountIdB64: "acct", defaults: d).markFailed()
        let relaunched = PendingSyncTracker(accountIdB64: "acct", defaults: d)
        XCTAssertTrue(relaunched.isPending)
        XCTAssertTrue(relaunched.finishFlush(relaunched.beginFlush()))
        XCTAssertFalse(relaunched.isPending)
    }

    func testClearForgetsTheFlag() {
        let tracker = PendingSyncTracker(accountIdB64: "acct", defaults: defaults())
        tracker.markFailed()
        tracker.clear()
        XCTAssertFalse(tracker.isPending)
    }

    // MARK: - HLC

    func testHlcMintsAboveAnObservedStamp() {
        let future = Hlc.mint() + 10_000_000
        Hlc.observe(future)
        XCTAssertGreaterThan(Hlc.mint(), future)
    }
}
