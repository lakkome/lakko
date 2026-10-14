// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlin.math.sqrt

/**
 * The sound level of each person in a group call, read on this device
 * (`docs/ux/group-call-grid.md` section 7.3 and section 10): the counterpart
 * of the web `audio-level.ts`.
 *
 * The levels come from the peer connection's own statistics: the decoded
 * audio of each incoming stream, after it was received and decrypted here,
 * and this device's own microphone. Nothing here reaches the network, the
 * call server is never asked who is speaking, and the audio-level RTP header
 * extension (RFC 6464), which would carry the level to the server in the
 * clear, is not what this reads.
 *
 * Two sources in a statistics entry, tried in this order:
 *
 *  1. the growth of `totalAudioEnergy` over the growth of
 *     `totalSamplesDuration` since the previous reading, which is the root
 *     mean square of everything played in between;
 *  2. where an entry reports no energy, or on a stream's first reading, its
 *     `audioLevel`.
 *
 * The web client decides from an analyser on the stream it plays and, where
 * that cannot be read, from the same growth of the receiver's audio energy;
 * the level on a receiver's synchronization sources is only printed in its
 * diagnostics and decides nothing. libwebrtc's Java API has neither an
 * analyser on a played stream nor the synchronization sources, so the
 * statistics are the only source here.
 *
 * This file is the pure half: picking the levels out of a statistics report.
 * Reading the report is [WebRtcPeerConnection.sampleAudioLevels], and what
 * the levels mean is the shared rule's ([SpeakingRule]).
 */

/** Where a level came from. */
public enum class LevelPath(public val wireName: String) {
    STATS_ENERGY("stats-energy"),
    STATS_LEVEL("stats-level"),
    NONE("none"),
}

/** One reading. A person with no entry could not be read. */
public data class CallAudioLevels(
    /** This device's own microphone, `0..1`, or `null` when it was not in the
     *  report. */
    val local: Double?,
    /** Each incoming audio stream's level, `0..1`, by MSID stream id, which
     *  on an SFU call is the id of the peer the sound comes from. */
    val remoteByStreamId: Map<String, Double>,
    /** Where [local] came from. */
    val localPath: LevelPath = if (local == null) LevelPath.NONE else LevelPath.STATS_LEVEL,
    /** Where each entry of [remoteByStreamId] came from; a stream with no
     *  entry here is [LevelPath.STATS_LEVEL]. */
    val remotePathByStreamId: Map<String, LevelPath> = emptyMap(),
)

/** One entry of a statistics report: its type, its members, and the report's
 *  id for it when the reader needs to follow an entry across reports. */
public class RtcStat(
    public val type: String,
    public val members: Map<String, Any?>,
    public val id: String? = null,
)

/**
 * Picks the levels out of successive statistics reports of one connection.
 * Keeps the energy counters of the previous report, by entry id.
 *
 * A remote level belongs to an `inbound-rtp` audio entry, and the entry names
 * its track (`trackIdentifier`); `streamIdByTrackId` is what the peer
 * connection recorded when that track arrived, and leads to the person. An
 * entry whose track is not in the map, or which carries no level of either
 * kind, is left out. Two entries for one person keep the louder.
 *
 * The local level belongs to the `media-source` audio entry.
 *
 * Not thread-safe: one caller, one thread.
 */
public class AudioLevelReader {
    private class Energy(val total: Double, val duration: Double)

    private var energyById: Map<String, Energy> = emptyMap()

    /** The levels in [stats]. */
    public fun read(stats: Iterable<RtcStat>, streamIdByTrackId: Map<String, String>): CallAudioLevels {
        var local: Double? = null
        var localPath = LevelPath.NONE
        val remote = HashMap<String, Double>()
        val remotePath = HashMap<String, LevelPath>()
        val nextEnergy = HashMap<String, Energy>()
        for (stat in stats) {
            if (stat.members["kind"] != "audio") continue
            if (stat.type != "media-source" && stat.type != "inbound-rtp") continue
            val (level, path) = levelOf(stat, nextEnergy) ?: continue
            if (stat.type == "media-source") {
                if (local == null || level > local) {
                    local = level
                    localPath = path
                }
            } else {
                val trackId = stat.members["trackIdentifier"] as? String ?: continue
                val streamId = streamIdByTrackId[trackId] ?: continue
                val before = remote[streamId]
                if (before == null || level > before) {
                    remote[streamId] = level
                    remotePath[streamId] = path
                }
            }
        }
        energyById = nextEnergy
        return CallAudioLevels(
            local = local,
            remoteByStreamId = remote,
            localPath = localPath,
            remotePathByStreamId = remotePath,
        )
    }

    private fun levelOf(stat: RtcStat, nextEnergy: MutableMap<String, Energy>): Pair<Double, LevelPath>? {
        val instant = number(stat, "audioLevel")?.coerceAtMost(1.0)
        val total = number(stat, "totalAudioEnergy")
        val duration = number(stat, "totalSamplesDuration")
        val id = stat.id
        if (id != null && total != null && duration != null) {
            nextEnergy[id] = Energy(total, duration)
            val before = energyById[id]
            if (before != null) {
                val energy = total - before.total
                val played = duration - before.duration
                // Counters that went back belong to a new stream: start again
                // from here, on the entry's own level if it has one.
                if (energy >= 0 && played >= 0) {
                    // Nothing played since the last reading is silence.
                    val level = if (played > 0) sqrt(energy / played).coerceAtMost(1.0) else 0.0
                    return level to LevelPath.STATS_ENERGY
                }
            }
        }
        return instant?.let { it to LevelPath.STATS_LEVEL }
    }

    private fun number(stat: RtcStat, key: String): Double? =
        (stat.members[key] as? Number)?.toDouble()?.takeIf { it.isFinite() && it >= 0 }
}
