// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * Part B (2026-11-14) — the owner device's re-admission Welcome can reach the
 * account mailbox before this device applied the commit removing its leaf.
 * Rule 1 ignores it then (the held group is still active), so it is held and
 * tried again by the publisher pass until the removal lands. The core runs
 * against a fake here, in the order the pass runs it (the host JVM loads no
 * native core).
 */
@RunWith(RobolectricTestRunner::class)
class HeldPublisherWelcomesTest {
    private lateinit var context: Context
    private val account = "AAECAwQFBgcICQoLDA0ODw=="
    private val channel = "ab".repeat(16)

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
    }

    @After
    fun tearDown() = HeldPublisherWelcomes.forgetAccount(context, account)

    /** The held group of the scenario: active until the removal is applied. */
    private class FakeGroup(var active: Boolean, var epoch: ULong)

    /** What `joinReplacingHeld` does with a verified owner-device Welcome at
     *  [welcomeEpoch]: hold it while the group waits for its removal, else
     *  replace it under the epoch rule. Returns the joined epoch or null. */
    private fun offer(group: FakeGroup, welcome: ByteArray, welcomeEpoch: ULong, nowMs: Long): ULong? {
        if (!heldGroupReplaceable(group.active, resetSuspected = false)) {
            assertTrue(HeldPublisherWelcomes.hold(context, account, channel, welcome, nowMs))
            return null
        }
        if (!joinedGroupReplacesHeld(welcomeEpoch, group.epoch, group.active)) return null
        group.active = true
        group.epoch = welcomeEpoch
        HeldPublisherWelcomes.clear(context, account, channel)
        return welcomeEpoch
    }

    /** One publisher pass's retry over the held Welcomes. */
    private fun retry(group: FakeGroup, epochOf: (ByteArray) -> ULong, nowMs: Long): Boolean {
        for (held in HeldPublisherWelcomes.heldFor(context, account, channel)) {
            val waiting = !heldGroupReplaceable(group.active, resetSuspected = false)
            if (waiting) {
                if (heldWelcomeStep(false, true, held.heldAtMs, nowMs) == HeldWelcomeStep.DROP) {
                    HeldPublisherWelcomes.drop(context, account, channel, held.welcomeB64)
                }
                continue
            }
            val bytes = java.util.Base64.getDecoder().decode(held.welcomeB64)
            val joined = offer(group, bytes, epochOf(bytes), nowMs) != null
            when (heldWelcomeStep(joined, stillWaiting = false, held.heldAtMs, nowMs)) {
                HeldWelcomeStep.JOINED_CLEAR_CHANNEL -> {
                    HeldPublisherWelcomes.clear(context, account, channel)
                    return true
                }
                HeldWelcomeStep.KEEP -> Unit
                HeldWelcomeStep.DROP -> HeldPublisherWelcomes.drop(context, account, channel, held.welcomeB64)
            }
        }
        return false
    }

    @Test
    fun `Welcome first, removal second - the device ends up joined and publishing`() {
        val group = FakeGroup(active = true, epoch = 5uL)
        val welcome = byteArrayOf(7, 7, 7)
        val epochOf = { _: ByteArray -> 7uL }
        // The mailbox delivers the re-admission Welcome first: held, not lost.
        assertEquals(null, offer(group, welcome, 7uL, nowMs = 1_000))
        assertEquals(1, HeldPublisherWelcomes.heldFor(context, account, channel).size)
        // A pass before the removal is read: still held.
        assertEquals(false, retry(group, epochOf, nowMs = 2_000))
        assertEquals(1, HeldPublisherWelcomes.heldFor(context, account, channel).size)
        // The journal poll applies the removal: the group is inactive at 6.
        group.active = false
        group.epoch = 6uL
        // The next pass replaces it with the held Welcome: publishing at 7.
        assertEquals(true, retry(group, epochOf, nowMs = 3_000))
        assertEquals(true, group.active)
        assertEquals(7uL, group.epoch)
        assertTrue(HeldPublisherWelcomes.heldFor(context, account, channel).isEmpty())
    }

    @Test
    fun `an old held Welcome is dropped once the rule runs and refuses it`() {
        val group = FakeGroup(active = true, epoch = 5uL)
        offer(group, byteArrayOf(1), 4uL, nowMs = 1_000)
        group.active = false
        group.epoch = 6uL
        assertEquals(false, retry(group, { 4uL }, nowMs = 2_000))
        assertTrue(HeldPublisherWelcomes.heldFor(context, account, channel).isEmpty())
        assertEquals(false, group.active)
    }

    @Test
    fun `at most three per channel, newest kept, the same Welcome once, too old dropped`() {
        val group = FakeGroup(active = true, epoch = 5uL)
        for (i in 1..4) offer(group, byteArrayOf(i.toByte()), 7uL, nowMs = i * 1_000L)
        offer(group, byteArrayOf(4), 7uL, nowMs = 9_000)
        val held = HeldPublisherWelcomes.heldFor(context, account, channel)
        assertEquals(HeldPublisherWelcomes.MAX_PER_CHANNEL, held.size)
        assertEquals(listOf(4_000L, 3_000L, 2_000L), held.map { it.heldAtMs })
        // Still active after the age bound: dropped.
        retry(group, { 7uL }, nowMs = 2_000L + HeldPublisherWelcomes.MAX_AGE_MS + 1)
        assertEquals(listOf(4_000L, 3_000L), HeldPublisherWelcomes.heldFor(context, account, channel).map { it.heldAtMs })
        assertEquals(HeldWelcomeStep.KEEP, heldWelcomeStep(false, true, heldAtMs = 0, nowMs = HeldPublisherWelcomes.MAX_AGE_MS))
        // A clock moved backwards keeps it.
        assertEquals(HeldWelcomeStep.KEEP, heldWelcomeStep(false, true, heldAtMs = 5_000, nowMs = 1_000))
        assertEquals(false, heldWelcomeExpired(heldAtMs = 5_000, nowMs = 1_000))
    }

    @Test
    fun `the account wipe removes the held Welcomes`() {
        HeldPublisherWelcomes.hold(context, account, channel, byteArrayOf(1), 1_000)
        HeldPublisherWelcomes.forgetAccount(context, account)
        assertTrue(HeldPublisherWelcomes.channels(context, account).isEmpty())
        assertTrue(me.lakko.app.account.AccountLocalWipe.steps.any { it.name == "channel-held-welcomes" })
    }
}
