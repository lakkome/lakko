// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.SealedBlobStorage
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * G-50 — the durable record of a PublisherGroup commit: the steps run in order
 * and each is recorded as it lands; a failure leaves the entry with what
 * landed and the next run resumes there; a restart drops an entry whose group
 * never reached its target epoch; a reseal already on the channels service is
 * not sent twice; a rotation's registration stays best effort while an
 * approve's is required. Mirrors the web and iOS suites.
 */
class PublisherCommitOutboxTest {

    private fun b64(s: String) = java.util.Base64.getEncoder().encodeToString(s.toByteArray())

    private val approve = PublisherCommitEntry(
        id = "0a".repeat(16),
        kind = PublisherCommitEntry.Kind.APPROVE,
        channelIdHex = "11".repeat(16),
        groupIdHex = "22".repeat(32),
        targetEpoch = 5,
        serverTargetEpoch = 8,
        commitB64 = b64("commit"),
        welcomeB64 = b64("welcome"),
        joinerKeyB64 = b64("j".repeat(32)),
        proofEpoch = 4,
        proofSeedB64 = b64("s".repeat(32)),
    )

    private val rotation = approve.copy(
        id = "0b".repeat(16),
        kind = PublisherCommitEntry.Kind.ROTATION,
        commitB64 = b64("rotation"),
        welcomeB64 = null,
        joinerKeyB64 = null,
    )

    private class Recorder(
        val failAt: String? = null,
        val serverEpoch: Long = 7,
        val lost: Boolean = false,
    ) : PublisherCommitPorts {
        val calls = mutableListOf<String>()
        val proofs = mutableListOf<Long?>()
        val saved = mutableListOf<PublisherCommitEntry>()
        var finished: PublisherCommitEntry? = null

        private fun step(name: String) {
            calls += name
            if (name == failAt) throw IllegalStateException("$name failed")
        }

        override suspend fun commitToJournal(commit: ByteArray, proofEpoch: Long?, proofSeed: ByteArray?) {
            proofs += proofEpoch
            step("commit:${commit.decodeToString()}")
        }
        override fun isLost(cause: Throwable): Boolean = lost
        override suspend fun deliverWelcome(welcome: ByteArray, joinerKey: ByteArray) =
            step("welcome:${welcome.decodeToString()}")
        override suspend fun serverEpoch(): Long {
            step("server-epoch")
            return serverEpoch
        }
        override suspend fun reseal(targetEpoch: Long) = step("reseal:$targetEpoch")
        override suspend fun appoint(joinerKey: ByteArray) = step("appoint")
        override fun save(entry: PublisherCommitEntry) {
            saved += entry
        }
        override fun finish(entry: PublisherCommitEntry) {
            finished = entry
        }
    }

    private val quiet: (String, Throwable) -> Unit = { _, _ -> }

    @Test
    fun `an approve runs the journal, the Welcome, the reseal and the appointment in order`() = runTest {
        val r = Recorder()
        runPublisherCommit(approve, r, quiet)
        assertEquals(
            listOf("commit:commit", "welcome:welcome", "server-epoch", "reseal:8", "appoint"),
            r.calls,
        )
        val steps = PublisherCommitEntry.Step.entries
        assertEquals(listOf(steps.take(1), steps.take(2), steps.take(3), steps.take(4)), r.saved.map { it.done })
        assertEquals(approve.id, r.finished?.id)
    }

    @Test
    fun `a rotation commits its epoch with its commit and reseals`() = runTest {
        val r = Recorder()
        runPublisherCommit(rotation, r, quiet)
        assertEquals(listOf("commit:rotation", "server-epoch", "reseal:8"), r.calls)
    }

    @Test
    fun `a failed step leaves what landed and the next run resumes there`() = runTest {
        val first = Recorder(failAt = "reseal:8")
        try {
            runPublisherCommit(approve, first, quiet)
            fail("the reseal failure must surface")
        } catch (_: IllegalStateException) {
        }
        assertNull(first.finished)
        val recorded = first.saved.last()
        assertEquals(listOf(PublisherCommitEntry.Step.RESEAL, PublisherCommitEntry.Step.APPOINT), recorded.remainingSteps)
        val second = Recorder()
        runPublisherCommit(recorded, second, quiet)
        assertEquals(listOf("server-epoch", "reseal:8", "appoint"), second.calls)
        assertEquals(PublisherCommitEntry.Step.entries.toList(), second.finished?.done)
    }

    @Test
    fun `a reseal the service already holds is not sent again`() = runTest {
        val r = Recorder(serverEpoch = 8)
        runPublisherCommit(rotation.copy(done = listOf(PublisherCommitEntry.Step.JOURNAL)), r, quiet)
        assertEquals(listOf("server-epoch"), r.calls)
        assertEquals(listOf(PublisherCommitEntry.Step.JOURNAL, PublisherCommitEntry.Step.RESEAL), r.finished?.done)
    }

