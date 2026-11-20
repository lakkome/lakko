// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0091 phase 1b — the MLS epoch a conversation's journal cursor was
 * advanced at.
 *
 * The poll persists the group snapshot and only then advances the cursor
 * ([DurableCursorGate], C-67), so the cursor never leads the snapshot written
 * with it. But a LATER write can put an older snapshot back (a stale handle of
 * the same group writing what it holds, a restore of an older provider). The
 * cursor is then past the commit that leaves the loaded epoch: that commit is
 * never read again and the device is stranded exactly as if it had skipped it.
 * Holding a row cannot catch this: the cursor is already past.
 *
 * The poll records, with each cursor advance, the epoch the group was at when
 * its snapshot reached disk ([GroupSettingsStore.cursorEpoch], per account and
 * group, the highest recorded, never backward, behind the ADR-0077 store gate,
 * dropped with the group and on Forget device). A group loaded below that epoch
 * has been rolled back under its cursor; the poll then reads again from the
 * commit the DS registered as leaving the loaded epoch
 * ([me.lakko.app.ds.FetchResult.registeredCommits]) instead of from the
 * cursor. Mirrors web `chat/cursor-epoch.ts` and iOS.
 */

/** Pages a rewind reads at most before it applies. Chosen, not measured: a
 *  rewind normally re-reads the rows of one missed commit. */
internal const val REWIND_MAX_PAGES: Int = 20

/**
 * Where a poll must read from instead of [after], or `null` when it reads from
 * [after] as usual.
 *
 * A rewind is due exactly when the group was loaded below the epoch recorded
 * with its cursor ([loaded] < [recorded]) and the DS names the commit leaving
 * the loaded epoch at or below the cursor: that commit was read before the
 * snapshot went back, and nothing after the cursor can bring the group
 * forward. The read restarts just before it. A commit the DS no longer names
 * (pruned, replaced) cannot be read again: no rewind.
 */
internal fun snapshotRewindFrom(
    after: Long,
    loaded: ULong?,
    recorded: ULong?,
    registeredCommits: Map<ULong, Long>?,
): Long? {
    if (loaded == null || recorded == null || loaded >= recorded) return null
    val leaving = registeredCommitLeaving(registeredCommits, loaded) ?: return null
    if (leaving > after || leaving < 1L) return null
    return leaving - 1L
}

/** The line logged on a rewind (no identifier). */
internal fun rewindLine(loaded: ULong, recorded: ULong): String =
    "chat snapshot behind its cursor: loaded=$loaded recorded=$recorded; " +
        "reading again from the commit that leaves the loaded epoch"

/** The epoch to store with the cursor: [current] when above [stored], else
 *  `null` (never backward). */
internal fun cursorEpochToSave(stored: ULong?, current: ULong): ULong? =
    if (stored == null || current > stored) current else null
