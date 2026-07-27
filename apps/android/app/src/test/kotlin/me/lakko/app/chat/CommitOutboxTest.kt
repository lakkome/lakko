// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.io.IOException
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/**
 * Host-JVM tests for the ADR-0040 (G-35) durable pending-commit outbox.
 *
 * The FFI `Group` / `Identity` cannot load on the host JVM (they need
 * `liblakko_ffi_uniffi.so` via JNA), so the four [ChatRepository] commit paths
 * themselves are covered by the instrumented suite. What IS covered here — the
 * load-bearing correctness core, all pure Kotlin:
 *
 *   1. **fail-closed abort before publish** — [runCrashSafeCommit] never invokes
 *      `publish` when EITHER durable write throws (entry-first-then-snapshot).
 *   2. **re-publish + echo re-arm** — [sweepPendingCommitEntries] re-publishes a
 *      matching-epoch un-acked entry and re-arms the own-echo filter.
 *   3. **torn-write discard** — an un-acked entry whose restored snapshot did not
 *      advance to `targetEpoch` is discarded (never published).
 *   4. **store round-trip + fail-closed throw** — [CommitOutboxStore] persists,
 *      reloads, acks, and THROWS on an unwritable target.
 */
class CommitOutboxTest {

    @get:Rule
    val tmp: TemporaryFolder = TemporaryFolder()

    private fun entry(
        id: String = "e1",
        targetEpoch: String = "5",
        commit: ByteArray = byteArrayOf(1, 2, 3),
        secondArtifact: ByteArray? = null,
        secondKind: SecondArtifactKind? = null,
        mailbox: ByteArray? = null,
        acked: Boolean = false,
    ): PendingCommitEntry =
        PendingCommitEntry(
            id = id,
            accountId = "acct-a",
            groupId = "grp-1",
            targetEpoch = targetEpoch,
            commitB64 = outboxB64(commit),
            secondArtifactB64 = secondArtifact?.let(::outboxB64),
            secondArtifactKind = secondKind,
            secondArtifactMailboxB64 = mailbox?.let(::outboxB64),
            acked = acked,
        )

    // -----------------------------------------------------------------
    // 1. fail-closed abort before publish (runCrashSafeCommit)
    // -----------------------------------------------------------------

    @Test
    fun `snapshot persist failure aborts the commit path — publish NEVER called`() {
        var snapshotRan = false
        var published = false
        assertThrows(IOException::class.java) {
            runBlocking {
                runCrashSafeCommit<Long>(
                    persistEntryOrThrow = { /* entry ok */ },
                    persistSnapshotOrThrow = {
                        snapshotRan = true
                        throw IOException("snapshot write failed")
                    },
                    publish = {
                        published = true
                        0L
                    },
                )
            }
        }
        assertTrue("the snapshot write was attempted", snapshotRan)
        assertFalse("publish must NOT run after a fail-closed persist", published)
    }

    @Test
    fun `entry persist failure aborts BEFORE the snapshot write and publish`() {
        var snapshotRan = false
        var published = false
        assertThrows(IOException::class.java) {
            runBlocking {
                runCrashSafeCommit<Long>(
                    persistEntryOrThrow = { throw IOException("entry write failed") },
                    persistSnapshotOrThrow = { snapshotRan = true },
                    publish = {
                        published = true
                        0L
                    },
                )
            }
        }
        assertFalse("entry-first: snapshot must NOT run when the entry write throws", snapshotRan)
        assertFalse("publish must NOT run when the entry write throws", published)
    }

    @Test
    fun `both durable writes succeed — publish runs and returns its value`() = runBlocking {
        val order = mutableListOf<String>()
        val result = runCrashSafeCommit(
            persistEntryOrThrow = { order.add("entry") },
            persistSnapshotOrThrow = { order.add("snapshot") },
            publish = {
                order.add("publish")
                42L
            },
        )
        assertEquals(42L, result)
        // Entry FIRST, then snapshot, then (only then) publish.
        assertEquals(listOf("entry", "snapshot", "publish"), order)
    }

    // -----------------------------------------------------------------
    // 2. re-publish + echo re-arm on sweep (matching epoch)
    // -----------------------------------------------------------------

