// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The per-frame rule of the camera background (ADR-0089 section 3), with no
 * browser in it, so a test can drive it: one camera frame in, at most one
 * PROCESSED frame out.
 *
 * The frame gate is the shared Rust rule (`lakko_seg_frame_action`): with a
 * background selected, a frame is sent processed while the segmenter is
 * Working, dropped while it is Starting, and dropped with the camera turned
 * off once it has Failed. This engine is only ever run with a background
 * selected, and it has no path that hands back the frame it was given: its
 * only output is what the compositor drew. A gate answer of "send the camera"
 * (impossible with a background selected) is dropped too. Anything that
 * throws (the model, the compositor, a lost GPU context) is a failure.
 *
 * Inference may run below the camera's frame rate; the frames in between are
 * composited with the last mask (ADR-0089 section 3: a late mask is not a
 * failure).
 */

/** Processor states, the codes `lakko_segment_wasm` uses. */
export const SEG_STARTING = 0;
export const SEG_WORKING = 1;
export const SEG_FAILED = 2;

/** Frame-gate answers, the codes `lakko_seg_frame_action` returns. */
export const ACTION_SEND_CAMERA = 0;
export const ACTION_SEND_PROCESSED = 1;
export const ACTION_DROP = 2;
export const ACTION_DROP_AND_STOP = 3;

/** How often the model runs, at most: chosen, not measured. Frames between
 *  two runs reuse the last mask. */
export const INFERENCE_INTERVAL_MS = 40;

/** The segmenter as the engine sees it (the wasm instance in production). */
export interface FrameSegmenter {
  /** A camera frame arrived; returns the state code to judge it by. */
  onFrame(nowMs: number): number;
  /** The RGBA model input (256 x 144 x 4 bytes) to write before {@link segment}. */
  inputBuffer(): Uint8Array;
  /** Segments the input into the mask; returns the state code. */
  segment(nowMs: number, tookMs: number): number;
  /** The mask (256 x 144 bytes, 255 = person), valid after a segment. */
  mask(): Uint8Array;
  /** The shared frame gate. */
  frameAction(selected: boolean, state: number): number;
  /** Forgets the mask history and restarts the health watch. */
  restart(nowMs: number): number;
}

/** Draws with the mask; `F` is a camera frame, `O` what goes out, `M` a mode. */
export interface FrameCompositor<F, O, M> {
  /** Writes the frame, scaled to the model's input size and not mirrored,
   *  into `out` (RGBA, rows top to bottom). */
  writeModelInput(frame: F, out: Uint8Array): void;
  /** The frame with its background replaced as `mode` says. */
  composite(frame: F, mask: Uint8Array, mode: M): O;
}

/** What to do with one camera frame. `send` carries the processed frame,
 *  never the camera frame. */
export type FrameOutcome<O> =
  | { readonly kind: "send"; readonly output: O }
  | { readonly kind: "drop" }
  | { readonly kind: "stop" };

/** The state the stage reports to the call screen. */
export type EngineState = "starting" | "working" | "failed";

function stateName(code: number): EngineState {
  if (code === SEG_WORKING) return "working";
  if (code === SEG_STARTING) return "starting";
  return "failed";
}

export class BackgroundEngine<F, O, M> {
  private mode: M | null = null;
  private lastRunAt: number | null = null;
  private lastTook = 0;
  private hasMask = false;
  private failed = false;
  private stateCode = SEG_STARTING;
  /** Why the engine failed, for the one log line (an error message or the
   *  watch's verdict; never a picture or a person's data). */
  failureReason: string | null = null;

  constructor(
    private readonly segmenter: FrameSegmenter,
    private readonly compositor: FrameCompositor<F, O, M>,
    private readonly clock: () => number,
    private readonly inferenceIntervalMs: number = INFERENCE_INTERVAL_MS,
  ) {}

  /** What to draw behind the person; `null` while it is being loaded (frames
   *  are then dropped, the model still runs so its watch stays honest). */
  setMode(mode: M | null): void {
    this.mode = mode;
  }

  /** The state of the last frame. */
  get state(): EngineState {
    return this.failed ? "failed" : stateName(this.stateCode);
  }

  /** Forgets the mask and restarts the watch (camera switched or turned on). */
  restart(): void {
    this.failureReason = null;
    this.hasMask = false;
    this.lastRunAt = null;
    this.failed = false;
    try {
      this.stateCode = this.segmenter.restart(this.clock());
    } catch {
      this.failed = true;
    }
  }

  /** One camera frame. Never returns `frame` itself. */
  process(frame: F): FrameOutcome<O> {
    if (this.failed) return { kind: "stop" };
    try {
      let state = this.segmenter.onFrame(this.clock());
      const now = this.clock();
      const due = this.lastRunAt === null || now - this.lastRunAt >= this.inferenceIntervalMs;
      if (state !== SEG_FAILED && due) {
        this.compositor.writeModelInput(frame, this.segmenter.inputBuffer());
        const started = this.clock();
        // The watch is told how long a run takes. The duration of this run is
        // known only after it, so the previous run's is passed with it; the
        // watch averages over thirty runs, so the lag of one does not matter.
        state = this.segmenter.segment(started, this.lastTook);
        this.lastTook = Math.max(0, this.clock() - started);
        this.lastRunAt = now;
        if (state === SEG_WORKING) this.hasMask = true;
      }
      this.stateCode = state;
      const action = this.segmenter.frameAction(true, state);
      if (action === ACTION_DROP_AND_STOP) {
        this.failed = true;
        this.failureReason ??= "the health watch failed";
        return { kind: "stop" };
      }
      if (action === ACTION_SEND_PROCESSED && this.hasMask && this.mode !== null) {
        return { kind: "send", output: this.compositor.composite(frame, this.segmenter.mask(), this.mode) };
      }
      // Starting, a mode still loading, and (never with a background
      // selected) "send the camera": nothing leaves.
      return { kind: "drop" };
    } catch (err) {
      this.failed = true;
      this.failureReason ??= err instanceof Error ? err.message : String(err);
      return { kind: "stop" };
    }
  }
}
