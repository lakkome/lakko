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
 * G-50 / ADR-0090 section 3 — the durable record of a STAGED PublisherGroup
 * commit: the journal step comes first and decides (taken: the stage merges,
 * then the other steps run in order, each recorded as it lands; `409`: the
 * stage is abandoned, the entry dropped and nothing else happens; unanswered:
 * the stage and the entry wait for the next run). The reseal follows the
 * merge, never the stage. A restart replays an entry whose group holds its
 * staged commit or reached its target.
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
        override suspend fun merge() = step("merge")
        override suspend fun abandon() = step("abandon")
        override suspend fun handOver(accountKey: ByteArray) = step("handover")
        override suspend fun deliverWelcome(entry: PublisherCommitEntry, welcome: ByteArray, accountKey: ByteArray) =
            step("welcome:${welcome.decodeToString()}")
        override suspend fun serverEpoch(): Long {
            step("server-epoch")
            return serverEpoch
        }
        override suspend fun reseal(targetEpoch: Long) = step("reseal:$targetEpoch")
        override suspend fun appoint(joinerKey: ByteArray) = step("appoint")
        override suspend fun unappoint(accountKey: ByteArray) = step("unappoint")
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
            listOf("commit:commit", "merge", "server-epoch", "reseal:8", "welcome:welcome", "appoint"),
            r.calls,
        )
        val steps = PublisherCommitEntry.stepsOf(PublisherCommitEntry.Kind.APPROVE)
        assertEquals(listOf(steps.take(1), steps.take(2), steps.take(3), steps.take(4)), r.saved.map { it.done })
        assertEquals(approve.id, r.finished?.id)
    }

    @Test
    fun `a rotation commits its epoch with its commit and reseals`() = runTest {
        val r = Recorder()
        runPublisherCommit(rotation, r, quiet)
        assertEquals(listOf("commit:rotation", "merge", "server-epoch", "reseal:8"), r.calls)
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
        assertEquals(
            listOf(PublisherCommitEntry.Step.RESEAL, PublisherCommitEntry.Step.WELCOME, PublisherCommitEntry.Step.APPOINT),
            recorded.remainingSteps,
        )
        val second = Recorder()
        runPublisherCommit(recorded, second, quiet)
        assertEquals(listOf("server-epoch", "reseal:8", "welcome:welcome", "appoint"), second.calls)
        assertEquals(
            listOf(
                PublisherCommitEntry.Step.JOURNAL,
                PublisherCommitEntry.Step.RESEAL,
                PublisherCommitEntry.Step.WELCOME,
                PublisherCommitEntry.Step.APPOINT,
            ),
            second.finished?.done,
        )
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
    fun `an unanswered rotation commit stages on and reseals nothing`() = runTest {
        // ADR-0090 C-90: no seed reaches subscribers from a commit the
        // journal did not take; the stage and the entry wait for the owed
        // verdict.
        val r = Recorder(failAt = "commit:rotation")
        try {
            runPublisherCommit(rotation, r, quiet)
            fail("the unanswered commit must surface")
        } catch (_: IllegalStateException) {
        }
        assertEquals(listOf("commit:rotation"), r.calls)
        assertTrue(r.saved.isEmpty())
        assertNull(r.finished)
        val again = Recorder()
        assertEquals(PublisherCommitOutcome.LANDED, runPublisherCommit(rotation, again, quiet))
        assertEquals(listOf("commit:rotation", "merge", "server-epoch", "reseal:8"), again.calls)
    }

    @Test
    fun `a lost rotation is abandoned and reseals nothing`() = runTest {
        val r = Recorder(failAt = "commit:rotation", lost = true)
        assertEquals(PublisherCommitOutcome.LOST, runPublisherCommit(rotation, r, quiet))
        assertEquals(listOf("commit:rotation", "abandon"), r.calls)
        assertNotNull(r.finished)
    }

    @Test
    fun `a lost approve is abandoned with no Welcome, reseal or appointment`() = runTest {
        val r = Recorder(failAt = "commit:commit", lost = true)
        assertEquals(PublisherCommitOutcome.LOST, runPublisherCommit(approve, r, quiet))
        assertEquals(listOf("commit:commit", "abandon"), r.calls)
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
    fun `an admission runs the journal, the merge, the reseal, the Welcome and the hand-over`() = runTest {
        val admission = approve.copy(id = "0d".repeat(16), kind = PublisherCommitEntry.Kind.ADMIT, commitB64 = b64("admit"))
        val r = Recorder()
        assertEquals(PublisherCommitOutcome.LANDED, runPublisherCommit(admission, r, quiet))
        // The Welcome after the reseal: the admitted device learns the channel
        // epoch its group's epoch serves from the service when it lands.
        assertEquals(listOf("commit:admit", "merge", "server-epoch", "reseal:8", "welcome:welcome", "handover"), r.calls)
    }

    @Test
    fun `a retirement alone sends no Welcome and hands nothing over`() = runTest {
        val retirement = approve.copy(
            id = "0e".repeat(16),
            kind = PublisherCommitEntry.Kind.ADMIT,
            commitB64 = b64("retire"),
            welcomeB64 = null,
            joinerKeyB64 = null,
        )
        val r = Recorder()
        runPublisherCommit(retirement, r, quiet)
        assertEquals(listOf("commit:retire", "merge", "server-epoch", "reseal:8"), r.calls)
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

    @Test
    fun `a restart replays a commit still staged one epoch below its target`() {
        // ADR-0090 — the staged commit was recorded with its snapshot; the DS
        // has not answered yet.
        assertEquals(PublisherCommitReplay.REPLAY, replayVerdict(approve, 4, staged = true))
        assertEquals(PublisherCommitReplay.DISCARD, replayVerdict(approve, 3, staged = true))
        assertEquals(PublisherCommitReplay.DISCARD, replayVerdict(approve, 4, staged = false))
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

    private val removal = approve.copy(
        id = "0c".repeat(16),
        kind = PublisherCommitEntry.Kind.REMOVE,
        commitB64 = b64("remove"),
        welcomeB64 = null,
    )

    @Test
    fun `a removal commits, reseals and then withdraws the appointment`() = runTest {
        val r = Recorder()
        runPublisherCommit(removal, r, quiet)
        assertEquals(listOf("commit:remove", "merge", "server-epoch", "reseal:8", "unappoint"), r.calls)
        assertEquals(removal.id, r.finished?.id)
    }

    @Test
    fun `a failed withdrawal does not keep the removal owed`() = runTest {
        val r = Recorder(failAt = "unappoint")
        runPublisherCommit(removal, r, quiet)
        assertEquals(removal.id, r.finished?.id)
    }

    // ---- Retry of recorded commits (2026-11-20) ----

    private val refusal = ChannelsApiException(401, "owner-proof-required", "this route needs a valid channel owner proof")

    private fun merged(n: Int, channel: String) = rotation.copy(
        id = n.toString(16).padStart(32, '0'),
        channelIdHex = channel,
        done = listOf(PublisherCommitEntry.Step.JOURNAL),
    )

    @Test
    fun `a refusal is the owner proof or a deleted channel, and nothing else`() {
        assertEquals(PublisherCommitFailure.REFUSED, PublisherCommitFailure.of(refusal))
        assertEquals(PublisherCommitFailure.REFUSED, PublisherCommitFailure.of(ChannelsApiException(410, "channel-deleted", "")))
        assertEquals(PublisherCommitFailure.REFUSED, PublisherCommitFailure.of(IllegalStateException("wrapped", refusal)))
        assertEquals(PublisherCommitFailure.TRANSIENT, PublisherCommitFailure.of(ChannelsApiException(503, "internal", "")))
        assertEquals(PublisherCommitFailure.TRANSIENT, PublisherCommitFailure.of(ChannelsApiException(409, "stale-target-epoch", "")))
        assertEquals(PublisherCommitFailure.TRANSIENT, PublisherCommitFailure.of(java.io.IOException("reset")))
        assertEquals("401 owner-proof-required", PublisherCommitFailure.logLabel(refusal))
    }

    @Test
    fun `a pile of refused entries of one channel costs one request and is held until the next start`() = runTest {
        var now = 1_000_000L
        val retry = PublisherCommitRetry(clock = { now }, warn = {})
        val entries = (0 until 172).map { merged(it, "11".repeat(16)) }
        var calls = 0
        retry.replay(entries, "acct") {
            calls += 1
            throw refusal
        }
        assertEquals(1, calls)
        assertEquals(true, retry.hold("acct", "11".repeat(16))?.refused)
        now += 24 * 3_600_000L
        retry.replay(entries, "acct") { calls += 1 }
        assertEquals(1, calls)
        retry.reset()
        retry.replay(entries, "acct") { calls += 1 }
        assertEquals(1 + entries.size, calls)
    }

    @Test
    fun `a refused channel does not hold another channel`() = runTest {
        val retry = PublisherCommitRetry(clock = { 0L }, warn = {})
        val a = merged(1, "11".repeat(16))
        val b = merged(2, "33".repeat(16))
        val c = merged(3, "11".repeat(16))
        val ran = ArrayList<String>()
        retry.replay(listOf(a, b, c), "acct") { e ->
            ran += e.id
            if (e.channelIdHex == a.channelIdHex) throw refusal
        }
        assertEquals(listOf(a.id, b.id), ran)
        assertTrue(retry.mayAttempt("acct", b.channelIdHex))
    }

    @Test
    fun `a transient failure waits 30 s doubling to 10 minutes`() = runTest {
        var now = 5_000_000L
        val retry = PublisherCommitRetry(clock = { now }, warn = {})
        val entries = listOf(merged(1, "11".repeat(16)), merged(2, "11".repeat(16)))
        var calls = 0
        val fail: suspend (PublisherCommitEntry) -> Unit = {
            calls += 1
            throw java.io.IOException("timeout")
        }
        retry.replay(entries, "acct", run = fail)
        assertEquals(1, calls)
        now += 29_999
        retry.replay(entries, "acct", run = fail)
        assertEquals(1, calls)
        now += 1
        retry.replay(entries, "acct", run = fail)
        assertEquals(2, calls)
        now += 59_999
        assertFalse(retry.mayAttempt("acct", "11".repeat(16)))
        now += 1
        assertTrue(retry.mayAttempt("acct", "11".repeat(16)))
        assertEquals(30_000L, PublisherCommitHold.backoffMs(1))
        assertEquals(600_000L, PublisherCommitHold.backoffMs(40))
        retry.replay(entries, "acct") { calls += 1 }
        assertNull(retry.hold("acct", "11".repeat(16)))
        assertEquals(4, calls)
    }

    @Test
    fun `no rotation is staged while a commit of the channel owes its journal step or reseal`() {
        val owed = merged(1, "11".repeat(16))
        assertTrue(PublisherCommitRetry.hasOwedCommit("11".repeat(16).uppercase(), listOf(owed)))
        assertFalse(PublisherCommitRetry.hasOwedCommit("33".repeat(16), listOf(owed)))
        assertFalse(PublisherCommitRetry.hasOwedCommit("11".repeat(16), listOf(owed.withStepDone(PublisherCommitEntry.Step.RESEAL))))
        val welcomeOnly = approve.withStepDone(PublisherCommitEntry.Step.JOURNAL).withStepDone(PublisherCommitEntry.Step.RESEAL)
        assertFalse(PublisherCommitRetry.hasOwedCommit(approve.channelIdHex, listOf(welcomeOnly)))
        assertTrue(PublisherCommitRetry.hasOwedCommit(approve.channelIdHex, listOf(approve)))
        assertFalse(PublisherCommitRetry.hasOwedCommit(approve.channelIdHex, listOf(approve.copy(journalLost = true))))
    }

    @Test
    fun `an entry owing its journal step is never held`() = runTest {
        val retry = PublisherCommitRetry(clock = { 0L }, warn = {})
        retry.noteFailure("acct", approve.channelIdHex, refusal)
        var calls = 0
        repeat(3) {
            retry.replay(listOf(approve), "acct", reread = { approve }) {
                calls += 1
                throw java.io.IOException("unanswered")
            }
        }
        assertEquals(3, calls)
        assertEquals(1, retry.hold("acct", approve.channelIdHex)?.failures)
        val fresh = PublisherCommitRetry(clock = { 0L }, warn = {})
        fresh.replay(
            listOf(approve),
            "acct",
            reread = { approve.withStepDone(PublisherCommitEntry.Step.JOURNAL) },
        ) { throw refusal }
        assertEquals(true, fresh.hold("acct", approve.channelIdHex)?.refused)
    }
}

