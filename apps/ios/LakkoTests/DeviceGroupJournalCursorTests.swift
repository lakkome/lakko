// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The DeviceGroup receive loop's persisted cursor: a restart resumes past this
/// device's own rows instead of re-reading the journal, and the stored value
/// never leads the durable snapshot (C-67). Mirrors Android's
/// `DeviceGroupJournalCursorTest.kt`.
final class DeviceGroupJournalCursorTests: XCTestCase {

    private let groupA = Data(repeating: 0x11, count: 32)
    private let groupB = Data(repeating: 0x22, count: 32)

    func testAStoredCursorRoundTripsForItsJournal() {
        let stored = DeviceGroupJournalCursor.encode(groupId: groupA, cursor: 276)
        XCTAssertEqual(DeviceGroupJournalCursor.decode(stored, groupId: groupA), 276)
        XCTAssertEqual(
            DeviceGroupJournalCursor.decode(
                DeviceGroupJournalCursor.encode(groupId: groupA, cursor: .max), groupId: groupA),
            .max)
    }

    /// A key reset founds a new DeviceGroup journal; the old position must
    /// never skip its rows.
    func testACursorOfAnotherJournalReadsAsZero() {
        let stored = DeviceGroupJournalCursor.encode(groupId: groupA, cursor: 500)
        XCTAssertEqual(DeviceGroupJournalCursor.decode(stored, groupId: groupB), 0)
    }

    func testAbsentOrDamagedBytesReadAsZero() {
        XCTAssertEqual(DeviceGroupJournalCursor.decode(nil, groupId: groupA), 0)
        XCTAssertEqual(DeviceGroupJournalCursor.decode(Data(count: 5), groupId: groupA), 0)
        XCTAssertEqual(
            DeviceGroupJournalCursor.decode(
                DeviceGroupJournalCursor.encode(groupId: groupA, cursor: -4), groupId: groupA),
            0)
    }

    /// ADR-0091: row 40 is the commit the DS registered as leaving epoch 7.
    private let registered: [UInt64: Int64] = [8: 40, 9: 52]

    func testAnOwnRowAlwaysPasses() {
        XCTAssertEqual(
            FailedRowVerdict.of(isOwnEcho: true, rowId: 40, localEpoch: 7, registeredCommits: registered), .pass)
        XCTAssertEqual(
            FailedRowVerdict.of(isOwnEcho: true, rowId: 40, localEpoch: nil, registeredCommits: nil), .pass)
    }

    func testTheRegisteredCommitLeavingTheHandleEpochHolds() {
        XCTAssertEqual(
            FailedRowVerdict.of(isOwnEcho: false, rowId: 40, localEpoch: 7, registeredCommits: registered), .hold)
        XCTAssertEqual(
            FailedRowVerdict.of(isOwnEcho: false, rowId: 52, localEpoch: 8, registeredCommits: registered), .hold)
    }

    func testEveryOtherFailingRowPasses() {
        XCTAssertEqual(
            FailedRowVerdict.of(isOwnEcho: false, rowId: 39, localEpoch: 7, registeredCommits: registered), .pass,
            "a commit-shaped row the DS did not register")
        XCTAssertEqual(
            FailedRowVerdict.of(isOwnEcho: false, rowId: 52, localEpoch: 7, registeredCommits: registered), .pass,
            "the commit of the epoch after the next")
        XCTAssertEqual(
            FailedRowVerdict.of(isOwnEcho: false, rowId: 40, localEpoch: 8, registeredCommits: registered), .pass,
            "a commit the handle is past")
        XCTAssertEqual(
            FailedRowVerdict.of(isOwnEcho: false, rowId: 40, localEpoch: nil, registeredCommits: registered), .pass,
            "an unreadable handle epoch")
        XCTAssertEqual(
            FailedRowVerdict.of(isOwnEcho: false, rowId: 40, localEpoch: 7, registeredCommits: nil), .pass,
            "a read that named no commits")
    }

