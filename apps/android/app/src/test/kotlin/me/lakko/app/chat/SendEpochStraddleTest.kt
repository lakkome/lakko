// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0045 — the SEND path is serialized against the regions that move the
 * group's epoch.
 *
 * ## The hazard, stated precisely
 *
 * It is NOT "two seals interleave" — the provider lock already prevents that.
 * And it is NOT "two application messages arrive out of order": within one epoch
 * MLS orders by generation, not by journal position, so a reordered pair of
 * same-epoch messages both decrypt.
 *
 * The hazard is that an application message can be SEALED at one epoch and
 * PUBLISHED on the far side of a commit that changes it. `lakko-mls` never calls
 * `MlsGroupCreateConfig::max_past_epochs`, so openmls's default of **0** applies
 * ("If `max_past_epochs` is 0, only the current epoch is being stored",
 * `openmls-0.8.1/src/group/mls_group/past_secrets.rs:44`). A peer therefore keeps
 * NO past-epoch secrets: a straddling message is not late, it is **permanently
 * undecryptable for every recipient** — dropped silently, with the sender's own
 * echo still on screen.
 *
 * Both directions are reachable whenever the publish sits outside the lock:
 *
 *   - **sealed AHEAD.** A commit region eager-merges N -> N+1 *inside* its create,
 *     then parks in its durable persist and its publish. A send firing in that
 *     window seals at N+1 and can reach the DS first, so peers still at N see a
 *     future-epoch message and drop it.
 *   - **sealed BEHIND.** An ADR-0045 sweep STAGES (the group stays at N) and
 *     publishes its commit, then spends a second round-trip reading the journal
 *     verdict before merging. `MlsGroup::create_message` gates on `is_active()`,
 *     **not** `is_operational()` (`application.rs:22`), so a pending commit does
 *     NOT block a send: it seals at N and, the commit already being on the wire,
 *     necessarily publishes after it. In that window the loss is not a race but a
 *     CERTAINTY, for the full duration of the verdict read.
 *
 * ## The oracle
 *
 * [replayAsPeer] is the assertion, not a hand-written expectation of call order:
 * it replays the recorded journal in DS id order exactly as a recipient does, at
 * `max_past_epochs = 0`, and reports what that recipient never sees. A test that
 * merely pinned an ordering would keep passing if the ordering changed for a good
 * reason; this one only passes when the journal is *applyable*, which is the
 * property that actually matters.
 *
 * ## Which of these discriminate (each break was RUN; quoted verbatim)
 *
 * Publishing OUTSIDE the per-group lock (`runGroupSend` dropping
 * `lock.withLock`) — BOTH straddle directions reproduce:
 *   - `a broadcast does not seal ahead of a commit parked in its publish`:
 *     `expected:<[]> but was:<[app@6]>`;
 *   - `a broadcast does not seal behind a staged sweep's published commit`:
 *     `expected:<[]> but was:<[app@5]>`.
 *
 * Re-keying the lock globally (`GroupLocks.forGroup` returning one shared
 * mutex):
 *   - `an unrelated group's broadcast is not blocked`:
 *     `DEADLOCK: unrelated group send`.
 *
 * ## Genuinely parallel, on purpose
 *
 * Every coroutine here runs on [Dispatchers.IO], not on a confined test
 * dispatcher. Android's push and chat scopes are genuinely parallel, and a
 * previous sprint found that a confined dispatcher MASKS exactly this class of
 * defect — a lock bug can look fine when only one coroutine can ever be running.
 * The gates below force the interleaving deterministically rather than hoping for
 * it, so the test is parallel without being flaky.
 */
class SendEpochStraddleTest {

    private companion object {
        const val TAG_APP = "app"
        const val TAG_COMMIT = "commit"
        const val START_EPOCH = 5
        const val DEADLOCK_MS = 5_000L
    }

    /** One published artifact, stamped with the epoch it was BUILT on — which is
     *  the whole point of the oracle. */
    private data class Artifact(val tag: String, val builtOn: Int) {
        override fun toString(): String = "$tag@$builtOn"
    }