    @Test
    fun `un-acked entry at a matching epoch is re-published and echo re-armed`() = runBlocking {
        val rendezvous = byteArrayOf(9, 9, 9)
        val e = entry(
            targetEpoch = "5",
            commit = byteArrayOf(1, 2, 3),
            secondArtifact = rendezvous,
            secondKind = SecondArtifactKind.RENDEZVOUS,
        )
        var republishedCommit: ByteArray? = null
        var republishedRendezvous: ByteArray? = null
        val ownPublished = mutableListOf<Long>()
        var ackedId: String? = null
        var ackedCommitId: Long? = null
        var ackedSecondId: Long? = null
        var discarded = false

        sweepPendingCommitEntries(
            entries = listOf(e),
            currentEpoch = 5uL,
            republishCommit = { bytes -> republishedCommit = bytes; 100L },
            republishRendezvous = { bytes -> republishedRendezvous = bytes; 101L },
            republishWelcome = { _, _ -> fail("welcome path must not run for a rendezvous entry") },
            markOwnPublished = { id -> ownPublished.add(id) },
            onAck = { id, commitId, secondId ->
                ackedId = id; ackedCommitId = commitId; ackedSecondId = secondId
            },
            onDiscard = { discarded = true },
        )

        assertArrayEquals("commit bytes re-published verbatim", byteArrayOf(1, 2, 3), republishedCommit)
        assertArrayEquals("rendezvous bytes re-published verbatim", rendezvous, republishedRendezvous)
        assertEquals("echo filter re-armed for the re-published commit id", listOf(100L), ownPublished)
        assertEquals("e1", ackedId)
        assertEquals(100L, ackedCommitId)
        assertEquals(101L, ackedSecondId)
        assertFalse("a re-published entry is acked, not discarded", discarded)
    }

    @Test
    fun `add-path welcome entry re-publishes to the mailbox — commit echo re-armed`() = runBlocking {
        val welcome = byteArrayOf(7, 7)
        val mailbox = byteArrayOf(0xA, 0xB)
        val e = entry(
            targetEpoch = "3",
            commit = byteArrayOf(4, 5),
            secondArtifact = welcome,
            secondKind = SecondArtifactKind.WELCOME,
            mailbox = mailbox,
        )
        var welcomeMailbox: ByteArray? = null
        var welcomeBytes: ByteArray? = null
        val ownPublished = mutableListOf<Long>()

        sweepPendingCommitEntries(
            entries = listOf(e),
            currentEpoch = 3uL,
            republishCommit = { _ -> 200L },
            republishRendezvous = { _ -> fail("rendezvous path must not run for a welcome entry") },
            republishWelcome = { mb, bytes -> welcomeMailbox = mb; welcomeBytes = bytes; 201L },
            markOwnPublished = { id -> ownPublished.add(id) },
            onAck = { _, _, _ -> },
            onDiscard = { fail("must not discard a matching-epoch entry") },
        )

        assertArrayEquals(mailbox, welcomeMailbox)
        assertArrayEquals(welcome, welcomeBytes)
        // Only the COMMIT re-arms the echo filter — the welcome goes to the new
        // member's mailbox and never echoes back to the committer.
        assertEquals(listOf(200L), ownPublished)
    }

    // -----------------------------------------------------------------
    // 3. torn-write discard (snapshot below targetEpoch)
    // -----------------------------------------------------------------

    @Test
    fun `un-acked entry whose snapshot did NOT reach targetEpoch is discarded — never published`() =
        runBlocking {
            val e = entry(targetEpoch = "5")
            var published = false
            val ownPublished = mutableListOf<Long>()
            var discardedId: String? = null

            sweepPendingCommitEntries(
                entries = listOf(e),
                currentEpoch = 4uL, // torn: snapshot rolled back to N (< N+1)
                republishCommit = { _ -> published = true; 1L },
                republishRendezvous = { _ -> published = true; 2L },
                republishWelcome = { _, _ -> published = true; 3L },
                markOwnPublished = { id -> ownPublished.add(id) },
                onAck = { _, _, _ -> fail("a torn-write orphan must not be acked") },
                onDiscard = { id -> discardedId = id },
            )

            assertFalse("a torn-write orphan must NEVER be published", published)
            assertTrue("no echo re-arm for a discarded entry", ownPublished.isEmpty())
            assertEquals("e1", discardedId)
        }

    @Test
    fun `reconcileVerdict — torn discard, matching or ahead re-publish, acked GC`() {
        val e = entry(targetEpoch = "5")
        assertEquals(SweepVerdict.DISCARD, reconcileVerdict(4uL, e)) // snapshot below target -> torn
        assertEquals(SweepVerdict.RE_PUBLISH, reconcileVerdict(5uL, e)) // exact -> re-publish
        assertEquals(SweepVerdict.RE_PUBLISH, reconcileVerdict(9uL, e)) // ahead -> re-publish
        assertEquals(
            SweepVerdict.DISCARD,
            reconcileVerdict(9uL, entry(acked = true)),
        ) // acked -> GC
        assertEquals(
            SweepVerdict.DISCARD,
            reconcileVerdict(9uL, entry(targetEpoch = "not-a-number")),
        ) // corrupt target -> discard
    }

