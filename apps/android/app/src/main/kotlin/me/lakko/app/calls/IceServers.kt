// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.webrtc.PeerConnection

/**
 * The ICE servers of a call: Lakko's own, and nobody else's.
 *
 * Until 2026-10-24 every call, one-to-one and group, on all three clients,
 * started from Google's public STUN servers (`stun.l.google.com`), which told
 * Google each person's network address and when they called. The rule now,
 * the same as the web client's `ice-servers.ts` and iOS `IceServers.swift`:
 *
 *   - the TURN entry `services/auth` mints with the call admission, as given;
 *   - a STUN entry on the host and port of each of its UDP `turn:` URIs
 *     (`transport=udp`, or no transport named), since Lakko's coturn answers
 *     STUN there. A WebRTC client sends STUN to a server over UDP only, so a
 *     `turn:` URI with `transport=tcp` and a `turns:` URI give none: their
 *     host may accept no UDP at all. The test stack runs two coturn
 *     instances: edge2.lakko.me takes UDP only, edge.lakko.me takes TCP and
 *     TLS only. Measured 2026-10-24 in Chromium with UDP forbidden: STUN
 *     gave no candidate, and a call connected through TURN over TCP
 *     (edge.lakko.me:3478) and over TLS (edge.lakko.me:443);
 *   - nothing else. Without a TURN entry (no TURN configured, or minting
 *     failed) the list is empty: host candidates only. A group call still
 *     connects then, since the SFU learns the person's outside address from
 *     their own packets; a one-to-one call across two NATs may not, which is
 *     what the TURN entry is for.
 *
 * Pure apart from [toWebRtc], so the host JVM tests cover the rule.
 */
internal object IceServers {

    private const val DEFAULT_STUN_PORT = 3478

    /** One ICE server entry. [toString] names the URLs only: the username
     *  and password of a TURN entry never reach a log. */
    data class Entry(
        val urls: List<String>,
        val username: String? = null,
        val password: String? = null,
    ) {
        override fun toString(): String = "Entry(urls=$urls)"
    }

    /** The STUN URLs on the hosts of UDP `turn:` URIs: `stun:host:port`, the
     *  port 3478 when the URI names none, each host and port once. */
    fun stunUrlsFromTurn(urls: List<String>): List<String> {
        val out = mutableListOf<String>()
        for (url in urls) {
            if (!url.lowercase().startsWith("turn:")) continue
            val afterScheme = url.substring("turn:".length)
            if (!isUdpQuery(afterScheme.substringAfter('?', ""))) continue
            val hostPort = parseHostPort(afterScheme.substringBefore('?')) ?: continue
            val stun = "stun:${hostPort.first}:${hostPort.second}"
            if (stun !in out) out.add(stun)
        }
        return out
    }

    /** True when a `turn:` URI's query names no transport or `transport=udp`. */
    private fun isUdpQuery(query: String): Boolean {
        for (pair in query.split('&')) {
            if (pair.substringBefore('=').lowercase() != "transport") continue
            return pair.contains('=') && pair.substringAfter('=').lowercase() == "udp"
        }
        return true
    }

    private fun parseHostPort(s: String): Pair<String, Int>? {
        if (s.isEmpty()) return null
        val host: String
        var portText: String? = null
        if (s.startsWith("[")) {
            val end = s.indexOf(']')
            if (end < 0) return null
            host = s.substring(0, end + 1)
            val after = s.substring(end + 1)
            if (after.isNotEmpty()) {
                if (!after.startsWith(":")) return null
                portText = after.substring(1)
            }
        } else {
            val colon = s.lastIndexOf(':')
            if (colon >= 0) {
                host = s.substring(0, colon)
                portText = s.substring(colon + 1)
            } else {
                host = s
            }
        }
        if (host.isEmpty() || host == "[]") return null
        if (portText == null) return host to DEFAULT_STUN_PORT
        if (portText.isEmpty() || portText.length > 5 || !portText.all { it in '0'..'9' }) return null
        val port = portText.toInt()
        return if (port in 1..65535) host to port else null
    }

    /** A call's ICE servers from the minted TURN credential, or none. */
    fun forTurn(turn: TurnCredential?): List<Entry> {
        if (turn == null) return emptyList()
        val turnEntry = Entry(turn.urls, turn.username, turn.password)
        val stun = stunUrlsFromTurn(turn.urls)
        return if (stun.isEmpty()) listOf(turnEntry) else listOf(Entry(stun), turnEntry)
    }

    /** The servers' URLs, for the call log: no username, no password. */
    fun logLine(servers: List<Entry>): String {
        val urls = servers.flatMap { it.urls }
        return if (urls.isEmpty()) "none (host candidates only)" else urls.joinToString(", ")
    }

    /** The entries as libwebrtc's ICE servers. */
    fun toWebRtc(servers: List<Entry>): List<PeerConnection.IceServer> = servers.map { entry ->
        val builder = PeerConnection.IceServer.builder(entry.urls)
        entry.username?.let { builder.setUsername(it) }
        entry.password?.let { builder.setPassword(it) }
        builder.createIceServer()
    }
}
