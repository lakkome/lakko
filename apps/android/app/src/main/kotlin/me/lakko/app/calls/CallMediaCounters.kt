// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.util.Locale

/**
 * What a group call's receivers report (ADR-0067 step 1), read from the peer
 * connection's own statistics and written to the call log once per interval
 * and as a total when the call ends, so a change to how calls survive packet
 * loss can be measured. Diagnostics only: counts and durations, never a name.
 *
 * The pure half, and the twin of the receiving part of the web
 * `media-counters.ts`: the same line formats and field names, so a log from a
 * phone and one from a browser read alike. Reading the report is
 * [WebRtcPeerConnection.sampleMediaStats]; the loop is in [SfuCallSession].
 */

/** How often the SFU session reads the counters, milliseconds. The web
 *  client's interval. */
internal const val MEDIA_COUNTERS_INTERVAL_MS: Long = 2_000L

/** The label of a per-interval line. */
internal const val MEDIA_COUNTERS_INTERVAL_LABEL: String = "call media interval"

/** The label of an end-of-call line. */
internal const val MEDIA_COUNTERS_TOTAL_LABEL: String = "call media total"

/** One remote stream's cumulative receiver counters. */
internal data class InboundCounters(
    /** `audio` or `video`. */
    val kind: String,
    /** A video receiver's layer: `main`, `light` (the light companion,
     *  `LightStream.kt`) or `-` when the report does not name the receiver's
     *  track; `null` for audio. */
    val layer: String?,
    val packetsReceived: Double,
    val packetsLost: Double,
    val nackCount: Double,
    val pliCount: Double,
    val freezeCount: Double,
    val totalFreezesDuration: Double,
    val framesDecoded: Double,
    val framesDropped: Double,
    val jitterBufferDelay: Double,
    val jitterBufferEmittedCount: Double,
    val concealedSamples: Double,
    val totalSamplesReceived: Double,
)

/** What this device's own camera sender reports for one simulcast layer (or
 *  for the whole camera when it is sent as one encoding). */
internal data class OutboundVideo(
    /** The layer's RTP stream id (`q`, `f`); `null` when the camera is sent
     *  as one encoding. */
    val rid: String?,
    val scalabilityMode: String?,
    val qualityLimitationReason: String?,
    val frameWidth: Double?,
    val frameHeight: Double?,
    val framesPerSecond: Double?,
)

/** One statistics report, reduced to what is logged. */
internal data class MediaReading(
    /** Keyed by the id of the `inbound-rtp` entry, which stays the same for a
     *  stream across readings. */
    val inbound: Map<String, InboundCounters>,
    /** Every layer the camera sender reports, in the report's order. */
    val outboundVideoLayers: List<OutboundVideo>,
)

private fun num(stat: RtcStat, key: String): Double =
    (stat.members[key] as? Number)?.toDouble()?.takeIf { it.isFinite() } ?: 0.0

private fun numOrNull(stat: RtcStat, key: String): Double? =
    (stat.members[key] as? Number)?.toDouble()?.takeIf { it.isFinite() }

private fun strOrNull(stat: RtcStat, key: String): String? = stat.members[key] as? String

/**
 * Reduce a statistics report to a reading. [lightTrackIds] holds the ids of
 * the remote tracks that are light companions; a video receiver whose
 * `trackIdentifier` is among them is `light`, any other is `main`.
 */
internal fun readMedia(stats: Iterable<RtcStat>, lightTrackIds: Set<String> = emptySet()): MediaReading {
    val inbound = LinkedHashMap<String, InboundCounters>()
    val outbound = ArrayList<OutboundVideo>()
    for (stat in stats) {
        val kind = stat.members["kind"] as? String
        if (kind != "audio" && kind != "video") continue
        if (stat.type == "inbound-rtp") {
            val id = stat.id ?: continue
            val trackId = strOrNull(stat, "trackIdentifier")
            inbound[id] = InboundCounters(
                kind = kind,
                layer = when {
                    kind == "audio" -> null
                    trackId == null -> "-"
                    trackId in lightTrackIds -> "light"
                    else -> "main"
                },
                packetsReceived = num(stat, "packetsReceived"),
                packetsLost = num(stat, "packetsLost"),
                nackCount = num(stat, "nackCount"),
                pliCount = num(stat, "pliCount"),
                freezeCount = num(stat, "freezeCount"),
                totalFreezesDuration = num(stat, "totalFreezesDuration"),
                framesDecoded = num(stat, "framesDecoded"),
                framesDropped = num(stat, "framesDropped"),
                jitterBufferDelay = num(stat, "jitterBufferDelay"),
                jitterBufferEmittedCount = num(stat, "jitterBufferEmittedCount"),
                concealedSamples = num(stat, "concealedSamples"),
                totalSamplesReceived = num(stat, "totalSamplesReceived"),
            )
        } else if (stat.type == "outbound-rtp" && kind == "video") {
            // One camera sender; one entry for each simulcast layer it sends.
            outbound += OutboundVideo(
                rid = strOrNull(stat, "rid"),
                scalabilityMode = strOrNull(stat, "scalabilityMode"),
                qualityLimitationReason = strOrNull(stat, "qualityLimitationReason"),
                frameWidth = numOrNull(stat, "frameWidth"),
                frameHeight = numOrNull(stat, "frameHeight"),
                framesPerSecond = numOrNull(stat, "framesPerSecond"),
            )
        }
    }
    return MediaReading(inbound = inbound, outboundVideoLayers = outbound)
}