    /**
     * A stand-in for an MLS group that stamps every artifact with the epoch it
     * was built on.
     *
     * [createCommit] eager-merges N -> N+1 inside the create, exactly as the four
     * direct paths do; [sealApp] seals at the live epoch and does NOT move it,
     * exactly as `MlsGroup::create_message` does; [stage] / [merge] model the
     * ADR-0045 staged pair — staging leaves the epoch alone, merging advances it.
     */
    private class StampingGroup(var epoch: Int = START_EPOCH) {
        fun createCommit(): Artifact {
            val builtOn = epoch
            epoch += 1 // the eager merge, inside the create
            return Artifact(TAG_COMMIT, builtOn)
        }

        fun sealApp(): Artifact = Artifact(TAG_APP, epoch)

        fun stage(): Artifact = Artifact(TAG_COMMIT, epoch)

        fun merge() {
            epoch += 1
        }
    }

    /** A DS double that keeps the journal in ARRIVAL order, which is the order
     *  the real DS assigns ids in and therefore the order every peer replays. */
    private class Journal {
        private val mutex = Mutex()
        val rows = mutableListOf<Artifact>()

        suspend fun publish(a: Artifact) {
            mutex.withLock { rows.add(a) }
        }
    }

    /**
     * Replay [rows] in DS id order the way a recipient does, at
     * `max_past_epochs = 0`, and return the labels of the rows that recipient
     * never sees. A commit applies only when it was built on the epoch the peer
     * is currently at; an application message decrypts only at exactly that epoch
     * — no past secrets are retained and nothing buffers a future epoch.
     */
    private fun replayAsPeer(rows: List<Artifact>, startEpoch: Int): List<String> {
        var peerEpoch = startEpoch
        val dropped = mutableListOf<String>()
        for (row in rows) {
            if (row.tag == TAG_COMMIT) {
                if (row.builtOn == peerEpoch) peerEpoch += 1 else dropped.add(row.toString())
            } else {
                if (row.builtOn != peerEpoch) dropped.add(row.toString())
            }
        }
        return dropped
    }

    /**
     * Drive a send the way production does — [runGroupSend], the real function
     * [ChatRepository.broadcastControlToGroup] and the application-message paths
     * call.
     */
    private suspend fun send(
        lock: Mutex,
        providerLock: Mutex,
        group: StampingGroup,
        journal: Journal,
        trace: MutableList<String>? = null,
    ) = runGroupSend(
        lock = lock,
        providerLock = providerLock,
        seal = {
            trace?.add("persist:enter")
            // The real seal advances the ratchet and then AWAITS a durable
            // persist; the suspension point is what makes overlap observable.
            kotlinx.coroutines.yield()
            val sealed = group.sealApp()
            trace?.add("persist:exit")
            sealed
        },
        publish = { sealed -> journal.publish(sealed) },
    )

    /** Fail loudly instead of hanging the suite when a lock deadlocks. */
    private suspend fun <T> within(what: String, block: suspend () -> T): T = try {
        withTimeout(DEADLOCK_MS) { block() }
    } catch (e: kotlinx.coroutines.TimeoutCancellationException) {
        throw AssertionError("DEADLOCK: $what", e)
    }

    // ------------------------------------------------------------------------

    /** Sealed AHEAD: a commit region that has eager-merged and is parked in its
     *  publish must not let a send seal at the epoch nobody has reached yet. */
    @Test
    fun `a broadcast does not seal ahead of a commit parked in its publish`() = runBlocking {
        val locks = GroupLocks()
        val lock = locks.forGroup("g1")
        val providerLock = Mutex()
        val group = StampingGroup()
        val journal = Journal()
        val merged = CompletableDeferred<Unit>()
        val releasePublish = CompletableDeferred<Unit>()

        val committer = launch(Dispatchers.IO) {
            lock.withLock {
                val commit = providerLock.withLock { group.createCommit() }
                merged.complete(Unit)
                // Parked in the durable persist + the DS publish, exactly where
                // the four direct paths park.
                releasePublish.await()
                journal.publish(commit)
            }
        }
        merged.await()
        val sender = launch(Dispatchers.IO) {
            send(lock, providerLock, group, journal)
        }
        // Give the sender every chance to slip in front.
        repeat(50) { kotlinx.coroutines.yield() }
        releasePublish.complete(Unit)
        within("committer/sender") { committer.join(); sender.join() }

        assertEquals(emptyList<String>(), replayAsPeer(journal.rows, START_EPOCH))
    }

