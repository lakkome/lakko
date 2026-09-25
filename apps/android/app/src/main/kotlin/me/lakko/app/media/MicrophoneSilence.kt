// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

/**
 * UX spec §2 "Hearing nothing" — when the measured input level has stayed at
 * zero for [SILENCE_WARNING_MS], the recording bar says
 * `No sound from the microphone`, and clears the moment any level is measured.
 *
 * The rule lives here, as a fold over samples with no clock and no recorder in
 * it, for the reason the spec gives: silence was invisible from inside the app.
 * A chain of voice messages recorded on an emulator whose microphone toggle was
 * off arrived as digital silence and nothing anywhere said so, and the cause was
 * found by measuring the file with an external tool. Every client already
 * samples the level to draw the live meter ([VoiceRecorder.sampleLevel]), so the
 * signal the warning needs was one each of them was holding and none of them
 * read. Put the same condition inline in the bar and it is a condition nothing
 * can run.
 *
 * What it is NOT: a loudness threshold. The take is never blocked and never
 * discarded on this account — a quiet room is not an error, and refusing to
 * send would be the client overruling the person about their own message — so
 * the warning is only ever a warning, and a person who speaks softly must never
 * see it. [SILENCE_LEVEL_FLOOR] is what keeps that true.
 */

/**
 * §2 — how long the level must stay at zero before the bar says so.
 *
 * The spec's number, shared by all three clients so the same room produces the
 * same warning on each: below about a second a microphone that is merely slow to
 * warm up would be accused of being dead, and past three the person has already
 * committed to the take.
 */
public const val SILENCE_WARNING_MS: Long = 2_000L

/**
 * The level at or below which the meter reported NOTHING, rather than something
 * quiet.
 *
 * "Zero" is the spec's word and it is exact on this platform —
 * `MediaRecorder.getMaxAmplitude` is an integer and a dead microphone returns
 * `0` — but it is not exact on every platform: iOS reads `-160` dBFS for digital
 * silence, which is `1e-8` once converted to the same 0..1 linear amplitude, and
 * an exact-zero test would therefore never fire there at all. So the three
 * clients share one floor over the one scale instead.
 *
 * `1e-6` is -120 dBFS, and it sits in a gap two orders of magnitude wide on
 * either side:
 *
 *   * a hundred times ABOVE the `1e-8` an iOS meter reports for silence, so a
 *     dead microphone lands under it on every platform;
 *   * thirty times BELOW one least-significant bit of 16-bit PCM
 *     (`1 / 32767` = `3.05e-5`, -90.3 dBFS), which is the quietest thing either
 *     recorder can represent as non-silence.
 *
 * So nothing that can be heard, and nothing a 16-bit converter can encode as
 * anything but silence, can sit beneath it: the floor separates "the meter
 * reported its own silence" from "one bit of real signal", and never a soft
 * voice from a loud one.
 *
 * CHOSEN from those two ranges, not measured — no device was available to read a
 * real microphone's noise floor on. If a real device turns out to report
 * something above this from a dead input, this is the number to revisit, not the
 * two seconds.
 */
public const val SILENCE_LEVEL_FLOOR: Double = 1e-6

/**
 * Did this sample hear anything at all? A non-finite reading counts as silence
 * rather than as sound: `NaN > floor` is already `false`, and a meter that
 * reports nonsense has not heard anything either.
 */
public fun hasInputSound(level: Double): Boolean = level > SILENCE_LEVEL_FLOOR

/**
 * How long the current unbroken run of silence has been going, and whether it
 * has earned the warning yet. One per take, folded forward by [observeLevel].
 *
 * @property silentSinceMs when the current run of no-sound samples began, in the
 *   take's own elapsed milliseconds, or `null` before the first sample and after
 *   anything was heard.
 * @property warns whether the bar should be saying `No sound from the
 *   microphone` right now. Read straight off the fold rather than recomputed, so
 *   there is exactly one place that decides it.
 */
public data class SilenceWatch(
    val silentSinceMs: Long? = null,
    val warns: Boolean = false,
)

/**
 * Fold one level sample, taken at [atMs] of the take's elapsed time, into
 * [watch].
 *
 * Anything heard resets the run outright — §2's "clears the moment any level is
 * measured" — so the warning describes the present rather than passing a verdict
 * on the take: a microphone unmuted mid-recording, or a person who simply had
 * not started speaking, must not carry an accusation through the rest of it.
 *
 * A run is timed from the first silent sample, so a take that opens with sound
 * and goes quiet later is measured from when it went quiet. [atMs] runs off a
 * monotonic clock; a value below where the run began would read as no time
 * passed, which is the safe way round — it delays the warning rather than
 * raising a false one.
 */
public fun observeLevel(watch: SilenceWatch, level: Double, atMs: Long): SilenceWatch {
    if (hasInputSound(level)) return SilenceWatch()
    val since = watch.silentSinceMs ?: atMs
    return SilenceWatch(silentSinceMs = since, warns = atMs - since >= SILENCE_WARNING_MS)
}
