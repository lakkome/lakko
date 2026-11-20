// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `lakko_segment.wasm` (ADR-0089, crate `lakko-segment-wasm`) as the
 * background engine uses it. Import-free: instantiated with `{}`. One
 * instance is one camera's processing (the model, its health watch and the
 * exchange buffers live in the instance).
 *
 * Runs in the background worker, or on the main thread where the browser has
 * no frame streams (the canvas path of `background-stage.ts`).
 */

import type { FrameSegmenter } from "./background-engine";

/** The model's input picture and mask size (`lakko_segment::INPUT_*`). */
export const SEG_INPUT_WIDTH = 256;
export const SEG_INPUT_HEIGHT = 144;

interface SegmentExports {
  readonly memory: WebAssembly.Memory;
  lakko_seg_init(nowMs: number): number;
  lakko_seg_input_ptr(): number;
  lakko_seg_mask_ptr(): number;
  lakko_seg_run(nowMs: number, tookMs: number): number;
  lakko_seg_on_frame(nowMs: number): number;
  lakko_seg_restart(nowMs: number): number;
  lakko_seg_frame_action(selected: number, state: number): number;
  lakko_seg_blur_divisor(strength: number, frameHeight: number): number;
  lakko_seg_blur_sigma(strength: number, frameHeight: number): number;
  lakko_seg_blur_radius(strength: number, frameHeight: number): number;
  lakko_seg_cover_rect(sw: number, sh: number, dw: number, dh: number): number;
  lakko_seg_model_input_rect(frameWidth: number, frameHeight: number): number;
  lakko_seg_builtin_count(): number;
  lakko_seg_builtin_render(index: number, width: number, height: number): number;
}

const REQUIRED = [
  "memory",
  "lakko_seg_init",
  "lakko_seg_input_ptr",
  "lakko_seg_mask_ptr",
  "lakko_seg_run",
  "lakko_seg_on_frame",
  "lakko_seg_restart",
  "lakko_seg_frame_action",
  "lakko_seg_blur_divisor",
  "lakko_seg_blur_sigma",
  "lakko_seg_blur_radius",
  "lakko_seg_cover_rect",
  "lakko_seg_model_input_rect",
  "lakko_seg_builtin_count",
  "lakko_seg_builtin_render",
] as const;

/** Blur parameters for one frame height (`lakko_segment::blur_parameters`). */
export interface BlurParameters {
  readonly divisor: number;
  readonly sigma: number;
  readonly radius: number;
}

/** What the compositor needs from the shared rules. */
export interface CompositeRules {
  blur(strong: boolean, frameHeight: number): BlurParameters;
  /** `[x, y, w, h]` of the part of a picture that fills a frame. */
  coverRect(srcW: number, srcH: number, dstW: number, dstH: number): [number, number, number, number];
  /** `[x, y, w, h]` in model pixels where a `frameW` x `frameH` frame is
   *  drawn into the model input (whole, shape kept, centred). */
  modelInputRect(frameW: number, frameH: number): [number, number, number, number];
  /** Built-in background `index` drawn at `width` x `height` (RGBA, a copy),
   *  or `null`. */
  builtin(index: number, width: number, height: number): Uint8Array | null;
}

export class SegmentInstance implements FrameSegmenter, CompositeRules {
  private constructor(private readonly ex: SegmentExports) {}

  /** Instantiates `module`; rejects when it lacks an export this code needs. */
  static async create(module: WebAssembly.Module): Promise<SegmentInstance> {
    const instance = await WebAssembly.instantiate(module, {});
    const exports = instance.exports as Record<string, unknown>;
    for (const name of REQUIRED) {
      if (exports[name] === undefined) throw new Error(`lakko_segment.wasm lacks ${name}`);
    }
    return new SegmentInstance(exports as unknown as SegmentExports);
  }

  /** Loads the model; `false` when it could not (the watch is Failed). */
  init(nowMs: number): boolean {
    return this.ex.lakko_seg_init(nowMs) === 1;
  }

  // Views are made per call: a growing memory detaches older ones.
  inputBuffer(): Uint8Array {
    return new Uint8Array(this.ex.memory.buffer, this.ex.lakko_seg_input_ptr(), SEG_INPUT_WIDTH * SEG_INPUT_HEIGHT * 4);
  }

  mask(): Uint8Array {
    return new Uint8Array(this.ex.memory.buffer, this.ex.lakko_seg_mask_ptr(), SEG_INPUT_WIDTH * SEG_INPUT_HEIGHT);
  }

  onFrame(nowMs: number): number {
    return this.ex.lakko_seg_on_frame(nowMs);
  }

  segment(nowMs: number, tookMs: number): number {
    return this.ex.lakko_seg_run(nowMs, tookMs);
  }

  restart(nowMs: number): number {
    return this.ex.lakko_seg_restart(nowMs);
  }

  frameAction(selected: boolean, state: number): number {
    return this.ex.lakko_seg_frame_action(selected ? 1 : 0, state);
  }

  blur(strong: boolean, frameHeight: number): BlurParameters {
    const s = strong ? 1 : 0;
    return {
      divisor: this.ex.lakko_seg_blur_divisor(s, frameHeight),
      sigma: this.ex.lakko_seg_blur_sigma(s, frameHeight),
      radius: this.ex.lakko_seg_blur_radius(s, frameHeight),
    };
  }

  coverRect(srcW: number, srcH: number, dstW: number, dstH: number): [number, number, number, number] {
    const at = this.ex.lakko_seg_cover_rect(srcW, srcH, dstW, dstH);
    const v = new Float32Array(this.ex.memory.buffer, at, 4);
    return [v[0] ?? 0, v[1] ?? 0, v[2] ?? srcW, v[3] ?? srcH];
  }

  modelInputRect(frameW: number, frameH: number): [number, number, number, number] {
    const at = this.ex.lakko_seg_model_input_rect(frameW, frameH);
    const v = new Float32Array(this.ex.memory.buffer, at, 4);
    return [v[0] ?? 0, v[1] ?? 0, v[2] ?? SEG_INPUT_WIDTH, v[3] ?? SEG_INPUT_HEIGHT];
  }

  builtin(index: number, width: number, height: number): Uint8Array | null {
    if (index < 0 || index >= this.ex.lakko_seg_builtin_count()) return null;
    const at = this.ex.lakko_seg_builtin_render(index, width, height);
    if (at === 0) return null;
    return new Uint8Array(this.ex.memory.buffer, at, width * height * 4).slice();
  }
}