    /**
     * Sealed BEHIND: an ADR-0045 sweep has STAGED (the group stays at N) and
     * published its commit, and is now spending a round-trip reading the verdict.
     *
     * `create_message` gates on `is_active()`, not `is_operational()`, so the core
     * will happily seal at N in that window — for the FULL duration of the verdict
     * read, which is a network round-trip. The loss here is a certainty, not a
     * race, which is why the lock rather than the core is what closes it.
     */
    @Test
    fun `a broadcast does not seal behind a staged sweep's published commit`() = runBlocking {
        val locks = GroupLocks()
        val lock = locks.forGroup("g1")
        val providerLock = Mutex()
        val group = StampingGroup()
        val journal = Journal()
        val commitPublished = CompletableDeferred<Unit>()
        val releaseVerdict = CompletableDeferred<Unit>()

        val sweeper = launch(Dispatchers.IO) {
            lock.withLock {
                val staged = providerLock.withLock { group.stage() }
                journal.publish(staged)
                commitPublished.complete(Unit)
                // The verdict read — a second DS round-trip, during which the
                // group is still at N with an un-merged commit.
                releaseVerdict.await()
                providerLock.withLock { group.merge() }
            }
        }
        commitPublished.await()
        val sender = launch(Dispatchers.IO) {
            send(lock, providerLock, group, journal)
        }
        repeat(50) { kotlinx.coroutines.yield() }
        releaseVerdict.complete(Unit)
        within("sweeper/sender") { sweeper.join(); sender.join() }

        assertEquals(emptyList<String>(), replayAsPeer(journal.rows, START_EPOCH))
    }

    /** The whole-provider persist is atomic against every other group's MLS work:
     *  two concurrent sends never overlap their provider regions. */
    @Test
    fun `concurrent sends do not overlap their provider regions`() = runBlocking {
        val locks = GroupLocks()
        val providerLock = Mutex()
        val journal = Journal()
        val trace = java.util.Collections.synchronizedList(mutableListOf<String>())
        val groupA = StampingGroup()
        val groupB = StampingGroup()

        val a = launch(Dispatchers.IO) {
            send(locks.forGroup("gA"), providerLock, groupA, journal, trace)
        }
        val b = launch(Dispatchers.IO) {
            send(locks.forGroup("gB"), providerLock, groupB, journal, trace)
        }
        within("concurrent sends") { a.join(); b.join() }

        assertEquals(
            listOf("persist:enter", "persist:exit", "persist:enter", "persist:exit"),
            trace.toList(),
        )
    }

    /** The lock is PER GROUP: one group's parked commit must not stall another
     *  group's send. Re-keying it globally turns this into a DEADLOCK. */
    @Test
    fun `an unrelated group's broadcast is not blocked`() = runBlocking {
        val locks = GroupLocks()
        val providerLock = Mutex()
        val journal = Journal()
        val held = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()

        val holder = launch(Dispatchers.IO) {
            locks.forGroup("gA").withLock {
                held.complete(Unit)
                release.await()
            }
        }
        held.await()
        within("unrelated group send") {
            send(locks.forGroup("gB"), providerLock, StampingGroup(), journal)
        }
        release.complete(Unit)
        holder.join()

        assertEquals(1, journal.rows.size)
    }

    /**
     * The oracle itself, pinned directly — so a change to [replayAsPeer] that
     * quietly stopped reporting drops would be caught rather than making every
     * test above vacuously pass.
     */
    @Test
    fun `the peer oracle reports both straddle directions`() {
        // Sealed AHEAD: the app message reaches the DS before the commit.
        assertEquals(
            listOf("app@6"),
            replayAsPeer(
                listOf(Artifact(TAG_APP, 6), Artifact(TAG_COMMIT, 5)),
                START_EPOCH,
            ),
        )
        // Sealed BEHIND: the app message was sealed at 5 but lands after the
        // commit that moved everyone to 6.
        assertEquals(
            listOf("app@5"),
            replayAsPeer(
                listOf(Artifact(TAG_COMMIT, 5), Artifact(TAG_APP, 5)),
                START_EPOCH,
            ),
        )
        // The correct journal drops nothing.
        assertTrue(
            replayAsPeer(
                listOf(Artifact(TAG_COMMIT, 5), Artifact(TAG_APP, 6)),
                START_EPOCH,
            ).isEmpty(),
        )
    }
}
