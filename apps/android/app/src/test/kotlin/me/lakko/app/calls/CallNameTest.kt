// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The name a participant types for one call (`docs/ux/calls.md` section 4.1,
 * `CallName.kt`): when this device sends its own, which received names it
 * takes, and the order a tile's name is chosen in. The compose / verify of
 * the core are stood in for, so no UniFFI `.so` is needed.
 */
class CallNameTest {
    /** A wire is `peer|seq|name`; a "signer key" is the peer id's bytes. */
    private class Harness {
        val sent = mutableListOf<String>()
        val names = mutableListOf<Pair<String, String?>>()
        var self: String? = "me"
        var canCompose = true
        val channel = CallNameChannel(
            CallNameChannelDeps(
                compose = { self, seq, name -> if (canCompose) "$self|$seq|$name".toByteArray() else null },
                verify = { from, key, bytes ->
                    val parts = String(bytes).split("|")
                    // Signed by the leaf the announcement named, for that peer.
                    if (parts.size != 3 || parts[0] != from || String(key) != from) {
                        null
                    } else {
                        VerifiedCallNameView(seq = parts[1].toULong(), name = parts[2].ifEmpty { null })
                    }
                },
                sendSealed = { plaintext ->
                    sent += String(plaintext)
                    true
                },
                selfPeerId = { self },
                onName = { peer, name -> names += peer to name },
            ),
        )

        fun wire(peer: String, seq: Int, name: String) = "$peer|$seq|$name".toByteArray()
    }

    @Test
    fun `nothing is sent before a name was set`() {
        val h = Harness()
        h.channel.resend()
        assertFalse(h.channel.setOwnName(""))
        h.channel.resend()
        assertTrue(h.sent.isEmpty())
    }

    @Test
    fun `a new name is sent at once and again after every announcement, with the same number`() {
        val h = Harness()
        assertTrue(h.channel.setOwnName("Anna"))
        assertEquals(listOf("me|1|Anna"), h.sent)
        h.channel.resend()
        assertEquals(listOf("me|1|Anna", "me|1|Anna"), h.sent)
        // The same name again changes nothing.
        assertFalse(h.channel.setOwnName("Anna"))
        assertEquals(2, h.sent.size)
        // Going back to the profile name is a new name too.
        assertTrue(h.channel.setOwnName(""))
        assertEquals("me|2|", h.sent.last())
        assertEquals("", h.channel.ownCallName())
    }

    @Test
    fun `a name set before the SFU named this device goes out with the next announcement`() {
        val h = Harness()
        h.self = null
        h.channel.setOwnName("Anna")
        assertTrue(h.sent.isEmpty())
        h.self = "me"
        h.channel.resend()
        assertEquals(listOf("me|1|Anna"), h.sent)
    }

    @Test
    fun `a link joiner with no group yet sends nothing until it can sign`() {
        val h = Harness()
        h.canCompose = false
        h.channel.setOwnName("Anna")
        assertTrue(h.sent.isEmpty())
        h.canCompose = true
        h.channel.resend()
        assertEquals(listOf("me|1|Anna"), h.sent)
    }

    @Test
    fun `a name that arrives before the announcement is read once the peer is identified`() {
        val h = Harness()
        h.channel.onReceived("p1", h.wire("p1", 1, "Old"))
        h.channel.onReceived("p1", h.wire("p1", 2, "Bob"))
        assertTrue(h.names.isEmpty())
        h.channel.onPeerIdentified("p1", "p1".toByteArray())
        // Only the latest one was kept.
        assertEquals(listOf("p1" to "Bob"), h.names)
    }

    @Test
    fun `only a strictly newer name replaces the one held, and a cleared name is null`() {
        val h = Harness()
        h.channel.onPeerIdentified("p1", "p1".toByteArray())
        h.channel.onReceived("p1", h.wire("p1", 2, "Bob"))
        h.channel.onReceived("p1", h.wire("p1", 2, "Bob"))
        h.channel.onReceived("p1", h.wire("p1", 1, "Older"))
        assertEquals(listOf("p1" to "Bob"), h.names)
        h.channel.onReceived("p1", h.wire("p1", 3, ""))
        assertEquals("p1" to null, h.names.last())
    }

    @Test
    fun `a name signed for another peer is dropped`() {
        val h = Harness()
        h.channel.onPeerIdentified("p1", "p1".toByteArray())
        h.channel.onReceived("p1", h.wire("p2", 5, "Mallory"))
        assertTrue(h.names.isEmpty())
    }

    @Test
    fun `a peer that leaves and comes back starts fresh`() {
        val h = Harness()
        h.channel.onPeerIdentified("p1", "p1".toByteArray())
        h.channel.onReceived("p1", h.wire("p1", 4, "Bob"))
        h.channel.onPeerLeft("p1")
        // Before it is identified again, a name waits.
        h.channel.onReceived("p1", h.wire("p1", 1, "Bobby"))
        assertEquals(1, h.names.size)
        h.channel.onPeerIdentified("p1", "p1".toByteArray())
        // A lower number than before is taken: the old one went with the peer.
        assertEquals("p1" to "Bobby", h.names.last())
    }

    @Test
    fun `the name typed for the call outranks every other source of a tile's name`() {
        assertEquals(
            "Captain",
            callTileName(resolved = "Anna", announcedName = "Ann", claimedUsername = "anna", callName = "Captain"),
        )
        assertEquals("Anna", callTileName(resolved = "Anna", announcedName = null, claimedUsername = null, callName = " "))
    }

    @Test
    fun `a peer's call name names its tile until it is cleared or the peer leaves`() {
        val names = CallPeerNames(personName = { "Anna" })
        names.peerIdentified("p1", "person", "leaf")
        assertEquals("Anna", names.name("p1"))
        assertTrue(names.callNamed("p1", "Captain"))
        assertEquals("Captain", names.name("p1"))
        assertFalse(names.callNamed("p1", "Captain"))
        assertTrue(names.callNamed("p1", null))
        assertEquals("Anna", names.name("p1"))
        names.callNamed("p1", "Captain")
        names.forget("p1")
        assertEquals("", names.name("p1"))
    }

    @Test
    fun `the own tile shows the call name, else the profile name`() {
        assertEquals("Captain", ownCallTileName("Captain", "Anna"))
        assertEquals("Anna", ownCallTileName("", "Anna"))
        assertEquals("", ownCallTileName("", ""))
    }

    @Test
    fun `a name typed for a call is remembered for that call only, and an empty one forgets`() {
        RememberedCallNames.clear()
        RememberedCallNames.put("aa", "Captain")
        assertEquals("Captain", RememberedCallNames.get("aa"))
        assertEquals("", RememberedCallNames.get("bb"))
        RememberedCallNames.put("aa", "")
        assertEquals("", RememberedCallNames.get("aa"))
        RememberedCallNames.clear()
    }
}
