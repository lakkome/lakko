// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A camera frame goes into the model input whole, upright and with its shape
 * kept (the shared Rust rule `model_input_rect`), and its mask is read back
 * through the same rectangle: frame -> model rectangle -> mask lookup.
 */

import { beforeAll, describe, expect, it } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { backgroundModelInputRect } from "../../wasm/lakko_ffi_wasm";
import { maskPixelOf, maskUvRect, modelViewport } from "./background-geometry";
import { SEG_INPUT_HEIGHT, SEG_INPUT_WIDTH, SegmentInstance } from "./background-segment-wasm";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function rectOf(fw: number, fh: number): [number, number, number, number] {
  const r = backgroundModelInputRect(fw, fh);
  return [r[0] ?? 0, r[1] ?? 0, r[2] ?? 0, r[3] ?? 0];
}

const FRAMES: [number, number][] = [
  [1280, 720],
  [640, 360],
  [720, 1280],
  [480, 640],
  [640, 480],
  [1080, 1920],
  [1920, 1080],
  [1000, 1000],
  [2560, 1080],
];

describe("frame -> model rectangle -> mask lookup", () => {
  it("a 16:9 frame fills the whole input, so a landscape webcam is unchanged", () => {
    const view = modelViewport(rectOf(1280, 720));
    expect(view).toEqual({ x: 0, y: 0, width: SEG_INPUT_WIDTH, height: SEG_INPUT_HEIGHT });
    expect(maskUvRect(view)).toEqual([0, 0, 1, 1]);
    expect(maskPixelOf(view, 1280, 720, 0, 0)).toEqual([0, 0]);
    expect(maskPixelOf(view, 1280, 720, 1279, 719)).toEqual([255, 143]);
  });

  it("a portrait frame keeps its shape, centred with bars at the sides", () => {
    const view = modelViewport(rectOf(720, 1280));
    expect(view.height).toBe(SEG_INPUT_HEIGHT);
    expect(view.width).toBe(81);
    expect(view.y).toBe(0);
    // Centred: the bars on the two sides differ by at most a pixel.
    const left = view.x;
    const right = SEG_INPUT_WIDTH - view.x - view.width;
    expect(Math.abs(left - right)).toBeLessThanOrEqual(1);
  });

  for (const [fw, fh] of FRAMES) {
    it(`${fw}x${fh}: the shape is kept and every frame pixel reads the mask inside the drawn rectangle`, () => {
      const view = modelViewport(rectOf(fw, fh));
      // Shape kept within one model pixel of rounding.
      expect(Math.abs(view.width / view.height - fw / fh) * view.height).toBeLessThanOrEqual(1.01);
      // A mask that is 255 where the frame was drawn and 0 in the bars.
      const mask = new Uint8Array(SEG_INPUT_WIDTH * SEG_INPUT_HEIGHT);
      for (let r = view.y; r < view.y + view.height; r++) {
        mask.fill(255, r * SEG_INPUT_WIDTH + view.x, r * SEG_INPUT_WIDTH + view.x + view.width);
      }
      const stepX = Math.max(1, Math.floor(fw / 97));
      const stepY = Math.max(1, Math.floor(fh / 61));
      const xs = [0, fw - 1];
      const ys = [0, fh - 1];
      for (let x = 0; x < fw; x += stepX) xs.push(x);
      for (let y = 0; y < fh; y += stepY) ys.push(y);
      for (const x of xs) {
        for (const y of ys) {
          const [col, row] = maskPixelOf(view, fw, fh, x, y);
          expect(mask[row * SEG_INPUT_WIDTH + col], `${x},${y} -> ${col},${row}`).toBe(255);
        }
      }
      // Corners map to the rectangle's corners, top stays top, left stays left.
      expect(maskPixelOf(view, fw, fh, 0, 0)).toEqual([view.x, view.y]);
      expect(maskPixelOf(view, fw, fh, fw - 1, fh - 1)).toEqual([view.x + view.width - 1, view.y + view.height - 1]);
    });
  }

  it("a degenerate rectangle still gives a pixel inside the input", () => {
    const view = modelViewport([Number.NaN, -5, 0, 1e9]);
    expect(view.x).toBe(0);
    expect(view.y).toBe(0);
    expect(view.width).toBeGreaterThanOrEqual(1);
    expect(view.y + view.height).toBeLessThanOrEqual(SEG_INPUT_HEIGHT);
  });
});

const segmentWasm = fileURLToPath(new URL("../../wasm/lakko_segment.wasm", import.meta.url));

describe.skipIf(!existsSync(segmentWasm))("the worker's rule", () => {
  it("lakko_segment.wasm gives the same rectangle as the main module", async () => {
    const seg = await SegmentInstance.create(await WebAssembly.compile(readFileSync(segmentWasm)));
    for (const [fw, fh] of FRAMES) {
      const a = seg.modelInputRect(fw, fh);
      const b = rectOf(fw, fh);
      for (let i = 0; i < 4; i++) expect(a[i]).toBeCloseTo(b[i] as number, 4);
    }
  }, 60_000);
});
