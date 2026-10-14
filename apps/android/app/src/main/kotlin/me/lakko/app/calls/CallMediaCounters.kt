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
    /** A video receiver's track: `main`, `middle` (the middle track), `light`
     *  (the light companion, `LightStream.kt`) or `-` when the report does
     *  not name the receiver's track; `null` for audio. */
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

/** What one of this device's own camera senders reports. */
internal data class OutboundVideo(
    /** Which sender: the main camera, its middle copy or its light copy. */
    val sender: CameraSenderKind,
    val scalabilityMode: String?,
    val qualityLimitationReason: String?,
    val frameWidth: Double?,
    val frameHeight: Double?,
    val framesPerSecond: Double?,
    /** Cumulative; the bitrate of an interval is its growth. */
    val bytesSent: Double,
)

/** One statistics report, reduced to what is logged. */
internal data class MediaReading(
    /** Keyed by the id of the `inbound-rtp` entry, which stays the same for a
     *  stream across readings. */
    val inbound: Map<String, InboundCounters>,
    /** Every camera sender that reports: the main one first, then the middle
     *  one, then the light one. */
    val outboundVideoSenders: List<OutboundVideo>,
)

private fun num(stat: RtcStat, key: String): Double =
    (stat.members[key] as? Number)?.toDouble()?.takeIf { it.isFinite() } ?: 0.0

private fun numOrNull(stat: RtcStat, key: String): Double? =
    (stat.members[key] as? Number)?.toDouble()?.takeIf { it.isFinite() }

private fun strOrNull(stat: RtcStat, key: String): String? =
    (stat.members[key] as? String)?.takeIf { it.isNotEmpty() }

/**
 * The id of the local track each `media-source` entry of a report describes,
 * by the entry's id: what an `outbound-rtp` entry's `mediaSourceId` leads to.
 */
internal fun mediaSourceTrackIds(stats: Iterable<RtcStat>): Map<String, String> {
    val out = HashMap<String, String>()
    for (stat in stats) {
        if (stat.type != "media-source") continue
        val id = stat.id ?: continue
        out[id] = strOrNull(stat, "trackIdentifier") ?: continue
    }
    return out
}

/**
 * Which of this device's camera senders a video `outbound-rtp` entry is, or
 * `null` when the report does not say. Two ways, tried in this order:
 *
 *  1. the entry's `mid`, looked up in [senderByMid] (the `mid` of each
 *     sender's media line, read from its transceiver);
 *  2. the entry's `mediaSourceId`, which names a `media-source` entry of the
 *     same report whose `trackIdentifier` is the id of the track the sender
 *     sends ([trackIdByMediaSource], from [mediaSourceTrackIds]), looked up
 *     in [senderByTrackId]. Each camera sender has a track of its own with a
 *     fixed id, so this needs neither the `mid` in the entry nor a
 *     transceiver to read one from (the main sender made by the plain
 *     `addTrack` has none).
 *
 * A `mid` that is absent, or that [senderByMid] does not know, falls through
 * to the second way and is not an answer by itself. The frame size is
 * deliberately not a third way: the encoder scales a sender down under load,
 * so a main sender can report the size the plan gives a copy.
 */
internal fun outboundSenderKind(
    stat: RtcStat,
    senderByMid: Map<String, CameraSenderKind>,
    senderByTrackId: Map<String, CameraSenderKind>,
    trackIdByMediaSource: Map<String, String>,
): CameraSenderKind? {
    strOrNull(stat, "mid")?.let { senderByMid[it] }?.let { return it }
    return strOrNull(stat, "mediaSourceId")
        ?.let { trackIdByMediaSource[it] }
        ?.let { senderByTrackId[it] }
}

/**
 * Reduce a statistics report to a reading.
 *
 * [lightTrackIds] and [middleTrackIds] hold the ids of the remote tracks that
 * are light companions and middle tracks; a video receiver whose
 * `trackIdentifier` is among them is `light` or `middle`, any other is
 * `main`. An id in both sets is read as light.
 *
 * [senderByMid] and [senderByTrackId] name this device's camera senders, by
 * the `mid` of their media line and by the id of the track they send: a video
 * `outbound-rtp` entry is the sender [outboundSenderKind] finds for it, and
 * the `main` one when neither way names it. (The web client reads each copy's
 * own sender report instead; libwebrtc's one report answers the same
 * question.)
 */
