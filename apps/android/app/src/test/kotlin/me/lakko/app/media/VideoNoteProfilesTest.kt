// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0047 / UX spec §8 — the rule that picks the camcorder profile a video
 * note records with.
 *
 * The case this exists for is the 2026-09-17 interop defect: the device's SD
 * profile carries **AMR-NB** audio, CameraX takes the audio codec from the
 * profile, and the resulting MP4 is refused whole by AVFoundation on iOS. A
 * profile is a format decision, so the rule must decide on codecs first and
 * size second, and must answer `null` rather than pick something unplayable.
 */
public class VideoNoteProfilesTest {

    private companion object {
        const val H264 = "video/avc"
        const val AAC = "audio/mp4a-latm"
        const val AMR_NB = "audio/3gpp"
        const val HEVC = "video/hevc"
    }

    private fun profile(
        name: String,
        width: Int,
        height: Int,
        video: String? = H264,
        audio: String? = AAC,
    ) = VideoNoteProfileOption(name, video, audio, width, height)

    /** The observed defect: the only SD profile records AMR-NB audio. */
    @Test
    public fun amrOnlyProfileIsNeverSelected() {
        val chosen = chooseVideoNoteProfile(
            listOf(profile("SD", 720, 480, audio = AMR_NB)),
        )
        assertNull("an AMR-NB profile must never be recorded with", chosen)
    }

    @Test
    public fun aac720pIsSelectedWhenItIsTheOnlyInteroperableProfile() {
        val chosen = chooseVideoNoteProfile(
            listOf(
                profile("SD", 720, 480, audio = AMR_NB),
                profile("HD", 1280, 720),
            ),
        )
        assertEquals("HD", chosen?.quality)
    }

    @Test
    public fun aac480pIsPreferredOverLargerInteroperableProfiles() {
        val chosen = chooseVideoNoteProfile(
            listOf(
                profile("FHD", 1920, 1080),
                profile("HD", 1280, 720),
                profile("SD", 720, 480),
            ),
        )
        assertEquals("the smallest profile reaching 480 on the short side", "SD", chosen?.quality)
    }

    @Test
    public fun noProfilesAtAllYieldNothing() {
        assertNull(chooseVideoNoteProfile(emptyList<VideoNoteProfileOption<String>>()))
    }

    /** Codecs decide before size: a bigger AAC profile beats a smaller AMR one. */
    @Test
    public fun sizeNeverOutranksTheCodecPair() {
        val chosen = chooseVideoNoteProfile(
            listOf(
                profile("SD", 720, 480, audio = AMR_NB),
                profile("UHD", 3840, 2160),
            ),
        )
        assertEquals("UHD", chosen?.quality)
    }

    @Test
    public fun nonH264VideoIsRejectedEvenWithAacAudio() {
        assertNull(chooseVideoNoteProfile(listOf(profile("HD", 1280, 720, video = HEVC))))
    }

    @Test
    public fun aProfileWithNoAudioTrackIsRejected() {
        assertNull(chooseVideoNoteProfile(listOf(profile("HD", 1280, 720, audio = null))))
    }

    /**
     * Below the 480 floor the largest interoperable profile still records:
     * smaller than the spec's box, but a clip every client can play.
     */
    @Test
    public fun belowTheFloorTheLargestInteroperableProfileIsUsed() {
        val chosen = chooseVideoNoteProfile(
            listOf(
                profile("QCIF", 176, 144),
                profile("CIF", 352, 288),
            ),
        )
        assertEquals("CIF", chosen?.quality)
    }

    @Test
    public fun exactlyTheFloorCounts() {
        val chosen = chooseVideoNoteProfile(
            listOf(
                profile("CIF", 352, 288),
                profile("SD", 640, VIDEO_NOTE_MIN_SHORT_SIDE),
            ),
        )
        assertEquals("SD", chosen?.quality)
    }

    /** A portrait profile is measured on its short side, like the square crop. */
    @Test
    public fun theShortSideIsWhatCounts() {
        val chosen = chooseVideoNoteProfile(
            listOf(
                profile("PORTRAIT_SD", 480, 720),
                profile("HD", 1280, 720),
            ),
        )
        assertEquals("PORTRAIT_SD", chosen?.quality)
    }

    /** Equal short sides break on the long side, so the answer is stable. */
    @Test
    public fun tiesBreakOnTheLongSideRatherThanOnListOrder() {
        val wide = profile("WIDE", 1280, 480)
        val narrow = profile("NARROW", 640, 480)
        assertEquals("NARROW", chooseVideoNoteProfile(listOf(wide, narrow))?.quality)
        assertEquals("NARROW", chooseVideoNoteProfile(listOf(narrow, wide))?.quality)
    }

    @Test
    public fun zeroSizedProfilesAreRejected() {
        assertNull(chooseVideoNoteProfile(listOf(profile("BROKEN", 0, 0))))
    }

    @Test
    public fun mimeComparisonIgnoresCase() {
        val chosen = chooseVideoNoteProfile(
            listOf(profile("SD", 720, 480, video = "VIDEO/AVC", audio = "Audio/MP4A-LATM")),
        )
        assertEquals("SD", chosen?.quality)
    }

    @Test
    public fun theRecordedFileCheckAcceptsOnlyTheInteroperablePair() {
        assertTrue(videoNoteCodecsInteroperable(H264, AAC))
        assertFalse("the shipped defect: H.264 video with AMR-NB audio", videoNoteCodecsInteroperable(H264, AMR_NB))
        assertFalse(videoNoteCodecsInteroperable(HEVC, AAC))
        assertFalse(videoNoteCodecsInteroperable(H264, null))
        assertFalse(videoNoteCodecsInteroperable(null, AAC))
    }
}
