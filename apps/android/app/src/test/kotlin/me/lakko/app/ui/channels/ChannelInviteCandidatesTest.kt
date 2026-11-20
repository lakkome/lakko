// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Sections 14.1 and 15.2 — `Invite people` and `Add publisher` do not offer
 * a person known to be in the channel. Base walk 2026-11-13: the owner's
 * Invite people listed a publisher of the channel.
 */
class ChannelInviteCandidatesTest {
    private val publisher = ByteArray(32) { 0x11 }
    private val owner = ByteArray(32) { 0x22 }
    private val anna = ByteArray(32) { 0x33 }
    private val subscriber = ByteArray(32) { 0x44 }

    private fun hex(b: ByteArray): String = b.joinToString("") { "%02x".format(it) }

    @Test
    fun aPublisherIsNotOffered() {
        val inChannel = channelInviteExclusions(listOf(owner, publisher), emptySet())
        val left = withoutInChannel(listOf(publisher, anna), { it }, inChannel)
        assertEquals(listOf(hex(anna)), left.map(::hex))
    }

    @Test
    fun aSubscriberKnownByTheirCardIsNotOffered() {
        val inChannel = channelInviteExclusions(listOf(owner), setOf(hex(subscriber)))
        val left = withoutInChannel(listOf(subscriber, anna, owner), { it }, inChannel)
        assertEquals(listOf(hex(anna)), left.map(::hex))
    }

    @Test
    fun everyContactIsOfferedWhenNobodyIsKnownToBeIn() {
        val all = listOf(anna, subscriber)
        assertEquals(all, withoutInChannel(all, { it }, emptySet()))
    }
}
