// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The chat of a group call (`docs/ux/calls.md` section 4.1.4, `CallChat.kt`):
 * numbering, sending, holding a message until its sender is named, showing
 * each message once, `Send again` with the same bytes, and the end of the
 * call. The core's compose / verify are stood in for, so no UniFFI `.so` is
 * needed.
 */
class CallChatTest {
    /** A wire is `peer|seq|text`; a "signer key" is the peer id's bytes. */
    private class Harness {
        val sent = mutableListOf<ByteArray>()
        val shown = mutableListOf<Triple<String, ULong, String>>()
        var rejected = 0
        var self: String? = "me"
        var canCompose = true
        var socketUp = true
        var composed = 0
        val channel = CallChatChannel(
            CallChatChannelDeps(
                compose = { self, seq, text ->
                    if (canCompose) {
                        composed += 1
                        "$self|$seq|$text".toByteArray()
                    } else {
                        null
                    }
                },
                verify = { from, key, bytes ->
                    val parts = String(bytes).split("|", limit = 3)
                    // Signed by the leaf the announcement named, for that peer.
                    if (parts.size != 3 || parts[0] != from || String(key) != from) {
                        null
                    } else {
                        VerifiedCallChatView(seq = parts[1].toULong(), text = parts[2])
                    }
                },
                sendSealed = { plaintext ->
                    if (socketUp) {
                        sent += plaintext
                        true
                    } else {
                        false
                    }
                },
                selfPeerId = { self },
                onMessage = { peer, seq, text -> shown += Triple(peer, seq, text) },
                onRejected = { rejected += 1 },
            ),
        )

        fun wire(peer: String, seq: Int, text: String) = "$peer|$seq|$text".toByteArray()

        fun identify(peer: String) = channel.onPeerIdentified(peer, peer.toByteArray())
    }

    @Test
    fun `own messages are numbered from 1 and sent at once`() {
        val h = Harness()
        assertEquals(CallChatSendResult(1uL, sent = true), h.channel.send("hi"))
        assertEquals(CallChatSendResult(2uL, sent = true), h.channel.send("there"))
        assertEquals(listOf("me|1|hi", "me|2|there"), h.sent.map { String(it) })
    }

    @Test
    fun `a message that could not be sent is sent again with the same bytes and number`() {
        val h = Harness()
        h.socketUp = false
        val result = h.channel.send("hello")
        assertEquals(CallChatSendResult(1uL, sent = false), result)
        assertTrue(h.sent.isEmpty())
        // A later message takes the next number.
        assertEquals(2uL, h.channel.send("next").seq)
        h.socketUp = true
        assertTrue(h.channel.resend(1uL))
        assertEquals("me|1|hello", String(h.sent.single()))
        // The bytes were made once: Send again does not sign anew.
        assertEquals(2, h.composed)
    }

    @Test
    fun `a message written before the device could sign is signed when sent again`() {
        val h = Harness()
        h.self = null
        assertFalse(h.channel.send("early").sent)
        h.self = "me"
        assertTrue(h.channel.resend(1uL))
        assertEquals("me|1|early", String(h.sent.single()))
        val first = h.sent.single()
        h.channel.resend(1uL)
        assertArrayEquals(first, h.sent.last())
    }

    @Test
    fun `send again of an unknown number does nothing`() {
        val h = Harness()
        assertFalse(h.channel.resend(7uL))
        assertTrue(h.sent.isEmpty())
    }

    @Test
    fun `a verified message is shown once per peer and number`() {
        val h = Harness()
        h.identify("p1")
        h.channel.onReceived("p1", h.wire("p1", 1, "hello"))
        // A relay frame re-sent after a reconnect.
        h.channel.onReceived("p1", h.wire("p1", 1, "hello"))
        h.channel.onReceived("p1", h.wire("p1", 2, "again"))
        assertEquals(
            listOf(Triple("p1", 1uL, "hello"), Triple("p1", 2uL, "again")),
            h.shown,
        )
    }

    @Test
    fun `the same number from two peers is two messages`() {
        val h = Harness()
        h.identify("p1")
        h.identify("p2")
        h.channel.onReceived("p1", h.wire("p1", 1, "a"))
        h.channel.onReceived("p2", h.wire("p2", 1, "b"))
        assertEquals(2, h.shown.size)
    }

    @Test
    fun `a message from a peer not yet named is held and shown in arrival order once named`() {
        val h = Harness()
        h.channel.onReceived("p1", h.wire("p1", 1, "first"))
        h.channel.onReceived("p1", h.wire("p1", 2, "second"))
        assertTrue(h.shown.isEmpty())
        h.identify("p1")
        assertEquals(listOf("first", "second"), h.shown.map { it.third })
    }

    @Test
    fun `at most 50 messages are held per peer, the oldest dropped`() {
        val h = Harness()
        repeat(CallChatChannel.MAX_PENDING_PER_PEER + 5) { i ->
            h.channel.onReceived("p1", h.wire("p1", i + 1, "m${i + 1}"))
        }
        h.identify("p1")
        assertEquals(CallChatChannel.MAX_PENDING_PER_PEER, h.shown.size)
        assertEquals(6uL, h.shown.first().second)
        assertEquals(55uL, h.shown.last().second)
    }

    @Test
    fun `a message held for a peer that leaves unnamed is never shown`() {
        val h = Harness()
        h.channel.onReceived("p1", h.wire("p1", 1, "ghost"))
        h.channel.onPeerLeft("p1")
        h.identify("p1")
        assertTrue(h.shown.isEmpty())
    }

    @Test
    fun `a message that does not verify is dropped and counted once`() {
        val h = Harness()
        h.identify("p1")
        // Signed as another peer.
        h.channel.onReceived("p1", h.wire("p2", 1, "forged"))
        assertTrue(h.shown.isEmpty())
        assertEquals(1, h.rejected)
    }

    @Test
    fun `a message is not sent again when the device announces itself again`() {
        val h = Harness()
        h.channel.send("once")
        // The session re-announces; nothing in the chat channel reacts.
        h.identify("p1")
        assertEquals(1, h.sent.size)
    }

    @Test
    fun `the end of the call clears everything and numbering starts over`() {
        val h = Harness()
        h.identify("p1")
        h.channel.onReceived("p1", h.wire("p1", 1, "hello"))
        h.channel.onReceived("p2", h.wire("p2", 1, "held"))
        h.channel.send("mine")
        h.channel.clear()
        assertFalse(h.channel.resend(1uL))
        // Peers must be named again; what was held is gone.
        h.channel.onReceived("p1", h.wire("p1", 1, "hello"))
        h.identify("p2")
        assertEquals(1, h.shown.size)
        assertEquals(1uL, h.channel.send("new call").seq)
    }
}