    @Test
    fun `the journal step is proven under the recorded epoch the commit left`() = runTest {
        // ADR-0081: a replay after a restart holds only the N+1 key; the
        // recorded epoch-N seed lets it send a commit that never reached the DS.
        val r = Recorder()
        runPublisherCommit(approve, r, quiet)
        assertEquals(listOf<Long?>(4), r.proofs)
    }

    @Test
    fun `a rotation commit the DS did not answer defers only its journal step`() = runTest {
        val r = Recorder(failAt = "commit:rotation")
        runPublisherCommit(rotation, r, quiet)
        assertEquals(listOf("commit:rotation", "server-epoch", "reseal:8"), r.calls)
        assertNull(r.finished)
        val kept = r.saved.last()
        assertEquals(listOf(PublisherCommitEntry.Step.JOURNAL), kept.remainingSteps)
        val again = Recorder(serverEpoch = 8)
        runPublisherCommit(kept, again, quiet)
        assertEquals(listOf("commit:rotation"), again.calls)
        assertEquals(listOf(PublisherCommitEntry.Step.RESEAL, PublisherCommitEntry.Step.JOURNAL), again.finished?.done)
    }

    @Test
    fun `a lost rotation commit does not hold the rotation`() = runTest {
        val r = Recorder(failAt = "commit:rotation", lost = true)
        runPublisherCommit(rotation, r, quiet)
        assertEquals(listOf("commit:rotation", "server-epoch", "reseal:8"), r.calls)
        assertNotNull(r.finished)
        assertTrue(r.finished!!.journalLost)
    }

    @Test
    fun `a lost approve delivers no Welcome and appoints nobody but reseals`() = runTest {
        val r = Recorder(failAt = "commit:commit", lost = true)
        runPublisherCommit(approve, r, quiet)
        assertEquals(listOf("commit:commit", "server-epoch", "reseal:8"), r.calls)
        assertTrue(r.finished!!.journalLost)
    }

    @Test
    fun `an unanswered commit stops an approve before the Welcome`() = runTest {
        val r = Recorder(failAt = "commit:commit")
        try {
            runPublisherCommit(approve, r, quiet)
            fail("the commit failure must surface")
        } catch (_: IllegalStateException) {
        }
        assertEquals(listOf("commit:commit"), r.calls)
        assertTrue(r.saved.isEmpty())
        assertNull(r.finished)
    }

    @Test
    fun `a failed appointment never holds the entry`() = runTest {
        val r = Recorder(failAt = "appoint")
        val done = listOf(PublisherCommitEntry.Step.JOURNAL, PublisherCommitEntry.Step.WELCOME, PublisherCommitEntry.Step.RESEAL)
        runPublisherCommit(approve.copy(done = done), r, quiet)
        assertNotNull(r.finished)
    }

    @Test
    fun `a restart drops an entry below its target or without a group and replays the rest`() {
        assertEquals(PublisherCommitReplay.DISCARD, replayVerdict(approve, 4))
        assertEquals(PublisherCommitReplay.DISCARD, replayVerdict(approve, null))
        assertEquals(PublisherCommitReplay.REPLAY, replayVerdict(approve, 5))
        assertEquals(PublisherCommitReplay.REPLAY, replayVerdict(approve, 6))
    }

    private class MemoryStorage : SealedBlobStorage {
        val files = mutableMapOf<String, ByteArray>()
        override fun read(name: String): ByteArray? = files[name]?.copyOf()
        override fun write(name: String, plaintext: ByteArray) {
            files[name] = plaintext.copyOf()
        }
        override fun delete(name: String) {
            files.remove(name)
        }
    }

    @Test
    fun `the store keeps entries per account, replaces by id, drops malformed ones`() {
        val storage = MemoryStorage()
        val store = PublisherCommitStore(storage)
        val account = java.util.Base64.getEncoder().encodeToString(ByteArray(16) { 1 })
        store.put(account, approve)
        store.put(account, rotation)
        store.put(account, approve.copy(done = listOf(PublisherCommitEntry.Step.JOURNAL)))
        val back = store.entries(account)
        assertEquals(2, back.size)
        assertEquals(listOf(PublisherCommitEntry.Step.JOURNAL), back.first { it.id == approve.id }.done)
        store.remove(account, approve.id)
        store.remove(account, rotation.id)
        assertTrue(store.entries(account).isEmpty())
        assertTrue(storage.files.isEmpty())
        assertFalse(approve.copy(groupIdHex = "zz").isWellFormed)
        assertFalse(approve.copy(commitB64 = null).isWellFormed)
        assertTrue(rotation.isWellFormed)
    }
}
