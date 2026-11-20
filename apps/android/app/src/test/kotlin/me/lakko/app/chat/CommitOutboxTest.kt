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
import org.junit.Assert.fail
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/**
 * Host-JVM tests for the ADR-0040 (G-35) durable pending-commit outbox.
 *
 * The FFI `Group` / `Identity` cannot load on the host JVM (they need
 * `liblakko_ffi_uniffi.so` via JNA). What IS covered here — the load-bearing
 * correctness core, all pure Kotlin:
 *
 *   1. **fail-closed abort before publish** — [prepareCrashSafeCommit] throws
 *      when EITHER durable write throws (entry-first-then-snapshot), so its
 *      caller never reaches the publish.
 *   2. **owed Welcome** — [sweepPendingCommitEntries] delivers the Welcome of a
 *      matching-epoch un-acked entry and never re-sends its commit (ADR-0081:
 *      the entry is written after the commit route took the commit).
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
    // 1. fail-closed abort before publish (prepareCrashSafeCommit)
    // -----------------------------------------------------------------

    /** The caller's shape: the durable writes, and only then the publish. */
    private fun commitThenPublish(
        persistEntryOrThrow: () -> Unit,
        persistSnapshotOrThrow: () -> Unit,
        publish: () -> Long,
    ): Long {
        prepareCrashSafeCommit(
            persistEntryOrThrow = persistEntryOrThrow,
            persistSnapshotOrThrow = persistSnapshotOrThrow,
        )
        return publish()
    }

    @Test
    fun `snapshot persist failure aborts the commit path — publish NEVER called`() {
        var snapshotRan = false
        var published = false
        assertThrows(IOException::class.java) {
            commitThenPublish(
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
        assertTrue("the snapshot write was attempted", snapshotRan)
        assertFalse("publish must NOT run after a fail-closed persist", published)
    }

    @Test
    fun `entry persist failure aborts BEFORE the snapshot write and publish`() {
        var snapshotRan = false
        var published = false
        assertThrows(IOException::class.java) {
            commitThenPublish(
                persistEntryOrThrow = { throw IOException("entry write failed") },
                persistSnapshotOrThrow = { snapshotRan = true },
                publish = {
                    published = true
                    0L
                },
            )
        }
        assertFalse("entry-first: snapshot must NOT run when the entry write throws", snapshotRan)
        assertFalse("publish must NOT run when the entry write throws", published)
    }

    @Test
    fun `both durable writes succeed — publish runs and returns its value`() {
        val order = mutableListOf<String>()
        val result = commitThenPublish(
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
    fun `add-path welcome entry delivers the owed welcome — the commit is never re-sent`() = runBlocking {
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
        var ackedId: String? = null
        var ackedCommitId: Long? = null
        var ackedSecondId: Long? = null
        var discarded = false

        sweepPendingCommitEntries(
            entries = listOf(e),
            currentEpoch = 3uL,
            republishWelcome = { mb, bytes -> welcomeMailbox = mb; welcomeBytes = bytes; 201L },
            onAck = { id, commitId, secondId ->
                ackedId = id; ackedCommitId = commitId; ackedSecondId = secondId
            },
            onDiscard = { discarded = true },
        )

        assertArrayEquals(mailbox, welcomeMailbox)
        assertArrayEquals(welcome, welcomeBytes)
        // ADR-0081 — the commit landed before its entry was written: it is
        // never re-sent, so nothing re-arms the echo filter here.
        assertTrue(ownPublished.isEmpty())
        assertEquals("e1", ackedId)
        assertEquals(null, ackedCommitId)
        assertEquals(201L, ackedSecondId)
        assertFalse("a re-published entry is acked, not discarded", discarded)
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
                republishWelcome = { _, _ -> published = true; 3L },
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

    // ADR-0091 walk 2, defect 2 — an acked entry owes no commit.

    private fun entry(id: String, acked: Boolean, isSweep: Boolean) = PendingCommitEntry(
        id = id,
        accountId = "acct",
        groupId = "g",
        targetEpoch = "2",
        commitB64 = "AA==",
        acked = acked,
        isSweep = isSweep,
    )

    @Test
    fun `only an un-acked entry or a staged sweep owes a commit`() {
        assertFalse(ownCommitOwedBy(stagedSweep = false, entries = emptyList()))
        assertFalse(
            "an acked sweep entry left on disk owes nothing",
            ownCommitOwedBy(stagedSweep = false, entries = listOf(entry("a", acked = true, isSweep = true))),
        )
        assertFalse(ownCommitOwedBy(stagedSweep = false, entries = listOf(entry("a", acked = true, isSweep = false))))
        assertTrue(ownCommitOwedBy(stagedSweep = false, entries = listOf(entry("a", acked = false, isSweep = true))))
        assertTrue(ownCommitOwedBy(stagedSweep = false, entries = listOf(entry("b", acked = false, isSweep = false))))
        assertTrue(ownCommitOwedBy(stagedSweep = true, entries = emptyList()))
    }

    @Test
    fun `an acked sweep entry is collectable, an un-acked or direct-path one is not`() {
        assertTrue(collectableSweepEntry(entry("a", acked = true, isSweep = true)))
        assertFalse("its verdict or Welcomes are still owed", collectableSweepEntry(entry("a", acked = false, isSweep = true)))
        assertFalse("collected by reconcileVerdict instead", collectableSweepEntry(entry("a", acked = true, isSweep = false)))
    }

    @Test
    fun `collecting acked sweep entries leaves the owed ones on disk`() {
        val dir = tmp.newFolder("outbox-gc")
        CommitOutboxStore.putEntryIn(dir, entry("done", acked = true, isSweep = true))
        CommitOutboxStore.putEntryIn(dir, entry("owed", acked = false, isSweep = true))
        CommitOutboxStore.putEntryIn(dir, entry("direct", acked = false, isSweep = false))
        for (e in CommitOutboxStore.loadAllIn(dir)) if (collectableSweepEntry(e)) CommitOutboxStore.deleteEntryIn(dir, e.id)
        val left = CommitOutboxStore.loadAllIn(dir).map { it.id }.toSet()
        assertEquals(setOf("owed", "direct"), left)
        assertTrue(ownCommitOwedBy(false, CommitOutboxStore.loadAllIn(dir)))
    }
}
