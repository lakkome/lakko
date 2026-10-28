// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The call path line (`CallPath.kt`), with the cases of the web twin
 * `apps/web/src/calls/call-path.test.ts`.
 */
class CallPathTest {

    private fun report(
        pairId: String = "CP1",
        local: Map<String, Any?> = emptyMap(),
        remote: Map<String, Any?> = emptyMap(),
        rtt: Double? = 0.012,
        state: String = "succeeded",
        viaTransport: Boolean = false,
    ): List<RtcStat> {
        val pair = mutableMapOf<String, Any?>(
            "state" to state,
            "nominated" to true,
            "localCandidateId" to "L1",
            "remoteCandidateId" to "R1",
        )
        if (rtt != null) pair["currentRoundTripTime"] = rtt
        val out = mutableListOf(
            RtcStat(
                "local-candidate",
                mapOf("candidateType" to "host", "protocol" to "udp", "address" to "192.168.1.20", "port" to 50000) +
                    local,
                "L1",
            ),
            RtcStat(
                "remote-candidate",
                mapOf("candidateType" to "host", "protocol" to "udp", "address" to "192.168.1.21", "port" to 50001) +
                    remote,
                "R1",
            ),
            RtcStat("candidate-pair", pair, pairId),
        )
        if (viaTransport) out += RtcStat("transport", mapOf("selectedCandidatePairId" to pairId), "T1")
        return out
    }

    @Test
    fun `prints a direct path with no address or port`() {
        val line = CallPath.describe(CallPath.read(report())!!)
        assertEquals("call path: local=host/udp remote=host/udp rtt=12ms", line)
        assertFalse(line.contains("192.168"))
        assertFalse(line.contains("5000"))
    }

    @Test
    fun `prints the TURN transport and server name of a relayed local candidate`() {
        val entries = report(
            local = mapOf(
                "candidateType" to "relay",
                "relayProtocol" to "tls",
                "url" to "turns:edge.lakko.me:443?transport=tcp",
                "address" to "46.224.156.97",
            ),
            remote = mapOf("candidateType" to "srflx"),
            rtt = 0.0414,
        )
        assertEquals(
            "call path: local=relay/udp/tls via=edge.lakko.me remote=srflx/udp rtt=41ms",
            CallPath.describe(CallPath.read(entries)!!),
        )
    }

    @Test
    fun `follows the transport's selected pair`() {
        assertEquals("host/udp", CallPath.read(report(state = "in-progress", viaTransport = true))?.local)
    }

    @Test
    fun `is null before a pair has succeeded`() {
        assertNull(CallPath.read(report(state = "in-progress")))
        assertNull(CallPath.read(emptyList()))
    }

    @Test
    fun `prints a dash without a round-trip time`() {
        assertEquals(
            "call path: local=host/udp remote=host/udp rtt=-",
            CallPath.describe(CallPath.read(report(rtt = null))!!),
        )
    }

    @Test
    fun `turn host name names a host, never an address`() {
        assertEquals("edge2.lakko.me", CallPath.turnHostName("turn:edge2.lakko.me:3478?transport=udp"))
        assertEquals("edge.lakko.me", CallPath.turnHostName("TURNS:Edge.Lakko.Me"))
        assertNull(CallPath.turnHostName("turn:46.224.156.97:3478"))
        assertNull(CallPath.turnHostName("turn:[2a01:4f8::1]:3478"))
        assertNull(CallPath.turnHostName("stun:edge2.lakko.me:3478"))
        assertNull(CallPath.turnHostName(null))
    }

    @Test
    fun `logs once per path, and again when the path changes`() {
        val log = CallPathLog()
        assertNull(log.note(emptyList()))
        assertEquals("call path: local=host/udp remote=host/udp rtt=12ms", log.note(report()))
        assertNull(log.note(report(rtt = 0.2)))
        val relay = mapOf("candidateType" to "relay", "relayProtocol" to "udp")
        assertEquals("call path: local=relay/udp/udp remote=host/udp rtt=12ms", log.note(report(local = relay)))
        assertNotNull(log.note(report(pairId = "CP2", local = relay)))
    }
}
