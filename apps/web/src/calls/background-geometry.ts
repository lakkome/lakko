// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where a camera frame sits in the model's 256 x 144 input picture, and so
 * where its mask is read back (ADR-0089).
 *
 * The rectangle comes from the shared rule (`lakko_segment::model_input_rect`:
 * the whole upright frame, its shape kept, centred, black bars around it), so
 * a portrait camera is not squeezed and every client draws the same thing.
 * The GPU draws into whole pixels, so the rectangle is rounded to them once,
 * here, and that same rounded rectangle is used both to draw the frame into
 * the model input and to read the mask back: the two cannot drift apart. A
 * 1280 x 720 frame gives the whole input.
 */

import { SEG_INPUT_HEIGHT, SEG_INPUT_WIDTH } from "./background-segment-wasm";

/** A rectangle in whole model pixels, top-left origin. */
export interface ModelViewport {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

/** Rounds the shared rule's rectangle `[x, y, w, h]` (model pixels) to whole
 *  pixels inside the input, at least one pixel each way. */
export function modelViewport(rect: readonly [number, number, number, number]): ModelViewport {
  const clamp = (v: number, lo: number, hi: number): number => Math.min(hi, Math.max(lo, v));
  const finite = (v: number, d: number): number => (Number.isFinite(v) ? v : d);
  const x = clamp(Math.round(finite(rect[0], 0)), 0, SEG_INPUT_WIDTH - 1);
  const y = clamp(Math.round(finite(rect[1], 0)), 0, SEG_INPUT_HEIGHT - 1);
  const width = clamp(Math.round(finite(rect[2], SEG_INPUT_WIDTH)), 1, SEG_INPUT_WIDTH - x);
  const height = clamp(Math.round(finite(rect[3], SEG_INPUT_HEIGHT)), 1, SEG_INPUT_HEIGHT - y);
  return { x, y, width, height };
}

/** The viewport as mask texture coordinates `[u, v, du, dv]`: a frame point
 *  at `(s, t)` in 0..1 reads the mask at `(u + s * du, v + t * dv)`. */
export function maskUvRect(view: ModelViewport): [number, number, number, number] {
  return [
    view.x / SEG_INPUT_WIDTH,
    view.y / SEG_INPUT_HEIGHT,
    view.width / SEG_INPUT_WIDTH,
    view.height / SEG_INPUT_HEIGHT,
  ];
}

/** The mask pixel (column, row) a frame pixel reads, by the same mapping the
 *  blend shader uses (pixel centres). */
export function maskPixelOf(
  view: ModelViewport,
  frameWidth: number,
  frameHeight: number,
  fx: number,
  fy: number,
): [number, number] {
  const [u, v, du, dv] = maskUvRect(view);
  const s = (fx + 0.5) / Math.max(1, frameWidth);
  const t = (fy + 0.5) / Math.max(1, frameHeight);
  const col = Math.min(SEG_INPUT_WIDTH - 1, Math.floor((u + s * du) * SEG_INPUT_WIDTH));
  const row = Math.min(SEG_INPUT_HEIGHT - 1, Math.floor((v + t * dv) * SEG_INPUT_HEIGHT));
  return [col, row];
}
