// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

/**
 * A call's ICE servers are Lakko's own only (`IceServers.kt`). The cases
 * follow the web client's `ice-servers.test.ts`.
 */
class IceServersTest {

    @Test
    fun `takes the host and port of each UDP turn URI, without its transport`() {
        assertEquals(
            listOf("stun:edge2.lakko.me:3478"),
            IceServers.stunUrlsFromTurn(
                listOf("turn:edge2.lakko.me:3478?transport=udp", "turn:edge2.lakko.me:3478"),
            ),
        )
    }

    @Test
    fun `gives nothing for a turn URI over TCP, whose host may accept no UDP`() {
        assertEquals(
            listOf("stun:edge2.lakko.me:3478"),
            IceServers.stunUrlsFromTurn(
                listOf(
                    "turn:edge2.lakko.me:3478?transport=udp",
                    "turn:edge.lakko.me:3478?transport=tcp",
                    "turn:edge.lakko.me:3478?TRANSPORT=TCP",
                    "turns:edge.lakko.me:443?transport=tcp",
                ),
            ),
        )
    }

    @Test
    fun `uses 3478 when the URI names no port, and keeps an IPv6 host bracketed`() {
        assertEquals(listOf("stun:turn.example:3478"), IceServers.stunUrlsFromTurn(listOf("turn:turn.example")))
        assertEquals(
            listOf("stun:[2001:db8::1]:3479"),
            IceServers.stunUrlsFromTurn(listOf("turn:[2001:db8::1]:3479")),
        )
        assertEquals(listOf("stun:edge2.lakko.me:3478"), IceServers.stunUrlsFromTurn(listOf("TURN:edge2.lakko.me")))
    }

    @Test
    fun `gives nothing for turns and for what does not parse`() {
        assertEquals(
            emptyList<String>(),
            IceServers.stunUrlsFromTurn(
                listOf("turns:edge2.lakko.me:5349?transport=tcp", "turn:", "turn:host:99999", "turn:[::1"),
            ),
        )
    }

    @Test
    fun `is empty without a TURN entry, so no public STUN server`() {
        assertEquals(emptyList<IceServers.Entry>(), IceServers.forTurn(null))
    }

    @Test
    fun `is STUN on the TURN host then the TURN entry, and nothing on google`() {
        val turn = TurnCredential(listOf("turn:edge2.lakko.me:3478?transport=udp"), "u", "c")
        val servers = IceServers.forTurn(turn)
        assertEquals(
            listOf(
                IceServers.Entry(listOf("stun:edge2.lakko.me:3478")),
                IceServers.Entry(listOf("turn:edge2.lakko.me:3478?transport=udp"), "u", "c"),
            ),
            servers,
        )
        assertFalse(servers.flatMap { it.urls }.any { it.contains("google") })
    }

    @Test
    fun `a TURN entry with only turns URIs stands alone`() {
        val turn = TurnCredential(listOf("turns:edge2.lakko.me:5349"), "u", "c")
        assertEquals(
            listOf(IceServers.Entry(listOf("turns:edge2.lakko.me:5349"), "u", "c")),
            IceServers.forTurn(turn),
        )
    }

    @Test
    fun `a TURN entry with no UDP turn URI stands alone`() {
        val turn = TurnCredential(listOf("turn:edge.lakko.me:3478?transport=tcp", "turns:edge.lakko.me:443"), "u", "c")
        assertEquals(
            listOf(IceServers.Entry(listOf("turn:edge.lakko.me:3478?transport=tcp", "turns:edge.lakko.me:443"), "u", "c")),
            IceServers.forTurn(turn),
        )
    }

    @Test
    fun `logs the URLs and never the credential`() {
        val servers = IceServers.forTurn(TurnCredential(listOf("turn:edge2.lakko.me:3478"), "user-x", "secret-y"))
        val line = IceServers.logLine(servers)
        assertEquals("stun:edge2.lakko.me:3478, turn:edge2.lakko.me:3478", line)
        assertFalse(line.contains("user-x"))
        assertFalse(line.contains("secret-y"))
        assertFalse(servers.toString().contains("user-x"))
        assertFalse(servers.toString().contains("secret-y"))
        assertEquals("none (host candidates only)", IceServers.logLine(emptyList()))
    }
}
