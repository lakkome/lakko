// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import me.lakko.app.push.looksLikePublisherChannelEnvelope
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0087 section 15 (2026-11-08) — the owner hands a new publisher the
 * channel's earlier seeds, the publisher keeps only epochs it lacks, and the
 * owner rotates on a current publisher's rotate request.
 */
class EarlierSeedsAndRotateRequestTest {

    private fun key(b: Int) = ByteArray(32) { b.toByte() }
    private val channel = ByteArray(16) { 1 }
    private val other = ByteArray(16) { 2 }

    // --- sender ---------------------------------------------------------------

    @Test
    fun the_newest_epochs_not_yet_sent_go_ascending() {
        assertEquals(listOf(0u, 1u, 2u), epochSeedEpochsToSend(setOf(2u, 0u, 1u), emptySet()))
        assertEquals(listOf(0u, 2u), epochSeedEpochsToSend(setOf(0u, 1u, 2u), setOf(epochSeedMark(1u))))
    }

    @Test
    fun at_most_the_newest_max_epochs_are_handed_over() {
        val held = (0u..99u).toSet()
        val sent = epochSeedEpochsToSend(held, emptySet(), max = 64)
        assertEquals(64, sent.size)
        assertEquals(36u, sent.first())
        assertEquals(99u, sent.last())
    }

    @Test
    fun everything_sent_owes_nothing() {
        val held = setOf(3u, 4u)
        assertTrue(epochSeedEpochsToSend(held, held.map(::epochSeedMark).toSet()).isEmpty())
    }

    // --- receiver ---------------------------------------------------------------

    @Test
    fun a_received_seed_is_added_only_for_an_epoch_with_none_held() {
        val keep = epochSeedsToKeep(listOf(1u to key(1), 2u to key(2), 3u to key(3)), held = setOf(2u))
        assertEquals(listOf(1u, 3u), keep.map { it.first })
        assertArrayEquals(key(1), keep[0].second)
    }

    @Test
    fun a_repeated_epoch_in_one_envelope_keeps_the_first() {
        val keep = epochSeedsToKeep(listOf(1u to key(1), 1u to key(9)), held = emptySet())
        assertEquals(1, keep.size)
        assertArrayEquals(key(1), keep[0].second)
    }

    // --- rotate request -----------------------------------------------------------

    @Test
    fun the_owner_honours_a_current_publishers_request_for_this_channel() {
        val publisher = key(7)
        val owner = key(1)
        assertTrue(honoursRotateRequest(true, channel, channel, publisher, owner, listOf(owner, publisher)))
    }

    @Test
    fun a_request_from_anyone_else_or_for_another_channel_is_dropped() {
        val publisher = key(7)
        val owner = key(1)
        assertFalse("a removed publisher", honoursRotateRequest(true, channel, channel, publisher, owner, listOf(owner)))
        assertFalse("another channel", honoursRotateRequest(true, other, channel, publisher, owner, listOf(owner, publisher)))
        assertFalse("its own echo", honoursRotateRequest(true, channel, channel, owner, owner, listOf(owner, publisher)))
        assertFalse("a publisher device never acts", honoursRotateRequest(false, channel, channel, publisher, key(7), listOf(owner, publisher)))
    }

    // --- background suppression ---------------------------------------------------

    @Test
    fun the_two_envelopes_are_suppressed_by_their_domain() {
        val seeds = "lakko/channel-epoch-seeds/v1\u0000".toByteArray(Charsets.US_ASCII) + ByteArray(134)
        val rotate = "lakko/channel-rotate-request/v1\u0000".toByteArray(Charsets.US_ASCII) + ByteArray(17)
        assertTrue(looksLikePublisherChannelEnvelope(seeds))
        assertTrue(looksLikePublisherChannelEnvelope(rotate))
        assertFalse(looksLikePublisherChannelEnvelope("lakko/channel-rotate-request/v1".toByteArray()))
    }

    // --- the pending request's sent time ------------------------------------------

    @Test
    fun ask_again_replaces_the_pending_record_and_restarts_the_seven_days() {
        val first = PendingPublisherRequests.Request("c", "g", "k", "n", requestedAtMs = 10L)
        val again = first.copy(requestedAtMs = 99L)
        val list = PendingPublisherRequests.withRecord(listOf(first), again)
        assertEquals(1, list.size)
        assertEquals(99L, list.single().requestedAtMs)
        assertEquals(99L, PendingPublisherRequests.decode(PendingPublisherRequests.encode(list)).single().requestedAtMs)
    }
}