    // -----------------------------------------------------------------
    // 4. store round-trip + fail-closed throw
    // -----------------------------------------------------------------

    @Test
    fun `store round-trips an entry, filters by group, and acks it`() {
        val dir = tmp.newFolder("outbox")
        val a = entry(id = "a", commit = byteArrayOf(1, 2))
        val b = entry(id = "b", commit = byteArrayOf(3, 4))
        CommitOutboxStore.putEntryIn(dir, a)
        CommitOutboxStore.putEntryIn(dir, b)

        val loaded = CommitOutboxStore.loadAllIn(dir).sortedBy { it.id }
        assertEquals(2, loaded.size)
        assertEquals("a", loaded[0].id)
        assertArrayEquals(byteArrayOf(1, 2), outboxUnb64(loaded[0].commitB64))
        assertFalse(loaded[0].acked)
        assertNull(loaded[0].commitMessageId)

        CommitOutboxStore.markAckedIn(dir, "a", commitMessageId = 55L, secondMessageId = 66L)
        val reloaded = CommitOutboxStore.loadAllIn(dir).first { it.id == "a" }
        assertTrue("entry a is now acked", reloaded.acked)
        assertEquals(55L, reloaded.commitMessageId)
        assertEquals(66L, reloaded.secondMessageId)

        CommitOutboxStore.deleteEntryIn(dir, "a")
        assertEquals(listOf("b"), CommitOutboxStore.loadAllIn(dir).map { it.id })
    }

    @Test
    fun `putEntry THROWS (fail-closed) when the target dir is unwritable`() {
        // A regular file where a directory is expected -> writeText cannot create
        // a child, so the durable write throws and the commit path aborts before
        // any publish (the fail-closed contract).
        val notADir = tmp.newFile("blocking-file")
        assertThrows(IOException::class.java) {
            CommitOutboxStore.putEntryIn(notADir, entry(id = "x"))
        }
    }

    @Test
    fun `loadAllIn skips corrupt files and ignores in-progress tmp files`() {
        val dir = tmp.newFolder("outbox2")
        CommitOutboxStore.putEntryIn(dir, entry(id = "good"))
        // A half-written / corrupt entry and a stray tmp file must not crash the
        // sweep loader (missing entry == torn write == clean).
        java.io.File(dir, "corrupt.json").writeText("{not valid json")
        java.io.File(dir, "half.json.tmp").writeText("partial")
        assertEquals(listOf("good"), CommitOutboxStore.loadAllIn(dir).map { it.id })
    }

    // -----------------------------------------------------------------
    // 5. ADR-0040 F1 (lakko-security) — reset the eager-merged in-memory
    //    group on a fail-closed persist-abort
    // -----------------------------------------------------------------

    /**
     * A host-JVM model of one group-membership commit op, exercising the F1 reset
     * contract over the pure [runCrashSafeCommit] orchestration. The real FFI
     * `Group` / `Identity` cannot load on the host JVM (they need
     * `liblakko_ffi_uniffi.so` via JNA), so the repository-level assertion over a
     * live handle is instrumented-only; this models the exact invariant.
     *
     * [inMemoryEpoch] models the OpenMLS provider's in-memory epoch; [durableEpoch]
     * models the epoch captured by the last SUCCESSFUL snapshot persist. A commit
     * eager-merges (bumps [inMemoryEpoch]) BEFORE the durable persist — exactly
     * like the core create-commit call — and [runCrashSafeCommit]'s
     * `onPersistAbort` resets [inMemoryEpoch] back to [durableEpoch] on a
     * fail-closed abort (the ADR-0040 F1 fix).
     */
    private class GroupEpochModel {
        var inMemoryEpoch: Long = 0
        var durableEpoch: Long = 0
        val published: MutableList<Long> = mutableListOf()

        /** One commit op through the real [runCrashSafeCommit]. [entryOk] /
         *  [snapshotOk] gate the two durable writes; [publishOk] gates the publish. */
        suspend fun commit(
            entryOk: Boolean = true,
            snapshotOk: Boolean = true,
            publishOk: Boolean = true,
        ): Long {
            // eager merge N -> N+1 in memory (the core create-commit call).
            inMemoryEpoch += 1
            val target = inMemoryEpoch
            return runCrashSafeCommit(
                persistEntryOrThrow = { if (!entryOk) throw IOException("entry write failed") },
                persistSnapshotOrThrow = {
                    if (!snapshotOk) throw IOException("snapshot write failed")
                    durableEpoch = target // durable snapshot now at N+1
                },
                publish = {
                    if (!publishOk) throw IOException("DS publish failed")
                    published.add(target)
                    target
                },
                // F1 reset: drop the un-published eager merge back to the durable
                // snapshot (the epoch the last successful persist captured).
                onPersistAbort = { inMemoryEpoch = durableEpoch },
            )
        }
    }

