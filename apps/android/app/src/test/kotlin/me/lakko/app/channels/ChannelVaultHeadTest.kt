// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import kotlinx.coroutines.test.runTest
import me.lakko.app.storage.VaultClient
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0064 section 7 (amendment of 2026-11-14) — the owned channel's vault
 * head: written before a PublisherGroup commit is sent and after an applied
 * one, merged on a `409` keeping the two newest epochs, never holding a
 * commit; and the entry of an owner device that holds no group, newest saved
 * epoch first. The core is faked (the host JVM loads no native library).
 */
class ChannelVaultHeadTest {

    private fun epoch(n: Long) = ReentryEpoch(n, ByteArray(32) { n.toByte() }, "gi$n".toByteArray())

    /** Seals a head as its epochs, `"3,2"`. */
    private class FakeCodec : VaultHeadCodec {
        override fun open(version: Long, sealed: ByteArray): OpenedHead {
            val text = sealed.decodeToString()
            val epochs = if (text.isEmpty()) emptyList() else text.split(",").map { it.toLong() }
            return OpenedHead(epochs.map { ReentryEpoch(it, ByteArray(32) { _ -> it.toByte() }, "gi$it".toByteArray()) })
        }

        override fun seal(version: Long, reentry: List<ReentryEpoch>): ByteArray =
            reentry.joinToString(",") { it.epoch.toString() }.toByteArray()
    }

    /** The storage service's view of one vault. [racer] runs once, after the
     *  first read: another owner device writes in between. */
    private class FakeStore(
        var claimed: Boolean = true,
        var version: Long = 0,
        var sealed: ByteArray? = null,
        var racer: (FakeStore.() -> Unit)? = null,
        val failRead: Boolean = false,
    ) : VaultHeadStore {
        val calls = mutableListOf<String>()

        override suspend fun read(): VaultClient.HeadRead {
            calls += "read"
            if (failRead) throw IllegalStateException("no route")
            val result = when {
                !claimed -> VaultClient.HeadRead.Refused
                sealed == null -> VaultClient.HeadRead.Missing
                else -> VaultClient.HeadRead.Found(version, sealed!!)
            }
            racer?.let {
                racer = null
                it()
            }
            return result
        }

        override suspend fun write(version: Long, sealed: ByteArray): VaultClient.HeadWrite {
            calls += "write:$version"
            if (!claimed) return VaultClient.HeadWrite.REFUSED
            if (version != this.version + 1) return VaultClient.HeadWrite.CONFLICT
            this.version = version
            this.sealed = sealed
            return VaultClient.HeadWrite.WRITTEN
        }

        override suspend fun claim() {
            calls += "claim"
            claimed = true
        }
    }

    // --- merge -----------------------------------------------------------

    @Test
    fun `a merge keeps the two newest epochs, newest first`() {
        val merged = mergeReentry(listOf(epoch(3), epoch(1)), listOf(epoch(4), epoch(2)))
        assertEquals(listOf(4L, 3L), merged.map { it.epoch })
    }

    @Test
    fun `an incoming epoch replaces a saved one of the same epoch`() {
        val incoming = ReentryEpoch(5, ByteArray(32), "new".toByteArray())
        val merged = mergeReentry(listOf(epoch(5), epoch(4)), listOf(incoming))
        assertEquals(listOf(5L, 4L), merged.map { it.epoch })
        assertEquals("new", merged[0].groupInfo.decodeToString())
    }

    // --- writer ----------------------------------------------------------

    @Test
    fun `a first write claims the vault and writes version 1`() = runTest {
        val store = FakeStore(claimed = false)
        assertTrue(writeVaultHead(store, FakeCodec(), listOf(epoch(7))))
        assertEquals(listOf("read", "claim", "write:1"), store.calls)
        assertEquals("7", store.sealed!!.decodeToString())
    }

    @Test
    fun `a write after a held head keeps the newest two`() = runTest {
        val store = FakeStore(version = 4, sealed = "6,5".toByteArray())
        assertTrue(writeVaultHead(store, FakeCodec(), listOf(epoch(7))))
        assertEquals(listOf("read", "write:5"), store.calls)
        assertEquals("7,6", store.sealed!!.decodeToString())
    }

    @Test
    fun `a 409 reads again, merges with what the other device wrote and keeps the two newest`() = runTest {
        // This device writes epoch 7; between its read and its write another
        // owner device wrote epoch 8 over a head holding 6 and 5.
        val store = FakeStore(version = 2, sealed = "6,5".toByteArray(), racer = {
            version = 3
            sealed = "8,6".toByteArray()
        })
        assertTrue(writeVaultHead(store, FakeCodec(), listOf(epoch(7))))
        assertEquals(listOf("read", "write:3", "read", "write:4"), store.calls)
        assertEquals("8,7", store.sealed!!.decodeToString())
    }

