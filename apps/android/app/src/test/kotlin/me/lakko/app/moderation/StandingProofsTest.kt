// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import java.util.Base64
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.runTest
import me.lakko.app.channels.RemovedChannels
import me.lakko.app.channels.channelRemovedLineKey
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0089 F2 — a `suspended` answer is acted on only after the moderation
 * log entry behind it proves a moderator signed it. The host suite has no
 * native core, so the snapshot check and the core's prove call are fakes;
 * what is tested is the client's rule: unproven = UNKNOWN at every site, a
 * proven standing acts, the proof is cached with the answer, and the entry is
 * fetched at the answer's own log size.
 */
class StandingProofsTest {

    private val pins = ModerationLogPins(ByteArray(32) { 7 }, emptyList(), 1)

    private fun key(first: Int, fill: Int = 0): ByteArray =
        ByteArray(32) { fill.toByte() }.also { it[0] = first.toByte() }

    private fun b64(bytes: ByteArray): String = Base64.getEncoder().encodeToString(bytes)

    private class Probe {
        var proofs = 0
        var proves = true
        var logIndex: ULong? = 9uL
        val seenIndexes = mutableListOf<ULong>()
        var now = 1_000_000L
    }

    private fun service(
        p: Probe,
        scope: kotlinx.coroutines.CoroutineScope,
        withProve: Boolean = true,
    ) = SuspensionStandings(
        pins = pins,
        prefixOf = { "%02x00".format(it[0].toInt() and 0xff) },
        fetch = { "answer" },
        verify = { _, _, _, _ -> VerifiedStanding(true, Long.MAX_VALUE, p.logIndex) },
        scope = scope,
        nowMs = { p.now },
        prove = if (withProve) {
            { answer, _, index, _ ->
                assertEquals("answer", answer)
                p.proofs += 1
                p.seenIndexes += index
                p.proves
            }
        } else {
            null
        },
    )

    private val savedLauncher = RemovedChannels.launcher

    @After
    fun reset() {
        RemovedChannels.launcher = savedLauncher
        SuspensionMarks.overrideForTests(null)
        SuspensionMarks.overrideChannelsForTests(null)
        SuspensionMarks.ownAccountKey = { null }
        RemovedChannels.clear()
        RemovedChannels.standingOf = { SuspensionMarks.channelAnswer(it) }
    }

    @Test
    fun `a suspended answer with no prover is unknown`() = runTest {
        val s = service(Probe(), this, withProve = false)
        assertEquals(AccountStanding.UNKNOWN, s.standing(b64(key(1))))
    }

    @Test
    fun `a suspended answer with no log index is unknown and proves nothing`() = runTest {
        val p = Probe().apply { logIndex = null }
        val s = service(p, this)
        assertEquals(AccountStanding.UNKNOWN, s.standing(b64(key(1))))
        assertEquals(0, p.proofs)
    }

    @Test
    fun `a proven suspension acts and the proof is kept with the answer`() = runTest {
        val p = Probe()
        val s = service(p, this)
        val k = b64(key(1))
        assertEquals(AccountStanding.SUSPENDED, s.standing(k))
        assertEquals(AccountStanding.SUSPENDED, s.standing(k))
        assertEquals(1, p.proofs)
        assertEquals(listOf(9uL), p.seenIndexes)
        // Another account in the same bucket is proven on its own.
        assertEquals(AccountStanding.SUSPENDED, s.standing(b64(key(1, fill = 3))))
        assertEquals(2, p.proofs)
    }

    @Test
    fun `a failed proof is unknown and not tried again for the back-off`() = runTest {
        val p = Probe().apply { proves = false }
        val s = service(p, this)
        val k = b64(key(2))
        assertEquals(AccountStanding.UNKNOWN, s.standing(k))
        p.proves = true
        p.now += SuspensionStandings.FAILURE_BACKOFF_MS - 1
        assertEquals(AccountStanding.UNKNOWN, s.standing(k))
        assertEquals(1, p.proofs)
        p.now += 1
        assertEquals(AccountStanding.SUSPENDED, s.standing(k))
        assertEquals(2, p.proofs)
    }

    @Test
    fun `a prover that throws is unknown`() = runTest {
        val s = SuspensionStandings(
            pins = pins,
            prefixOf = { "0100" },
            fetch = { "answer" },
            verify = { _, _, _, _ -> VerifiedStanding(true, Long.MAX_VALUE, 1uL) },
            scope = this,
            prove = { _, _, _, _ -> error("bad-list") },
        )
        assertEquals(AccountStanding.UNKNOWN, s.standing(b64(key(1))))
    }

