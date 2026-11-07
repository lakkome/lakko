// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The real `lakko_noise.wasm` (built by `npm run build:noise`) driven
 * through the same `NoiseKernel` the audio worklet uses, in 128-sample
 * blocks as a worklet hands them over (ADR-0088).
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { NoiseKernel } from "./noise-kernel";

function wasmBytes(): Uint8Array<ArrayBuffer> {
  const path = fileURLToPath(new URL("../../wasm/lakko_noise.wasm", import.meta.url));
  return new Uint8Array(readFileSync(path));
}

/** Deterministic low-passed noise, the stationary kind RNNoise removes
 *  (same generator and filter as the `lakko-noise` Rust test). */
function lowPassedNoise(length: number, amplitude: number, seed: number): Float32Array {
  const out = new Float32Array(length);
  let state = seed >>> 0;
  let acc = 0;
  for (let i = 0; i < length; i++) {
    state = (Math.imul(state, 1_664_525) + 1_013_904_223) >>> 0;
    const unit = (state >>> 8) / (1 << 24);
    const white = (unit * 2 - 1) * amplitude;
    acc = acc * 0.95 + white * 0.05;
    out[i] = acc * 4;
  }
  return out;
}

function rms(samples: Float32Array): number {
  let sum = 0;
  for (const s of samples) sum += s * s;
  return Math.sqrt(sum / samples.length);
}

function runInBlocks(kernel: NoiseKernel, input: Float32Array, block: number): Float32Array {
  const output = new Float32Array(input.length);
  for (let start = 0; start < input.length; start += block) {
    const end = Math.min(start + block, input.length);
    const out = new Float32Array(end - start);
    expect(kernel.process(input.subarray(start, end), out)).toBe(true);
    output.set(out, start);
  }
  return output;
}

describe("NoiseKernel over lakko_noise.wasm", () => {
  it("reports the model's rate, one frame of latency and a block bound", () => {
    const kernel = NoiseKernel.fromBytes(wasmBytes());
    expect(kernel.sampleRate).toBe(48_000);
    expect(kernel.latencySamples).toBe(480);
    expect(kernel.maxBlock).toBeGreaterThanOrEqual(128);
  });

  it("attenuates stationary noise fed as 128-sample render quanta", () => {
    const kernel = NoiseKernel.fromBytes(wasmBytes());
    const input = lowPassedNoise(96_000, 0.1, 42);
    const output = runInBlocks(kernel, input, 128);
    // The first frame of output is the constant delay: silence.
    expect(Array.from(output.subarray(0, kernel.latencySamples)).every((s) => s === 0)).toBe(true);
    const settle = 24_000;
    const rin = rms(input.subarray(settle));
    const rout = rms(output.subarray(settle));
    // Loose on purpose: proves the module is wired up end to end, not its
    // quality, which needs recordings.
    expect(rout).toBeLessThan(rin * 0.1);
    expect(Array.from(output).every((s) => Number.isFinite(s) && s >= -1 && s <= 1)).toBe(true);
  });

  it("gives the same output whatever the block size", () => {
    const input = lowPassedNoise(48_000, 0.2, 7);
    const a = runInBlocks(NoiseKernel.fromBytes(wasmBytes()), input, 128);
    const b = runInBlocks(NoiseKernel.fromBytes(wasmBytes()), input, 480);
    expect(Array.from(a)).toEqual(Array.from(b));
  });

  it("refuses a block longer than its bound or of mismatched length", () => {
    const kernel = NoiseKernel.fromBytes(wasmBytes());
    const big = new Float32Array(kernel.maxBlock + 1);
    expect(kernel.process(big, new Float32Array(big.length))).toBe(false);
    expect(kernel.process(new Float32Array(128), new Float32Array(127))).toBe(false);
  });

  it("throws on bytes that are not the module", () => {
    expect(() => NoiseKernel.fromBytes(new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]))).toThrow();
  });
});
