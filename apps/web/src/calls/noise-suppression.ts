// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Noise suppression on the outgoing microphone (ADR-0088, UX
 * `docs/ux/calls.md` section 4.1.3).
 *
 * The audio path, after the browser's own capture processing (echo
 * cancellation first, as the browser does it):
 *
 *   raw track -> AudioContext(48 kHz) -> MediaStreamAudioSourceNode
 *     -> AudioWorkletNode "lakko-noise" (RNNoise in wasm)
 *     -> MediaStreamAudioDestinationNode -> processed track
 *
 * The processed track goes onto the microphone's `RTCRtpSender` with
 * `replaceTrack`. The SFrame transform belongs to the sender, so it keeps
 * sealing every frame and nothing is renegotiated. Everything here runs
 * before the encoder, on this device's own audio; nothing reaches the wire
 * but what the sender already sent.
 *
 * Rules the controller keeps:
 *
 * - **Publish first, swap after.** A session publishes the raw track; the
 *   controller swaps the processed one in when its graph runs. Building the
 *   graph never delays joining.
 * - **Off means the raw track.** Turning the switch off puts the raw track
 *   back on the sender and closes the graph.
 * - **Any failure means the raw track**, one log line, and nothing shown
 *   (UX section 4.1.3): no worklet, no wasm, a context that will not run at
 *   48 kHz or will not start, a context suspended later (which would
 *   otherwise send silence), a worklet that reports an error.
 * - **Every swap is ordered by a generation.** A build that finishes after
 *   the switch moved, the microphone changed or the call ended disposes of
 *   itself instead of landing on the sender.
 */

import {
  noiseSuppressionOn,
  onNoiseSuppressionChange,
} from "./noise-setting";
import type { NoiseWorkletMessage } from "./noise-worklet";

/** A processed track and the graph behind it. */
export interface DenoisedTrack {
  readonly track: MediaStreamTrack;
  /** Stops the track and closes the graph. Idempotent. */
  dispose(): void;
}

/**
 * Builds the processed track for `raw`. Rejects when it cannot; calls
 * `onFailure` at most once if the graph stops working later.
 */
export type DenoiserFactory = (
  raw: MediaStreamTrack,
  onFailure: (reason: string) => void,
) => Promise<DenoisedTrack>;

export interface NoiseSuppressionDeps {
  /** Names the call kind in the log line (`group`, `1x1`). */
  readonly label: string;
  /** Puts `track` on the microphone sender. */
  replaceSenderTrack(track: MediaStreamTrack): Promise<void>;
  /** Defaults to the worklet graph ({@link createWorkletDenoiser}). */
  readonly factory?: DenoiserFactory;
  /** Defaults to `console.info`. */
  readonly log?: (line: string) => void;
  /** Defaults to the stored switch. */
  readonly initialOn?: boolean;
  /** Defaults to {@link onNoiseSuppressionChange}. */
  readonly subscribe?: (listener: (on: boolean) => void) => () => void;
}

/** What the browser reports it applied to the raw microphone. */
export function describeCaptureProcessing(track: MediaStreamTrack): string {
  let settings: MediaTrackSettings = {};
  try {
    settings = track.getSettings();
  } catch {
    settings = {};
  }
  const flag = (v: boolean | undefined): string => (v === undefined ? "n/a" : String(v));
  return (
    `echoCancellation=${flag(settings.echoCancellation)} ` +
    `noiseSuppression=${flag(settings.noiseSuppression)} ` +
    `autoGainControl=${flag(settings.autoGainControl)}`
  );
}