    @Test
    fun `a head that keeps conflicting gives up after the bounded attempts`() = runTest {
        val store = object : VaultHeadStore {
            var writes = 0
            override suspend fun read() = VaultClient.HeadRead.Missing
            override suspend fun write(version: Long, sealed: ByteArray): VaultClient.HeadWrite {
                writes += 1
                return VaultClient.HeadWrite.CONFLICT
            }
            override suspend fun claim() = Unit
        }
        assertFalse(writeVaultHead(store, FakeCodec(), listOf(epoch(1))))
        assertEquals(HEAD_WRITE_ATTEMPTS, store.writes)
    }

    // --- the commit hooks ------------------------------------------------

    private fun b64(s: String) = java.util.Base64.getEncoder().encodeToString(s.toByteArray())

    private val rotation = PublisherCommitEntry(
        id = "0b".repeat(16),
        kind = PublisherCommitEntry.Kind.ROTATION,
        channelIdHex = "11".repeat(16),
        groupIdHex = "22".repeat(32),
        targetEpoch = 5,
        serverTargetEpoch = 8,
        commitB64 = b64("rotation"),
        proofEpoch = 4,
        proofSeedB64 = b64("s".repeat(32)),
    )

    private val entering = rotation.copy(id = "0e".repeat(16), kind = PublisherCommitEntry.Kind.ENTER, commitB64 = b64("enter"))

    /** Ports that record the order; [store] is the head the hooks write. */
    private class Ports(
        val store: FakeStore,
        val failHead: Boolean = false,
        val lost: Boolean = false,
    ) : PublisherCommitPorts {
        val calls = mutableListOf<String>()
        var finished = false

        override suspend fun beforeSend() {
            calls += "head-staged"
            if (failHead) throw IllegalStateException("storage unreachable")
            writeVaultHead(store, FakeCodec(), listOf(ReentryEpoch(5, ByteArray(32), ByteArray(1))))
        }

        override suspend fun writeHead() {
            calls += "head-current"
            if (failHead) throw IllegalStateException("storage unreachable")
            writeVaultHead(store, FakeCodec(), listOf(ReentryEpoch(5, ByteArray(32), ByteArray(1))))
        }

        override suspend fun commitToJournal(commit: ByteArray, proofEpoch: Long?, proofSeed: ByteArray?) {
            calls += "commit:${commit.decodeToString()}"
            if (lost) throw IllegalStateException("409")
        }
        override fun isLost(cause: Throwable): Boolean = lost
        override suspend fun merge() {
            calls += "merge"
        }
        override suspend fun abandon() {
            calls += "abandon"
        }
        override suspend fun deliverWelcome(entry: PublisherCommitEntry, welcome: ByteArray, accountKey: ByteArray) = Unit
        override suspend fun serverEpoch(): Long = 7
        override suspend fun reseal(targetEpoch: Long) {
            calls += "reseal:$targetEpoch"
        }
        override suspend fun appoint(joinerKey: ByteArray) = Unit
        override fun save(entry: PublisherCommitEntry) = Unit
        override fun finish(entry: PublisherCommitEntry) {
            finished = true
        }
    }

    private val quiet: (String, Throwable) -> Unit = { _, _ -> }

    @Test
    fun `the head is written before the commit is sent`() = runTest {
        val store = FakeStore()
        val ports = Ports(store)
        runPublisherCommit(rotation, ports, quiet)
        assertEquals(listOf("head-staged", "commit:rotation", "merge", "reseal:8"), ports.calls)
        assertEquals("5", store.sealed!!.decodeToString())
    }

    @Test
    fun `a failed head write does not stop the commit`() = runTest {
        val ports = Ports(FakeStore(), failHead = true)
        val outcome = runPublisherCommit(rotation, ports, quiet)
        assertEquals(PublisherCommitOutcome.LANDED, outcome)
        assertEquals(listOf("head-staged", "commit:rotation", "merge", "reseal:8"), ports.calls)
        assertTrue(ports.finished)
    }

    @Test
    fun `an entry merges, reseals and then writes the head, and a failed head write still finishes it`() = runTest {
        val ports = Ports(FakeStore(), failHead = true)
        val outcome = runPublisherCommit(entering, ports, quiet)
        assertEquals(PublisherCommitOutcome.LANDED, outcome)
        assertEquals(listOf("head-staged", "commit:enter", "merge", "reseal:8", "head-current"), ports.calls)
        assertTrue(ports.finished)
    }

