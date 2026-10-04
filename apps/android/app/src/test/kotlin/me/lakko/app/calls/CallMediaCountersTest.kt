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
    fun `a video receiver is the light companion when its track is one, else main`() {
        val reading = readMedia(
            listOf(
                inboundVideo("V1", "main-track", 10),
                inboundVideo("V2", "light-track", 5),
                inboundVideo("V3", null, 1),
                inboundAudio("A1", 50, 960, 0),
            ),
            lightTrackIds = setOf("light-track"),
        )
        assertEquals("main", reading.inbound["V1"]?.layer)
        assertEquals("light", reading.inbound["V2"]?.layer)
        assertEquals("-", reading.inbound["V3"]?.layer)
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
        assertTrue(reading.outboundVideoLayers.isEmpty())
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

    @Test
    fun `the camera sender is one line for each simulcast layer it sends`() {
        fun outbound(rid: String?, width: Int?, height: Int?, fps: Double?) = RtcStat(
            type = "outbound-rtp",
            id = "O-$rid",
            members = mapOf(
                "kind" to "video",
                "rid" to rid,
                "scalabilityMode" to "L1T3",
                "qualityLimitationReason" to "none",
                "frameWidth" to width,
                "frameHeight" to height,
                "framesPerSecond" to fps,
            ),
        )
        val two = readMedia(listOf(outbound("q", 320, 180, 30.0), outbound("f", 1280, 720, 29.6)))
        assertEquals(
            listOf(
                "call media interval send video mode=L1T3 size=320x180 fps=30 limited_by=none rid=q",
                "call media interval send video mode=L1T3 size=1280x720 fps=30 limited_by=none rid=f",
            ),
            describeOutboundLayers(MEDIA_COUNTERS_INTERVAL_LABEL, two.outboundVideoLayers),
        )
        // One encoding: no rid, as before simulcast. A layer that is not
        // being sent reports no size and no frame rate.
        val one = readMedia(listOf(outbound(null, null, null, null)))
        assertEquals(
            listOf("call media interval send video mode=L1T3 size=- fps=- limited_by=none"),
            describeOutboundLayers(MEDIA_COUNTERS_INTERVAL_LABEL, one.outboundVideoLayers),
        )
    }
}
