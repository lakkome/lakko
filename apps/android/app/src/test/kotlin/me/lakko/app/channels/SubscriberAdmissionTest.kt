// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side tests for the [SubscriberAdmission] pass against a fake port —
 * no UniFFI native library, no network. Pins: every key not yet let in at
 * the current epoch is sealed once, a failure is retried by the next pass,
 * an epoch change lets everyone in again, a channel without publisher keys
 * is left alone, and a second pass while one runs returns at once.
 */
class SubscriberAdmissionTest {

    private class FakePort : SubscriberAdmission.Port {
        var owned = mutableSetOf("c1")
        var subscribers = listOf("aa", "bb")
        var epoch = 3u
        val failing = mutableSetOf<String>()
        val delivered = mutableListOf<Triple<String, String, UInt>>()
        var listGate: CompletableDeferred<Unit>? = null
        var listCalls = 0

        override fun canPost(channelIdHex: String) = channelIdHex in owned

        override suspend fun listSubscribers(channelIdHex: String): List<String> {
            listCalls += 1
            listGate?.await()
            return subscribers
        }

        override suspend fun currentEpoch(channelIdHex: String) = epoch

        override suspend fun sealAndDeliver(channelIdHex: String, pubkeyHex: String, epoch: UInt) {
            if (pubkeyHex in failing) throw IllegalStateException("server said no")
            delivered += Triple(channelIdHex, pubkeyHex, epoch)
        }
    }

    @Test
    fun a_pass_lets_every_new_key_in_once() = runTest {
        val port = FakePort()
        val admission = SubscriberAdmission(port)
        admission.admitNewSubscribers("c1")
        assertEquals(listOf(Triple("c1", "aa", 3u), Triple("c1", "bb", 3u)), port.delivered)
        val entry = admission.state.value["c1"]!!
        assertEquals(listOf("aa", "bb"), entry.subscribers)
        assertEquals(SubscriberAdmission.Status.Ready, entry.statusByKey["aa"]!!.status)

        // A second pass with one newcomer delivers to the newcomer only.
        port.subscribers = listOf("aa", "bb", "cc")
        admission.admitNewSubscribers("c1")
        assertEquals(Triple("c1", "cc", 3u), port.delivered.last())
        assertEquals(3, port.delivered.size)
    }

    @Test
    fun a_failed_key_is_recorded_and_retried_by_the_next_pass() = runTest {
        val port = FakePort()
        port.failing += "bb"
        val admission = SubscriberAdmission(port)
        admission.admitNewSubscribers("c1")
        assertEquals(SubscriberAdmission.Status.Failed, admission.state.value["c1"]!!.statusByKey["bb"]!!.status)

        port.failing.clear()
        admission.admitNewSubscribers("c1")
        assertEquals(Triple("c1", "bb", 3u), port.delivered.last())
        assertEquals(SubscriberAdmission.Status.Ready, admission.state.value["c1"]!!.statusByKey["bb"]!!.status)
    }

    @Test
    fun a_new_epoch_lets_everyone_in_again() = runTest {
        val port = FakePort()
        val admission = SubscriberAdmission(port)
        admission.admitNewSubscribers("c1")
        port.epoch = 4u
        admission.admitNewSubscribers("c1")
        assertEquals(4, port.delivered.size)
        assertTrue(port.delivered.drop(2).all { it.third == 4u })
    }

    @Test
    fun a_channel_without_publisher_keys_is_left_alone() = runTest {
        val port = FakePort()
        val admission = SubscriberAdmission(port)
        admission.admitAll(listOf("c2"))
        assertEquals(0, port.listCalls)
        assertNull(admission.state.value["c2"])
        assertFalse(admission.admit("c2", "aa"))
    }

    @Test
    fun a_second_pass_while_one_runs_returns_at_once() = runTest {
        val port = FakePort()
        val gate = CompletableDeferred<Unit>()
        port.listGate = gate
        val admission = SubscriberAdmission(port)
        val first = async { admission.admitNewSubscribers("c1") }
        testScheduler.runCurrent()
        admission.admitNewSubscribers("c1")
        assertEquals(1, port.listCalls)
        gate.complete(Unit)
        first.await()
        assertEquals(2, port.delivered.size)
    }

    @Test
    fun the_manual_retry_admits_one_key() = runTest {
        val port = FakePort()
        val admission = SubscriberAdmission(port)
        assertTrue(admission.admit("c1", "bb"))
        assertEquals(listOf(Triple("c1", "bb", 3u)), port.delivered)
        admission.clear()
        assertTrue(admission.state.value.isEmpty())
    }
}