    @Test
    fun `an entry the DS refuses is abandoned and nothing else runs`() = runTest {
        val ports = Ports(FakeStore(), lost = true)
        val outcome = runPublisherCommit(entering, ports, quiet)
        assertEquals(PublisherCommitOutcome.LOST, outcome)
        assertEquals(listOf("head-staged", "commit:enter", "abandon"), ports.calls)
    }

    @Test
    fun `an entry record needs its saved epoch and access seed`() {
        assertTrue(entering.isWellFormed)
        assertFalse(entering.copy(proofSeedB64 = null).isWellFormed)
        assertFalse(entering.copy(proofEpoch = null).isWellFormed)
        assertEquals(
            listOf(PublisherCommitEntry.Step.JOURNAL, PublisherCommitEntry.Step.RESEAL, PublisherCommitEntry.Step.HEAD),
            PublisherCommitEntry.stepsOf(PublisherCommitEntry.Kind.ENTER),
        )
    }

    @Test
    fun `an owner device writes the current head only after it applied a commit and holds none of its own`() {
        assertTrue(headWriteAfterPoll(owner = true, epochMoved = true, hasStagedCommit = false))
        assertFalse(headWriteAfterPoll(owner = true, epochMoved = false, hasStagedCommit = false))
        assertFalse(headWriteAfterPoll(owner = true, epochMoved = true, hasStagedCommit = true))
        assertFalse(headWriteAfterPoll(owner = false, epochMoved = true, hasStagedCommit = false))
    }

    // --- entry -----------------------------------------------------------

    @Test
    fun `an entry tries the newest saved epoch first and stops when it lands`() = runTest {
        val tried = mutableListOf<Long>()
        val entered = enterNewestFirst(listOf(epoch(3), epoch(4))) { r ->
            tried += r.epoch
            EntryAttempt.ENTERED
        }
        assertTrue(entered)
        assertEquals(listOf(4L), tried)
    }

    @Test
    fun `an entry falls back to the older epoch when the newest is not taken`() = runTest {
        val tried = mutableListOf<Long>()
        val entered = enterNewestFirst(listOf(epoch(4), epoch(3))) { r ->
            tried += r.epoch
            if (r.epoch == 4L) EntryAttempt.NOT_TAKEN else EntryAttempt.ENTERED
        }
        assertTrue(entered)
        assertEquals(listOf(4L, 3L), tried)
    }

    @Test
    fun `with no head, or every epoch not taken, the device waits`() = runTest {
        var tries = 0
        assertFalse(enterNewestFirst(emptyList()) { _ -> tries += 1; EntryAttempt.ENTERED })
        assertEquals(0, tries)
        assertFalse(enterNewestFirst(listOf(epoch(2), epoch(1))) { _ -> EntryAttempt.NOT_TAKEN })
    }

    @Test
    fun `no answer ends the entry and surfaces, leaving it to the next pass`() = runTest {
        val tried = mutableListOf<Long>()
        try {
            enterNewestFirst(listOf(epoch(4), epoch(3))) { r ->
                tried += r.epoch
                throw IllegalStateException("no answer")
            }
            fail("an unanswered entry must surface")
        } catch (_: IllegalStateException) {
        }
        assertEquals(listOf(4L), tried)
    }

    @Test
    fun `a head nobody wrote or claimed reads as none, an unreachable one throws`() = runTest {
        assertTrue(readVaultReentry(FakeStore(claimed = false), FakeCodec()).isEmpty())
        assertTrue(readVaultReentry(FakeStore(), FakeCodec()).isEmpty())
        assertEquals(
            listOf(6L, 5L),
            readVaultReentry(FakeStore(version = 3, sealed = "6,5".toByteArray()), FakeCodec()).map { it.epoch },
        )
        try {
            readVaultReentry(FakeStore(failRead = true), FakeCodec())
            fail("an unreachable head must not read as none")
        } catch (_: IllegalStateException) {
        }
    }