    @Test
    fun `an unproven suspension is unknown at every account site`() = runTest {
        val p = Probe().apply { proves = false }
        SuspensionMarks.overrideForTests(service(p, this))
        val k = key(4)
        // Banner, card line, member marker: isSuspended.
        assertFalse(SuspensionMarks.isSuspended(b64(k)))
        assertFalse(suspendedBannerShows(blocked = false, suspended = SuspensionMarks.isSuspended(b64(k))))
        assertFalse(suspendedCardLineShows(blocked = false, suspended = SuspensionMarks.isSuspended(b64(k))))
        // Contact-request drop, held requests, link-request decision.
        assertEquals(listOf(k), SuspensionMarks.withoutSuspendedRequests(listOf(k), keyOf = { it }))
        // Invite Add.
        assertTrue(inviteAddMayProceed(k, 3_000) { SuspensionMarks.standing(it) })
        assertFalse(suspendedWithin(k, 3_000) { SuspensionMarks.standing(it) })
        // Search filter: nothing is confirmed, nothing filtered.
        val confirmed = listOf(k).filter { SuspensionMarks.isSuspended(b64(it)) }.map(::b64).toSet()
        assertEquals(listOf("row"), withoutSuspendedRows(listOf("row"), confirmed) { b64(k) })
    }

    @Test
    fun `a proven suspension acts at every account site`() = runTest {
        val p = Probe()
        SuspensionMarks.overrideForTests(service(p, this))
        val k = key(5)
        assertTrue(SuspensionMarks.isSuspended(b64(k)))
        assertTrue(suspendedBannerShows(blocked = false, suspended = SuspensionMarks.isSuspended(b64(k))))
        assertTrue(SuspensionMarks.withoutSuspendedRequests(listOf(k), keyOf = { it }).isEmpty())
        assertFalse(inviteAddMayProceed(k, 3_000) { SuspensionMarks.standing(it) })
        val confirmed = listOf(k).filter { SuspensionMarks.isSuspended(b64(it)) }.map(::b64).toSet()
        assertTrue(withoutSuspendedRows(listOf("row"), confirmed) { b64(k) }.isEmpty())
    }

    @Test
    fun `an unproven channel removal shows the neutral line, a proven one the removal`() = runBlocking {
        val channel = "00112233445566778899aabbccddeeff"
        val p = Probe().apply { proves = false }
        // The removal check blocks its caller (the test launcher below), so
        // the service runs on its own threads, not the test's.
        val pool = kotlinx.coroutines.CoroutineScope(kotlinx.coroutines.Dispatchers.Default)
        SuspensionMarks.overrideChannelsForTests(service(p, pool))
        RemovedChannels.launcher = { block -> runBlocking { block() } }
        RemovedChannels.noteRemoved(channel)
        // The 410 still marks it removed (writes withheld) ...
        assertTrue(RemovedChannels.isRemoved(channel))
        // ... but without a proof the line is the neutral one.
        assertEquals("channels.unavailable", channelRemovedLineKey(RemovedChannels.state.value[channel]))

        RemovedChannels.clear()
        SuspensionMarks.overrideChannelsForTests(service(Probe(), pool))
        RemovedChannels.noteRemoved(channel)
        assertEquals("suspended.channel_removed", channelRemovedLineKey(RemovedChannels.state.value[channel]))
    }

    // -----------------------------------------------------------------
    // StandingProofs + ModeratorListCache
    // -----------------------------------------------------------------

    private val answer = """{"period":1,"root":{"tree_root_b64":"x","log_size":42,"timestamp_unix":1,"signature_b64":"s","witness_cosigs_b64":[]}}"""

    private class ListSource {
        var fetches = 0
        var lists = ArrayDeque<ByteArray>()
        fun fetch(): ModeratorListFetch {
            fetches += 1
            return lists.removeFirstOrNull()?.let { ModeratorListFetch.Ok(it) } ?: ModeratorListFetch.Network
        }
    }

    private class Floor(var value: ULong? = null) : ModeratorListFloor {
        override fun highest(): ULong? = value
        override fun record(version: ULong) {
            if (value == null || version > value!!) value = version
        }
    }

