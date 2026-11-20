// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * ADR-0090 — the device-local records the publisher loop keeps: the
 * re-founding owed after this device founded the account's DeviceGroup alone
 * (decision A), and the hand-over owed again to an account whose device was
 * admitted (section 6).
 */
@RunWith(RobolectricTestRunner::class)
class Adr0090PrefsTest {
    private lateinit var context: Context
    private val account = "AAECAwQFBgcICQoLDA0ODw=="
    private val channel = "11".repeat(16)

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        me.lakko.app.account.AccountStoreGate.resetForTest()
        SubscriberAdmission.appContext = context
    }

    @After
    fun tearDown() {
        me.lakko.app.account.AccountStoreGate.resetForTest()
        ChannelRefounding.forgetAccount(context, account)
        ChannelAdmissionSeedShare.forgetAccount(context, account)
        SubscriberAdmission.appContext = null
    }

    @Test
    fun a_founding_alone_owes_a_refounding_per_channel_until_finished() {
        assertFalse(ChannelRefounding.isOwed(context, account))
        ChannelRefounding.markFoundedAlone(context, account)
        assertTrue(ChannelRefounding.isOwed(context, account))
        assertFalse(ChannelRefounding.isDone(context, account, channel))
        ChannelRefounding.markDone(context, account, channel)
        assertTrue(ChannelRefounding.isDone(context, account, channel))
        ChannelRefounding.finish(context, account)
        assertFalse(ChannelRefounding.isOwed(context, account))
        assertFalse(ChannelRefounding.isDone(context, account, channel))
    }

    @Test
    fun an_admission_to_the_own_account_makes_it_owed_the_hand_over() {
        val ownKey = ByteArray(32) { 7 }
        val keyHex = ownKey.joinToString("") { "%02x".format(it) }
        val prefs = context.getSharedPreferences("lakko.channels.copublishers", Context.MODE_PRIVATE)
        // An earlier hand-over to this account was marked sent.
        prefs.edit().putStringSet("sent|$account|$channel|$keyHex", setOf("read-key", "0:aabbcc")).commit()
        // Nothing recorded for this account yet: nothing owed.
        assertTrue(ChannelAdmissionSeedShare.fillPublisherAccountIds(context, account, channel).isEmpty())
        ChannelAdmissionSeedShare.owedAgain(account, channel, ownKey, ownKey)
        val reachable = ChannelAdmissionSeedShare.fillPublisherAccountIds(context, account, channel)
        assertEquals(1, reachable.size)
        // The record names this account's own id (hex of the base64 id).
        assertEquals("000102030405060708090a0b0c0d0e0f", reachable.values.single())
        // The read key and the admission seeds are owed to it again: the
        // sent marks are gone.
        assertFalse(prefs.contains("sent|$account|$channel|$keyHex"))
    }
}
