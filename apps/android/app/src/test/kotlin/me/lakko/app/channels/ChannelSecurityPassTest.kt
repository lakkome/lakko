// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * lakko-security findings of 2026-11-15 on Android: F1 (planted rows of a
 * private channel), F2 / F3 (a replayed old PublisherGroup Welcome) and F5
 * (New channel retry over a finished create). The PublisherGroup provider is
 * a fake here, driven in the order [CoPublisher.joinReplacingHeld] drives it
 * (the host JVM loads no native core).
 */
@RunWith(RobolectricTestRunner::class)
class ChannelSecurityPassTest {
    private lateinit var context: Context
    private val account = "AAECAwQFBgcICQoLDA0ODw=="
    private val channel = "cd".repeat(16)

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
    }

    @After
    fun tearDown() {
        JoinedPublisherWelcomes.forgetAccount(context, account)
        DeletedPublisherGroupEpochs.forgetAccount(context, account)
        ChannelCreatePending.forgetAccount(context, account)
        ChannelSuspensions.forgetAccount(context, account)
    }

    // ---------------------------------------------------------------- F1

    /** The service's rows (some planted) and what a pass did with them. */
    private class Channel(val rows: List<Long>) {
        var mark = 0L
        val seeds = ArrayList<Pair<UShort, ByteArray>>()
        var mints = 0
        fun held(): Pair<UShort, ByteArray>? = seeds.lastOrNull()?.let { it.first to it.second.copyOf() }
        fun mint(): Pair<UShort, ByteArray> {
            mints++
            val s = 0u.toUShort() to ByteArray(32) { 7 }
            seeds += s
            return s.first to s.second.copyOf()
        }
        fun markRows() {
            mark = maxOf(mark, rows.maxOrNull() ?: 0L)
        }
    }

    private fun seedFor(reason: AdmissionSeedReason, ch: Channel) = runBlocking {
        admissionSeedFor(reason, held = ch::held, mint = ch::mint, markRowsAsReaders = { ch.markRows() })
    }

    @Test
    fun `creating a private channel mints version 0 and marks no row, even with rows present`() {
        val ch = Channel(rows = listOf(1L, 2L, 3L))
        val seed = seedFor(AdmissionSeedReason.CREATE, ch)
        assertEquals(0u.toUShort(), seed?.first)
        assertEquals(1, ch.mints)
        assertEquals("no planted row counts as admitted", 0L, ch.mark)
        // A retry of the create finds the seed it minted.
        seedFor(AdmissionSeedReason.CREATE, ch)
        assertEquals(1, ch.mints)
        assertEquals(0L, ch.mark)
    }

    @Test
    fun `a device without seeds for an existing private channel neither mints nor marks`() {
        val ch = Channel(rows = listOf(4L, 9L))
        assertNull(seedFor(AdmissionSeedReason.USE, ch))
        assertEquals(0, ch.mints)
        assertEquals(0L, ch.mark)
        // Once the register's seed arrives it is used, still nothing marked.
        ch.seeds += 0u.toUShort() to ByteArray(32) { 3 }
        assertEquals(0u.toUShort(), seedFor(AdmissionSeedReason.USE, ch)?.first)
        assertEquals(0, ch.mints)
        assertEquals(0L, ch.mark)
    }

    @Test
    fun `a switch to private made here keeps today's rows as readers and mints once`() {
        val ch = Channel(rows = listOf(2L, 5L))
        assertEquals(0u.toUShort(), seedFor(AdmissionSeedReason.GO_PRIVATE, ch)?.first)
        assertEquals(5L, ch.mark)
        assertEquals(1, ch.mints)
    }

    // ------------------------------------------------------------- F2 / F3

    /** The fake provider: the group this device holds for the channel. */
    private class Provider(var epoch: ULong?, var active: Boolean = true)

    /**
     * `joinReplacingHeld` against [provider] for a verified owner-device
     * Welcome joining at [welcomeEpoch]: the joined epoch, or `null` when it
     * was refused (the provider is then unchanged).
     */
    private fun land(provider: Provider, welcome: ByteArray, welcomeEpoch: ULong): ULong? {
        if (JoinedPublisherWelcomes.contains(context, account, channel, welcome)) return null
        val suspected = ChannelSuspensions.isSuspended(context, account, channel)
        val deleted = DeletedPublisherGroupEpochs.get(context, account, channel)
        val held = provider.epoch
        val joined: ULong? = runCatching {
            if (held == null) {
                replaceHeldThenJoin(
                    snapshot = { provider.epoch to provider.active },
                    delete = {},
                    join = {
                        provider.epoch = welcomeEpoch
                        provider.active = true
                        if (!joinWithoutHeldGroupAllowed(welcomeEpoch, deleted, suspected)) throw StaleWelcomeRefused()
                        welcomeEpoch
                    },
                    restore = { (e, a) -> provider.epoch = e; provider.active = a },
                )
            } else {
                if (!heldGroupReplaceable(provider.active, suspected)) return null
                val heldActive = provider.active
                replaceHeldThenJoin(
                    snapshot = { provider.epoch to provider.active },
                    delete = { provider.epoch = null },
                    join = {
                        provider.epoch = welcomeEpoch
                        provider.active = true
                        if (!joinedGroupReplacesHeld(welcomeEpoch, held, heldActive)) throw StaleWelcomeRefused()
                        welcomeEpoch
                    },
                    restore = { (e, a) -> provider.epoch = e; provider.active = a },
                )
            }
        }.getOrNull()
        if (joined != null) {
            assertTrue(JoinedPublisherWelcomes.record(context, account, channel, welcome))
            DeletedPublisherGroupEpochs.clear(context, account, channel)
        }
        return joined
    }

    @Test
    fun `a replayed Welcome this device already joined is refused under reset suspicion`() {
        val provider = Provider(epoch = null)
        val old = byteArrayOf(1, 2, 3)
        assertEquals(3uL, land(provider, old, welcomeEpoch = 3uL))
        provider.epoch = 11uL
        // The DS raises a suspicion (refused reads) and replays the old one.
        ChannelSuspensions.suspend(context, account, channel)
        assertNull(land(provider, old, welcomeEpoch = 3uL))
        assertEquals("the group stays at its epoch", 11uL, provider.epoch)
    }

    @Test
    fun `a new Welcome of a channel founded again is accepted under suspicion`() {
        val provider = Provider(epoch = null)
        assertEquals(3uL, land(provider, byteArrayOf(1), welcomeEpoch = 3uL))
        provider.epoch = 11uL
        ChannelSuspensions.suspend(context, account, channel)
        // Decision A restarts the chain low: a new Welcome joins at epoch 1.
        assertEquals(1uL, land(provider, byteArrayOf(9, 9), welcomeEpoch = 1uL))
    }

    @Test
    fun `after an inactive group was deleted, a Welcome joining at an epoch not above it is refused`() {
        val provider = Provider(epoch = 12uL, active = false)
        // The entry deletes the inactive group and keeps its epoch.
        assertTrue(DeletedPublisherGroupEpochs.save(context, account, channel, provider.epoch!!))
        provider.epoch = null
        for (old in listOf(7uL, 12uL)) {
            assertNull(land(provider, byteArrayOf(old.toByte()), welcomeEpoch = old))
            assertNull("nothing stays joined", provider.epoch)
        }
        // A Welcome above the deleted epoch joins, and clears the floor.
        assertEquals(13uL, land(provider, byteArrayOf(13), welcomeEpoch = 13uL))
        assertNull(DeletedPublisherGroupEpochs.get(context, account, channel))
    }

    @Test
    fun `under reset suspicion a low epoch joins after a deletion (a re-founded chain)`() {
        assertTrue(DeletedPublisherGroupEpochs.save(context, account, channel, 12uL))
        ChannelSuspensions.suspend(context, account, channel)
        assertEquals(2uL, land(Provider(epoch = null), byteArrayOf(2), welcomeEpoch = 2uL))
    }

    @Test
    fun `the joined record is bounded, per channel, and goes with the channel and the account`() {
        repeat(JoinedPublisherWelcomes.MAX_PER_CHANNEL + 2) { i ->
            assertTrue(JoinedPublisherWelcomes.record(context, account, channel, byteArrayOf(i.toByte())))
        }
        assertFalse("the oldest are dropped", JoinedPublisherWelcomes.contains(context, account, channel, byteArrayOf(0)))
        assertTrue(JoinedPublisherWelcomes.contains(context, account, channel, byteArrayOf((JoinedPublisherWelcomes.MAX_PER_CHANNEL + 1).toByte())))
        assertFalse(JoinedPublisherWelcomes.contains(context, account, "ef".repeat(16), byteArrayOf(5)))
        JoinedPublisherWelcomes.clear(context, account, channel)
        assertFalse(JoinedPublisherWelcomes.contains(context, account, channel, byteArrayOf(5)))
        JoinedPublisherWelcomes.record(context, account, channel, byteArrayOf(5))
        DeletedPublisherGroupEpochs.save(context, account, channel, 4uL)
        JoinedPublisherWelcomes.forgetAccount(context, account)
        DeletedPublisherGroupEpochs.forgetAccount(context, account)
        assertFalse(JoinedPublisherWelcomes.contains(context, account, channel, byteArrayOf(5)))
        assertNull(DeletedPublisherGroupEpochs.get(context, account, channel))
        val steps = me.lakko.app.account.AccountLocalWipe.steps.map { it.name }
        assertTrue("channel-joined-welcomes" in steps)
        assertTrue("channel-deleted-group-epochs" in steps)
        assertTrue("channel-create-pending" in steps)
    }

    // ---------------------------------------------------------------- F5

    private class Rows : HandleClaimRows {
        val held = HashMap<String, String>()
        override fun get(key: String): String? = held[key]
        override fun put(key: String, value: String) {
            held[key] = value
        }
        override fun remove(key: String) {
            held.remove(key)
        }
    }

    @Test
    fun `a retry reuses the id only while that create is recorded as unfinished`() {
        var minted = 0
        val claims = ChannelHandleClaims(Rows()) { "id${++minted}" }
        val pending = { cid: String -> ChannelCreatePending.isPending(context, account, cid) }
        val first = newChannelCreateId(claims, account, "trip", pending)
        // The create starts (marked) and fails: the retry reuses the id.
        ChannelCreatePending.mark(context, account, first)
        assertEquals(first, newChannelCreateId(claims, account, "trip", pending))
        // The create finishes here, but the kept id outlived it (a crash
        // before it was forgotten): a retry never founds under it again.
        ChannelCreatePending.clear(context, account, first)
        val next = newChannelCreateId(claims, account, "trip", pending)
        assertNotEquals(first, next)
        assertEquals(next, claims.idFor(account, "trip"))
    }
}