function reasonOf(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * One call session's microphone: which track its sender carries, raw or
 * processed. Created by the session after its first capture; told about a
 * microphone change with {@link detach} / {@link attach}; disposed with the
 * session.
 */
export class NoiseSuppressionController {
  private raw: MediaStreamTrack | null = null;
  private processed: DenoisedTrack | null = null;
  /** The track this controller last put, or left, on the sender. */
  private sent: MediaStreamTrack | null = null;
  private on: boolean;
  private generation = 0;
  private disposed = false;
  private readonly unsubscribe: () => void;
  private readonly factory: DenoiserFactory;
  private readonly log: (line: string) => void;

  constructor(private readonly deps: NoiseSuppressionDeps) {
    this.on = deps.initialOn ?? noiseSuppressionOn();
    this.factory = deps.factory ?? createWorkletDenoiser;
    // eslint-disable-next-line no-console
    this.log = deps.log ?? ((line: string): void => console.info(line));
    this.unsubscribe = (deps.subscribe ?? onNoiseSuppressionChange)((on) => {
      void this.setEnabled(on);
    });
  }

  /** Whether the switch is on for this session. */
  get enabled(): boolean {
    return this.on;
  }

  /** The processed track while it is the one the sender carries. */
  get processedTrack(): MediaStreamTrack | null {
    return this.processed?.track ?? null;
  }

  /**
   * `raw` is this session's microphone now. Makes sure the sender carries
   * it (a build that lost a race may have left an older processed track
   * there), lets the previous graph go, and, when the switch is on, builds
   * the processed track and swaps it in. Resolves once settled (swapped, or
   * left on `raw`).
   */
  async attach(raw: MediaStreamTrack): Promise<void> {
    if (this.disposed) return;
    const generation = ++this.generation;
    const previous = this.processed;
    this.processed = null;
    this.raw = raw;
    if (this.sent !== raw) {
      try {
        await this.deps.replaceSenderTrack(raw);
      } catch (err) {
        this.log(`[noise] ${this.deps.label}: could not put the raw microphone on the sender (${reasonOf(err)})`);
      }
      this.sent = raw;
    }
    previous?.dispose();
    if (generation !== this.generation || this.disposed) return;
    const captured = describeCaptureProcessing(raw);
    if (!this.on) {
      this.log(`[noise] ${this.deps.label}: off (switch); raw ${captured}`);
      return;
    }
    await this.buildAndSwap(generation, captured);
  }

  /**
   * The session is about to put a new microphone on the sender itself (a
   * device switch). Any build in flight is dropped; the old processed track
   * is disposed at the next {@link attach}, once it is off the sender.
   */
  detach(): void {
    ++this.generation;
    this.raw = null;
  }

  /** The switch moved. Applies at once. */
  async setEnabled(on: boolean): Promise<void> {
    if (this.disposed || this.on === on) return;
    this.on = on;
    const generation = ++this.generation;
    const raw = this.raw;
    if (raw === null) return;
    if (on) {
      await this.buildAndSwap(generation, describeCaptureProcessing(raw));
      return;
    }
    await this.sendRaw(raw, "off (switch)");
  }

  /** Mirrors the microphone mute onto the processed track. */
  setMuted(muted: boolean): void {
    const track = this.processed?.track;
    if (track !== undefined) track.enabled = !muted;
  }

  /** Stops the processed track and closes the graph. The session stops the
   *  raw track itself. */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    ++this.generation;
    this.unsubscribe();
    this.dropProcessed();
    this.raw = null;
    this.sent = null;
  }

  private dropProcessed(): void {
    const processed = this.processed;
    this.processed = null;
    processed?.dispose();
  }

  private current(generation: number, raw: MediaStreamTrack): boolean {
    return !this.disposed && generation === this.generation && this.raw === raw;
  }

  private async buildAndSwap(generation: number, captured: string): Promise<void> {
    const raw = this.raw;
    if (raw === null || raw.readyState === "ended") return;
    // A failure the graph reports before this build is on the sender is
    // held here and acted on below; one after it goes to onRuntimeFailure.
    const progress: { built: DenoisedTrack | null; adopted: boolean; early: string | null } = {
      built: null,
      adopted: false,
      early: null,
    };
    try {
      progress.built = await this.factory(raw, (reason) => {
        if (!progress.adopted) {
          progress.early ??= reason;
          return;
        }
        void this.onRuntimeFailure(progress.built, reason);
      });
    } catch (err) {
      if (this.current(generation, raw)) {
        this.log(
          `[noise] ${this.deps.label}: platform processing only (${reasonOf(err)}); raw ${captured}`,
        );
      }
      return;
    }
    const built = progress.built;
    if (!this.current(generation, raw)) {
      built.dispose();
      return;
    }
    if (progress.early !== null) {
      built.dispose();
      this.log(
        `[noise] ${this.deps.label}: platform processing only (${progress.early}); raw ${captured}`,
      );
      return;
    }
    built.track.enabled = raw.enabled;
    try {
      await this.deps.replaceSenderTrack(built.track);
    } catch (err) {
      built.dispose();
      if (this.current(generation, raw)) {
        this.log(
          `[noise] ${this.deps.label}: platform processing only (replaceTrack: ${reasonOf(err)}); raw ${captured}`,
        );
      }
      return;
    }
    if (!this.current(generation, raw)) {
      // Superseded while the track was being replaced. A later switch-off
      // or device switch queued its own replaceTrack behind this one, so
      // the sender ends on the right track; this graph is not needed.
      built.dispose();
      return;
    }
    this.processed = built;
    this.sent = built.track;
    progress.adopted = true;
    if (progress.early !== null) {
      // The graph failed while its track was being put on the sender.
      ++this.generation;
      await this.sendRaw(raw, `platform processing only (${progress.early})`);
      return;
    }
    this.log(`[noise] ${this.deps.label}: rnnoise applied; raw ${captured}`);
  }

  private async sendRaw(raw: MediaStreamTrack, why: string): Promise<void> {
    const processed = this.processed;
    this.processed = null;
    if (this.sent !== raw) {
      try {
        await this.deps.replaceSenderTrack(raw);
        this.sent = raw;
      } catch (err) {
        this.log(`[noise] ${this.deps.label}: could not put the raw microphone back (${reasonOf(err)})`);
      }
    }
    processed?.dispose();
    this.log(`[noise] ${this.deps.label}: ${why}; raw ${describeCaptureProcessing(raw)}`);
  }

  private async onRuntimeFailure(built: DenoisedTrack | null, reason: string): Promise<void> {
    if (this.disposed || built === null || this.processed !== built) {
      // Not (or no longer) the graph on the sender: just let it go.
      built?.dispose();
      return;
    }
    const raw = this.raw;
    ++this.generation;
    if (raw === null) {
      this.dropProcessed();
      return;
    }
    await this.sendRaw(raw, `platform processing only (${reason})`);
  }
}