    @Test
    fun `only an owner device that holds nothing for the channel enters`() {
        fun wanted(
            ownerRole: Boolean = true,
            subscribed: Boolean = true,
            holdsKeys: Boolean = false,
            holdsGroup: Boolean = false,
            suspended: Boolean = false,
            orphaned: Boolean = false,
        ) = entryWanted(ownerRole, subscribed, holdsKeys, holdsGroup, suspended, orphaned)
        assertTrue(wanted())
        // A co-publisher's device is admitted by an owner device, never enters.
        assertFalse(wanted(ownerRole = false))
        // A device that holds the group (or posts already) does not enter.
        assertFalse(wanted(holdsGroup = true))
        assertFalse(wanted(holdsKeys = true))
        // A suspected reset: this device holds the group.
        assertFalse(wanted(suspended = true))
        assertFalse(wanted(orphaned = true))
        assertFalse(wanted(subscribed = false))
    }

    // --- deleting vaults -------------------------------------------------

    @Test
    fun `the vault goes after the channel delete, and its failure does not undo it`() = runTest {
        val calls = mutableListOf<String>()
        deleteChannelThenVault(
            deleteChannel = { calls += "channel" },
            warn = { calls += "warn" },
        ) {
            calls += "vault"
            throw IllegalStateException("storage unreachable")
        }
        assertEquals(listOf("channel", "vault", "warn"), calls)
    }

    @Test
    fun `a failed channel delete deletes no vault`() = runTest {
        val calls = mutableListOf<String>()
        try {
            deleteChannelThenVault(deleteChannel = { throw IllegalStateException("refused") }, warn = {}) { calls += "vault" }
            fail("the channel delete failure must surface")
        } catch (_: IllegalStateException) {
        }
        assertTrue(calls.isEmpty())
    }

    @Test
    fun `account deletion deletes every owned vault, past a failure`() = runTest {
        val tried = mutableListOf<Int>()
        val deleted = deleteVaultsBounded(listOf(byteArrayOf(1), byteArrayOf(2), byteArrayOf(3)), warn = {}) { id ->
            tried += id[0].toInt()
            if (id[0].toInt() == 2) throw IllegalStateException("500")
        }
        assertEquals(listOf(1, 2, 3), tried)
        assertEquals(2, deleted)
    }

    @Test
    fun `a slow storage service cannot hold the account deletion`() = runTest {
        val warnings = mutableListOf<String>()
        val start = testScheduler.currentTime
        val deleted = deleteVaultsBounded(listOf(byteArrayOf(1), byteArrayOf(2)), timeoutMs = 1_000L, warn = { warnings += it }) {
            kotlinx.coroutines.delay(60_000L)
        }
        assertEquals(0, deleted)
        assertEquals(1_000L, testScheduler.currentTime - start)
        assertEquals(1, warnings.size)
    }

    // --- security pass ---------------------------------------------------

    @Test
    fun `a re-founding write replaces the dead group's epochs with the new group's`() = runTest {
        val store = FakeStore(version = 9, sealed = "41,40".toByteArray())
        assertTrue(writeVaultHead(store, FakeCodec(), listOf(epoch(1)), replace = true))
        assertEquals(listOf("read", "write:10"), store.calls)
        assertEquals("1", store.sealed!!.decodeToString())
        // A merge would have kept the dead group's GroupInfos.
        val merged = FakeStore(version = 9, sealed = "41,40".toByteArray())
        writeVaultHead(merged, FakeCodec(), listOf(epoch(1)))
        assertEquals("41,40", merged.sealed!!.decodeToString())
    }

    @Test
    fun `applying an entry runs the own-device reconcile in the same pass`() {
        val a = ByteArray(32) { 1 }
        val b = ByteArray(32) { 2 }
        val entered = ByteArray(32) { 3 }
        // Another device entered: a new owner-account leaf after the read.
        assertTrue(reconcileAfterRead(listOf(a, b), epochMoved = true) { listOf(a, b, entered) })
        // No added leaf, no moved epoch, or a co-publisher's device: not now.
        assertFalse(reconcileAfterRead(listOf(a, b), epochMoved = true) { listOf(a, b) })
        assertFalse(reconcileAfterRead(listOf(a, b), epochMoved = false) { listOf(a, b, entered) })
        assertFalse(reconcileAfterRead(null, epochMoved = true) { listOf(a, b, entered) })
        assertTrue(ownLeafAdded(listOf(a), listOf(b)))
        assertFalse(ownLeafAdded(listOf(a, b), listOf(b)))
    }

    @Test
    fun `an incoming epoch older than both saved ones writes nothing and reports false`() = runTest {
        val store = FakeStore(version = 4, sealed = "6,5".toByteArray())
        assertFalse(writeVaultHead(store, FakeCodec(), listOf(epoch(3))))
        assertEquals(listOf("read"), store.calls)
        assertEquals(4L, store.version)
        assertEquals("6,5", store.sealed!!.decodeToString())
    }
}
