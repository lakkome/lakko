// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The group call's media counters (`CallMediaCounters.kt`): reading a
 * statistics report, the change between two readings, and the log lines, which
 * carry the web client's field names.
 */
class CallMediaCountersTest {

    private fun inboundVideo(
        id: String,
        trackId: String?,
        packets: Int,
        decoded: Int = 0,
        extra: Map<String, Any?> = emptyMap(),
    ) = RtcStat(
        type = "inbound-rtp",
        id = id,
        members = mapOf<String, Any?>(
            "kind" to "video",
            "trackIdentifier" to trackId,
            "packetsReceived" to packets,
            "framesDecoded" to decoded,
        ) + extra,
    )

    private fun inboundAudio(id: String, packets: Int, samples: Int, concealed: Int) = RtcStat(
        type = "inbound-rtp",
        id = id,
        members = mapOf(
            "kind" to "audio",
            "trackIdentifier" to "a-track",
            "packetsReceived" to packets,
            "totalSamplesReceived" to samples,
            "concealedSamples" to concealed,
        ),
    )

    @Test
    fun `a video receiver is named by the copy its track is, else main`() {
        val reading = readMedia(
            listOf(
                inboundVideo("V1", "main-track", 10),
                inboundVideo("V2", "light-track", 5),
                inboundVideo("V3", null, 1),
                inboundVideo("V4", "middle-track", 7),
                // In both sets: read as light.
                inboundVideo("V5", "both", 2),
                inboundAudio("A1", 50, 960, 0),
            ),
            lightTrackIds = setOf("light-track", "both"),
            middleTrackIds = setOf("middle-track", "both"),
        )
        assertEquals("main", reading.inbound["V1"]?.layer)
        assertEquals("light", reading.inbound["V2"]?.layer)
        assertEquals("-", reading.inbound["V3"]?.layer)
        assertEquals("middle", reading.inbound["V4"]?.layer)
        assertEquals("light", reading.inbound["V5"]?.layer)
        assertNull(reading.inbound["A1"]?.layer)
    }

    @Test
    fun `entries without an id or of another kind are left out`() {
        val reading = readMedia(
            listOf(
                RtcStat("inbound-rtp", mapOf("kind" to "video", "packetsReceived" to 3)),
                RtcStat("inbound-rtp", mapOf("packetsReceived" to 3), id = "X"),
                RtcStat("candidate-pair", mapOf("kind" to "video"), id = "Y"),
            ),
        )
        assertTrue(reading.inbound.isEmpty())
        assertTrue(reading.outboundVideoSenders.isEmpty())
    }

    @Test
    fun `the video line ends with the receiver's layer`() {
        val before = readMedia(
            listOf(
                inboundVideo("V1", "main-track", 100, decoded = 40),
                inboundVideo("V2", "light-track", 20, decoded = 40),
            ),
            setOf("light-track"),
        )
        val now = readMedia(
            listOf(
                inboundVideo(
                    "V1", "main-track", 350, decoded = 90,
                    extra = mapOf(
                        "packetsLost" to 4,
                        "nackCount" to 3,
                        "pliCount" to 1,
                        "freezeCount" to 1,
                        "totalFreezesDuration" to 0.42,
                        "framesDropped" to 2,
                        "jitterBufferDelay" to 2.5,
                        "jitterBufferEmittedCount" to 50,
                    ),
                ),
                inboundVideo("V2", "light-track", 70, decoded = 90),
            ),
            setOf("light-track"),
        )
        assertEquals(
            listOf(
                "call media interval video#1 pkts=250 lost=4 nack=3 jb_ms=50 pli=1 freezes=1" +
                    " freeze_s=0.42 decoded=50 dropped=2 layer=main",
                "call media interval video#2 pkts=50 lost=0 nack=0 jb_ms=- pli=0 freezes=0" +
                    " freeze_s=0.00 decoded=50 dropped=0 layer=light",
            ),
            describeDeltas(MEDIA_COUNTERS_INTERVAL_LABEL, deltaOf(before, now)),
        )
    }

