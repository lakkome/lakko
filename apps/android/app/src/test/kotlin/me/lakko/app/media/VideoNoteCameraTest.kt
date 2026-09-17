// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * UX spec §3 / §6 — which camera a video note binds, and the hint a take that
 * ends without a clip shows. The 2026-09-17 emulator finding (a device whose
 * only camera faces back showed a recording UI, then sent nothing and said
 * nothing) is pinned by the back-only and still-starting cases.
 */
class VideoNoteCameraTest {

    @Test
    fun front_camera_is_preferred_when_present() {
        assertEquals(CameraLens.FRONT, chooseVideoNoteLens(setOf(CameraLens.BACK, CameraLens.FRONT)))
    }

    @Test
    fun back_only_device_records_on_the_back_camera() {
        assertEquals(CameraLens.BACK, chooseVideoNoteLens(setOf(CameraLens.BACK)))
    }

    @Test
    fun external_only_device_records_on_that_camera() {
        assertEquals(CameraLens.OTHER, chooseVideoNoteLens(setOf(CameraLens.OTHER)))
    }

    @Test
    fun built_in_back_camera_wins_over_an_external_one_without_a_front() {
        assertEquals(CameraLens.BACK, chooseVideoNoteLens(listOf(CameraLens.OTHER, CameraLens.BACK)))
    }

    @Test
    fun no_camera_chooses_nothing() {
        assertNull(chooseVideoNoteLens(emptySet()))
    }

    @Test
    fun flip_needs_both_built_in_lenses() {
        val both = setOf(CameraLens.FRONT, CameraLens.BACK)
        assertEquals(CameraLens.BACK, flipTarget(both, CameraLens.FRONT))
        assertEquals(CameraLens.FRONT, flipTarget(both, CameraLens.BACK))
        assertNull(flipTarget(setOf(CameraLens.BACK), CameraLens.BACK))
        assertNull(flipTarget(setOf(CameraLens.OTHER, CameraLens.FRONT), CameraLens.OTHER))
    }

    private fun outcome(
        take: Long? = null,
        failure: VideoStartFailure? = null,
        recording: Boolean = false,
        recordedMs: Long = 0,
        sinceStartMs: Long = 0,
    ) = videoNoteFinishOutcome(take, failure, recording, recordedMs, sinceStartMs)

    @Test
    fun a_clip_of_the_minimum_length_is_usable() {
        assertEquals(VideoNoteOutcome.USABLE, outcome(take = MIN_TAKE_MS, recording = true, recordedMs = 6_000))
        assertEquals(VideoNoteOutcome.HOLD_TO_RECORD, outcome(take = MIN_TAKE_MS - 1, recording = true))
    }

    @Test
    fun a_start_that_found_no_camera_says_no_camera_found() {
        assertEquals(
            VideoNoteOutcome.NO_CAMERA,
            outcome(failure = VideoStartFailure.NO_CAMERA, sinceStartMs = 6_000),
        )
    }

    @Test
    fun a_start_that_failed_says_so_even_on_a_short_hold() {
        assertEquals(VideoNoteOutcome.FAILED, outcome(failure = VideoStartFailure.FAILED, sinceStartMs = 100))
    }

    @Test
    fun a_long_hold_whose_camera_never_came_up_is_a_failure_not_silence() {
        assertEquals(VideoNoteOutcome.FAILED, outcome(sinceStartMs = 6_000))
    }

    @Test
    fun releasing_while_the_camera_is_still_starting_is_a_short_hold() {
        assertEquals(VideoNoteOutcome.HOLD_TO_RECORD, outcome(sinceStartMs = VIDEO_START_GRACE_MS - 1))
        assertEquals(VideoNoteOutcome.FAILED, outcome(sinceStartMs = VIDEO_START_GRACE_MS))
    }

    @Test
    fun a_recording_that_produced_no_file_is_a_failure() {
        assertEquals(VideoNoteOutcome.FAILED, outcome(recording = true, recordedMs = 4_000, sinceStartMs = 5_000))
    }

    @Test
    fun a_recording_stopped_almost_at_once_is_a_short_take() {
        assertEquals(
            VideoNoteOutcome.HOLD_TO_RECORD,
            outcome(recording = true, recordedMs = MIN_TAKE_MS - 1, sinceStartMs = 4_000),
        )
    }
}
