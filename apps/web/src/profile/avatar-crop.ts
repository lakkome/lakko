// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * docs/ux/profile-screen.md section 2.2 — the crop screen's arithmetic, and
 * the encode loops, with no DOM.
 *
 * The picture is drawn behind a circle of side `viewport` (screen pixels),
 * centred. At zoom 1 the picture just fills the circle (its shorter side
 * equals the circle's); the person zooms up to 4x and drags. `pan` is the
 * picture's centre offset from the circle's centre, in screen pixels, always
 * clamped so the circle stays covered.
 */

import { STILL_MAX_BYTES, STILL_MAX_SIDE, VIDEO_MAX_DURATION_MS } from "./avatar-model";

export const MIN_ZOOM = 1;
export const MAX_ZOOM = 4;

export interface CropState {
  readonly zoom: number;
  readonly panX: number;
  readonly panY: number;
}

export const INITIAL_CROP: CropState = { zoom: 1, panX: 0, panY: 0 };

/** Screen pixels per source pixel at `zoom`. */
export function cropScale(sourceW: number, sourceH: number, viewport: number, zoom: number): number {
  const fill = viewport / Math.max(1, Math.min(sourceW, sourceH));
  return fill * clampZoom(zoom);
}

export function clampZoom(zoom: number): number {
  if (!Number.isFinite(zoom)) return MIN_ZOOM;
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, zoom));
}

/** Clamp a crop so the picture still covers the circle. */
export function clampCrop(sourceW: number, sourceH: number, viewport: number, crop: CropState): CropState {
  const zoom = clampZoom(crop.zoom);
  const scale = cropScale(sourceW, sourceH, viewport, zoom);
  const maxX = Math.max(0, (sourceW * scale - viewport) / 2);
  const maxY = Math.max(0, (sourceH * scale - viewport) / 2);
  const clamp = (v: number, m: number): number => (Number.isFinite(v) ? Math.min(m, Math.max(-m, v)) : 0);
  return { zoom, panX: clamp(crop.panX, maxX), panY: clamp(crop.panY, maxY) };
}

/** Drag by `(dx, dy)` screen pixels. */
export function dragCrop(
  sourceW: number,
  sourceH: number,
  viewport: number,
  crop: CropState,
  dx: number,
  dy: number,
): CropState {
  return clampCrop(sourceW, sourceH, viewport, { ...crop, panX: crop.panX + dx, panY: crop.panY + dy });
}

/** Zoom to `zoom`, keeping the point under the circle's centre fixed. */
export function zoomCrop(sourceW: number, sourceH: number, viewport: number, crop: CropState, zoom: number): CropState {
  const next = clampZoom(zoom);
  const ratio = next / clampZoom(crop.zoom);
  return clampCrop(sourceW, sourceH, viewport, { zoom: next, panX: crop.panX * ratio, panY: crop.panY * ratio });
}

/** The square of the source the circle shows, in source pixels. */
export interface SourceSquare {
  readonly sx: number;
  readonly sy: number;
  readonly side: number;
}

export function sourceSquare(sourceW: number, sourceH: number, viewport: number, crop: CropState): SourceSquare {
  const c = clampCrop(sourceW, sourceH, viewport, crop);
  const scale = cropScale(sourceW, sourceH, viewport, c.zoom);
  const side = Math.min(sourceW, sourceH, viewport / scale);
  // The source point under the circle's centre.
  const cx = sourceW / 2 - c.panX / scale;
  const cy = sourceH / 2 - c.panY / scale;
  const sx = Math.min(Math.max(0, cx - side / 2), sourceW - side);
  const sy = Math.min(Math.max(0, cy - side / 2), sourceH - side);
  return { sx, sy, side };
}

/** The output side for a source square: its side, capped, at least 1.
 *  Even when `even` (an H.264 encoder wants even dimensions). */
export function outputSide(sourceSide: number, maxSide: number, even = false): number {
  const capped = Math.max(1, Math.floor(Math.min(sourceSide, maxSide)));
  if (!even) return capped;
  return Math.max(2, Math.floor(capped / 2) * 2);
}

/** The JPEG qualities the still is tried at, best first. */
export const JPEG_QUALITIES: readonly number[] = [0.92, 0.85, 0.78, 0.7, 0.6, 0.5, 0.4, 0.3];

/**
 * Encode the still under the byte bound: each side (starting at `side`, then
 * smaller by a fifth, never under `minSide`) is tried at decreasing quality
 * until one fits. `null` when nothing fits.
 */
export async function encodeUnderLimit(
  side: number,
  encode: (side: number, quality: number) => Promise<Blob | null>,
  limit: number = STILL_MAX_BYTES,
  minSide = 96,
): Promise<{ readonly blob: Blob; readonly side: number } | null> {
  let s = Math.min(STILL_MAX_SIDE, Math.max(1, Math.floor(side)));
  for (;;) {
    for (const q of JPEG_QUALITIES) {
      const blob = await encode(s, q);
      if (blob !== null && blob.size > 0 && blob.size <= limit) return { blob, side: s };
    }
    if (s <= minSide) return null;
    s = Math.max(minSide, Math.floor(s * 0.8));
  }
}

/** The latest a profile video's 10 seconds may start, in ms. */
export function maxVideoStartMs(durationMs: number): number {
  if (!Number.isFinite(durationMs) || durationMs <= VIDEO_MAX_DURATION_MS) return 0;
  return Math.floor(durationMs - VIDEO_MAX_DURATION_MS);
}

export function clampVideoStart(startMs: number, durationMs: number): number {
  if (!Number.isFinite(startMs)) return 0;
  return Math.min(maxVideoStartMs(durationMs), Math.max(0, Math.floor(startMs)));
}

/** How long the kept part runs, in ms (1..=10000), or `null` for a video with
 *  no usable length. */
export function keptVideoDurationMs(durationMs: number, startMs: number): number | null {
  if (!Number.isFinite(durationMs) || durationMs <= 0) return null;
  const start = clampVideoStart(startMs, durationMs);
  const kept = Math.floor(Math.min(VIDEO_MAX_DURATION_MS, durationMs - start));
  return kept >= 1 ? kept : null;
}

/**
 * Section 2.2 fallback — a device that cannot re-encode accepts a picked video
 * only when it is usable as it is: MP4, square, within the side, size and
 * duration bounds, with no sound (or sound that cannot be ruled out: refused).
 */
export function usableWithoutReencode(v: {
  readonly mp4: boolean;
  readonly width: number;
  readonly height: number;
  readonly bytes: number;
  readonly durationMs: number;
  readonly hasAudio: boolean | null;
  readonly maxSide: number;
  readonly maxBytes: number;
}): boolean {
  return (
    v.mp4 &&
    v.width > 0 &&
    v.width === v.height &&
    v.width <= v.maxSide &&
    v.bytes > 0 &&
    v.bytes <= v.maxBytes &&
    v.durationMs > 0 &&
    v.durationMs <= VIDEO_MAX_DURATION_MS &&
    v.hasAudio === false
  );
}
