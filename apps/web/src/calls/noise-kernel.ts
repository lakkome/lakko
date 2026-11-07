// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The JavaScript side of `lakko_noise.wasm` (ADR-0088): instantiate the
 * import-free module and run blocks of samples through it.
 *
 * Shared by the audio worklet (`noise-worklet.ts`), where it runs on the
 * audio thread, and by the unit test, which drives the real `.wasm` under
 * Node. It touches no DOM and no Web Audio type, only `WebAssembly`.
 *
 * The exchange: the module owns two buffers of `maxBlock` samples in its
 * linear memory. A block is copied into the input buffer, `lakko_ns_process`
 * cleans it into the output buffer, and the block is copied out. The views
 * are rebuilt whenever the memory has grown (growth detaches the old
 * `ArrayBuffer`).
 */

/** What `lakko_noise.wasm` exports (`crates/lakko-noise-wasm`). */
interface NoiseExports {
  readonly memory: WebAssembly.Memory;
  lakko_ns_init(): number;
  lakko_ns_input_ptr(): number;
  lakko_ns_output_ptr(): number;
  lakko_ns_process(n: number): number;
  lakko_ns_voice_probability(): number;
  lakko_ns_max_block(): number;
  lakko_ns_sample_rate(): number;
  lakko_ns_latency_samples(): number;
}

const REQUIRED_EXPORTS = [
  "lakko_ns_init",
  "lakko_ns_input_ptr",
  "lakko_ns_output_ptr",
  "lakko_ns_process",
  "lakko_ns_voice_probability",
  "lakko_ns_max_block",
  "lakko_ns_sample_rate",
  "lakko_ns_latency_samples",
] as const;

function asNoiseExports(exports: WebAssembly.Exports): NoiseExports {
  if (!(exports.memory instanceof WebAssembly.Memory)) {
    throw new Error("lakko_noise.wasm exports no memory");
  }
  for (const name of REQUIRED_EXPORTS) {
    if (typeof exports[name] !== "function") {
      throw new Error(`lakko_noise.wasm lacks ${name}`);
    }
  }
  return exports as unknown as NoiseExports;
}

/** One suppressor: one wasm instance, one microphone. */
export class NoiseKernel {
  private input: Float32Array;
  private output: Float32Array;
  private viewedBuffer: ArrayBuffer;

  /** The only sample rate the kernel may run at (48 000). */
  readonly sampleRate: number;
  /** The delay, in samples, the kernel adds (one RNNoise frame). */
  readonly latencySamples: number;
  /** The largest block {@link process} accepts. */
  readonly maxBlock: number;

  private constructor(private readonly exports: NoiseExports) {
    if (exports.lakko_ns_init() !== 1) throw new Error("lakko_ns_init refused");
    this.sampleRate = exports.lakko_ns_sample_rate();
    this.latencySamples = exports.lakko_ns_latency_samples();
    this.maxBlock = exports.lakko_ns_max_block();
    if (this.maxBlock <= 0) throw new Error("lakko_noise.wasm reports no block size");
    // Placeholders replaced by `views()` before any use.
    this.viewedBuffer = new ArrayBuffer(0);
    this.input = new Float32Array(0);
    this.output = new Float32Array(0);
    this.views();
  }

  /**
   * Compiles and instantiates the module synchronously. Off the main thread
   * (a worklet, a test) a synchronous compile of any size is allowed.
   * Throws when the bytes are not the expected module.
   */
  static fromBytes(bytes: BufferSource): NoiseKernel {
    const module = new WebAssembly.Module(bytes);
    const instance = new WebAssembly.Instance(module, {});
    return new NoiseKernel(asNoiseExports(instance.exports));
  }

  private views(): void {
    const buffer = this.exports.memory.buffer;
    if (buffer === this.viewedBuffer && this.input.length > 0) return;
    const inPtr = this.exports.lakko_ns_input_ptr();
    const outPtr = this.exports.lakko_ns_output_ptr();
    if (inPtr === 0 || outPtr === 0) throw new Error("lakko_noise.wasm has no buffers");
    this.viewedBuffer = buffer;
    this.input = new Float32Array(buffer, inPtr, this.maxBlock);
    this.output = new Float32Array(buffer, outPtr, this.maxBlock);
  }

  /**
   * Cleans `input` into `output` (equal lengths, at most {@link maxBlock}).
   * Returns false, leaving `output` untouched, when the module refuses.
   */
  process(input: Float32Array, output: Float32Array): boolean {
    const n = input.length;
    if (n !== output.length || n > this.maxBlock) return false;
    this.views();
    this.input.set(input);
    if (this.exports.lakko_ns_process(n) !== 1) return false;
    // `lakko_ns_process` does not allocate, so the views are still valid.
    output.set(this.output.subarray(0, n));
    return true;
  }

  /** RNNoise's voice probability for the last whole frame. */
  voiceProbability(): number {
    return this.exports.lakko_ns_voice_probability();
  }
}
