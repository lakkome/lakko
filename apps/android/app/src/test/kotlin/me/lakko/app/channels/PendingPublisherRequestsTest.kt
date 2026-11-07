// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The durable record of a publisher request (ADR-0087 section 15, ADR-0045
 * decision 2: no deadline), and how a `410 channel-deleted` names its
 * channel.
 *
 * Seen on a live walk: the owner approved minutes after the Accept page had
 * stopped waiting, and the Welcome was never joined.
 */
class PendingPublisherRequestsTest {
    private fun request(channel: String, at: Long = 1L) = PendingPublisherRequests.Request(
        channelIdHex = channel,
        groupIdHex = "ab".repeat(32),
        joinerKeyB64 = "a2V5",
        name = "Pixel Pub Walk",
        requestedAtMs = at,
    )

    @Test
    fun `a record survives its stored form`() {
        val list = listOf(request("11".repeat(16)), request("22".repeat(16)))
        assertEquals(list, PendingPublisherRequests.decode(PendingPublisherRequests.encode(list)))
    }

    @Test
    fun `an unreadable row reads as none`() {
        assertEquals(emptyList<PendingPublisherRequests.Request>(), PendingPublisherRequests.decode("not json"))
        assertEquals(emptyList<PendingPublisherRequests.Request>(), PendingPublisherRequests.decode(null))
    }

    @Test
    fun `a second request for a channel replaces the first`() {
        val first = request("11".repeat(16), at = 1L)
        val other = request("22".repeat(16), at = 2L)
        val again = request("11".repeat(16), at = 3L)
        val list = PendingPublisherRequests.withRecord(listOf(first, other), again)
        assertEquals(listOf(other, again), list)
    }

    @Test
    fun `dropping a channel keeps the others`() {
        val a = request("11".repeat(16))
        val b = request("22".repeat(16))
        assertEquals(listOf(b), PendingPublisherRequests.without(listOf(a, b), a.channelIdHex))
    }

    @Test
    fun `a channels route names its channel`() {
        val id = "fa7c37d205a487618b4ca1722bc604c9"
        assertEquals(id, ChannelsClient.channelIdInPath("/v1/channels/$id/epoch"))
        assertEquals(id, ChannelsClient.channelIdInPath("/native/channels/v1/channels/${id.uppercase()}"))
        assertNull(ChannelsClient.channelIdInPath("/v1/channels"))
        assertNull(ChannelsClient.channelIdInPath("/v1/channels/not-an-id/epoch"))
    }
}
