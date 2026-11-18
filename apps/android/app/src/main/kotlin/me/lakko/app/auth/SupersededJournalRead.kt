// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.CancellationException

/*
 * ADR-0074 section 3.3 - a device whose session is answered
 * `key-generation-superseded` and that holds no pending `key-rotation`
 * envelope first reads its DeviceGroup journal to the head, once, before it
 * decides anything. The DS takes no session token (its proofs come from the
 * DeviceGroup's epoch key in this device's own snapshot), so it still can.
 *
 * When the sync runtime is polling, it does the read ([readDeviceGroupJournalOnce]
 * waits for it). When it is not (the refusal came at launch before the session
 * shell started it, or the runtime could not open), the device opens its OWN
 * persisted DeviceGroup state into a throwaway handle, reads from the persisted
 * cursor to the head, hands every key-rotation envelope it finds to the
 * intake, persists nothing, and drops the handle. The session that lands later
 * reads the same rows again from the same cursor.
 *
 * The orchestration is here, free of the FFI, the DS and the disk, so its rules
 * are tested; `DeviceGroupStandaloneJournal` builds the real handle. Mirror of
 * the web `standalone-journal-read.ts`.
 */

/** How long the read may take in all (the live runtime's wait too). */
public const val STANDALONE_JOURNAL_DEADLINE_MS: Long = 40_000L

/** What one read of the DeviceGroup journal found out. */
public enum class JournalCatchUp {
    /** The DS said there is nothing more: every row up to the head was read. */
    Read,

    /** This device holds no usable DeviceGroup state: nothing to read. */
    NotLoaded,

    /** The journal refuses this device (`401` / `403`, or no such journal). */
    Unreadable,

    /** No answer, or the deadline passed first: try again later, never a
     *  wipe. */
    Offline,
}

/** What the superseded device does after a read. */
public enum class JournalReadStep {
    /** Decide now: an envelope the read stored is tried, none means a reset. */
    Decide,

    /** Nothing is known: wait and read again. */
    Later,
}

/** [Offline] waits; everything else is enough to decide on. */
public fun journalReadStep(catchUp: JournalCatchUp): JournalReadStep =
    if (catchUp == JournalCatchUp.Offline) JournalReadStep.Later else JournalReadStep.Decide

/** One journal row. */
public class StandaloneJournalRow(public val id: Long, public val payload: ByteArray)

/** One page of rows after a cursor. */
public class StandaloneJournalPage(public val rows: List<StandaloneJournalRow>, public val hasMore: Boolean)

/** This device's DeviceGroup, opened for one read. */
public interface StandaloneJournal {
    /** Where this device's persisted state left the journal. */
    public val cursor: Long

    /** The rows after [after], signed under the handle's CURRENT epoch.
     *  Throws what the DS client threw. */
    public suspend fun fetch(after: Long): StandaloneJournalPage

    /** Apply one row to the throwaway handle; the key-rotation envelope it
     *  carried, or `null`. May throw (an own row, a stale row): the read
     *  goes on. */
    public fun apply(row: StandaloneJournalRow): ByteArray?

    /** Drop the handle. */
    public fun close()
}

/**
 * Read to the head once. [open] returns `null` when the device holds no
 * usable DeviceGroup state; [intake] stores a verified envelope as pending;
 * [classify] says what a failed fetch means.
 */
public suspend fun readJournalStandalone(
    open: suspend () -> StandaloneJournal?,
    intake: (ByteArray) -> Unit,
    classify: (Throwable) -> JournalCatchUp,
    deadlineMs: Long = STANDALONE_JOURNAL_DEADLINE_MS,
    now: () -> Long = { System.currentTimeMillis() },
): JournalCatchUp {
    val deadline = now() + deadlineMs
    val journal = try {
        open()
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (_: Throwable) {
        return JournalCatchUp.Offline
    } ?: return JournalCatchUp.NotLoaded
    try {
        var cursor = journal.cursor
        while (now() < deadline) {
            val page = try {
                journal.fetch(cursor)
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (cause: Throwable) {
                return classify(cause)
            }
            for (row in page.rows) {
                if (row.id > cursor) cursor = row.id
                val envelope = try {
                    journal.apply(row)
                } catch (_: Throwable) {
                    null
                } ?: continue
                try {
                    intake(envelope)
                } catch (_: Throwable) {
                    // A row the intake could not store says nothing about the others.
                }
            }
            if (!page.hasMore || page.rows.isEmpty()) return JournalCatchUp.Read
        }
        return JournalCatchUp.Offline
    } finally {
        try {
            journal.close()
        } catch (_: Throwable) {
            // already closed
        }
    }
}
