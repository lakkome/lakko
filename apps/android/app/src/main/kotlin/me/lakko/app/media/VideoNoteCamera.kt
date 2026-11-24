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
 * What ONE video-note take bound to the camera, and the only thing allowed to
 * say what it lets go of.
 *
 * `ProcessCameraProvider` is a **process singleton**, so `unbindAll()` is not
 * a take's own teardown — it is everybody's. The recorder used to call it, and
 * a take's teardown is not instantaneous: the SEND path only reached it after
 * the finished file had been read for metadata and a poster frame decoded and
 * JPEG-compressed. A second take begun inside that window bound the camera
 * and was then unbound by the first take's teardown, from a `VideoNoteRecorder`
 * that no longer owned anything. Nothing in the code said which use cases
 * belonged to which take, so nothing could refuse.
 *
 * This says it. A take binds through [onBound], lets go through
 * [releaseBound] — which names exactly what that take bound and nothing else —
 * and afterwards owns nothing and may never bind again ([mayBind]). Releasing
 * twice is therefore inert rather than destructive, which is what makes the
 * recorder's `finally { release() }` safe to keep after [releaseBound] has
 * already run on the finalize path.
 *
 * Generic in the use-case type only so it can be exercised on the host JVM;
 * the recorder instantiates it over CameraX's `UseCase`.
 */
public class CameraOwnership<T : Any> {
    private val bound = LinkedHashSet<T>()
    private var released = false

    /** This take has let go; it owns nothing and can never bind again. */
    public val isReleased: Boolean get() = released

    /** How many use cases this take is holding right now. */
    public val boundCount: Int get() = bound.size

    /** May this take bind? `false` once it has been released. */
    public fun mayBind(): Boolean = !released

    /** Record a successful bind. Ignored after a release, so a bind that
     *  raced the teardown cannot re-enter the ledger. */
    public fun onBound(useCases: Collection<T>) {
        if (released) return
        bound += useCases
    }

    /**
     * Everything this take bound, in bind order, exactly once. The ledger is
     * then empty and closed: a second call names nothing, so a late teardown
     * cannot unbind a LATER take's camera.
     */
    public fun releaseBound(): List<T> {
        released = true
        val out = bound.toList()
        bound.clear()
        return out
    }
}

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

    /**
     * A camera exists, but offers no H.264 + AAC profile, so a note it
     * recorded would not play on the other clients (UX spec §8).
     */
    NO_COMPATIBLE_FORMAT,

    /** A camera exists, but binding or starting the recording failed. */
    FAILED,
}

/**
 * UX spec §3 states — whether this device can record a video note at all,
 * decided before any take: the lens a take would bind ([chooseVideoNoteLens])
 * and whether that lens offers an H.264 + AAC profile
 * ([chooseVideoNoteProfile]). The same two questions `VideoNoteRecorder.bind`
 * asks, in the same order, so the answer given up front is the answer a hold
 * would get.
 */
public enum class VideoNoteAvailability {
    /** A camera with an interoperable profile: camera mode is offered. */
    AVAILABLE,

    /** No camera to bind: `No camera found`. */
    NO_CAMERA,

    /** A camera, but no H.264 + AAC profile on the lens a take would use:
     *  `Video messages can't be recorded on this device`. */
    NO_COMPATIBLE_FORMAT,
}

/** [VideoNoteAvailability] from each lens's camcorder profiles. */
public fun <Q> videoNoteAvailability(
    optionsByLens: Map<CameraLens, List<VideoNoteProfileOption<Q>>>,
): VideoNoteAvailability {
    val lens = chooseVideoNoteLens(optionsByLens.keys) ?: return VideoNoteAvailability.NO_CAMERA
    return if (chooseVideoNoteProfile(optionsByLens[lens].orEmpty()) == null) {
        VideoNoteAvailability.NO_COMPATIBLE_FORMAT
    } else {
        VideoNoteAvailability.AVAILABLE
    }
}

/**
 * UX spec §3 states — why camera mode is not offered, or `null` when it is.
 *
 * [cameraFeature] is the package manager's word; [availability] is what the
 * CameraX probe found, `null` while it has not answered. An unanswered probe
 * does not refuse: the switch is allowed and taken back, with the notice, the
 * moment the probe says no (the probe runs when the chat opens, so in practice
 * it has answered before the first tap).
 */
public fun videoModeRefusal(
    cameraFeature: Boolean,
    availability: VideoNoteAvailability?,
): VideoStartFailure? = when {
    !cameraFeature -> VideoStartFailure.NO_CAMERA
    availability == VideoNoteAvailability.NO_CAMERA -> VideoStartFailure.NO_CAMERA
    availability == VideoNoteAvailability.NO_COMPATIBLE_FORMAT -> VideoStartFailure.NO_COMPATIBLE_FORMAT
    else -> null
}

/** What a finished (sent or previewed) video-note take becomes. */
public enum class VideoNoteOutcome {
    /** A usable clip: send it, or show it in preview. */
    USABLE,

    /** Too short to keep: `Hold to record`. */
    HOLD_TO_RECORD,

    /** `No camera found`. */
    NO_CAMERA,

    /** `Video messages can't be recorded on this device`. */
    NO_COMPATIBLE_FORMAT,

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
    startFailure == VideoStartFailure.NO_COMPATIBLE_FORMAT -> VideoNoteOutcome.NO_COMPATIBLE_FORMAT
    startFailure == VideoStartFailure.FAILED -> VideoNoteOutcome.FAILED
    recording ->
        if (recordedMs < MIN_TAKE_MS) VideoNoteOutcome.HOLD_TO_RECORD else VideoNoteOutcome.FAILED
    sinceStartMs < VIDEO_START_GRACE_MS -> VideoNoteOutcome.HOLD_TO_RECORD
    else -> VideoNoteOutcome.FAILED
}
