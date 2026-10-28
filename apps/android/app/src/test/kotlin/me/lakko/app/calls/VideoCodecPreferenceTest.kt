// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.webrtc.RtpCapabilities

/**
 * VP8 before every description (`VideoCodecPreference.kt`): the preference
 * list built from the receive capabilities, and its application to the video
 * transceivers only. The cases follow the web `video-codec-preference.test.ts`.
 * The host JVM loads no native library, so the libwebrtc calls themselves are
 * not exercised here; `RtpCapabilities.CodecCapability` is a plain Java class.
 */
class VideoCodecPreferenceTest {

    private fun cap(name: String, payloadType: Int): RtpCapabilities.CodecCapability =
        RtpCapabilities.CodecCapability().apply {
            this.name = name
            mimeType = "video/$name"
            clockRate = 90_000
            preferredPayloadType = payloadType
        }

    /** The order WebKit and iOS list: H264 first. */
    private val webkitLike = listOf(
        cap("H264", 96),
        cap("rtx", 97),
        cap("H264", 98),
        cap("H265", 99),
        cap("VP8", 100),
        cap("rtx", 101),
        cap("VP9", 102),
        cap("AV1", 103),
        cap("red", 104),
        cap("ulpfec", 105),
        cap("flexfec-03", 106),
    )

    @Test
    fun `H264 first becomes VP8 first, then the wrappers in listed order`() {
        val prefs = VideoCodecPreference.vp8Preferences(webkitLike)
        assertEquals(
            listOf(100, 97, 101, 104, 105, 106),
            prefs?.map { it.preferredPayloadType },
        )
        assertEquals(
            listOf("VP8", "rtx", "rtx", "red", "ulpfec", "flexfec-03"),
            prefs?.map { it.name },
        )
    }

    @Test
    fun `no media codec but VP8 survives`() {
        val prefs = VideoCodecPreference.vp8Preferences(webkitLike) ?: error("expected a list")
        val media = prefs.map { it.name }.filter { it !in setOf("rtx", "red", "ulpfec", "flexfec-03") }
        assertEquals(listOf("VP8"), media)
    }

    @Test
    fun `no VP8 offered gives null`() {
        val noVp8 = listOf(cap("H264", 96), cap("rtx", 97), cap("VP9", 98), cap("red", 99))
        assertNull(VideoCodecPreference.vp8Preferences(noVp8))
        assertNull(VideoCodecPreference.vp8Preferences(emptyList<RtpCapabilities.CodecCapability>()))
    }

    @Test
    fun `MIME type is matched without regard to case, and the name stands in for a missing one`() {
        val lower = RtpCapabilities.CodecCapability().apply { mimeType = "video/vp8"; preferredPayloadType = 1 }
        val nameOnly = RtpCapabilities.CodecCapability().apply { name = "VP8"; preferredPayloadType = 2 }
        val prefs = VideoCodecPreference.vp8Preferences(listOf(cap("H264", 0), lower, nameOnly))
        assertEquals(listOf(1, 2), prefs?.map { it.preferredPayloadType })
    }

    @Test
    fun `every VP8 entry is kept`() {
        val prefs = VideoCodecPreference.vp8Preferences(
            listOf("video/H264", "video/VP8", "video/VP8", "video/rtx"),
        ) { it }
        assertEquals(listOf("video/VP8", "video/VP8", "video/rtx"), prefs)
    }

    private class FakeTransceiver(val video: Boolean, val refuses: Boolean = false) {
        var set: List<String>? = null
    }

    private fun apply(transceivers: List<FakeTransceiver>, prefs: List<String>?) =
        VideoCodecPreference.preferVp8(
            transceivers = transceivers,
            isVideo = { it.video },
            preferences = prefs,
            set = { tr, list ->
                if (tr.refuses) throw IllegalStateException("refused")
                tr.set = list
            },
        )

    @Test
    fun `only video transceivers are set, audio is left alone`() {
        val audio = FakeTransceiver(video = false)
        val camera = FakeTransceiver(video = true)
        val copy = FakeTransceiver(video = true)
        val prefs = listOf("video/VP8", "video/rtx")
        val outcome = apply(listOf(audio, camera, copy), prefs)
        assertEquals(VideoCodecPreference.Outcome(applied = 2, unsupported = 0), outcome)
        assertNull(audio.set)
        assertEquals(prefs, camera.set)
        assertEquals(prefs, copy.set)
    }

    @Test
    fun `no VP8 leaves every video transceiver as it is and counts it`() {
        val audio = FakeTransceiver(video = false)
        val camera = FakeTransceiver(video = true)
        val outcome = apply(listOf(audio, camera), null)
        assertEquals(VideoCodecPreference.Outcome(applied = 0, unsupported = 1), outcome)
        assertNull(camera.set)
    }

    @Test
    fun `a refused set is counted and the others still get the list`() {
        val refusing = FakeTransceiver(video = true, refuses = true)
        val camera = FakeTransceiver(video = true)
        val outcome = apply(listOf(refusing, camera), listOf("video/VP8"))
        assertEquals(VideoCodecPreference.Outcome(applied = 1, unsupported = 1), outcome)
        assertNull(refusing.set)
        assertEquals(listOf("video/VP8"), camera.set)
    }

    @Test
    fun `setting again before the next description gives the same list`() {
        val camera = FakeTransceiver(video = true)
        val prefs = VideoCodecPreference.vp8Preferences(webkitLike)
        apply(listOf(camera), prefs?.map { it.mimeType })
        val first = camera.set
        apply(listOf(camera), prefs?.map { it.mimeType })
        assertEquals(first, camera.set)
    }
}
