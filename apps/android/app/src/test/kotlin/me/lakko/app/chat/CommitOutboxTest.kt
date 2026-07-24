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

    private fun fail(msg: String): Nothing = throw AssertionError(msg)
}
