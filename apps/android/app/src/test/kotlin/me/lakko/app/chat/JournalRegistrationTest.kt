// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.runTest
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.DsRegistrationUnavailable
import me.lakko.app.ds.FakeJournalSigner
import me.lakko.app.ds.RecordingStatementSigner
import me.lakko.app.ds.DsCommitAnswer
import me.lakko.app.ds.commitEpochWith
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0081 decision 1 / ADR-0057 obligation 5 for a DIRECT (eager-merged)
 * commit: the commit and its epoch's registration reach the DS in one request
 * after the create call and BEFORE the ADR-0040 outbox write, and a commit
 * that does not land resets the group to its durable epoch with nothing
 * recorded.
 */
class JournalRegistrationTest {

    private fun Signer(epoch: ULong): DsJournalSigner = FakeJournalSigner(epoch)

    private val journal = ByteArray(32) { 7 }

    /** The direct-commit sequence as the repository runs it, over fakes. */
    private suspend fun directCommit(
        trace: MutableList<String>,
        register: suspend () -> Unit,
    ) {
        trace += "create"
        val answer = registerOrReset(reset = { trace += "reset" }) {
            commitEpochWith(
                journalId = journal,
                before = Signer(4uL),
                after = Signer(5uL),
                statements = RecordingStatementSigner("root"),
                revoke = emptyList(),
                row = byteArrayOf(1),
            ) { _, proof ->
                trace += "commit@proof${proof.epoch}"
                register()
                DsCommitAnswer(id = 9, createdAt = "t", resent = false)
            }
        }
        trace += "persist-entry"
        trace += "persist-snapshot"
        trace += "landed#${answer.id}"
    }

    @Test
    fun `the commit lands before the outbox write and is proven under N`() = runTest {
        val trace = mutableListOf<String>()
        directCommit(trace) {}
        assertEquals(
            listOf("create", "commit@proof4", "persist-entry", "persist-snapshot", "landed#9"),
            trace,
        )
    }

    @Test
    fun `a refused registration resets the group and writes nothing`() {
        val trace = mutableListOf<String>()
        val refused = IllegalStateException("401")
        val thrown = assertThrows(IllegalStateException::class.java) {
            runBlocking { directCommit(trace) { throw refused } }
        }
        assertSame(refused, thrown)
        assertEquals(listOf("create", "commit@proof4", "reset"), trace)
    }

    @Test
    fun `a missing signer resets too, and never commits`() {
        val trace = mutableListOf<String>()
        assertThrows(DsRegistrationUnavailable::class.java) {
            runBlocking {
                registerOrReset(reset = { trace += "reset" }) {
                    commitEpochWith(journal, null, Signer(1uL), RecordingStatementSigner("root"), emptyList(), byteArrayOf(1)) { _, _ ->
                        trace += "register"
                        DsCommitAnswer(1, "t", false)
                    }
                }
            }
        }
        assertEquals(listOf("reset"), trace)
    }

    @Test
    fun `a failed reset is the error, with the registration failure attached`() {
        val registration = IllegalStateException("401")
        val poisoned = IllegalStateException("snapshot unreadable")
        val thrown = assertThrows(IllegalStateException::class.java) {
            runBlocking {
                registerOrReset<Unit>(reset = { throw poisoned }) { throw registration }
            }
        }
        assertSame(poisoned, thrown)
        assertTrue(thrown.suppressed.contains(registration))
    }
}
