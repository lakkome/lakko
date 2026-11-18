// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.test.runTest
import me.lakko.app.devices.sync.DeviceGroupStandaloneJournal
import me.lakko.app.ds.DsHttpError
import me.lakko.app.ds.DsNetworkError
import me.lakko.app.ds.DsProofRefusedError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0074 section 3.3 - a superseded device that holds no pending envelope
 * reads its DeviceGroup journal once before it decides; when no sync runtime
 * is polling it reads on its own ([readJournalStandalone]). A kept device of a
 * rotation whose envelope is in the journal adopts; without one it is a reset;
 * no answer is "try later", never a wipe.
 */
class SupersededJournalReadTest {

    private val rotationEnvelope = byteArrayOf(7, 7, 7)

    private fun pendingOf(envelope: ByteArray) = PendingRotation(
        newSignaturePubkey = ByteArray(32) { envelope[0] },
        newEntropy = ByteArray(16) { 1 },
        supersessionProof = ByteArray(145) { 2 },
        preparedAtMs = 1_000L,
        proofExpiresAtUnixSecs = 9_000L,
    )

    /** A journal of [pages]; a row whose payload is [rotationEnvelope] carries
     *  the key-rotation envelope, any other carries none; a payload of `[0]`
     *  throws on apply (an own row). */
    private class FakeJournal(
        override val cursor: Long,
        private val pages: MutableList<() -> StandaloneJournalPage>,
        private val rotation: ByteArray,
    ) : StandaloneJournal {
        val fetchedAfter = mutableListOf<Long>()
        var closed = false

        override suspend fun fetch(after: Long): StandaloneJournalPage {
            fetchedAfter += after
            return pages.removeAt(0)()
        }

        override fun apply(row: StandaloneJournalRow): ByteArray? {
            if (row.payload.contentEquals(byteArrayOf(0))) throw IllegalStateException("own row")
            return if (row.payload.contentEquals(rotation)) row.payload else null
        }

        override fun close() {
            closed = true
        }
    }

    private fun page(vararg rows: Pair<Long, ByteArray>, hasMore: Boolean = false): () -> StandaloneJournalPage =
        { StandaloneJournalPage(rows.map { StandaloneJournalRow(it.first, it.second) }, hasMore) }

    /** The decision the superseded device takes after the read, with the
     *  envelopes the intake stored. */
    private fun decide(catchUp: JournalCatchUp, stored: List<PendingRotation>): KeptDeviceVerdict? =
        when (journalReadStep(catchUp)) {
            JournalReadStep.Later -> null
            JournalReadStep.Decide -> keptDeviceVerdict(stored, publishedKey = null, superseded = true, journalRead = true)
        }

    @Test
    fun `a kept device whose envelope is in the journal adopts the new keys`() = runTest {
        val stored = mutableListOf<PendingRotation>()
        val journal = FakeJournal(
            cursor = 10,
            pages = mutableListOf(
                page(11L to byteArrayOf(1), 12L to byteArrayOf(0), hasMore = true),
                page(13L to rotationEnvelope),
            ),
            rotation = rotationEnvelope,
        )
        val catchUp = readJournalStandalone(
            open = { journal },
            intake = { stored += pendingOf(it) },
            classify = DeviceGroupStandaloneJournal::classify,
        )
        assertEquals(JournalCatchUp.Read, catchUp)
        // From the persisted cursor, page by page; an own row that fails to
        // apply does not stop the read.
        assertEquals(listOf(10L, 12L), journal.fetchedAfter)
        assertTrue(journal.closed)
        assertEquals(1, stored.size)
        val verdict = decide(catchUp, stored)
        assertTrue(verdict is KeptDeviceVerdict.TryUpgrade)
        val tried = (verdict as KeptDeviceVerdict.TryUpgrade).candidates.single()
        val pair = SessionPair("a2", 1, "r2", 2, keyGeneration = 2)
        val resolution = keptResolutionOf(listOf(tried to UpgradeOutcome.Upgraded(pair)))
        assertTrue(resolution is KeptResolution.Adopted)
    }

    @Test
    fun `a journal read to its head with no envelope is a reset`() = runTest {
        val stored = mutableListOf<PendingRotation>()
        val catchUp = readJournalStandalone(
            open = { FakeJournal(5, mutableListOf(page(6L to byteArrayOf(1))), rotationEnvelope) },
            intake = { stored += pendingOf(it) },
            classify = DeviceGroupStandaloneJournal::classify,
        )
        assertEquals(JournalCatchUp.Read, catchUp)
        assertEquals(KeptDeviceVerdict.TreatAsReset, decide(catchUp, stored))
    }

    @Test
    fun `no answer from the journal is later, never a wipe`() = runTest {
        val offline = readJournalStandalone(
            open = {
                FakeJournal(
                    5,
                    mutableListOf({ throw DsNetworkError("/ds/group/x/messages", java.io.IOException("no route")) }),
                    rotationEnvelope,
                )
            },
            intake = {},
            classify = DeviceGroupStandaloneJournal::classify,
        )
        assertEquals(JournalCatchUp.Offline, offline)
        assertEquals(JournalReadStep.Later, journalReadStep(offline))
        assertEquals(null, decide(offline, emptyList()))

        // A server fault, a clock the DS refuses, and a state that cannot be
        // opened right now: the same.
        assertEquals(JournalCatchUp.Offline, DeviceGroupStandaloneJournal.classify(DsHttpError(503, "/ds", "")))
        assertEquals(
            JournalCatchUp.Offline,
            DeviceGroupStandaloneJournal.classify(DsProofRefusedError("/ds", clockSkewed = true, body = "")),
        )
        assertEquals(
            JournalCatchUp.Offline,
            readJournalStandalone(open = { throw IllegalStateException("attest key") }, intake = {}, classify = { JournalCatchUp.Read }),
        )
    }

    @Test
    fun `a journal that refuses the device, or no state at all, is decided as read`() = runTest {
        assertEquals(
            JournalCatchUp.Unreadable,
            DeviceGroupStandaloneJournal.classify(DsProofRefusedError("/ds", clockSkewed = false, body = "")),
        )
        assertEquals(JournalCatchUp.Unreadable, DeviceGroupStandaloneJournal.classify(DsHttpError(403, "/ds", "")))
        val none = readJournalStandalone(open = { null }, intake = {}, classify = { JournalCatchUp.Offline })
        assertEquals(JournalCatchUp.NotLoaded, none)
        assertEquals(KeptDeviceVerdict.TreatAsReset, decide(none, emptyList()))
        assertEquals(KeptDeviceVerdict.TreatAsReset, decide(JournalCatchUp.Unreadable, emptyList()))
    }

    @Test
    fun `a read that outlasts its deadline is later`() = runTest {
        var clock = 0L
        val journal = FakeJournal(
            0,
            mutableListOf(
                { clock += 30_000L; StandaloneJournalPage(listOf(StandaloneJournalRow(1, byteArrayOf(1))), true) },
                { clock += 30_000L; StandaloneJournalPage(listOf(StandaloneJournalRow(2, byteArrayOf(1))), true) },
            ),
            rotationEnvelope,
        )
        val catchUp = readJournalStandalone(open = { journal }, intake = {}, classify = { JournalCatchUp.Offline }, now = { clock })
        assertEquals(JournalCatchUp.Offline, catchUp)
        assertTrue(journal.closed)
    }
}
