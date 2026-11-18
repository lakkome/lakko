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

    // ADR-0073 R.5 / R.13b - the lineage is in the backup wherever the other
    // synced maps are, and comes back on restore.

    func testTheLineageSurvivesABackupRoundTripAndARestoreMerge() throws {
        let successor = Data(repeating: 0xA1, count: 32)
        let root = Data(repeating: 0xB2, count: 32)
        let device = SyncedState()
        _ = try device.applyPayload(payload: flagsPayload(hlc: 30, pinned: true))
        XCTAssertTrue(try device.setLineage(hlc: 31, successorGroupId: successor, rootConversationId: root))
        // What the backup composer takes: the persisted bytes as they are.
        let backedUp = try device.toBytes()
        // The restore path: `SyncedStateStore.mergeIntoStored` reads the
        // bytes and folds them into whatever the device holds.
        let incoming = try SyncedState.fromBytes(bytes: backedUp)
        XCTAssertEqual(try incoming.lineageRoot(successorGroupId: successor)?.rootConversationId, root)
        let restored = SyncedState()
        XCTAssertTrue(SyncedStateMerge.merge(incoming, into: restored))
        XCTAssertEqual(
            try restored.lineageRoot(successorGroupId: successor)?.rootConversationId, root,
            "a restore that drops the lineage draws a moved conversation as two")
        XCTAssertEqual(try restored.allLineage().count, 1)
        XCTAssertEqual(try restored.flags(groupId: gid)?.pinned, true)
        XCTAssertFalse(SyncedStateMerge.merge(incoming, into: restored), "a second merge changes nothing")
    }

    func testARestoreMergeKeepsALineageThisDeviceAlreadyHolds() throws {
        let successor = Data(repeating: 0xA1, count: 32)
        let root = Data(repeating: 0xB2, count: 32)
        let held = SyncedState()
        XCTAssertTrue(try held.setLineage(hlc: 9, successorGroupId: successor, rootConversationId: root))
        let backup = SyncedState()
        _ = try backup.applyPayload(payload: flagsPayload(hlc: 3, pinned: true))
        _ = SyncedStateMerge.merge(backup, into: held)
        XCTAssertEqual(try held.lineageRoot(successorGroupId: successor)?.rootConversationId, root)
    }

    // MARK: - A refused encoding is "none held" (version 1, ADR-0065)

    /// A fresh account id for the real on-disk store, removed afterwards.
    private func storeAccount() -> String {
        let account = "synced-state-version-\(UUID().uuidString)"
        addTeardownBlock { SyncedStateStore.delete(accountIdB64: account) }
        return account
    }

    /// A version 1 blob: the version byte `0x01` followed by anything.
    private var versionOneBlob: Data { Data([0x01, 0x00, 0x00, 0x00, 0x00]) }

    /// This build's own bytes with the version byte set back to 1: what a
    /// device that wrote the previous encoding holds.
    private func downgraded(_ state: SyncedState) throws -> Data {
        var bytes = try state.toBytes()
        XCTAssertEqual(bytes.first, 0x0E, "the encoding this build writes is version 14 (ADR-0090 poll choices)")
        bytes[bytes.startIndex] = 0x01
        return bytes
    }

    func testAVersionOneBlobIsRefusedByTheCore() throws {
        XCTAssertThrowsError(try SyncedState.fromBytes(bytes: versionOneBlob))
        let s = SyncedState()
        _ = try s.applyPayload(payload: flagsPayload(hlc: 10, pinned: true))
        XCTAssertThrowsError(try SyncedState.fromBytes(bytes: try downgraded(s)))
    }

    func testAStoredVersionOneBlobLoadsAsNoneHeldAndTheNextWriteReplacesIt() throws {
        let held = SyncedState()
        _ = try held.applyPayload(payload: flagsPayload(hlc: 10, pinned: true))
        for blob in [versionOneBlob, try downgraded(held)] {
            let account = storeAccount()
            SyncedStateStore.store().putSnapshot(accountIdB64: account, blob: blob)
            XCTAssertEqual(
                SyncedStateStore.store().getSnapshot(accountIdB64: account), blob,
                "the blob is on disk before the load")

            // Loaded as nothing held, twice over (the second read is the one
            // that must not log again or crash), and never offered to a backup.
            let loaded = SyncedStateStore.load(accountIdB64: account)
            XCTAssertEqual(try loaded.maxHlc(), 0)
            XCTAssertNil(try loaded.flags(groupId: gid))
            XCTAssertEqual(try SyncedStateStore.load(accountIdB64: account).maxHlc(), 0)
            XCTAssertNil(SyncedStateStore.bytes(accountIdB64: account))
            XCTAssertEqual(
                SyncedStateStore.store().getSnapshot(accountIdB64: account), blob,
                "a read leaves the entry for the next write to replace")

            // The next write replaces the entry with this build's encoding.
            _ = try loaded.applyPayload(payload: flagsPayload(hlc: 20, pinned: true))
            SyncedStateStore.save(loaded, accountIdB64: account)
            let replaced = try XCTUnwrap(SyncedStateStore.bytes(accountIdB64: account))
            XCTAssertEqual(replaced.first, 0x0E)
            XCTAssertEqual(
                try SyncedStateStore.load(accountIdB64: account).flags(groupId: gid)?.pinned, true)
        }
    }

    /// ADR-0087 flag day: a state written before the conversation-end register
    /// (version 6) is on test devices. It reads as none held, and the device
    /// carries on: no throw, a state to write into, and the next write
    /// replaces the blob.
    func testAStoredVersionSixBlobStartsCleanAndTheNextWriteReplacesIt() throws {
        let held = SyncedState()
        _ = try held.applyPayload(payload: flagsPayload(hlc: 10, pinned: true))
        var six = try held.toBytes()
        six[six.startIndex] = 0x06
        XCTAssertThrowsError(try SyncedState.fromBytes(bytes: six))

        let account = storeAccount()
        SyncedStateStore.store().putSnapshot(accountIdB64: account, blob: six)
        let loaded = SyncedStateStore.load(accountIdB64: account)
        XCTAssertEqual(try loaded.maxHlc(), 0, "starts clean")
        XCTAssertNil(SyncedStateStore.bytes(accountIdB64: account), "and offers no backup of it")

        _ = try loaded.applyPayload(payload: flagsPayload(hlc: 20, pinned: true))
        SyncedStateStore.save(loaded, accountIdB64: account)
        XCTAssertEqual(SyncedStateStore.bytes(accountIdB64: account)?.first, 0x0E)
    }

    func testABackupStateInVersionOneIsNoneHeldAndLeavesTheStoreAlone() throws {
        let account = storeAccount()
        let held = SyncedState()
        _ = try held.applyPayload(payload: flagsPayload(hlc: 30, pinned: true))
        SyncedStateStore.save(held, accountIdB64: account)
        let before = try XCTUnwrap(SyncedStateStore.bytes(accountIdB64: account))

        XCTAssertEqual(
            SyncedStateStore.mergeIntoStored(bytes: versionOneBlob, accountIdB64: account),
            .unreadable)
        XCTAssertEqual(
            SyncedStateStore.mergeIntoStored(bytes: try downgraded(held), accountIdB64: account),
            .unreadable)
        XCTAssertEqual(SyncedStateStore.bytes(accountIdB64: account), before)

        // Nothing stored at all: still no crash, and still nothing stored.
        let empty = storeAccount()
        XCTAssertEqual(
            SyncedStateStore.mergeIntoStored(bytes: versionOneBlob, accountIdB64: empty),
            .unreadable)
        XCTAssertNil(SyncedStateStore.store().getSnapshot(accountIdB64: empty))
    }

    func testAReadableBackupStateReplacesAStoredVersionOneBlob() throws {
        let account = storeAccount()
        SyncedStateStore.store().putSnapshot(accountIdB64: account, blob: versionOneBlob)
        let backup = SyncedState()
        _ = try backup.applyPayload(payload: flagsPayload(hlc: 40, pinned: true))

        XCTAssertEqual(
            SyncedStateStore.mergeIntoStored(bytes: try backup.toBytes(), accountIdB64: account),
            .merged(changed: true))
        XCTAssertEqual(SyncedStateStore.bytes(accountIdB64: account)?.first, 0x0E)
        XCTAssertEqual(
            try SyncedStateStore.load(accountIdB64: account).flags(groupId: gid)?.pinned, true)
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