    /** A list "verifies" when its first byte is not 0; its version is that byte. */
    private fun cache(
        src: ListSource,
        root: ByteArray? = ByteArray(32),
        now: () -> Long = { 0L },
        floor: ModeratorListFloor? = Floor(),
    ) = ModeratorListCache(
        rootKey = { root },
        fetch = { src.fetch() },
        versionOf = { list, _ -> if (list[0].toInt() == 0) error("bad-list") else list[0].toULong() },
        floor = { floor },
        nowMs = now,
    )

    @Test
    fun `the answer's log size is read from its root`() {
        assertEquals(42uL, answerLogSize(answer))
        assertNull(answerLogSize("""{"root":{"log_size":"42"}}"""))
        assertNull(answerLogSize("not json"))
        assertNull(answerLogSize("""{"period":1}"""))
    }

    @Test
    fun `the entry is fetched at the answer's log index and size and checked with the list and root`() = runTest {
        val src = ListSource().apply { lists.add(byteArrayOf(1)) }
        val root = ByteArray(32) { 9 }
        val asked = mutableListOf<Pair<ULong, ULong>>()
        val proofs = StandingProofs({ root }, { i, size -> asked += i to size; "entry" }, cache(src, root))
        var checked: Triple<String, ByteArray, ByteArray>? = null
        assertTrue(proofs.prove(answer, 7uL) { e, l, r -> checked = Triple(e, l, r) })
        assertEquals(listOf(7uL to 42uL), asked)
        assertEquals("entry", checked!!.first)
        assertTrue(checked!!.second.contentEquals(byteArrayOf(1)))
        assertTrue(checked!!.third.contentEquals(root))
    }

    @Test
    fun `no pinned root, no entry or no list proves nothing`() = runTest {
        val src = ListSource().apply { lists.add(byteArrayOf(1)) }
        var entryAsked = 0
        assertFalse(StandingProofs({ null }, { _, _ -> entryAsked++; "e" }, cache(src)).prove(answer, 1uL) { _, _, _ -> })
        assertEquals(0, entryAsked)
        assertFalse(StandingProofs({ ByteArray(32) }, { _, _ -> null }, cache(src)).prove(answer, 1uL) { _, _, _ -> })
        assertFalse(StandingProofs({ ByteArray(32) }, { _, _ -> "e" }, cache(ListSource())).prove(answer, 1uL) { _, _, _ -> })
        assertFalse(StandingProofs({ ByteArray(32) }, { _, _ -> "e" }, cache(src)).prove("{}", 1uL) { _, _, _ -> })
    }

    @Test
    fun `a refusal with a kept list is tried once with a list fetched now`() = runTest {
        val src = ListSource().apply {
            lists.add(byteArrayOf(1))
            lists.add(byteArrayOf(2))
        }
        val c = cache(src)
        c.get() // keeps version 1
        val proofs = StandingProofs({ ByteArray(32) }, { _, _ -> "e" }, c)
        val seen = mutableListOf<Int>()
        assertTrue(proofs.prove(answer, 1uL) { _, l, _ -> seen += l[0].toInt(); if (l[0].toInt() == 1) error("not-authorised") })
        assertEquals(listOf(1, 2), seen)
        // A refusal with a list fetched now is final.
        assertFalse(proofs.prove(answer, 1uL) { _, _, _ -> error("wrong-target") })
    }

    @Test
    fun `the list cache keeps only verified lists and never a lower version`() = runTest {
        val src = ListSource().apply { lists.add(byteArrayOf(0)) }
        val c = cache(src)
        assertNull(c.get())
        c.offer(byteArrayOf(3))
        assertEquals(3, c.get()!!.bytes[0].toInt())
        assertEquals(1, src.fetches)
        c.offer(byteArrayOf(2))
        assertEquals(3, c.get()!!.bytes[0].toInt())
        c.offer(byteArrayOf(0))
        assertEquals(3, c.get()!!.bytes[0].toInt())
    }

    @Test
    fun `a kept list is fetched again after its age`() = runTest {
        var now = 0L
        val src = ListSource().apply {
            lists.add(byteArrayOf(1))
            lists.add(byteArrayOf(4))
        }
        val c = cache(src, now = { now })
        assertTrue(c.get()!!.fetchedNow)
        assertFalse(c.get()!!.fetchedNow)
        now += ModeratorListCache.MAX_AGE_MS
        assertEquals(4, c.get()!!.bytes[0].toInt())
        assertEquals(2, src.fetches)
    }

