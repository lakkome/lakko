// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import android.content.Context
import me.lakko.app.devices.DeviceGroupSnapshotStore

/**
 * Where the DeviceGroup receive loop resumes after a restart.
 *
 * ## What it fixes
 *
 * The loop started every process at cursor 0, so each launch fetched every
 * row the account's DeviceGroup journal had ever carried and handed it to
 * OpenMLS again. Every one failed: this device's own rows as
 * `CannotDecryptOwnMessage` (a sender cannot open what it sent), every other
 * device's rows because their generation was consumed the first time round.
 * Each failure logged a stack trace; a journal of a few hundred rows wrote
 * well over a thousand log lines per start. The web client fixed the same
 * thing in `apps/web/src/devices/sync/journal-batch.ts`.
 *
 * ## The rule (C-67)
 *
 * The persisted cursor may LAG the durable DeviceGroup snapshot, never LEAD
 * it. So it is written only after the snapshot that reflects every row up to
 * it is on disk, and it never moves past a row that might still take this
 * device forward if read again: a foreign commit that failed while sealed at
 * or above the handle's epoch. A row that can never apply (an own echo, an
 * older epoch's commit, an application message whose generation is consumed,
 * bytes that are not MLS) is passed, exactly as the in-session loop always
 * passed it.
 *
 * The cursor is bound to the journal it was read on: a key reset founds a new
 * DeviceGroup journal (ADR-0078 phase 6), and a position on the old one must
 * never skip rows of the new one.
 */
internal object DeviceGroupJournalCursor {

    private const val CURSOR_LEN = 8

    /** `group_id || cursor (8 bytes, big-endian)`. */
    fun encode(groupId: ByteArray, cursor: Long): ByteArray {
        val out = ByteArray(groupId.size + CURSOR_LEN)
        System.arraycopy(groupId, 0, out, 0, groupId.size)
        for (i in 0 until CURSOR_LEN) {
            out[groupId.size + i] = (cursor ushr (8 * (CURSOR_LEN - 1 - i))).toByte()
        }
        return out
    }

    /** The cursor stored for [groupId], or 0 when [stored] is absent, names
     *  another journal, or is not a usable position. */
    fun decode(stored: ByteArray?, groupId: ByteArray): Long {
        if (stored == null || stored.size != groupId.size + CURSOR_LEN) return 0L
        for (i in groupId.indices) if (stored[i] != groupId[i]) return 0L
        var value = 0L
        for (i in 0 until CURSOR_LEN) {
            value = (value shl 8) or (stored[groupId.size + i].toLong() and 0xff)
        }
        return if (value < 0) 0L else value
    }

    /** The stored cursor of [accountIdB64]'s DeviceGroup journal [groupId];
     *  0 when none can be read (re-reading is the safe direction). */
    fun read(context: Context, accountIdB64: String, groupId: ByteArray): Long =
        decode(
            DeviceGroupSnapshotStore.getPending(
                context, accountIdB64, DeviceGroupSnapshotStore.PendingKind.JOURNAL_CURSOR,
            ),
            groupId,
        )

    /** Store [cursor]. Throws on a failed write or an ended account session. */
    fun write(context: Context, accountIdB64: String, groupId: ByteArray, cursor: Long) {
        DeviceGroupSnapshotStore.putPending(
            context, accountIdB64, DeviceGroupSnapshotStore.PendingKind.JOURNAL_CURSOR,
            encode(groupId, cursor),
        )
    }
}

/** What a row that failed to apply means for the persisted cursor. */
internal enum class FailedRowVerdict {
    /** The row can never apply: the cursor may move past it. */
    PASS,

    /** The row is a commit that might still apply if read again: the
     *  persisted cursor stays before it. */
    HOLD,
}

/**
 * The verdict for a row `processIncomingVerifyingRemovals` refused (ADR-0091,
 * open question 6 closed: the same exact rule the conversation poll uses,
 * [me.lakko.app.chat.registeredCommitVerdict]).
 *
 * HOLD exactly when the row is the commit the DS registered as leaving the
 * handle's epoch ([registeredCommits], the read's `commits`). An own echo
 * ([me.lakko.app.push.isOwnMessageEcho]: its effect is already in the snapshot,
 * an own commit is merged before it is published, ADR-0081), an application
 * message, a commit of an epoch already passed and a commit-shaped row posted
 * through the plain publish route all PASS. [localEpoch]: the handle's epoch
 * when the row was read, `null` when unreadable.
 */
internal fun failedRowVerdict(
    isOwnEcho: Boolean,
    rowId: Long,
    localEpoch: ULong?,
    registeredCommits: Map<ULong, Long>?,
): FailedRowVerdict =
    if (me.lakko.app.chat.registeredCommitVerdict(isOwnEcho, rowId, localEpoch, registeredCommits) ==
        me.lakko.app.chat.RegisteredCommitVerdict.HOLD
    ) {
        FailedRowVerdict.HOLD
    } else {
        FailedRowVerdict.PASS
    }

/**
 * The persisted position of one run of the receive loop, under the rule in
 * [DeviceGroupJournalCursor]. Not thread-safe: one loop owns it.
 */
internal class JournalCursorTracker(start: Long) {
    /** The highest row id such that every row up to it is reflected in the
     *  in-memory state or can never apply. */
    private var safe: Long = start

    /** A held row was met: [safe] moves no further this run. */
    private var holding = false

    /** A snapshot write failed: the in-memory state is ahead of the disk
     *  until a later snapshot write succeeds. */
    private var dirty = false

    private var written: Long = start

    /** A row applied, or refused in a way that can never apply. */
    fun passed(rowId: Long) {
        if (!holding && rowId > safe) safe = rowId
    }

    /** A row refused; see [failedRowVerdict]. */
    fun refused(rowId: Long, verdict: FailedRowVerdict) {
        when (verdict) {
            FailedRowVerdict.PASS -> passed(rowId)
            FailedRowVerdict.HOLD -> holding = true
        }
    }

    fun snapshotPersisted() {
        dirty = false
    }

    fun snapshotPersistFailed() {
        dirty = true
    }

    /** The cursor to store now, or `null` when nothing new may be stored. */
    fun cursorToWrite(): Long? = if (!dirty && safe > written) safe else null

    fun cursorWritten(value: Long) {
        if (value > written) written = value
    }
}
