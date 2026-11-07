// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.push.looksLikePublisherChannelEnvelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0087 phase B round 4: the idle receive cadence and the background peek
 * that keeps the step-down and read-key envelopes off every preview.
 */
class Adr0087PhaseBRound4Test {

    @Test
    fun the_sweep_is_short_only_in_a_wake_window_or_a_call() {
        assertEquals(ReceiveCadence.ACTIVE_MS, ReceiveCadence.intervalMs(true, false, true))
        assertEquals(ReceiveCadence.ACTIVE_MS, ReceiveCadence.intervalMs(false, true, false))
        assertEquals(ReceiveCadence.IDLE_PUSH_MS, ReceiveCadence.intervalMs(false, false, true))
        assertEquals(ReceiveCadence.IDLE_NO_PUSH_MS, ReceiveCadence.intervalMs(false, false, false))
        assertEquals(4_000L, ReceiveCadence.ACTIVE_MS)
        assertEquals(120_000L, ReceiveCadence.IDLE_PUSH_MS)
        assertEquals(30_000L, ReceiveCadence.IDLE_NO_PUSH_MS)
    }

    @Test
    fun a_wake_opens_the_window_for_one_minute() {
        val at = 1_000_000L
        ReceiveCadence.noteWake(at)
        assertTrue(ReceiveCadence.wakeWindowOpen(at))
        assertTrue(ReceiveCadence.wakeWindowOpen(at + ReceiveCadence.WAKE_WINDOW_MS - 1))
        assertFalse(ReceiveCadence.wakeWindowOpen(at + ReceiveCadence.WAKE_WINDOW_MS))
        assertFalse(ReceiveCadence.wakeWindowOpen(at - 1))
    }

    @Test
    fun step_down_and_read_key_envelopes_are_recognised_by_their_domain() {
        val stepDown = "lakko/publisher-step-down/v1\u0000".toByteArray(Charsets.US_ASCII) + ByteArray(17)
        val readKey = "lakko/channel-read-key/v1\u0000".toByteArray(Charsets.US_ASCII) + ByteArray(40)
        assertTrue(looksLikePublisherChannelEnvelope(stepDown))
        assertTrue(looksLikePublisherChannelEnvelope(readKey))
        // The domain without its NUL is not the envelope.
        assertFalse(looksLikePublisherChannelEnvelope("lakko/publisher-step-down/v1".toByteArray()))
        assertFalse(looksLikePublisherChannelEnvelope("{\"v\":1}".toByteArray()))
        assertFalse(looksLikePublisherChannelEnvelope(ByteArray(0)))
    }
}
