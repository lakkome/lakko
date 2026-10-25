// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * Which network path a call's media takes, as one log line. The twin of the
 * web `apps/web/src/calls/call-path.ts` and iOS `CallPath.swift`.
 *
 * A delay spike that hits both directions at once (a burst of concealed audio
 * with nothing lost, measured on a one-to-one call on 2026-10-25) is either the
 * local network or a relay in between, and nothing in the logs said which.
 * This line does: the selected ICE candidate pair's local and remote candidate
 * types and protocols, the transport to the TURN server for a relayed local
 * candidate, and the pair's round-trip time.
 *
 *     call path: local=relay/udp/tls via=edge.lakko.me remote=host/udp rtt=41ms
 *
 * No address or port of either side is printed. `via=` names the TURN server,
 * which is Lakko's own, by its host name, and only when it is a name. The line
 * is written when the selected pair is first known and when the path changes,
 * not on every statistics reading; a changed round-trip time alone is not a
 * new path. Pure, so the host JVM tests cover it.
 */
internal object CallPath {

    /** The selected pair of a statistics report, reduced to what the line prints. */
    data class Reading(
        /** `<type>/<protocol>[/<relayProtocol>]` of the local candidate. */
        val local: String,
        /** `<type>/<protocol>` of the remote candidate. */
        val remote: String,
        /** The TURN server's host name for a relayed local candidate. */
        val via: String?,
        /** The pair's current round-trip time in milliseconds, when reported. */
        val rttMs: Int?,
        /** What identifies the path for change detection (no round-trip time). */
        val key: String,
    )

    private fun RtcStat?.str(key: String): String? =
        (this?.members?.get(key) as? String)?.takeIf { it.isNotEmpty() }

    private fun selectedPair(stats: List<RtcStat>, byId: Map<String, RtcStat>): RtcStat? {
        for (s in stats) {
            if (s.type != "transport") continue
            val pair = s.str("selectedCandidatePairId")?.let { byId[it] }
            if (pair != null) return pair
        }
        val pairs = stats.filter { it.type == "candidate-pair" }
        fun succeeded(s: RtcStat): Boolean = s.str("state") == "succeeded"
        return pairs.firstOrNull { it.members["selected"] == true }
            ?: pairs.firstOrNull { succeeded(it) && it.members["nominated"] == true }
            ?: pairs.firstOrNull(::succeeded)
    }

    /** The host of a TURN URI (`turn:host:port?...`), or null when it names an
     *  address rather than a host name, or does not parse. */
    fun turnHostName(url: String?): String? {
        if (url == null) return null
        val lower = url.lowercase()
        val rest = when {
            lower.startsWith("turns:") -> lower.removePrefix("turns:")
            lower.startsWith("turn:") -> lower.removePrefix("turn:")
            else -> return null
        }
        val hostPort = rest.substringBefore('?')
        if (hostPort.startsWith("[")) return null
        val host = hostPort.substringBefore(':')
        if (host.isEmpty() || host.all { it.isDigit() || it == '.' }) return null
        return host.takeIf { h -> h.all { it in 'a'..'z' || it in '0'..'9' || it == '.' || it == '-' } }
    }

    /** The selected pair of a statistics report, or null before there is one. */
    fun read(stats: List<RtcStat>): Reading? {
        val byId = stats.mapNotNull { s -> s.id?.let { it to s } }.toMap()
        val pair = selectedPair(stats, byId) ?: return null
        val local = pair.str("localCandidateId")?.let { byId[it] } ?: return null
        val remote = pair.str("remoteCandidateId")?.let { byId[it] } ?: return null
        val localType = local.str("candidateType") ?: "?"
        val relayProtocol = if (localType == "relay") local.str("relayProtocol") else null
        val localText = "$localType/${local.str("protocol") ?: "?"}" +
            (relayProtocol?.let { "/$it" } ?: "")
        val remoteText = "${remote.str("candidateType") ?: "?"}/${remote.str("protocol") ?: "?"}"
        val via = if (localType == "relay") turnHostName(local.str("url")) else null
        val rttMs = (pair.members["currentRoundTripTime"] as? Number)?.toDouble()
            ?.takeIf { it.isFinite() && it >= 0.0 }
            ?.let { Math.round(it * 1000.0).toInt() }
        return Reading(
            local = localText,
            remote = remoteText,
            via = via,
            rttMs = rttMs,
            key = "${pair.id ?: "?"}|$localText|${via ?: ""}|$remoteText",
        )
    }

    /** The log line for a path. */
    fun describe(path: Reading): String =
        "call path: local=${path.local}" + (path.via?.let { " via=$it" } ?: "") +
            " remote=${path.remote} rtt=${path.rttMs?.let { "${it}ms" } ?: "-"}"
}

/** One call's path log: a line when the path is first known or changes. */
internal class CallPathLog {
    private var lastKey: String? = null

    /** The line to log for this reading, or null when the path is unchanged or
     *  not known yet. */
    fun note(stats: List<RtcStat>): String? {
        val path = CallPath.read(stats) ?: return null
        if (path.key == lastKey) return null
        lastKey = path.key
        return CallPath.describe(path)
    }
}