internal fun readMedia(
    stats: Iterable<RtcStat>,
    lightTrackIds: Set<String> = emptySet(),
    middleTrackIds: Set<String> = emptySet(),
    senderByMid: Map<String, CameraSenderKind> = emptyMap(),
    senderByTrackId: Map<String, CameraSenderKind> = emptyMap(),
): MediaReading {
    val inbound = LinkedHashMap<String, InboundCounters>()
    val trackIdByMediaSource = mediaSourceTrackIds(stats)
    val bySender = CAMERA_SENDER_KINDS.associateWith { ArrayList<OutboundVideo>() }
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
                    trackId in middleTrackIds -> "middle"
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
            // Three camera senders at most, one entry each.
            val sender = outboundSenderKind(stat, senderByMid, senderByTrackId, trackIdByMediaSource)
                ?: CameraSenderKind.MAIN
            bySender.getValue(sender) += OutboundVideo(
                sender = sender,
                scalabilityMode = strOrNull(stat, "scalabilityMode"),
                qualityLimitationReason = strOrNull(stat, "qualityLimitationReason"),
                frameWidth = numOrNull(stat, "frameWidth"),
                frameHeight = numOrNull(stat, "frameHeight"),
                framesPerSecond = numOrNull(stat, "framesPerSecond"),
                bytesSent = num(stat, "bytesSent"),
            )
        }
    }
    return MediaReading(
        inbound = inbound,
        outboundVideoSenders = CAMERA_SENDER_KINDS.flatMap { bySender.getValue(it) },
    )
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
 *  `layer=`: `main`, `middle`, `light`, or `-` when unknown. */
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

/**
 * What each camera sender sent over the interval since [before], kilobits a
 * second, from the growth of its `bytesSent` over [elapsedMs]. A sender that
 * did not report in both readings, or whose counter went back, has no entry.
 */
internal fun senderKbps(
    before: MediaReading?,
    now: MediaReading,
    elapsedMs: Long,
): Map<CameraSenderKind, Double> {
    if (before == null || elapsedMs <= 0) return emptyMap()
    val out = LinkedHashMap<CameraSenderKind, Double>()
    for (cur in now.outboundVideoSenders) {
        val prev = before.outboundVideoSenders.firstOrNull { it.sender == cur.sender } ?: continue
        val bytes = cur.bytesSent - prev.bytesSent
        if (bytes < 0) continue
        out[cur.sender] = bytes * 8.0 / elapsedMs
    }
    return out
}

/**
 * One camera sender in one line. The line names which sender it is
 * (`sender=main`, `sender=middle` or `sender=light`), what it sent over the
 * interval (`kbps=`, `-` when unknown; a field the web client's line does not
 * have) and, when [activity] is given, ends with the SFU's last word for it
 * and whether its encoding is active: `needed=yes|no|- active=yes|no`. When
 * [ceiling] is given (`PublishBudget.kt`: the bitrate ceiling the sender's
 * encoding holds) the line ends with `ceiling=N` in kbit/s, or `ceiling=-`.
 */
internal fun describeOutbound(
    label: String,
    out: OutboundVideo,
    activity: SenderActivityNote? = null,
    kbps: Double? = null,
    ceiling: SenderCeilingNote? = null,
): String {
    val size = if (out.frameWidth != null && out.frameHeight != null) {
        "${count(out.frameWidth)}x${count(out.frameHeight)}"
    } else {
        "-"
    }
    return "$label send video mode=${out.scalabilityMode ?: "-"} size=$size" +
        " fps=${fixed(out.framesPerSecond, 0)} kbps=${fixed(kbps, 0)}" +
        " limited_by=${out.qualityLimitationReason ?: "-"}" +
        " sender=${out.sender.wireName}" +
        (activity?.let(::describeSenderActivity) ?: "") +
        describeSenderCeiling(ceiling)
}

/**
 * The camera, one line for each of its senders (main, middle, light). A
 * sender libwebrtc is sending nothing on reports no size and no frame rate
 * (`-`). [activityOf] adds each line's `needed=` and `active=`, [ceilingOf]
 * its `ceiling=`. A sender
 * named in [existing] that the report has no outbound stream for still has
 * its line, all dashes, so a sender that was switched off does not vanish
 * from the log.
 */
internal fun describeOutboundSenders(
    label: String,
    senders: List<OutboundVideo>,
    activityOf: (CameraSenderKind) -> SenderActivityNote? = { null },
    existing: List<CameraSenderKind> = emptyList(),
    kbpsOf: (CameraSenderKind) -> Double? = { null },
    ceilingOf: (CameraSenderKind) -> SenderCeilingNote? = { null },
): List<String> {
    val lines = ArrayList<String>()
    val reported = HashSet<CameraSenderKind>()
    for (sender in senders) {
        reported += sender.sender
        lines += describeOutbound(
            label,
            sender,
            activityOf(sender.sender),
            kbpsOf(sender.sender),
            ceilingOf(sender.sender),
        )
    }
    for (kind in existing) {
        if (kind in reported) continue
        lines += describeOutbound(
            label,
            OutboundVideo(
                sender = kind,
                scalabilityMode = null,
                qualityLimitationReason = null,
                frameWidth = null,
                frameHeight = null,
                framesPerSecond = null,
                bytesSent = 0.0,
            ),
            activityOf(kind),
            null,
            ceilingOf(kind),
        )
    }
    return lines
}

