// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

/**
 * ADR-0047 §1 — the voice-message waveform: at most [MAX_WAVEFORM_BYTES]
 * amplitude bytes carried in the envelope so a recipient can draw the bubble
 * before the audio is downloaded.
 *
 * Port of `apps/web/src/media/waveform.ts`. The recorder samples a level at a
 * steady cadence while capturing (`MediaRecorder.getMaxAmplitude`, see
 * [levelFromMaxAmplitude]); [downsampleWaveform] folds that series into the
 * envelope's bytes. Bucketing takes the PEAK of each bucket rather than the
 * mean, so a short loud syllable survives the fold instead of being averaged
 * away into the silence around it.
 */

/** The envelope bound — `lakko-attachments` drops a larger waveform. */
public const val MAX_WAVEFORM_BYTES: Int = 128

/** `MediaRecorder.getMaxAmplitude` reports a 16-bit PCM peak. */
public const val MAX_RECORDER_AMPLITUDE: Int = 32767

/**
 * Fold [levels] (non-negative amplitudes, any scale) into at most [buckets]
 * bytes, normalised so the loudest bucket is 255. Non-finite and negative
 * samples count as silence.
 *
 * Returns `null` for an empty series AND for an all-silent one — the envelope
 * field is then simply absent. This used to hand back a non-empty row of
 * zeros, on the reasoning that it was honest about having measured nothing.
 * It was not: [waveformBars] floors a zero byte at `0.08` while an ABSENT
 * waveform draws the `0.15` placeholder, so "we measured nothing" rendered
 * FLATTER than "we have no waveform" — the wrong way round, and at that height
 * every bar is wider than it is tall, which is the dotted line a silent
 * capture (an emulator, a microphone whose amplitude never reports) put in the
 * bubble. There is nothing to draw either way; say so once, in the way that
 * draws the better placeholder.
 */
public fun downsampleWaveform(
    levels: List<Double>,
    buckets: Int = MAX_WAVEFORM_BYTES,
): ByteArray? {
    val n = levels.size
    if (n == 0) return null
    val count = maxOf(1, minOf(buckets, MAX_WAVEFORM_BYTES, n))
    val peaks = DoubleArray(count)
    for (i in 0 until n) {
        val raw = levels[i]
        val v = if (raw.isFinite() && raw > 0.0) raw else 0.0
        val b = minOf(count - 1, ((i.toLong() * count) / n).toInt())
        if (v > peaks[b]) peaks[b] = v
    }
    val max = peaks.maxOrNull() ?: 0.0
    if (max == 0.0) return null
    val out = ByteArray(count)
    for (i in 0 until count) {
        val scaled = Math.round((peaks[i] / max) * 255.0).toInt().coerceIn(0, 255)
        out[i] = scaled.toByte()
    }
    return out
}

/**
 * Resample stored waveform bytes to [bars] display bars (0..1 each), so every
 * bubble draws the same number of bars whatever length the sender produced.
 * An absent or empty waveform draws a flat, low placeholder rather than
 * nothing, so the seek slider still has a visible track.
 */
public fun waveformBars(waveform: ByteArray?, bars: Int): List<Float> {
    val count = maxOf(1, bars)
    if (waveform == null || waveform.isEmpty()) return List(count) { 0.15f }
    return List(count) { i ->
        val src = minOf(waveform.size - 1, (i.toLong() * waveform.size / count).toInt())
        val unsigned = waveform[src].toInt() and 0xFF
        // A floor keeps silent stretches visible as a track.
        maxOf(0.08f, unsigned / 255f)
    }
}

/** A `getMaxAmplitude` reading as a 0..1 level. Out-of-range readings clamp. */
public fun levelFromMaxAmplitude(amplitude: Int): Double =
    (amplitude.coerceIn(0, MAX_RECORDER_AMPLITUDE)).toDouble() / MAX_RECORDER_AMPLITUDE