/** How one stream changed between two readings. */
internal data class StreamDelta(
    val kind: String,
    /** A video receiver's layer; `null` for audio. */
    val layer: String?,
    val packetsReceived: Double,
    val packetsLost: Double,
    val nacks: Double,
    val keyframeRequests: Double,
    val freezes: Double,
    val freezeSeconds: Double,
    val framesDecoded: Double,
    val framesDropped: Double,
    /** Average time a frame or sample spent in the jitter buffer over the
     *  interval, milliseconds; `null` when nothing left it. */
    val jitterBufferMs: Double?,
    /** Share of received audio samples that were concealed, 0..1; `null` for
     *  video or when no samples arrived. */
    val concealedShare: Double?,
)

/**
 * The change of every stream present in [now] since [before]. A stream that is
 * new in [now] is measured from zero; counters that went backwards (a stream
 * replaced under the same id) are measured from zero as well.
 */
internal fun deltaOf(before: MediaReading?, now: MediaReading): Map<String, StreamDelta> {
    val out = LinkedHashMap<String, StreamDelta>()
    for ((id, cur) in now.inbound) {
        val prev = before?.inbound?.get(id)
        val base = prev?.takeIf { it.packetsReceived <= cur.packetsReceived }
        fun d(read: (InboundCounters) -> Double): Double =
            maxOf(0.0, read(cur) - (base?.let(read) ?: 0.0))
        val emitted = d { it.jitterBufferEmittedCount }
        val samples = d { it.totalSamplesReceived }
        out[id] = StreamDelta(
            kind = cur.kind,
            layer = cur.layer,
            packetsReceived = d { it.packetsReceived },
            packetsLost = d { it.packetsLost },
            nacks = d { it.nackCount },
            keyframeRequests = d { it.pliCount },
            freezes = d { it.freezeCount },
            freezeSeconds = d { it.totalFreezesDuration },
            framesDecoded = d { it.framesDecoded },
            framesDropped = d { it.framesDropped },
            jitterBufferMs = if (emitted > 0) d { it.jitterBufferDelay } / emitted * 1000 else null,
            concealedShare =
                if (cur.kind == "audio" && samples > 0) d { it.concealedSamples } / samples else null,
        )
    }
    return out
}

private fun fixed(n: Double?, digits: Int): String =
    if (n == null) "-" else String.format(Locale.ROOT, "%.${digits}f", n)

private fun count(n: Double): String = n.toLong().toString()

/** One plain-text line per stream (ASCII, no names). A video line ends with
 *  `layer=`: `main`, `light`, or `-` when unknown. */
internal fun describeDeltas(label: String, deltas: Map<String, StreamDelta>): List<String> {
    val lines = ArrayList<String>()
    var n = 0
    for (d in deltas.values) {
        n += 1
        val common =
            "$label ${d.kind}#$n pkts=${count(d.packetsReceived)} lost=${count(d.packetsLost)}" +
                " nack=${count(d.nacks)} jb_ms=${fixed(d.jitterBufferMs, 0)}"
        lines += if (d.kind == "video") {
            "$common pli=${count(d.keyframeRequests)} freezes=${count(d.freezes)}" +
                " freeze_s=${fixed(d.freezeSeconds, 2)} decoded=${count(d.framesDecoded)}" +
                " dropped=${count(d.framesDropped)} layer=${d.layer ?: "-"}"
        } else {
            "$common concealed=${fixed(d.concealedShare?.let { it * 100 }, 1)}%"
        }
    }
    return lines
}

/** One camera layer in one line. A simulcast layer's line ends with its
 *  `rid`; a single encoding's line has none. */
internal fun describeOutbound(label: String, out: OutboundVideo): String {
    val size = if (out.frameWidth != null && out.frameHeight != null) {
        "${count(out.frameWidth)}x${count(out.frameHeight)}"
    } else {
        "-"
    }
    return "$label send video mode=${out.scalabilityMode ?: "-"} size=$size" +
        " fps=${fixed(out.framesPerSecond, 0)} limited_by=${out.qualityLimitationReason ?: "-"}" +
        (if (out.rid == null) "" else " rid=${out.rid}")
}

/** The camera sender, one line for each layer it sends: how a log shows
 *  whether libwebrtc took the two simulcast encodings. */
internal fun describeOutboundLayers(label: String, layers: List<OutboundVideo>): List<String> =
    layers.map { describeOutbound(label, it) }