    @Test
    fun `the audio line has no layer`() {
        val now = readMedia(listOf(inboundAudio("A1", 100, 48_000, 480)))
        assertEquals(
            listOf("call media total audio#1 pkts=100 lost=0 nack=0 jb_ms=- concealed=1.0%"),
            describeDeltas(MEDIA_COUNTERS_TOTAL_LABEL, deltaOf(null, now)),
        )
    }

    @Test
    fun `a new stream and one whose counters went backwards are measured from zero`() {
        val before = readMedia(listOf(inboundVideo("V1", "t", 500, decoded = 200)))
        val now = readMedia(
            listOf(
                inboundVideo("V1", "t", 30, decoded = 9),
                inboundVideo("V9", "t2", 12, decoded = 4),
            ),
        )
        val deltas = deltaOf(before, now)
        assertEquals(30.0, deltas["V1"]!!.packetsReceived, 0.0)
        assertEquals(9.0, deltas["V1"]!!.framesDecoded, 0.0)
        assertEquals(12.0, deltas["V9"]!!.packetsReceived, 0.0)
    }

    private fun outbound(
        id: String,
        mid: String?,
        width: Int?,
        height: Int?,
        fps: Double?,
        bytes: Long = 0,
        mode: String? = null,
        extra: Map<String, Any?> = emptyMap(),
    ) = RtcStat(
        type = "outbound-rtp",
        id = id,
        members = mapOf<String, Any?>(
            "kind" to "video",
            "mid" to mid,
            "scalabilityMode" to mode,
            "qualityLimitationReason" to "none",
            "frameWidth" to width,
            "frameHeight" to height,
            "framesPerSecond" to fps,
            "bytesSent" to bytes,
        ) + extra,
    )

    private val mids = mapOf(
        "1" to CameraSenderKind.MAIN,
        "2" to CameraSenderKind.LIGHT,
        "3" to CameraSenderKind.MIDDLE,
    )

    @Test
    fun `each camera sender is named by the mid of its media line, main first`() {
        val reading = readMedia(
            listOf(
                outbound("O-l", "2", 320, 180, 30.0),
                outbound("O-m", "3", 640, 360, 30.0),
                outbound("O-f", "1", 1280, 720, 29.6, mode = "L1T3"),
            ),
            senderByMid = mids,
        )
        assertEquals(
            listOf(CameraSenderKind.MAIN, CameraSenderKind.MIDDLE, CameraSenderKind.LIGHT),
            reading.outboundVideoSenders.map { it.sender },
        )
        // An entry with no mid, or one nobody named, is the main sender.
        val unnamed = readMedia(listOf(outbound("O-x", null, 1, 1, 1.0), outbound("O-y", "9", 1, 1, 1.0)), senderByMid = mids)
        assertEquals(
            listOf(CameraSenderKind.MAIN, CameraSenderKind.MAIN),
            unnamed.outboundVideoSenders.map { it.sender },
        )
    }

