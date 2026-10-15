// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import kotlinx.coroutines.runBlocking
import me.lakko.app.StaleBindingException
import me.lakko.app.account.AccountStoreGate
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * G-51 on Android: a publish counter is handed out only once its floor is
 * stored. A refused write (the account's session ended, ADR-0077 decision 4)
 * ends the caller, so no post goes out on a floor a relaunch would not find.
 * Robolectric for a real `SharedPreferences` and `org.json`.
 */
@RunWith(RobolectricTestRunner::class)
class ChannelPublishCounterDurabilityTest {

    private val account = "CgoKCgoKCgoKCgoKCgoKCg=="
    private val chan = "34".repeat(16)
    private val publisherKeys = ChannelPublisherKeys(seed = ByteArray(32) { 3 }, signingKeySeed = ByteArray(32) { 4 })

    @Before
    fun setUp() = AccountStoreGate.resetForTest()

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    private fun publisherPrefs() =
        RuntimeEnvironment.getApplication().getSharedPreferences("test-publisher-durable", Context.MODE_PRIVATE)

    private fun state(): ChannelsUiState = ChannelsUiState(publisherPrefs = publisherPrefs())

    private fun storedRow(): String? =
        publisherPrefs().getString(ChannelsUiState.accountKey(ChannelsUiState.PUBLISHER_COUNTERS_PREF_KEY, account), null)

    @Test
    fun `the floor a counter is handed out under is stored before it is returned`() = runBlocking {
        val s = state()
        s.bind(account, ByteArray(64))
        s.setKeys(chan, publisherKeys)
        assertEquals(0uL, s.nextCounter(chan))
        assertTrue(storedRow()!!.contains("\"counter\":\"1\""))
        assertEquals(1uL, s.nextCounter(chan))
        assertTrue(storedRow()!!.contains("\"counter\":\"2\""))
    }

    @Test
    fun `a counter whose floor cannot be stored is not handed out`() = runBlocking {
        val s = state()
        s.bind(account, ByteArray(64))
        s.setKeys(chan, publisherKeys)
        assertEquals(0uL, s.nextCounter(chan))
        val before = storedRow()

        // The session ended (a sign-out seals the account's stores).
        AccountStoreGate.seal(account)
        try {
            s.nextCounter(chan)
            fail("a post must not go out on a floor that was not stored")
        } catch (_: StaleBindingException) {
            // expected
        }
        assertEquals(before, storedRow())

        // Signed in again: the next value is above everything handed out.
        AccountStoreGate.open(account)
        val relaunched = state()
        relaunched.bind(account, ByteArray(64))
        assertEquals(1uL, relaunched.nextCounter(chan))
    }
}
