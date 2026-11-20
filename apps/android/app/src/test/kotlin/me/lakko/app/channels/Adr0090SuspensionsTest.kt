// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import org.junit.After
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * ADR-0090 security pass — a suspected reset is recorded per account and
 * channel (the device stops posting, keeps the group) until an owner-signed
 * Welcome replaces the group; the account wipe removes the record.
 */
@RunWith(RobolectricTestRunner::class)
class Adr0090SuspensionsTest {
    private lateinit var context: Context
    private val account = "AAECAwQFBgcICQoLDA0ODw=="
    private val other = "EBESExQVFhcYGRobHB0eHw=="
    private val channel = "AB".repeat(16)

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
    }

    @After
    fun tearDown() {
        ChannelSuspensions.forgetAccount(context, account)
        ChannelSuspensions.forgetAccount(context, other)
    }

    @Test
    fun a_suspension_holds_per_account_until_cleared_or_wiped() {
        ChannelSuspensions.suspend(context, account, channel)
        assertTrue(ChannelSuspensions.isSuspended(context, account, channel.lowercase()))
        assertFalse(ChannelSuspensions.isSuspended(context, other, channel))
        ChannelSuspensions.clear(context, account, channel)
        assertFalse(ChannelSuspensions.isSuspended(context, account, channel))
        ChannelSuspensions.suspend(context, account, channel)
        ChannelSuspensions.forgetAccount(context, account)
        assertFalse(ChannelSuspensions.isSuspended(context, account, channel))
    }

    @Test
    fun a_false_alarm_lifts_after_two_matching_reads_and_a_lower_epoch_keeps_it() {
        // Suspended after refused reads; the reads succeed again.
        val first = suspensionProbe(previousOk = 0, readOk = true, latestEpoch = 7uL, groupEpoch = 7uL)
        assertFalse(first.lift)
        val second = suspensionProbe(first.consecutiveOk, readOk = true, latestEpoch = 7uL, groupEpoch = 7uL)
        assertTrue(second.lift)
        // A lower DS epoch (a chain this device does not hold) keeps it.
        val lower = suspensionProbe(previousOk = 1, readOk = true, latestEpoch = 3uL, groupEpoch = 7uL)
        assertFalse(lower.lift)
        assertTrue(lower.consecutiveOk == 0)
        // A refused read or no epoch named starts the count again.
        assertFalse(suspensionProbe(1, readOk = false, latestEpoch = 7uL, groupEpoch = 7uL).lift)
        assertFalse(suspensionProbe(1, readOk = true, latestEpoch = null, groupEpoch = 7uL).lift)
        // The record lists what is suspended, until it is lifted.
        ChannelSuspensions.suspend(context, account, channel)
        assertTrue(channel.lowercase() in ChannelSuspensions.all(context, account))
        ChannelSuspensions.clear(context, account, channel)
        assertFalse(channel.lowercase() in ChannelSuspensions.all(context, account))
    }

    @Test
    fun a_failed_join_puts_the_held_group_back() {
        // Against a fake provider: the host JVM loads no native core, so the
        // real PublisherGroup delete / join cannot run here.
        val provider = mutableSetOf("held-group", "chat-group")
        val failed = runCatching {
            replaceHeldThenJoin(
                snapshot = { provider.toSet() },
                delete = { provider.remove("held-group") },
                join = { error("the Welcome did not join") },
                restore = { before -> provider.clear(); provider.addAll(before) },
            )
        }
        assertTrue(failed.isFailure)
        assertTrue("held-group" in provider)
        assertTrue("chat-group" in provider)
        // A join that lands replaces the held group.
        val joined = replaceHeldThenJoin(
            snapshot = { provider.toSet() },
            delete = { provider.remove("held-group") },
            join = { provider.add("joined-group"); 7uL },
            restore = { before -> provider.clear(); provider.addAll(before) },
        )
        assertTrue(joined == 7uL)
        assertFalse("held-group" in provider)
        assertTrue("joined-group" in provider)
    }
}
