// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import kotlinx.coroutines.test.runTest
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsStatement
import me.lakko.app.ds.RecordingStatementSigner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0057 — only the channel OWNER claims or resets a PublisherGroup journal.
 *
 * A co-publisher's journal root is derived from its own account seed, so a
 * claim from a co-publisher device would squat an unclaimed journal against
 * the owner. These pin that a device marked as a co-publisher sends nothing
 * (not a claim, not a reset, not even a root signer is built), whatever state
 * the journal is in, and that the owner's path is the unchanged claim-or-reset.
 */
class CoPublishedJournalsTest {

    private val journal = ByteArray(32) { 0x5a }
    private val journalHex = journal.joinToString("") { "%02x".format(it) }
    private val epochKey = ByteArray(32) { 9 }

    private class Recorder {
        val paths = mutableListOf<String>()
        var rootsBuilt = 0
    }

    private suspend fun claim(
        marks: CoPublishedJournalMarks,
        epoch: ULong,
        recorder: Recorder,
        root: RecordingStatementSigner = RecordingStatementSigner("root"),
        respond: (DsStatement) -> Unit = {},
    ): PublisherJournalClaim = claimPublisherJournal(
        journalId = journal,
        journalIdHex = journalHex,
        epoch = epoch,
        epochKey = epochKey,
        coPublished = marks,
        root = {
            recorder.rootsBuilt += 1
            root
        },
        put = { make ->
            val statement = make()
            recorder.paths += statement.path
            respond(statement)
        },
    )

    @Test
    fun `a co-publisher never claims an unclaimed journal`() = runTest {
        val marks = InMemoryCoPublishedJournalMarks().apply { mark(journalHex) }
        val recorder = Recorder()
        // The DS would accept a claim here (unclaimed journal): that is the squat.
        val outcome = claim(marks, epoch = 3uL, recorder = recorder)
        assertEquals(PublisherJournalClaim.NOT_OWNER, outcome)
        assertEquals(emptyList<String>(), recorder.paths)
        assertEquals(0, recorder.rootsBuilt)
    }

    @Test
    fun `a co-publisher never resets, even at the founding epoch`() = runTest {
        val marks = InMemoryCoPublishedJournalMarks().apply { mark(journalHex) }
        val recorder = Recorder()
        val outcome = claim(marks, epoch = 0uL, recorder = recorder) {
            if (it.path.endsWith("/claim")) throw DsHttpError(409, "/claim", "")
        }
        assertEquals(PublisherJournalClaim.NOT_OWNER, outcome)
        assertEquals(emptyList<String>(), recorder.paths)
        assertEquals(0, recorder.rootsBuilt)
    }

    @Test
    fun `the mark is matched whatever the hex case`() = runTest {
        val marks = InMemoryCoPublishedJournalMarks().apply { mark(journalHex.uppercase()) }
        assertTrue(marks.contains(journalHex))
        val recorder = Recorder()
        assertEquals(
            PublisherJournalClaim.NOT_OWNER,
            claim(marks, epoch = 1uL, recorder = recorder),
        )
        assertEquals(emptyList<String>(), recorder.paths)
    }

    @Test
    fun `a mark for another journal does not stop the owner`() = runTest {
        val marks = InMemoryCoPublishedJournalMarks().apply { mark("00".repeat(32)) }
        val root = RecordingStatementSigner("root")
        val recorder = Recorder()
        val outcome = claim(marks, epoch = 2uL, recorder = recorder, root = root)
        assertEquals(PublisherJournalClaim.CLAIMED, outcome)
        assertEquals(listOf("claim@2"), root.calls)
        assertEquals(listOf("/ds/group/$journalHex/claim"), recorder.paths)
    }

    @Test
    fun `the owner leaves a live chain alone`() = runTest {
        val root = RecordingStatementSigner("root")
        val outcome = claim(
            InMemoryCoPublishedJournalMarks(),
            epoch = 4uL,
            recorder = Recorder(),
            root = root,
        ) { if (it.path.endsWith("/claim")) throw DsHttpError(409, "/claim", "") }
        assertEquals(PublisherJournalClaim.ALREADY_CLAIMED, outcome)
        assertEquals(listOf("claim@4"), root.calls)
    }

    /**
     * docs/ux/second-device.md section 4a, the trap rule: the owner's
     * publisher loop never resets the channel journal, not even for a group at
     * its founding epoch. Before this, a `409` at epoch 0 reset the chain,
     * which is what a second device of the owner that founded a fresh group
     * at the derived id did to the live channel. Only New channel resets (its
     * own claim, `foundAndClaimAtCreation`).
     */
    @Test
    fun `the owner never resets the journal, even at the founding epoch`() = runTest {
        val root = RecordingStatementSigner("root")
        val recorder = Recorder()
        val outcome = claim(
            InMemoryCoPublishedJournalMarks(),
            epoch = 0uL,
            recorder = recorder,
            root = root,
        ) { if (it.path.endsWith("/claim")) throw DsHttpError(409, "/claim", "") }
        assertEquals(PublisherJournalClaim.ALREADY_CLAIMED, outcome)
        assertEquals(listOf("claim@0"), root.calls)
        assertEquals(listOf("/ds/group/$journalHex/claim"), recorder.paths)
    }

    @Test
    fun `an unmarked journal is not reported as co-published`() {
        val marks = InMemoryCoPublishedJournalMarks()
        assertFalse(marks.contains(journalHex))
        marks.mark(journalHex)
        marks.mark(journalHex)
        assertTrue(marks.contains(journalHex))
    }
}
