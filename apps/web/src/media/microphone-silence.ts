// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX spec §2 "Hearing nothing" — when the measured input level has stayed at
 * zero for {@link SILENCE_WARNING_MS}, the recording bar says
 * `No sound from the microphone`, and clears the moment any level is measured.
 *
 * The rule lives here, as a fold over samples with no clock and no recorder in
 * it, for the reason the spec gives: silence was invisible from inside the app.
 * A chain of voice messages recorded on an emulator whose microphone toggle was
 * off arrived as digital silence and nothing anywhere said so — the cause was
 * found by measuring the file with an external tool. Every client already
 * samples the level to drive the live meter, so the signal the warning needs was
 * one each of them was holding and none of them read. Put the same condition
 * inline in the bar and it becomes a condition nothing can run.
 *
 * The Kotlin twin is `me.lakko.app.media.MicrophoneSilence` and the Swift one is
 * `MicrophoneSilence` in `Lakko/Media/VoiceLatency.swift`; all three carry the
 * same two constants over the same 0..1 linear scale, so the same room produces
 * the same warning on every client. This is one of the rules that has no home in
 * `lakko-ui-rules` yet — it is three hand-written copies, and the drift risk is
 * real; the reason it is not in Rust is that the fold is four lines while the
 * FFI round trip for a value sampled twenty times a second is not obviously
 * cheaper than the copies. Revisit that if a fourth client appears.
 *
 * What it is NOT: a loudness threshold. The take is never blocked and never
 * discarded on this account — a quiet room is not an error, and refusing to send
 * would be the client overruling the person about their own message — so the
 * warning is only ever a warning, and a person who speaks softly must never see
 * it. {@link SILENCE_LEVEL_FLOOR} is what keeps that true.
 */
import { t } from "../l10n";

/**
 * §2 — how long the level must stay at zero before the bar says so.
 *
 * The spec's number, shared by all three clients: below about a second a
 * microphone that is merely slow to warm up would be accused of being dead, and
 * past three the person has already committed to the take.
 */
export const SILENCE_WARNING_MS = 2000;

/**
 * The level at or below which the meter reported NOTHING, rather than something
 * quiet.
 *
 * "Zero" is the spec's word, and it is exact on this platform as it is on
 * Android: an `AnalyserNode` fed a muted track hands back a buffer of exact
 * zeros, so `rmsLevel` returns exactly `0`. It is NOT exact everywhere — iOS
 * reads `-160` dBFS for digital silence, which is `1e-8` on this same 0..1
 * linear scale, and an exact-zero test would therefore never fire there at all.
 * So the three clients share one floor rather than three notions of zero.
 *
 * `1e-6` is -120 dBFS, and it sits in a gap two orders of magnitude wide on
 * either side:
 *
 *   * a hundred times ABOVE the `1e-8` an iOS meter reports for silence, so a
 *     dead microphone lands under it on every platform;
 *   * thirty times BELOW one least-significant bit of 16-bit PCM
 *     (`1 / 32767` = `3.05e-5`, -90.3 dBFS), the quietest thing any of the three
 *     recorders can represent as non-silence.
 *
 * So the floor separates "the meter reported its own silence" from "one bit of
 * real signal", and never a soft voice from a loud one.
 *
 * CHOSEN from those two ranges, not measured — no device was available to read a
 * real microphone's noise floor on. If a real device reports something above
 * this from a dead input, this is the number to revisit, not the two seconds.
 */
export const SILENCE_LEVEL_FLOOR = 1e-6;

/** The one string the bar shows; §2's wording, so a test can pin it. */
export const SILENT_MIC_NOTICE = (): string => t("chat.record.silent_mic");

/**
 * Did this sample hear anything at all? A non-finite reading counts as silence
 * rather than as sound — `NaN > floor` is already `false`, and a meter that
 * reports nonsense has not heard anything either.
 */
export function hasInputSound(level: number): boolean {
  return level > SILENCE_LEVEL_FLOOR;
}

/**
 * How long the current unbroken run of silence has been going, and whether it
 * has earned the warning yet. One per take, folded forward by
 * {@link observeLevel}.
 */
export interface SilenceWatch {
  /** When the current run of no-sound samples began, in the take's own elapsed
   *  milliseconds, or `null` before the first sample and after anything was
   *  heard. */
  readonly silentSinceMs: number | null;
  /** Whether the bar should be saying {@link SILENT_MIC_NOTICE} right now. Read
   *  straight off the fold rather than recomputed, so exactly one place decides
   *  it. */
  readonly warns: boolean;
}

/** A take that has not been sampled yet. */
export const QUIET_WATCH: SilenceWatch = { silentSinceMs: null, warns: false };

/**
 * Fold one level sample, taken at `atMs` of the take's elapsed time, into
 * `watch`.
 *
 * Anything heard resets the run outright — §2's "clears the moment any level is
 * measured" — so the warning describes the present rather than passing a verdict
 * on the take: a microphone unmuted mid-recording, or a person who simply had
 * not started speaking, must not carry an accusation through the rest of it.
 *
 * A run is timed from the first silent sample, so a take that opens with sound
 * and goes quiet later is measured from when it went quiet. A sample whose `atMs`
 * is below where the run began reads as no time passed, which is the safe way
 * round: it delays the warning rather than raising a false one.
 */
export function observeLevel(watch: SilenceWatch, level: number, atMs: number): SilenceWatch {
  if (hasInputSound(level)) return QUIET_WATCH;
  const since = watch.silentSinceMs ?? atMs;
  return { silentSinceMs: since, warns: atMs - since >= SILENCE_WARNING_MS };
}
