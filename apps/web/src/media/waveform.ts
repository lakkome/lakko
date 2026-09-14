// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 §1 — the voice-message waveform: at most `MAX_WAVEFORM_BYTES`
 * amplitude bytes carried in the envelope so a recipient can draw the bubble
 * before the audio is downloaded.
 *
 * The recorder samples a level (0..1, RMS of the analyser's time-domain
 * window) at a steady cadence while capturing; `downsampleWaveform` folds that
 * series into the envelope's bytes. Bucketing takes the PEAK of each bucket
 * rather than the mean, so a short loud syllable survives the fold instead of
 * being averaged away into the silence around it.
 */

/** The envelope bound — `lakko-attachments` drops a larger waveform. */
export const MAX_WAVEFORM_BYTES = 128;

/**
 * Fold `levels` (non-negative amplitudes, any scale) into at most `buckets`
 * bytes, normalised so the loudest bucket is 255. Returns `undefined` for an
 * empty series — the envelope field is then simply absent. A silent take
 * yields all zeros, which is honest: there was nothing to draw.
 */
export function downsampleWaveform(
  levels: ArrayLike<number>,
  buckets: number = MAX_WAVEFORM_BYTES,
): Uint8Array | undefined {
  const n = levels.length;
  if (n === 0) return undefined;
  const count = Math.max(1, Math.min(Math.floor(buckets), MAX_WAVEFORM_BYTES, n));
  const peaks = new Array<number>(count).fill(0);
  for (let i = 0; i < n; i += 1) {
    const raw = levels[i];
    const v = typeof raw === "number" && Number.isFinite(raw) && raw > 0 ? raw : 0;
    const b = Math.min(count - 1, Math.floor((i * count) / n));
    if (v > (peaks[b] ?? 0)) peaks[b] = v;
  }
  let max = 0;
  for (const p of peaks) if (p > max) max = p;
  const out = new Uint8Array(count);
  if (max === 0) return out;
  for (let i = 0; i < count; i += 1) {
    out[i] = Math.max(0, Math.min(255, Math.round(((peaks[i] ?? 0) / max) * 255)));
  }
  return out;
}

/**
 * Resample stored waveform bytes to `bars` display bars (0..1 each), so every
 * bubble draws the same number of bars whatever length the sender produced.
 * An absent or empty waveform draws a flat, low placeholder rather than
 * nothing, so the seek slider still has a visible track.
 */
export function waveformBars(waveform: Uint8Array | undefined, bars: number): number[] {
  const count = Math.max(1, Math.floor(bars));
  if (waveform === undefined || waveform.length === 0) {
    return new Array<number>(count).fill(0.15);
  }
  const out = new Array<number>(count);
  for (let i = 0; i < count; i += 1) {
    const src = Math.min(waveform.length - 1, Math.floor((i * waveform.length) / count));
    // A floor keeps silent stretches visible as a track.
    out[i] = Math.max(0.08, (waveform[src] ?? 0) / 255);
  }
  return out;
}

/** RMS of a time-domain window, 0..1 — the live level meter's reading. */
export function rmsLevel(samples: ArrayLike<number>): number {
  const n = samples.length;
  if (n === 0) return 0;
  let sum = 0;
  for (let i = 0; i < n; i += 1) {
    const s = samples[i] ?? 0;
    sum += s * s;
  }
  return Math.min(1, Math.sqrt(sum / n));
}