// ---------------------------------------------------------------------------
// The worklet graph
// ---------------------------------------------------------------------------

/** RNNoise's only rate (`lakko_noise::SAMPLE_RATE`). */
const MODEL_SAMPLE_RATE = 48_000;
/** How long the worklet may take to report `ready`; chosen, not measured. */
const READY_TIMEOUT_MS = 3_000;
/** How long `AudioContext.resume()` may take; chosen, not measured. */
const RESUME_TIMEOUT_MS = 2_000;

let wasmBytes: Promise<ArrayBuffer> | null = null;

/** Fetches `lakko_noise.wasm` once per page; a failed fetch is retried by the
 *  next build. */
function loadNoiseWasm(url: string): Promise<ArrayBuffer> {
  if (wasmBytes === null) {
    wasmBytes = fetch(url).then(async (response) => {
      if (!response.ok) throw new Error(`lakko_noise.wasm: HTTP ${response.status}`);
      return response.arrayBuffer();
    });
    wasmBytes.catch(() => {
      wasmBytes = null;
    });
  }
  return wasmBytes;
}

function withTimeout<T>(promise: Promise<T>, ms: number, what: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${what} timed out`)), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (err: unknown) => {
        clearTimeout(timer);
        reject(err instanceof Error ? err : new Error(String(err)));
      },
    );
  });
}

/** The default {@link DenoiserFactory}: the Web Audio graph described at the
 *  top of this file. */
export const createWorkletDenoiser: DenoiserFactory = async (raw, onFailure) => {
  const Ctx = (globalThis as { AudioContext?: typeof AudioContext }).AudioContext;
  if (Ctx === undefined) throw new Error("no AudioContext");
  if (typeof AudioWorkletNode === "undefined") throw new Error("no AudioWorkletNode");
  const ctx = new Ctx({ sampleRate: MODEL_SAMPLE_RATE, latencyHint: "interactive" });
  let disposed = false;
  let failureReported = false;
  const reportFailure = (reason: string): void => {
    if (disposed || failureReported) return;
    failureReported = true;
    onFailure(reason);
  };
  try {
    if (ctx.sampleRate !== MODEL_SAMPLE_RATE) {
      throw new Error(`AudioContext runs at ${ctx.sampleRate} Hz`);
    }
    if (ctx.audioWorklet === undefined) throw new Error("no AudioWorklet");
    const { noiseWorkletUrl, noiseWasmUrl } = await import("./noise-assets");
    const bytes = await loadNoiseWasm(noiseWasmUrl);
    await ctx.audioWorklet.addModule(noiseWorkletUrl);

    const source = ctx.createMediaStreamSource(new MediaStream([raw]));
    const node = new AudioWorkletNode(ctx, "lakko-noise", {
      numberOfInputs: 1,
      numberOfOutputs: 1,
      outputChannelCount: [1],
      channelCount: 1,
      channelCountMode: "explicit",
      channelInterpretation: "speakers",
      // Structured clone copies the bytes; the cached copy stays intact.
      processorOptions: { wasm: bytes },
    });
    const ready = new Promise<void>((resolve, reject) => {
      node.port.onmessage = (event: MessageEvent<NoiseWorkletMessage>): void => {
        const message = event.data;
        if (message.type === "ready") resolve();
        else if (message.type === "failed") {
          reject(new Error(`worklet: ${message.reason}`));
          reportFailure(`worklet: ${message.reason}`);
        }
      };
      node.onprocessorerror = (): void => {
        reject(new Error("worklet processor error"));
        reportFailure("worklet processor error");
      };
    });
    const destination = ctx.createMediaStreamDestination();
    destination.channelCount = 1;
    source.connect(node).connect(destination);
    if (ctx.state !== "running") {
      await withTimeout(ctx.resume(), RESUME_TIMEOUT_MS, "AudioContext.resume");
    }
    await withTimeout(ready, READY_TIMEOUT_MS, "worklet ready");
    if (ctx.state !== "running") throw new Error(`AudioContext is ${ctx.state}`);
    const track = destination.stream.getAudioTracks()[0];
    if (track === undefined) throw new Error("no processed track");

    // A context that stops running would send silence: hand the sender back
    // to the raw microphone instead.
    ctx.onstatechange = (): void => {
      if (ctx.state !== "running") reportFailure(`AudioContext ${ctx.state}`);
    };
    return {
      track,
      dispose(): void {
        if (disposed) return;
        disposed = true;
        ctx.onstatechange = null;
        node.port.onmessage = null;
        track.stop();
        try {
          source.disconnect();
          node.disconnect();
        } catch {
          // already disconnected
        }
        void ctx.close().catch(() => undefined);
      },
    };
  } catch (err) {
    disposed = true;
    void ctx.close().catch(() => undefined);
    throw err;
  }
};