    func testTheOwnEchoIsRecognisedFromTheCoreError() {
        struct Echo: Error, CustomStringConvertible {
            var description: String { "Group(\"process_message: ValidationError(CannotDecryptOwnMessage)\")" }
        }
        struct Other: Error {}
        XCTAssertTrue(DeviceGroupJournalCursor.isOwnMessageEcho(Echo()))
        XCTAssertFalse(DeviceGroupJournalCursor.isOwnMessageEcho(Other()))
    }

    func testABatchOfOwnEchoesMovesTheStoredCursorPastThem() {
        var tracker = JournalCursorTracker(start: 0)
        for id in Int64(1)...229 {
            tracker.refused(id, verdict: .of(isOwnEcho: true, rowId: id, localEpoch: 4, registeredCommits: nil))
        }
        XCTAssertEqual(tracker.cursorToWrite(), 229)
        tracker.cursorWritten(229)
        XCTAssertNil(tracker.cursorToWrite(), "nothing new to store")
    }

    func testTheStoredCursorStopsBeforeAHeldCommitForTheRestOfTheRun() {
        var tracker = JournalCursorTracker(start: 10)
        tracker.passed(11)
        tracker.refused(12, verdict: .hold)
        tracker.passed(13)
        tracker.snapshotPersisted()
        XCTAssertEqual(tracker.cursorToWrite(), 11)
        tracker.cursorWritten(11)
        tracker.passed(20)
        XCTAssertNil(tracker.cursorToWrite())
    }

    func testAFailedSnapshotWriteKeepsTheCursorUntilALaterWriteSucceeds() {
        var tracker = JournalCursorTracker(start: 0)
        tracker.passed(5)
        tracker.snapshotPersistFailed()
        XCTAssertNil(tracker.cursorToWrite(), "rows applied in memory only")
        // A later batch of own echoes only: still nothing on disk covers row 5.
        tracker.refused(6, verdict: .pass)
        XCTAssertNil(tracker.cursorToWrite())
        tracker.passed(7)
        tracker.snapshotPersisted()
        XCTAssertEqual(tracker.cursorToWrite(), 7)
    }

    /// iOS reloads from disk when another handle wrote the snapshot; what this
    /// run applied since the last stored position may not be on disk.
    func testASnapshotMovedUnderTheHandleStoresNothingMoreThisRun() {
        var tracker = JournalCursorTracker(start: 0)
        tracker.passed(3)
        tracker.snapshotPersisted()
        XCTAssertEqual(tracker.cursorToWrite(), 3)
        tracker.cursorWritten(3)
        tracker.passed(8)
        tracker.snapshotMovedUnderHandle()
        tracker.passed(9)
        tracker.snapshotPersisted()
        XCTAssertNil(tracker.cursorToWrite())
    }

    func testTheCursorNeverMovesBackward() {
        var tracker = JournalCursorTracker(start: 50)
        tracker.passed(40)
        XCTAssertNil(tracker.cursorToWrite())
    }

    // MARK: - The store: beside the snapshot, deleted with it

    func testTheCursorIsStoredBesideTheSnapshotAndGoesWithItOnForgetDevice() throws {
        let store = DeviceGroupSnapshotStore(directoryName: "lakko-dg-test-\(UUID().uuidString)")
        let account = "acct-\(UUID().uuidString)"
        try store.putSnapshotDurable(accountIdB64: account, blob: Data("epoch N".utf8))
        try DeviceGroupJournalCursor.write(accountIdB64: account, groupId: groupA, cursor: 42, store: store)
        XCTAssertEqual(DeviceGroupJournalCursor.read(accountIdB64: account, groupId: groupA, store: store), 42)
        XCTAssertEqual(DeviceGroupJournalCursor.read(accountIdB64: account, groupId: groupB, store: store), 0)
        // A rejoin keeps it (the position is before the commit that admits the
        // device again).
        store.dropStateForRejoin(accountIdB64: account)
        XCTAssertEqual(DeviceGroupJournalCursor.read(accountIdB64: account, groupId: groupA, store: store), 42)
        // Forget device deletes it with the snapshot.
        store.deleteSnapshot(accountIdB64: account)
        XCTAssertEqual(DeviceGroupJournalCursor.read(accountIdB64: account, groupId: groupA, store: store), 0)
        XCTAssertNil(store.getPending(accountIdB64: account, kind: .journalCursor))
    }
}
