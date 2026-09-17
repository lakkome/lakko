// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

/**
 * ADR-0047 / UX spec §8 — which camcorder profile a video note records with,
 * kept free of CameraX so it runs on the host JVM.
 *
 * The spec pins one recording format per kind so that "every client plays
 * every other's messages without transcoding": a video note is **H.264 + AAC
 * in MP4**. CameraX does not let an app name the audio encoder; it derives it
 * from the `EncoderProfiles` of the `Quality` the app selected (with
 * `OUTPUT_FORMAT_AUTO`, which is the only output format a `Recorder` can be
 * built with, `AudioConfigUtil` takes the profile's audio mime type verbatim).
 * So the profile IS the format decision, and picking a quality without looking
 * at its profile picks a format blind.
 *
 * That is how the 2026-09-17 interop defect happened: the recorder asked for
 * `Quality.SD` with a lower-or-higher fallback, the device answered with a
 * profile whose audio codec is **AMR-NB at 8 kHz**, and the resulting MP4 —
 * H.264 video, AMR-NB audio — is refused whole by AVFoundation on iOS
 * (`-11800`). A note recorded on Android could not be played on iOS at all.
 *
 * The rule below therefore selects on the codecs first and the size second:
 *
 *   * **Only interoperable profiles are candidates** — video `video/avc`
 *     AND audio `audio/mp4a-latm`. A profile whose audio is AMR-NB (or
 *     anything else), or whose video is not H.264, is never selected, however
 *     well its resolution fits.
 *   * Among those, the **smallest whose short side still reaches
 *     [VIDEO_NOTE_MIN_SHORT_SIDE]**, because the square is cropped from the
 *     short side and the spec caps a note at 480 px a side: the smallest
 *     qualifying profile is the one closest to that cap from above.
 *   * If every interoperable profile is below that floor (a device offering
 *     only CIF-class profiles), the **largest** of them is used — smaller than
 *     the spec's box, but still a clip every client can play, which is the
 *     property that matters.
 *   * If there is no interoperable profile at all, the answer is `null` and
 *     the caller refuses to record (UX spec §8: a client that cannot record
 *     the format disables recording rather than sending one only some clients
 *     can play).
 *
 * Note the floor is a *minimum*, not a cap that this rule can enforce: CameraX
 * offers a fixed ladder of qualities, so when the smallest interoperable
 * profile is 720p the note is 720x720 rather than 480x480. Oversized is a
 * bandwidth cost; wrong-codec is an unplayable message.
 */

/** ADR-0047 §2 — the only video codec a video note may carry. */
public const val VIDEO_NOTE_VIDEO_CODEC: String = "video/avc"

/** ADR-0047 §2 — the only audio codec a video note may carry. */
public const val VIDEO_NOTE_AUDIO_CODEC: String = "audio/mp4a-latm"

/**
 * The smallest short side worth selecting: the spec's 480 px square. A profile
 * below this still records (see the rule above), it is simply not preferred.
 */
public const val VIDEO_NOTE_MIN_SHORT_SIDE: Int = 480

/**
 * One camcorder profile as the selection rule sees it: a quality handle [Q]
 * (CameraX `Quality` in the app, a name in the tests) plus the codecs and the
 * frame size that profile records. [audioMime] is `null` when the profile
 * declares no audio track at all.
 */
public data class VideoNoteProfileOption<Q>(
    val quality: Q,
    val videoMime: String?,
    val audioMime: String?,
    val width: Int,
    val height: Int,
) {
    /** The side the square crop is taken from. */
    internal val shortSide: Int get() = minOf(width, height)

    /**
     * `true` when a clip recorded with this profile is the format every Lakko
     * client plays: H.264 video and AAC audio, with a usable frame size.
     */
    internal val interoperable: Boolean
        get() = width > 0 && height > 0 &&
            videoMime.equals(VIDEO_NOTE_VIDEO_CODEC, ignoreCase = true) &&
            audioMime.equals(VIDEO_NOTE_AUDIO_CODEC, ignoreCase = true)
}

/**
 * The profile a video note records with, or `null` when this camera offers no
 * H.264 + AAC profile at all — in which case the caller must refuse the take
 * rather than record a clip other clients cannot play.
 *
 * Ties (two profiles with the same short side) are broken on the long side, so
 * the choice is deterministic rather than dependent on the order CameraX
 * happened to list its qualities in.
 */
public fun <Q> chooseVideoNoteProfile(
    options: List<VideoNoteProfileOption<Q>>,
): VideoNoteProfileOption<Q>? {
    val playable = options.filter { it.interoperable }
    if (playable.isEmpty()) return null
    val bySize = compareBy<VideoNoteProfileOption<Q>>({ it.shortSide }, { maxOf(it.width, it.height) })
    return playable
        .filter { it.shortSide >= VIDEO_NOTE_MIN_SHORT_SIDE }
        .minWithOrNull(bySize)
        ?: playable.maxWithOrNull(bySize)
}

/**
 * Whether a recorded file's actual track codecs are the interoperable pair.
 * Read back from the finished file: the profile says what the device promised,
 * this says what it produced.
 */
public fun videoNoteCodecsInteroperable(videoMime: String?, audioMime: String?): Boolean =
    videoMime.equals(VIDEO_NOTE_VIDEO_CODEC, ignoreCase = true) &&
        audioMime.equals(VIDEO_NOTE_AUDIO_CODEC, ignoreCase = true)
