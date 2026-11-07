// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The audio-thread half of noise suppression (ADR-0088): an
 * `AudioWorkletProcessor` named `lakko-noise` that runs each 128-sample
 * render quantum of the microphone through `lakko_noise.wasm`.
 *
 * Loaded with `audioWorklet.addModule` (Vite bundles it as a worker chunk;
 * `noise-assets.ts`). The page passes the module's bytes in
 * `processorOptions.wasm`; the processor compiles and instantiates them
 * synchronously, which the audio thread allows, and posts `ready` or
 * `failed`. After a failure it passes the input through unchanged and the
 * page swaps the raw microphone back onto the sender, so the person is
 * never left sending silence.
 *
 * The node is created with one explicit input channel, so Web Audio has
 * already mixed the microphone down to mono.
 */

import { NoiseKernel } from "./noise-kernel";

/** The parts of the worklet global scope this file uses; `lib.dom` does
 *  not declare them. */
interface ProcessorBase {
  readonly port: MessagePort;
}
interface WorkletScope {
  AudioWorkletProcessor: new (options?: unknown) => ProcessorBase;
  registerProcessor(name: string, ctor: new (options?: unknown) => ProcessorBase): void;
  sampleRate: number;
}
const scope = globalThis as unknown as WorkletScope;

/** Messages to the page. */
export type NoiseWorkletMessage =
  | { readonly type: "ready"; readonly latencySamples: number }
  | { readonly type: "failed"; readonly reason: string };

function wasmBytesOf(options: unknown): ArrayBuffer | null {
  if (typeof options !== "object" || options === null) return null;
  const processorOptions = (options as { processorOptions?: unknown }).processorOptions;
  if (typeof processorOptions !== "object" || processorOptions === null) return null;
  const wasm = (processorOptions as { wasm?: unknown }).wasm;
  return wasm instanceof ArrayBuffer ? wasm : null;
}

class LakkoNoiseProcessor extends scope.AudioWorkletProcessor {
  private kernel: NoiseKernel | null = null;

  constructor(options?: unknown) {
    super(options);
    try {
      const bytes = wasmBytesOf(options);
      if (bytes === null) throw new Error("no module bytes");
      const kernel = NoiseKernel.fromBytes(bytes);
      if (kernel.sampleRate !== scope.sampleRate) {
        throw new Error(`context runs at ${scope.sampleRate} Hz, the model needs ${kernel.sampleRate}`);
      }
      this.kernel = kernel;
      this.post({ type: "ready", latencySamples: kernel.latencySamples });
    } catch (err) {
      this.fail(`init: ${err instanceof Error ? err.message : String(err)}`);
    }
  }

  private post(message: NoiseWorkletMessage): void {
    this.port.postMessage(message);
  }

  private fail(reason: string): void {
    this.kernel = null;
    this.post({ type: "failed", reason });
  }

  process(inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const out = outputs[0]?.[0];
    if (out === undefined) return true;
    const input = inputs[0]?.[0];
    if (input === undefined || input.length !== out.length) {
      // No input connected (the source ended) or an unexpected shape.
      out.fill(0);
      return true;
    }
    const kernel = this.kernel;
    if (kernel === null) {
      out.set(input);
      return true;
    }
    if (!kernel.process(input, out)) {
      this.fail("process refused a block");
      out.set(input);
    }
    return true;
  }
}

scope.registerProcessor("lakko-noise", LakkoNoiseProcessor);
