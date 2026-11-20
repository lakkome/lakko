// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// The rules the automatic backup turns on. Mirror of the web
/// `backup/auto-backup.test.ts`.
final class AutoBackupTests: XCTestCase {

    private let account = "YWNjb3VudC1pZA=="
    private var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        // A suite of its own, so a test never reads or writes the app's real
        // bookkeeping.
        let name = "lakko.tests.autobackup.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: name)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: defaults.description)
        defaults = nil
        super.tearDown()
    }

    // MARK: - verdict

    func testCleanWhileNothingHasChanged() {
        let state = AutoBackup.State(lastUploadAtMs: 1000, dirtySinceMs: nil)
        XCTAssertEqual(AutoBackup.verdict(state, nowMs: 9_000_000_000), .clean)
    }

    func testDueTheFirstTime() {
        let state = AutoBackup.State(lastUploadAtMs: 0, dirtySinceMs: 10)
        XCTAssertEqual(AutoBackup.verdict(state, nowMs: 20), .due)
    }

    func testThrottledInsideTheInterval() {
        let state = AutoBackup.State(lastUploadAtMs: 1_000_000, dirtySinceMs: 1_000_001)
        XCTAssertEqual(
            AutoBackup.verdict(state, nowMs: 1_000_000 + AutoBackup.minIntervalMs - 1),
            .throttled
        )
    }

    func testDueOnceTheIntervalHasPassed() {
        let state = AutoBackup.State(lastUploadAtMs: 1_000_000, dirtySinceMs: 1_000_001)
        XCTAssertEqual(
            AutoBackup.verdict(state, nowMs: 1_000_000 + AutoBackup.minIntervalMs),
            .due
        )
    }

    func testBackwardsClockIsDueRatherThanWedgedOffForever() {
        // A `lastUploadAtMs` in the future makes every later `now - last`
        // negative, so a naive comparison would suppress backups permanently.
        let state = AutoBackup.State(lastUploadAtMs: 9_000_000, dirtySinceMs: 1_000)
        XCTAssertEqual(AutoBackup.verdict(state, nowMs: 1_000_000), .due)
    }

    // MARK: - marking

    func testMarkedDirtyRecordsWhenItFirstBecameWorthBackingUp() {
        XCTAssertEqual(AutoBackup.markedDirty(.clean, nowMs: 500).dirtySinceMs, 500)
    }

    func testMarkedDirtyDoesNotPushTheClockForward() {
        // Otherwise a person editing several things in a row keeps resetting
        // the moment the change happened, and the throttle is never met.
        let first = AutoBackup.markedDirty(.clean, nowMs: 500)
        XCTAssertEqual(AutoBackup.markedDirty(first, nowMs: 900), first)
    }

    func testMarkedUploadedRestartsTheClockAndKeepsTheGroupKey() {
        let state = AutoBackup.State(lastUploadAtMs: 1, dirtySinceMs: 2, lastGroupsKey: "g1")
        let next = AutoBackup.markedUploaded(state, nowMs: 777)
        XCTAssertEqual(next.lastUploadAtMs, 777)
        XCTAssertNil(next.dirtySinceMs)
        XCTAssertEqual(next.lastGroupsKey, "g1")
    }

    // MARK: - persistence

    func testRoundTrip() {
        let state = AutoBackup.State(lastUploadAtMs: 5, dirtySinceMs: 7, lastGroupsKey: "g1")
        AutoBackupStore.save(accountIdB64: account, state: state, defaults: defaults)
        XCTAssertEqual(AutoBackupStore.load(accountIdB64: account, defaults: defaults), state)
    }

    func testUnknownAccountReadsClean() {
        XCTAssertEqual(
            AutoBackupStore.load(accountIdB64: "never-seen", defaults: defaults),
            .clean
        )
    }

    func testCorruptRowReadsCleanRatherThanThrowing() {
        defaults.set(Data([0x00, 0x01, 0x02]), forKey: "lakko.backup.auto.\(account)")
        XCTAssertEqual(AutoBackupStore.load(accountIdB64: account, defaults: defaults), .clean)
    }

    func testClearRemovesTheRow() {
        AutoBackupStore.markDirty(accountIdB64: account, nowMs: 1, defaults: defaults)
        AutoBackupStore.clear(accountIdB64: account, defaults: defaults)
        XCTAssertEqual(AutoBackupStore.load(accountIdB64: account, defaults: defaults), .clean)
    }

    // MARK: - group membership

    func testFirstAskReportsAChange() {
        // Nothing here can vouch for what the server already holds, so an
        // unknown set counts as changed rather than as "same as before".
        XCTAssertTrue(
            AutoBackupStore.knownGroupsChanged(
                accountIdB64: account, groupIds: ["g1"], defaults: defaults)
        )
    }

    func testSameSetReportsNoChange() {
        _ = AutoBackupStore.knownGroupsChanged(
            accountIdB64: account, groupIds: ["g1", "g2"], defaults: defaults)
        XCTAssertFalse(
            AutoBackupStore.knownGroupsChanged(
                accountIdB64: account, groupIds: ["g1", "g2"], defaults: defaults)
        )
    }

    func testOrderIsIgnored() {
        _ = AutoBackupStore.knownGroupsChanged(
            accountIdB64: account, groupIds: ["g1", "g2"], defaults: defaults)
        XCTAssertFalse(
            AutoBackupStore.knownGroupsChanged(
                accountIdB64: account, groupIds: ["g2", "g1"], defaults: defaults)
        )
    }

    func testJoinAndDepartureBothReportAChange() {
        _ = AutoBackupStore.knownGroupsChanged(
            accountIdB64: account, groupIds: ["g1"], defaults: defaults)
        XCTAssertTrue(
            AutoBackupStore.knownGroupsChanged(
                accountIdB64: account, groupIds: ["g1", "g2"], defaults: defaults)
        )
        XCTAssertTrue(
            AutoBackupStore.knownGroupsChanged(
                accountIdB64: account, groupIds: ["g2"], defaults: defaults)
        )
    }

    func testTheGroupKeySurvivesSoALaunchIsNotAMembershipChange() {
        _ = AutoBackupStore.knownGroupsChanged(
            accountIdB64: account, groupIds: ["g1"], defaults: defaults)
        // An in-memory memo would start empty here and report a change, turning
        // "back up when membership moves" into "back up once per launch".
        XCTAssertEqual(
            AutoBackupStore.load(accountIdB64: account, defaults: defaults).lastGroupsKey,
            "g1"
        )
    }

    func testTheGroupCheckDoesNotDisturbADirtyMarkItDidNotSet() {
        AutoBackupStore.markDirty(accountIdB64: account, nowMs: 42, defaults: defaults)
        _ = AutoBackupStore.knownGroupsChanged(
            accountIdB64: account, groupIds: ["g1"], defaults: defaults)
        XCTAssertEqual(
            AutoBackupStore.load(accountIdB64: account, defaults: defaults).dirtySinceMs,
            42
        )
    }
    // MARK: - synced state (walk of 2026-11-14)

    func testTheSyncedStateDigestReportsAChangeOnlyWhenTheBytesMove() {
        XCTAssertTrue(AutoBackupStore.syncedStateChanged(
            accountIdB64: account, bytes: Data([1, 2]), defaults: defaults), "first ask")
        XCTAssertFalse(AutoBackupStore.syncedStateChanged(
            accountIdB64: account, bytes: Data([1, 2]), defaults: defaults))
        XCTAssertTrue(AutoBackupStore.syncedStateChanged(
            accountIdB64: account, bytes: Data([1, 3]), defaults: defaults))
        XCTAssertEqual(
            AutoBackupStore.load(accountIdB64: account, defaults: defaults).lastSyncedStateKey,
            AutoBackup.syncedStateKey(Data([1, 3])))
    }

    /// Creating a channel writes the channel-subscription register; with
    /// nothing else changed, the backup becomes due and carries it.
    @MainActor
    func testCreatingAChannelMakesTheBackupDue() async throws {
        let synced = SyncedState()
        var bytes = try synced.toBytes()
        var uploads: [AutoBackupDriver.Upload] = []
        let accountId = Data(repeating: 0x33, count: 16)
        let driver = AutoBackupDriver(
            inputs: .init(
                accountId: { accountId },
                accountIdB64: { [account] in account },
                sessionToken: { "tok" },
                accountIdentitySeed: { Data(repeating: 0x44, count: 32) },
                knownGroupIds: { ["g1"] },
                syncedState: { bytes }
            ),
            uploader: { upload in
                uploads.append(upload)
                return .uploaded
            },
            defaults: defaults
        )
        let first = await driver.runOnce(nowMs: 1_000)
        XCTAssertEqual(first, .uploaded, "the first run after install")
        let quiet = await driver.runOnce(nowMs: 1_000 + AutoBackup.minIntervalMs + 1)
        XCTAssertEqual(quiet, .clean, "nothing changed")

        let channelId = Data(repeating: 0x55, count: 16)
        XCTAssertTrue(try synced.setChannelSubscription(
            hlc: 7, channelId: channelId, role: .owner, subscriberSecret: Data(repeating: 0x66, count: 32),
            readKey: Data(), publisherGroupId: Data(), ownerAccountKey: Data(), name: "News", handle: ""))
        bytes = try synced.toBytes()
        let after = await driver.runOnce(nowMs: 1_000 + 2 * AutoBackup.minIntervalMs + 2)
        XCTAssertEqual(after, .uploaded)
        XCTAssertEqual(uploads.count, 2)
        let carried = try SyncedState.fromBytes(bytes: try XCTUnwrap(uploads.last?.syncedState))
        XCTAssertTrue(try carried.isChannelSubscribed(channelId: channelId))

        // An admission seed (ADR-0090) is a synced-state change too.
        XCTAssertTrue(try synced.setChannelAdmissionSeed(
            hlc: 8, channelId: channelId, admissionVersion: 1, seed: Data(repeating: 0x77, count: 32)))
        bytes = try synced.toBytes()
        let seeded = await driver.runOnce(nowMs: 1_000 + 3 * AutoBackup.minIntervalMs + 3)
        XCTAssertEqual(seeded, .uploaded)
        let carriedSeed = try SyncedState.fromBytes(bytes: try XCTUnwrap(uploads.last?.syncedState))
        XCTAssertEqual(try carriedSeed.channelAdmissionSeeds(channelId: channelId).count, 1)
    }

    /// A restore onto a device that kept its synced state merges the backup's
    /// channel-subscription register and admission seeds in; they are never
    /// dropped for the device's own registers.
    func testARestoreMergeKeepsTheChannelRegisters() throws {
        let account = "merge-\(UUID().uuidString)"
        defer { SyncedStateStore.delete(accountIdB64: account) }
        let kept = SyncedState()
        XCTAssertTrue(try kept.setChannelSubscription(
            hlc: 3, channelId: Data(repeating: 0x01, count: 16), role: .owner,
            subscriberSecret: Data(repeating: 0x11, count: 32), readKey: Data(), publisherGroupId: Data(),
            ownerAccountKey: Data(), name: "Kept", handle: ""))
        SyncedStateStore.save(kept, accountIdB64: account)

        let backup = SyncedState()
        let channelId = Data(repeating: 0x02, count: 16)
        XCTAssertTrue(try backup.setChannelSubscription(
            hlc: 4, channelId: channelId, role: .owner, subscriberSecret: Data(repeating: 0x22, count: 32),
            readKey: Data(), publisherGroupId: Data(), ownerAccountKey: Data(), name: "Restored", handle: ""))
        XCTAssertTrue(try backup.setChannelAdmissionSeed(
            hlc: 5, channelId: channelId, admissionVersion: 1, seed: Data(repeating: 0x33, count: 32)))

        XCTAssertEqual(SyncedStateStore.mergeIntoStored(bytes: try backup.toBytes(), accountIdB64: account),
                       .merged(changed: true))
        let merged = SyncedStateStore.load(accountIdB64: account)
        XCTAssertTrue(try merged.isChannelSubscribed(channelId: Data(repeating: 0x01, count: 16)))
        XCTAssertTrue(try merged.isChannelSubscribed(channelId: channelId))
        XCTAssertEqual(try merged.channelAdmissionSeeds(channelId: channelId).count, 1)
    }
}