    @Test
    fun `F1 — snapshot persist-abort resets the in-memory epoch to the durable N`() {
        val g = GroupEpochModel()
        // op A: eager-merges to epoch 1, the snapshot persist FAILS -> abort + reset.
        assertThrows(IOException::class.java) { runBlocking { g.commit(snapshotOk = false) } }
        assertEquals("in-memory epoch reset to the durable N (0)", 0L, g.inMemoryEpoch)
        assertEquals("durable snapshot untouched at N (0)", 0L, g.durableEpoch)
        assertTrue("nothing published on a persist-abort", g.published.isEmpty())
    }

    @Test
    fun `F1 — entry persist-abort also resets the in-memory epoch`() {
        val g = GroupEpochModel()
        assertThrows(IOException::class.java) { runBlocking { g.commit(entryOk = false) } }
        assertEquals("entry-first abort also resets to N (0)", 0L, g.inMemoryEpoch)
        assertTrue("nothing published", g.published.isEmpty())
    }

    @Test
    fun `F1 — after a persist-abort a subsequent same-session commit builds on N`() {
        val g = GroupEpochModel()
        // op A aborts at eager-merged epoch 1 -> reset to 0.
        assertThrows(IOException::class.java) { runBlocking { g.commit(snapshotOk = false) } }
        assertEquals(0L, g.inMemoryEpoch)
        // op B succeeds: eager-merges 0 -> 1 (NOT 1 -> 2), publishes commit(0->1).
        runBlocking { g.commit() }
        assertEquals("op B built on N (0), reaching N+1 = 1", 1L, g.inMemoryEpoch)
        assertEquals("durable snapshot advanced to 1", 1L, g.durableEpoch)
        // The crux: WITHOUT the reset, op B would eager-merge 1 -> 2 and publish
        // commit(1->2), which peers still at 0/1 drop as wrong-epoch — stranding the
        // group. WITH the reset it publishes commit(0->1) exactly once.
        assertEquals("op B published commit for epoch 1, not 2", listOf(1L), g.published)
    }

    @Test
    fun `F1 — happy-path commit leaves the in-memory epoch at N+1 and never resets`() {
        val g = GroupEpochModel()
        runBlocking { g.commit() }
        assertEquals("happy path advances to N+1 = 1", 1L, g.inMemoryEpoch)
        assertEquals("durable snapshot at N+1 = 1", 1L, g.durableEpoch)
        assertEquals("commit published exactly once", listOf(1L), g.published)
    }

    @Test
    fun `F1 — a publish failure does NOT reset (the commit is durably recorded)`() {
        val g = GroupEpochModel()
        // eager-merge to 1, both persists OK, the publish FAILS.
        assertThrows(IOException::class.java) { runBlocking { g.commit(publishOk = false) } }
        // The device is legitimately at N+1 with a durable commit -> the restart
        // sweep re-publishes it. Resetting here would be WRONG (it would lose a
        // durably-recorded commit), so onPersistAbort must NOT fire on a publish
        // failure (runCrashSafeCommit's abort catch wraps ONLY the two persists).
        assertEquals("in-memory stays at N+1 after a publish failure", 1L, g.inMemoryEpoch)
        assertEquals("durable snapshot recorded the N+1 commit", 1L, g.durableEpoch)
        assertTrue("nothing successfully published", g.published.isEmpty())
    }

    @Test
    fun `F1 — a failed reset surfaces the poisoned-group error, persist failure attached`() {
        val poison = IllegalStateException("durable snapshot gone — poisoned group")
        val thrown = assertThrows(IllegalStateException::class.java) {
            runBlocking {
                runCrashSafeCommit<Long>(
                    persistEntryOrThrow = {},
                    persistSnapshotOrThrow = { throw IOException("snapshot write failed") },
                    publish = { 0L },
                    onPersistAbort = { throw poison },
                )
            }
        }
        // Fail-safe: the reset failure (poisoned group) is surfaced as the hard
        // error — NOT masked by the benign-looking persist failure, which is
        // attached as a suppressed exception for diagnostics.
        assertEquals(poison, thrown)
        assertTrue(
            "the original persist failure is attached",
            thrown.suppressed.any { it is IOException },
        )
    }

    private fun fail(msg: String): Nothing = throw AssertionError(msg)
}
