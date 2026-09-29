// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.runTest
import me.lakko.app.ds.DsJournalSigner
import me.lakko.app.ds.DsRegistrationUnavailable
import me.lakko.app.ds.FakeJournalSigner
import me.lakko.app.ds.RecordingStatementSigner
import me.lakko.app.ds.registerEpochBeforePublish
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0053 client contract point 3 / ADR-0057 obligation 5 for a DIRECT
 * (eager-merged) commit: the
 * registration runs after the create call and before the ADR-0040 outbox
 * write, and a registration that does not land aborts exactly like a failed
 * outbox write — the group is reset to its durable epoch and nothing is
 * persisted or published.
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
        val signer = registerOrReset(reset = { trace += "reset" }) {
            registerEpochBeforePublish(
                journalId = journal,
                before = Signer(4uL),
                after = Signer(5uL),
                statements = RecordingStatementSigner("root"),
                revoke = emptyList(),
            ) { _ ->
                trace += "register"
                register()
            }
        }
        prepareCrashSafeCommit(
            persistEntryOrThrow = { trace += "persist-entry" },
            persistSnapshotOrThrow = { trace += "persist-snapshot" },
        )
        trace += "publish@${signer.epoch}"
    }

    @Test
    fun `the registration lands before the outbox write and the commit is signed at N`() = runTest {
        val trace = mutableListOf<String>()
        directCommit(trace) {}
        assertEquals(
            listOf("create", "register", "persist-entry", "persist-snapshot", "publish@4"),
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
        assertEquals(listOf("create", "register", "reset"), trace)
    }

    @Test
    fun `a missing signer resets too, and never registers`() {
        val trace = mutableListOf<String>()
        assertThrows(DsRegistrationUnavailable::class.java) {
            runBlocking {
                registerOrReset(reset = { trace += "reset" }) {
                    registerEpochBeforePublish(journal, null, Signer(1uL), RecordingStatementSigner("root"), emptyList()) {
                        trace += "register"
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
