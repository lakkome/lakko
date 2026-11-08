// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Where the DeviceGroup receive loop resumes after a restart.
///
/// ## What it fixes
///
/// The loop started every process at cursor 0, so each launch fetched every
/// row the account's DeviceGroup journal had ever carried and handed it to
/// OpenMLS again. Every one failed: this device's own rows as
/// `CannotDecryptOwnMessage` (a sender cannot open what it sent), every other
/// device's rows because their generation was consumed the first time round,
/// and each failure logged a line. Mirror of Android's
/// `DeviceGroupJournalCursor.kt` and web's `devices/sync/journal-batch.ts`.
///
/// ## The rule (C-67)
///
/// The persisted cursor may LAG the durable DeviceGroup snapshot, never LEAD
/// it. So it is written only after the snapshot that reflects every row up to
/// it was written successfully in the same run, and it never moves past a row
/// that might still take this device forward if read again: a foreign commit
/// that failed while sealed at or above the handle's epoch. A row that can
/// never apply (an own echo, an older epoch's commit, an application message
/// whose generation is consumed, bytes that are not MLS) is passed, exactly as
/// the in-session loop always passed it.
///
/// The cursor is bound to the journal it was read on: a key reset founds a new
/// DeviceGroup journal (ADR-0078 phase 6), and a position on the old one must
/// never skip rows of the new one. It is stored beside the snapshot under the
/// same wrap key (``DeviceGroupSnapshotStore/PendingKind/journalCursor``) and
/// deleted with it on Forget device.
enum DeviceGroupJournalCursor {

    private static let cursorLength = 8

    /// `group_id || cursor (8 bytes, big-endian)`.
    static func encode(groupId: Data, cursor: Int64) -> Data {
        var out = Data(groupId)
        withUnsafeBytes(of: UInt64(bitPattern: cursor).bigEndian) { out.append(contentsOf: $0) }
        return out
    }

    /// The cursor stored for `groupId`, or 0 when `stored` is absent, names
    /// another journal, or is not a usable position.
    static func decode(_ stored: Data?, groupId: Data) -> Int64 {
        guard let stored, stored.count == groupId.count + cursorLength,
              stored.prefix(groupId.count) == groupId else { return 0 }
        var value: UInt64 = 0
        for byte in stored.suffix(cursorLength) { value = (value << 8) | UInt64(byte) }
        let signed = Int64(bitPattern: value)
        return signed < 0 ? 0 : signed
    }

    /// The stored cursor of `accountIdB64`'s DeviceGroup journal `groupId`;
    /// 0 when none can be read (re-reading is the safe direction).
    static func read(
        accountIdB64: String, groupId: Data, store: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore()
    ) -> Int64 {
        decode(store.getPending(accountIdB64: accountIdB64, kind: .journalCursor), groupId: groupId)
    }

    /// Store `cursor`. Throws on a failed write or an ended account session.
    static func write(
        accountIdB64: String, groupId: Data, cursor: Int64,
        store: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore()
    ) throws {
        try store.putPending(
            accountIdB64: accountIdB64, kind: .journalCursor, blob: encode(groupId: groupId, cursor: cursor))
    }

    /// Whether a row's decrypt failure is this device's own row coming back:
    /// openmls's `ValidationError::CannotDecryptOwnMessage` reaches the client
    /// inside the error text. Its effect is already in the snapshot (an own
    /// commit is merged before it is published, ADR-0081; an own application
    /// message changes nothing here).
    static func isOwnMessageEcho(_ error: Error) -> Bool {
        String(describing: error).contains("CannotDecryptOwnMessage")
    }
}

/// What a row that failed to apply means for the persisted cursor.
enum FailedRowVerdict: Equatable {
    /// The row can never apply: the cursor may move past it.
    case pass
    /// The row is a commit that might still apply if read again: the persisted
    /// cursor stays before it.
    case hold

    /// The verdict for a row `processIncomingVerifyingRemovals` refused.
    ///
    /// `sealedEpoch` / `epochAfter`: the row's framed epoch and the epoch after
    /// it (`epochOfMessage` / `epochAfterMessage`; `after > sealed` only for a
    /// commit), `nil` when the bytes are not an MLS message. `localEpoch`: the
    /// handle's epoch when the row was read, `nil` when unreadable.
    static func of(isOwnEcho: Bool, sealedEpoch: UInt64?, epochAfter: UInt64?, localEpoch: UInt64?) -> Self {
        if isOwnEcho { return .pass }
        // Not MLS framing: no later read can process it either.
        guard let sealedEpoch, let epochAfter else { return .pass }
        // Not a commit: an application message or a proposal cannot move this
        // device's epoch, and a consumed generation never opens again.
        if epochAfter <= sealedEpoch { return .pass }
        // A commit of an epoch this handle is already past can never apply.
        if let localEpoch, sealedEpoch < localEpoch { return .pass }
        return .hold
    }
}

/// The persisted position of one run of the receive loop, under the rule in
/// ``DeviceGroupJournalCursor``. One loop owns it.
struct JournalCursorTracker {
    /// The highest row id such that every row up to it is reflected in the
    /// in-memory state or can never apply.
    private var safe: Int64
    /// A held row was met (or the snapshot moved under this handle): `safe`
    /// moves no further this run.
    private var holding = false
    /// A snapshot write failed: the in-memory state is ahead of the disk
    /// until a later snapshot write succeeds.
    private var dirty = false
    private var written: Int64

    init(start: Int64) {
        safe = start
        written = start
    }

    /// A row applied, or refused in a way that can never apply.
    mutating func passed(_ rowId: Int64) {
        if !holding, rowId > safe { safe = rowId }
    }

    /// A row refused; see ``FailedRowVerdict/of(isOwnEcho:sealedEpoch:epochAfter:localEpoch:)``.
    mutating func refused(_ rowId: Int64, verdict: FailedRowVerdict) {
        switch verdict {
        case .pass: passed(rowId)
        case .hold: holding = true
        }
    }

    mutating func snapshotPersisted() { dirty = false }

    mutating func snapshotPersistFailed() { dirty = true }

    /// Another handle wrote the snapshot past this one and the loop reloads
    /// from disk: what this run applied in memory since the last stored
    /// position may not be on disk, so the position goes back to the stored
    /// one and nothing more is stored this run (the next start reads again
    /// from there; rows already applied fail as consumed and are passed).
    mutating func snapshotMovedUnderHandle() {
        safe = written
        holding = true
    }

    /// The cursor to store now, or `nil` when nothing new may be stored.
    func cursorToWrite() -> Int64? { !dirty && safe > written ? safe : nil }

    mutating func cursorWritten(_ value: Int64) {
        if value > written { written = value }
    }
}
