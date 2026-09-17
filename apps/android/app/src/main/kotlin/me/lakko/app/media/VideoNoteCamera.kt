// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

/**
 * ADR-0047 / UX spec §3, §6 — the pure decisions around the video-note camera,
 * kept free of CameraX so they run on the host JVM.
 *
 *   * **Which camera.** §3 records the front camera. A device without one (an
 *     emulator exposing only a back camera, a tablet with a single rear lens,
 *     a ChromeOS machine with only an external webcam) still records, on the
 *     back camera, else on any camera present. Only a device with no camera
 *     at all gets `No camera found`.
 *   * **What the user sees when a take ends without a clip.** A recording UI
 *     that was shown must never vanish silently (the 2026-09-17 emulator
 *     finding: the full recording UI came up, the camera never bound, and
 *     release sent nothing and said nothing). [videoNoteFinishOutcome] names
 *     the hint for every way a take can end without a file.
 *
 * Mirroring needs no decision here: CameraX mirrors the preview of a
 * front-facing camera only, and the capture is built with
 * `MIRROR_MODE_ON_FRONT_ONLY`, so both follow the lens actually bound.
 */

/** A camera's facing, as CameraX reports it. */
public enum class CameraLens { FRONT, BACK, OTHER }

/**
 * The lens a video note records with: [preferred] when present, then the
 * opposite built-in lens, then any other camera (external / unknown facing).
 * `null` only when [available] is empty.
 */
public fun chooseVideoNoteLens(
    available: Collection<CameraLens>,
    preferred: CameraLens = CameraLens.FRONT,
): CameraLens? {
    val order = when (preferred) {
        CameraLens.FRONT -> listOf(CameraLens.FRONT, CameraLens.BACK, CameraLens.OTHER)
        CameraLens.BACK -> listOf(CameraLens.BACK, CameraLens.FRONT, CameraLens.OTHER)
        CameraLens.OTHER -> listOf(CameraLens.OTHER, CameraLens.FRONT, CameraLens.BACK)
    }
    return order.firstOrNull { it in available }
}

/** The lens a flip moves to, or `null` when there is nothing to flip to. */
public fun flipTarget(available: Collection<CameraLens>, current: CameraLens): CameraLens? {
    val target = when (current) {
        CameraLens.FRONT -> CameraLens.BACK
        CameraLens.BACK -> CameraLens.FRONT
        CameraLens.OTHER -> return null
    }
    return target.takeIf { it in available }
}

/** Why a video-note capture never started. */
public enum class VideoStartFailure {
    /** No camera could be found to bind. */
    NO_CAMERA,

    /** A camera exists, but binding or starting the recording failed. */
    FAILED,
}

/** What a finished (sent or previewed) video-note take becomes. */
public enum class VideoNoteOutcome {
    /** A usable clip: send it, or show it in preview. */
    USABLE,

    /** Too short to keep: `Hold to record`. */
    HOLD_TO_RECORD,

    /** `No camera found`. */
    NO_CAMERA,

    /** `Couldn't start recording`. */
    FAILED,
}

/**
 * How long the camera may take to come up before a release that finds it
 * still starting reads as a failure rather than a too-short hold. A real
 * device binds well inside this; a camera that has not started after a hold
 * this long did not start.
 */
public const val VIDEO_START_GRACE_MS: Long = 3_000L

/**
 * The outcome of a video-note take the user released (SEND) or stopped
 * (PREVIEW), judged after the recorder handed back [takeDurationMs] (`null`
 * when it produced no file).
 *
 *   * A clip of at least [MIN_TAKE_MS] is usable; a shorter one is a
 *     too-short hold.
 *   * No clip, and the start already failed: that failure's hint.
 *   * No clip from a capture that was recording: a too-short take when it had
 *     recorded under [MIN_TAKE_MS] ([recordedMs], measured from the moment
 *     recording actually began), otherwise the recording failed.
 *   * No clip from a capture still starting: a too-short hold inside
 *     [VIDEO_START_GRACE_MS] of the press ([sinceStartMs]), a failure after.
 */
public fun videoNoteFinishOutcome(
    takeDurationMs: Long?,
    startFailure: VideoStartFailure?,
    recording: Boolean,
    recordedMs: Long,
    sinceStartMs: Long,
): VideoNoteOutcome = when {
    takeDurationMs != null ->
        if (takeDurationMs >= MIN_TAKE_MS) VideoNoteOutcome.USABLE else VideoNoteOutcome.HOLD_TO_RECORD
    startFailure == VideoStartFailure.NO_CAMERA -> VideoNoteOutcome.NO_CAMERA
    startFailure == VideoStartFailure.FAILED -> VideoNoteOutcome.FAILED
    recording ->
        if (recordedMs < MIN_TAKE_MS) VideoNoteOutcome.HOLD_TO_RECORD else VideoNoteOutcome.FAILED
    sinceStartMs < VIDEO_START_GRACE_MS -> VideoNoteOutcome.HOLD_TO_RECORD
    else -> VideoNoteOutcome.FAILED
}