/** What the sending side of the call reports, cumulative where it counts. */
internal data class SenderReading(
    /** Camera `outbound-rtp`: frames and packets this device produced, summed
     *  over every camera sender. */
    val framesEncoded: Double,
    val packetsSent: Double,
    val retransmittedPacketsSent: Double,
    val nackCount: Double,
    val pliCount: Double,
    val firCount: Double,
    val hugeFramesSent: Double,
    /** Bits a second the encoders are currently asked for, summed over the
     *  senders; `null` if unknown. */
    val targetBitrate: Double?,
    /** What the far end (the SFU) says it lost of the camera, 0..1; the worse
     *  of the senders' figures. */
    val remoteFractionLost: Double?,
    /** Round trip the far end's reports give, in milliseconds; the largest
     *  across the senders. */
    val remoteRttMs: Double?,
    /** libwebrtc's own estimate of what it may send, bits a second. */
    val availableOutgoingBitrate: Double?,
    /** The formats on the wire, e.g. `audio/opus`, `video/VP8`; `null` when
     *  the report does not say. */
    val audioCodec: String?,
    val videoCodec: String?,
)

/**
 * Reduce a statistics report to the sender reading, or `null` with no camera.
 * Every camera sender is summed. [senderByMid] and [senderByTrackId] are as
 * in [readMedia]; they only decide whose codec is named (the main sender's,
 * when it reports).
 */
internal fun readSender(
    stats: Iterable<RtcStat>,
    senderByMid: Map<String, CameraSenderKind> = emptyMap(),
    senderByTrackId: Map<String, CameraSenderKind> = emptyMap(),
): SenderReading? {
    val trackIdByMediaSource = mediaSourceTrackIds(stats)
    val outs = ArrayList<RtcStat>()
    val remotes = ArrayList<RtcStat>()
    var pair: RtcStat? = null
    var audioOut: RtcStat? = null
    val codecs = HashMap<String, String>()
    for (stat in stats) {
        val kind = stat.members["kind"] as? String
        when {
            stat.type == "codec" -> {
                val id = stat.id
                val mime = strOrNull(stat, "mimeType")
                if (id != null && mime != null) codecs[id] = mime
            }
            stat.type == "outbound-rtp" && kind == "audio" -> if (audioOut == null) audioOut = stat
            stat.type == "outbound-rtp" && kind == "video" -> outs += stat
            stat.type == "remote-inbound-rtp" && kind == "video" -> remotes += stat
            stat.type == "candidate-pair" && stat.members["availableOutgoingBitrate"] != null ->
                if (pair == null || stat.members["nominated"] == true) pair = stat
        }
    }
    fun isCopy(stat: RtcStat): Boolean {
        val kind = outboundSenderKind(stat, senderByMid, senderByTrackId, trackIdByMediaSource)
        return kind != null && kind != CameraSenderKind.MAIN
    }
    val first = outs.firstOrNull { !isCopy(it) } ?: outs.firstOrNull() ?: return null
    fun sum(key: String): Double = outs.sumOf { num(it, key) }
    fun largest(from: List<RtcStat>, key: String): Double? = from.mapNotNull { numOrNull(it, key) }.maxOrNull()
    val targets = outs.mapNotNull { numOrNull(it, "targetBitrate") }
    fun codecOf(stat: RtcStat?): String? = (stat?.members?.get("codecId") as? String)?.let { codecs[it] }
    return SenderReading(
        framesEncoded = sum("framesEncoded"),
        packetsSent = sum("packetsSent"),
        retransmittedPacketsSent = sum("retransmittedPacketsSent"),
        nackCount = sum("nackCount"),
        pliCount = sum("pliCount"),
        firCount = sum("firCount"),
        hugeFramesSent = sum("hugeFramesSent"),
        targetBitrate = if (targets.isEmpty()) null else targets.sum(),
        remoteFractionLost = largest(remotes, "fractionLost"),
        remoteRttMs = largest(remotes, "roundTripTime")?.let { it * 1000 },
        availableOutgoingBitrate = pair?.let { numOrNull(it, "availableOutgoingBitrate") },
        audioCodec = codecOf(audioOut),
        videoCodec = codecOf(first),
    )
}

/** One plain-text line on the sender for the interval since [before]. */
internal fun describeSender(label: String, before: SenderReading?, now: SenderReading): String {
    fun d(read: (SenderReading) -> Double): Double {
        val base = before?.let(read)?.takeIf { it <= read(now) } ?: 0.0
        return maxOf(0.0, read(now) - base)
    }
    fun kbps(bps: Double?): String = fixed(bps?.let { it / 1000 }, 0)
    return "$label sender encoded=${count(d { it.framesEncoded })} pkts=${count(d { it.packetsSent })}" +
        " rtx=${count(d { it.retransmittedPacketsSent })}" +
        " nack_in=${count(d { it.nackCount })} pli_in=${count(d { it.pliCount })}" +
        " fir_in=${count(d { it.firCount })} huge=${count(d { it.hugeFramesSent })}" +
        " target_kbps=${kbps(now.targetBitrate)} estimate_kbps=${kbps(now.availableOutgoingBitrate)}" +
        " remote_lost=${fixed(now.remoteFractionLost?.let { it * 100 }, 0)}%" +
        " remote_rtt_ms=${fixed(now.remoteRttMs, 0)}" +
        " audio=${now.audioCodec ?: "-"} video=${now.videoCodec ?: "-"}"
}