    @Test
    fun `no pinned root keeps no list`() = runTest {
        val src = ListSource().apply { lists.add(byteArrayOf(1)) }
        val c = cache(src, root = null)
        c.offer(byteArrayOf(1))
        assertNull(c.get())
    }

    @Test
    fun `a list older than the device's floor is refused and a newer one raises it`() = runTest {
        val floor = Floor(5uL)
        val src = ListSource().apply { lists.add(byteArrayOf(4)) }
        val c = cache(src, floor = floor)
        // The server hands an older list: unknown.
        assertNull(c.get())
        c.offer(byteArrayOf(3))
        assertNull(c.get())
        assertEquals(5uL, floor.value)
        // The floor's own version is accepted; a newer one raises the floor.
        c.offer(byteArrayOf(5))
        assertEquals(5, c.get()!!.bytes[0].toInt())
        c.offer(byteArrayOf(7))
        assertEquals(7uL, floor.value)
        assertEquals(7, c.get()!!.bytes[0].toInt())
    }

    @Test
    fun `a kept list overtaken by the report sheet's floor is not used`() = runTest {
        val floor = Floor()
        val src = ListSource().apply {
            lists.add(byteArrayOf(2))
            lists.add(byteArrayOf(2))
        }
        val c = cache(src, floor = floor)
        assertEquals(2, c.get()!!.bytes[0].toInt())
        // The report sheet saw version 6 since.
        floor.record(6uL)
        // The kept 2 is below the floor, and the server again hands a 2.
        assertNull(c.get())
    }

    @Test
    fun `a proof with a list below the floor proves nothing`() = runTest {
        val src = ListSource().apply { lists.add(byteArrayOf(3)) }
        var checked = 0
        val proofs = StandingProofs({ ByteArray(32) }, { _, _ -> "e" }, cache(src, floor = Floor(4uL)))
        assertFalse(proofs.prove(answer, 1uL) { _, _, _ -> checked++ })
        assertEquals(0, checked)
    }

    @Test
    fun `no floor store keeps no list`() = runTest {
        val src = ListSource().apply { lists.add(byteArrayOf(1)) }
        val c = cache(src, floor = null)
        c.offer(byteArrayOf(1))
        assertNull(c.get())
    }

    // The suspension appeal seals to a list checked the same way.

    @Test
    fun `the appeal list is verified and refused when older than the floor`() = runTest {
        val floor = Floor(5uL)
        val src = ListSource().apply {
            lists.add(byteArrayOf(4))
            lists.add(byteArrayOf(0))
            lists.add(byteArrayOf(6))
        }
        val c = cache(src, floor = floor)
        // Older than a list this device has seen: unavailable, nothing sealed.
        assertEquals(ModeratorListFetch.Unavailable, c.fetchVerified())
        assertEquals(5uL, floor.value)
        // Does not verify under the root: unavailable.
        assertEquals(ModeratorListFetch.Unavailable, c.fetchVerified())
        // Newer: sealed to, and the floor rises.
        val ok = c.fetchVerified() as ModeratorListFetch.Ok
        assertEquals(6, ok.bytes[0].toInt())
        assertEquals(6uL, floor.value)
        // No answer at all is a network failure, not "unavailable".
        assertEquals(ModeratorListFetch.Network, c.fetchVerified())
    }

    @Test
    fun `the appeal list needs the pinned root and the floor store`() = runTest {
        assertEquals(
            ModeratorListFetch.Unavailable,
            cache(ListSource().apply { lists.add(byteArrayOf(1)) }, root = null).fetchVerified(),
        )
        assertEquals(
            ModeratorListFetch.Unavailable,
            cache(ListSource().apply { lists.add(byteArrayOf(1)) }, floor = null).fetchVerified(),
        )
    }

    @Test
    fun `the appeal sheet takes its list through the cache`() {
        val source = java.io.File("src/main/kotlin/me/lakko/app/auth/AuthRepository.kt").readText()
        val appeal = source.substringAfter("    private suspend fun sendSuspensionAppeal(").substringBefore("\n    }\n")
        assertTrue("ModeratorListCache.shared.fetchVerified()" in appeal)
        assertFalse("moderatorList()" in appeal)
        assertTrue(appeal.indexOf("fetchVerified()") < appeal.indexOf("sealModerationAppeal("))
    }
}
