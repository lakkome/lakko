// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

/**
 * The channel subscriber keypairs, publisher signing seeds and publish
 * counters belong to the account (2026-09-30: they lived under one
 * device-wide row each, so a second account on the same phone reused the
 * first account's subscriber keypair — the publisher would see one key for
 * two accounts — and could sign under its publisher seed). Robolectric for a
 * real `SharedPreferences` + `org.json`.
 */
@RunWith(RobolectricTestRunner::class)
class ChannelKeysAccountScopeTest {

    private val a = "CgoKCgoKCgoKCgoKCgoKCg=="
    private val b = "CwsLCwsLCwsLCwsLCwsLCw=="
    private val chan = "12".repeat(16)

    private fun state(): ChannelsUiState {
        val context = RuntimeEnvironment.getApplication()
        return ChannelsUiState(
            prefs = context.getSharedPreferences("test-subscriber", Context.MODE_PRIVATE),
            publisherPrefs = context.getSharedPreferences("test-publisher", Context.MODE_PRIVATE),
        )
    }

    private val subscriberKeys = ChannelSubscriberKeys(ByteArray(32) { 1 }, ByteArray(32) { 2 })
    private val publisherKeys = ChannelPublisherKeys(seed = ByteArray(32) { 3 }, signingKeySeed = ByteArray(32) { 4 })

    @Test
    fun `a second account never sees the first account's channel keys`() = runBlocking {
        val first = state()
        first.bind(a, ByteArray(64))
        first.setSubscriberKeys(chan, subscriberKeys)
        first.setKeys(chan, publisherKeys)
        assertEquals(0uL, first.nextCounter(chan))
        assertEquals(1uL, first.nextCounter(chan))

        first.bind(b, ByteArray(64))
        assertTrue(first.subscriberKeysByChannel.value.isEmpty())
        assertNull(first.persistedPublisherSigningSeed(chan))
        assertTrue(first.counterByChannel.value.isEmpty())

        // A relaunch (a fresh state) restores exactly the bound account's rows.
        val relaunched = state()
        relaunched.bind(a, ByteArray(64))
        assertArrayEquals(subscriberKeys.publicKey, relaunched.subscriberKeysByChannel.value[chan]?.publicKey)
        assertArrayEquals(publisherKeys.signingKeySeed, relaunched.persistedPublisherSigningSeed(chan))
        assertEquals(2uL, relaunched.nextCounter(chan))

        val other = state()
        other.bind(b, ByteArray(64))
        assertTrue(other.subscriberKeysByChannel.value.isEmpty())
        assertNull(other.persistedPublisherSigningSeed(chan))
    }

    @Test
    fun `logout keeps the keys and forget device drops only that account's`() = runBlocking {
        val s = state()
        s.bind(a, ByteArray(64))
        s.setSubscriberKeys(chan, subscriberKeys)
        s.setKeys(chan, publisherKeys)
        s.nextCounter(chan)
        s.bind(b, ByteArray(64))
        s.setSubscriberKeys(chan, subscriberKeys)
        s.setKeys(chan, publisherKeys)

        s.unbind()
        assertTrue(s.subscriberKeysByChannel.value.isEmpty())
        s.bind(a, ByteArray(64))
        assertTrue(s.subscriberKeysByChannel.value.containsKey(chan))

        s.forgetAccount()
        val again = state()
        again.bind(a, ByteArray(64))
        assertFalse(again.subscriberKeysByChannel.value.containsKey(chan))
        assertNull(again.persistedPublisherSigningSeed(chan))
        // The nonce floor is kept, like removeChannel keeps it.
        assertEquals(1uL, again.nextCounter(chan))

        val stillB = state()
        stillB.bind(b, ByteArray(64))
        assertTrue(stillB.subscriberKeysByChannel.value.containsKey(chan))
        assertArrayEquals(publisherKeys.signingKeySeed, stillB.persistedPublisherSigningSeed(chan))
    }

    @Test
    fun `nothing is written before an account is bound`() {
        val s = state()
        s.setSubscriberKeys(chan, subscriberKeys)
        s.nextCounter(chan)
        val context = RuntimeEnvironment.getApplication()
        assertTrue(context.getSharedPreferences("test-subscriber", Context.MODE_PRIVATE).all.isEmpty())
        assertTrue(context.getSharedPreferences("test-publisher", Context.MODE_PRIVATE).all.isEmpty())
    }

    @Test
    fun `counter floors merge to the higher value`() {
        val merged = ChannelsUiState.mergeCounterFloors(mapOf("x" to 5uL, "y" to 1uL), mapOf("x" to 3uL, "z" to 7uL))
        assertEquals(mapOf("x" to 5uL, "y" to 1uL, "z" to 7uL), merged)
    }
}
