// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * The sound level of each person in a group call, read on this device
 * (`docs/ux/group-call-grid.md` section 7.3 and section 10).
 *
 * The levels come from the peer connection's own statistics: the decoded
 * audio of each incoming stream, after it was received and decrypted here,
 * and this device's own microphone. Nothing here reaches the network, and
 * the call server is never asked who is speaking.
 *
 * This file is the pure half: picking the levels out of a statistics report.
 * Reading the report is [WebRtcPeerConnection.sampleAudioLevels], and what
 * the levels mean is [SpeakingTracker].
 */

/** One reading. A person with no entry could not be read; they are fed
 *  nothing rather than a guess. */
public data class CallAudioLevels(
    /** This device's own microphone, `0..1`, or `null` when it was not in the
     *  report. */
    val local: Double?,
    /** Each incoming audio stream's level, `0..1`, by MSID stream id — which
     *  on an SFU call is the id of the peer the sound comes from. */
    val remoteByStreamId: Map<String, Double>,
)

/** One entry of a statistics report: its type and its members. */
public class RtcStat(public val type: String, public val members: Map<String, Any?>)

/**
 * The levels in [stats].
 *
 * A remote level is the `audioLevel` of an `inbound-rtp` audio entry, and the
 * entry names its track (`trackIdentifier`); [streamIdByTrackId] is what the
 * peer connection recorded when that track arrived, and leads to the person.
 * An entry whose track is not in the map, or which carries no level, is left
 * out. Two entries for one person keep the louder.
 *
 * The local level is the `audioLevel` of the `media-source` audio entry.
 */
public fun audioLevelsFrom(stats: Iterable<RtcStat>, streamIdByTrackId: Map<String, String>): CallAudioLevels {
    var local: Double? = null
    val remote = HashMap<String, Double>()
    for (stat in stats) {
        if (stat.members["kind"] != "audio") continue
        val level = (stat.members["audioLevel"] as? Number)?.toDouble()?.takeIf { it.isFinite() } ?: continue
        when (stat.type) {
            "media-source" -> local = maxOf(local ?: 0.0, level)
            "inbound-rtp" -> {
                val trackId = stat.members["trackIdentifier"] as? String ?: continue
                val streamId = streamIdByTrackId[trackId] ?: continue
                remote[streamId] = maxOf(remote[streamId] ?: 0.0, level)
            }
        }
    }
    return CallAudioLevels(local = local, remoteByStreamId = remote)
}
