// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0091 phase 1b — the MLS epoch a conversation's journal cursor was
/// advanced at, and when a poll reads again from the commit leaving a
/// rolled-back epoch. Mirror of web `chat/cursor-epoch.ts` and of the Android
/// client.
///
/// ## What it catches
///
/// The poll persists the MLS snapshot and only then advances the cursor
/// (C-67, `Chat/DurableCursor.swift`), so the cursor never leads the snapshot
/// written with it. But a LATER write can put an older snapshot back (a stale
/// handle writing what it holds, a restore of an older provider). The cursor
/// is then past the commit that leaves the loaded epoch: that commit is never
/// read again, and the device is stranded exactly as if it had skipped it
/// (holding a row cannot catch this: the cursor is already past).
///
/// ## The record
///
/// ``GroupRecord/cursorEpoch``: the highest epoch the group was at when the
/// poll advanced its cursor after a durable snapshot write. It never moves
/// backward, and it is written into the message log slot together with the
/// cursor (``PersistedGroup/cursorEpoch``), so it is per account, behind the
/// same store gate, dropped with the group's slot and on Forget device.
///
/// A group loaded below its recorded epoch has been rolled back under its
/// cursor. The poll then reads again from just before the commit the DS
/// registered as leaving the loaded epoch (``FetchResult/registeredCommits``,
/// ADR-0091 open question 6) instead of from the cursor. The cursor itself
/// never moves backward; the rows between are applied again and filed by DS id.
enum CursorEpoch {

    /// Pages a rewind reads at most before it applies. Chosen, not measured:
    /// a rewind normally re-reads the rows of one missed commit.
    static let maxPages = 20

    /// Where a poll must read from instead of `after`, or `nil` when it reads
    /// from `after` as usual: the group was loaded below the epoch recorded
    /// with its cursor, and the DS names the commit leaving the loaded epoch at
    /// or below the cursor. A commit the DS no longer names (pruned, replaced)
    /// cannot be read again: no rewind.
    static func rewindFrom(
        after: Int64,
        loaded: UInt64?,
        recorded: UInt64?,
        registeredCommits: [UInt64: Int64]?
    ) -> Int64? {
        guard let loaded, let recorded, loaded < recorded,
              let leaving = RegisteredCommitVerdict.leaving(
                registeredCommits: registeredCommits, localEpoch: loaded),
              leaving >= 1, leaving <= after
        else { return nil }
        return leaving - 1
    }

    /// The line logged on a rewind (no identifier).
    static func rewindLine(loaded: UInt64, recorded: UInt64) -> String {
        "chat snapshot behind its cursor: loaded=\(loaded) recorded=\(recorded); "
            + "reading again from the commit that leaves the loaded epoch"
    }

    /// The recorded epoch after a cursor advance at `epoch`: never backward.
    static func advanced(_ recorded: UInt64?, to epoch: UInt64?) -> UInt64? {
        guard let epoch else { return recorded }
        guard let recorded else { return epoch }
        return max(recorded, epoch)
    }
}
