// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.io.IOException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Pins the ordering and single-flight rules of [KeyPackageRepublisher] — the
 * fix for the published KeyPackage being unusable.
 *
 *  1. Mint and persist happen under the provider lock, the upload after it and
 *     outside it: persist strictly BEFORE upload.
 *  2. No upload when the persist fails (fail closed), and none when the mint
 *     fails or there is no session.
 *  3. A burst of requests made before a run mints uploads ONCE.
 *  4. A request made after a run minted gets its OWN run — never answered with
 *     the KeyPackage minted before it (the post-Welcome-join case).
 *  5. A failed run does not wedge later requests.
 *
 * Pure fakes: no Android Context, no native code.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class KeyPackageRepublishTest {

    private class FakePorts : KeyPackageRepublishPorts<String> {
        val events = mutableListOf<String>()
        val providerLock = Mutex()
        var session: String? = "acct"
        var failCreate = false
        var failPersist = false
        var failUpload = false
        var mintCount = 0
        val uploaded = mutableListOf<ByteArray>()

        /** When non-null, [upload] suspends on it. */
        var uploadGate: CompletableDeferred<Unit>? = null

        /** Completed when an upload starts waiting on [uploadGate]. */
        val uploadEntered = CompletableDeferred<Unit>()

        override suspend fun <T> exclusiveProvider(block: suspend () -> T): T =
            providerLock.withLock {
                events += "lock"
                try {
                    block()
                } finally {
                    events += "unlock"
                }
            }

        override fun currentSession(): String? = session

        override fun createKeyPackage(session: String): ByteArray {
            check(providerLock.isLocked) { "mint outside the provider lock" }
            if (failCreate) throw IllegalStateException("mint failed")
            mintCount += 1
            events += "mint$mintCount"
            return byteArrayOf(mintCount.toByte())
        }

        override fun persistSnapshotDurably(session: String) {
            check(providerLock.isLocked) { "persist outside the provider lock" }
            if (failPersist) throw IOException("disk full")
            events += "persist"
        }

        override suspend fun upload(session: String, keyPackage: ByteArray) {
            events += "upload${keyPackage[0]}"
            if (providerLock.isLocked) events += "UPLOAD-UNDER-LOCK"
            uploadGate?.let {
                uploadEntered.complete(Unit)
                it.await()
            }
            if (failUpload) throw IOException("network")
            uploaded += keyPackage
        }
    }

    private fun republisher(ports: FakePorts) =
        KeyPackageRepublisher(ports) { _, _ -> }

    // -----------------------------------------------------------------
    // 1 + 2. ordering and fail-closed
    // -----------------------------------------------------------------

    @Test
    fun `mint and persist under the lock, then upload outside it`() = runTest {
        val ports = FakePorts()
        val outcome = republisher(ports).republish()
        assertEquals(KeyPackageRepublishOutcome.UPLOADED, outcome)
        assertEquals(listOf("lock", "mint1", "persist", "unlock", "upload1"), ports.events)
    }

    @Test
    fun `a failed persist uploads nothing`() = runTest {
        val ports = FakePorts().apply { failPersist = true }
        val outcome = republisher(ports).republish()
        assertEquals(KeyPackageRepublishOutcome.PERSIST_FAILED, outcome)
        assertTrue(ports.uploaded.isEmpty())
        assertFalse(ports.events.any { it.startsWith("upload") })
    }

    @Test
    fun `a failed mint neither persists nor uploads`() = runTest {
        val ports = FakePorts().apply { failCreate = true }
        assertEquals(KeyPackageRepublishOutcome.CREATE_FAILED, republisher(ports).republish())
        assertEquals(listOf("lock", "unlock"), ports.events)
    }

    @Test
    fun `no session does nothing`() = runTest {
        val ports = FakePorts().apply { session = null }
        assertEquals(KeyPackageRepublishOutcome.NO_SESSION, republisher(ports).republish())
        assertEquals(listOf("lock", "unlock"), ports.events)
    }

    @Test
    fun `a failed upload is reported, after a successful persist`() = runTest {
        val ports = FakePorts().apply { failUpload = true }
        assertEquals(KeyPackageRepublishOutcome.UPLOAD_FAILED, republisher(ports).republish())
        assertEquals(listOf("lock", "mint1", "persist", "unlock", "upload1"), ports.events)
    }

    // -----------------------------------------------------------------
    // 3 + 4. single flight
    // -----------------------------------------------------------------

    @Test
    fun `a burst of requests made before the mint uploads once`() = runTest {
        val ports = FakePorts()
        val rep = republisher(ports)
        // Numbered synchronously, the way requestKeyPackageRepublish does.
        val tickets = List(5) { rep.markRequested() }
        val outcomes = tickets.map { t -> async { rep.serve(t) } }.awaitAll()
        assertEquals("one mint for the whole burst", 1, ports.mintCount)
        assertEquals(1, ports.uploaded.size)
        assertEquals(1, outcomes.count { it == KeyPackageRepublishOutcome.UPLOADED })
        assertEquals(4, outcomes.count { it == KeyPackageRepublishOutcome.COVERED })
    }

    @Test
    fun `a request made after the mint gets a fresh KeyPackage of its own`() = runTest {
        val ports = FakePorts().apply { uploadGate = CompletableDeferred() }
        val rep = republisher(ports)
        val first = async { rep.republish() }
        ports.uploadEntered.await() // first run has minted KP1 and is uploading
        // e.g. a Welcome join just consumed KP1; its follow-up request:
        val later = rep.markRequested()
        val second = async { rep.serve(later) }
        runCurrent()
        ports.uploadGate!!.complete(Unit)
        assertEquals(KeyPackageRepublishOutcome.UPLOADED, first.await())
        assertEquals(KeyPackageRepublishOutcome.UPLOADED, second.await())
        assertEquals(2, ports.mintCount)
        assertEquals(listOf(1.toByte(), 2.toByte()), ports.uploaded.map { it[0] })
    }

    @Test
    fun `requests queued behind an in-flight run collapse into one follow-up`() = runTest {
        val ports = FakePorts().apply { uploadGate = CompletableDeferred() }
        val rep = republisher(ports)
        val first = async { rep.republish() }
        ports.uploadEntered.await()
        val queued = List(4) { rep.markRequested() }.map { t -> async { rep.serve(t) } }
        runCurrent()
        ports.uploadGate!!.complete(Unit)
        first.await()
        val outcomes = queued.awaitAll()
        advanceUntilIdle()
        assertEquals("the in-flight run plus exactly one follow-up", 2, ports.mintCount)
        assertEquals(1, outcomes.count { it == KeyPackageRepublishOutcome.UPLOADED })
        assertEquals(3, outcomes.count { it == KeyPackageRepublishOutcome.COVERED })
    }

    // -----------------------------------------------------------------
    // 5. failure does not wedge
    // -----------------------------------------------------------------

    @Test
    fun `a failed run does not block the next request`() = runTest {
        val ports = FakePorts().apply { failPersist = true }
        val rep = republisher(ports)
        assertEquals(KeyPackageRepublishOutcome.PERSIST_FAILED, rep.republish())
        ports.failPersist = false
        assertEquals(KeyPackageRepublishOutcome.UPLOADED, rep.republish())
        assertEquals(1, ports.uploaded.size)
    }
}