    @Test
    fun `a sender's line says its size, what it sent, the SFU's word and whether it is active`() {
        val before = readMedia(
            listOf(outbound("O-f", "1", 1280, 720, 30.0, bytes = 100_000), outbound("O-l", "2", 320, 180, 30.0, bytes = 10_000)),
            senderByMid = mids,
        )
        val now = readMedia(
            listOf(
                outbound("O-f", "1", 1280, 720, 29.6, bytes = 350_000, mode = "L1T3"),
                outbound("O-l", "2", 320, 180, 30.0, bytes = 45_000),
            ),
            senderByMid = mids,
        )
        val kbps = senderKbps(before, now, elapsedMs = 2_000)
        assertEquals(1_000.0, kbps.getValue(CameraSenderKind.MAIN), 1e-9)
        assertEquals(140.0, kbps.getValue(CameraSenderKind.LIGHT), 1e-9)
        val needed = VideoNeeded(main = true, middle = false)
        assertEquals(
            listOf(
                "call media interval send video mode=L1T3 size=1280x720 fps=30 kbps=1000 limited_by=none" +
                    " sender=main needed=yes active=yes",
                "call media interval send video mode=- size=320x180 fps=30 kbps=140 limited_by=none" +
                    " sender=light needed=- active=yes",
                // The middle sender exists and reports nothing: it was
                // switched off, and still has its line.
                "call media interval send video mode=- size=- fps=- kbps=- limited_by=-" +
                    " sender=middle needed=no active=no",
            ),
            describeOutboundSenders(
                MEDIA_COUNTERS_INTERVAL_LABEL,
                now.outboundVideoSenders,
                activityOf = { kind ->
                    SenderActivityNote(neededWord(kind, needed), active = kind != CameraSenderKind.MIDDLE)
                },
                existing = CAMERA_SENDER_KINDS,
                kbpsOf = { kbps[it] },
            ),
        )
    }

    @Test
    fun `a sender's bitrate is unknown on the first reading and when its counter went back`() {
        val a = readMedia(listOf(outbound("O-f", "1", 1, 1, 1.0, bytes = 500)), senderByMid = mids)
        val b = readMedia(listOf(outbound("O-f", "1", 1, 1, 1.0, bytes = 100)), senderByMid = mids)
        assertTrue(senderKbps(null, a, 2_000).isEmpty())
        assertTrue(senderKbps(a, b, 2_000).isEmpty())
        assertTrue(senderKbps(a, a, 0).isEmpty())
    }

    @Test
    fun `the sender line sums the camera senders and names the main one's codec`() {
        val stats = listOf(
            RtcStat("codec", mapOf("mimeType" to "video/VP8"), id = "C-v"),
            RtcStat("codec", mapOf("mimeType" to "audio/opus"), id = "C-a"),
            RtcStat("outbound-rtp", mapOf("kind" to "audio", "codecId" to "C-a"), id = "O-a"),
            outbound(
                "O-l", "2", 320, 180, 30.0,
                extra = mapOf("framesEncoded" to 60, "packetsSent" to 80, "targetBitrate" to 150_000.0),
            ),
            outbound(
                "O-f", "1", 1280, 720, 30.0,
                extra = mapOf(
                    "framesEncoded" to 60, "packetsSent" to 400, "retransmittedPacketsSent" to 3,
                    "nackCount" to 2, "pliCount" to 1, "targetBitrate" to 1_200_000.0, "codecId" to "C-v",
                ),
            ),
            RtcStat("remote-inbound-rtp", mapOf("kind" to "video", "fractionLost" to 0.02, "roundTripTime" to 0.045)),
            RtcStat("remote-inbound-rtp", mapOf("kind" to "video", "fractionLost" to 0.1, "roundTripTime" to 0.03)),
            RtcStat("candidate-pair", mapOf("availableOutgoingBitrate" to 2_000_000.0, "nominated" to true)),
        )
        val reading = readSender(stats, mids)!!
        assertEquals(
            "call media interval sender encoded=120 pkts=480 rtx=3 nack_in=2 pli_in=1 fir_in=0 huge=0" +
                " target_kbps=1350 estimate_kbps=2000 remote_lost=10% remote_rtt_ms=45" +
                " audio=audio/opus video=video/VP8",
            describeSender(MEDIA_COUNTERS_INTERVAL_LABEL, null, reading),
        )
        // The next interval counts from the last reading.
        assertTrue(
            describeSender(MEDIA_COUNTERS_INTERVAL_LABEL, reading, reading)
                .startsWith("call media interval sender encoded=0 pkts=0 rtx=0"),
        )
        // No camera, no line.
        assertNull(readSender(listOf(RtcStat("outbound-rtp", mapOf("kind" to "audio"), id = "O-a"))))
    }
}
