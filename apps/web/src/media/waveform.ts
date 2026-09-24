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
  // An entirely silent take has no waveform, and saying so is not the same as
  // saying "every bar is zero". A zero row is floored at 0.08 by
  // `waveformBars`, while an ABSENT waveform draws the 0.15 placeholder - so
  // recording the measurement would draw FLATTER than having no measurement
  // at all, which is backwards. At that height a bar is wider than it is
  // tall, which is what turned a waveform into a row of dots. Found on
  // Android, fixed in all three ports.
  if (max === 0) return undefined;
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

/**
 * Whether the bar at `index` of `count` is behind the playhead, so it draws
 * at full strength rather than at the low opacity ahead of it (UX spec
 * section 2: "its bars filling with the text colour as playback passes
 * them"). A bar fills once the playhead passes its MIDPOINT, so the filled
 * edge tracks the position rather than running half a bar ahead or behind.
 *
 * Here rather than in the two components because the bubble and the composer's
 * preview player drew the same waveform by two different rules — the bubble
 * from the bar's midpoint, the preview from its trailing edge — which is one
 * of them being wrong about the same picture.
 *
 * A zero or unknown duration fills nothing: there is no playhead yet.
 */
export function barIsPlayed(index: number, count: number, positionMs: number, durationMs: number): boolean {
  if (!(durationMs > 0) || !(count > 0) || !Number.isFinite(positionMs)) return false;
  return (index + 0.5) / count <= positionMs / durationMs;
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
