// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The camera background's worker (ADR-0089 section 4, the frame-stream way):
 * reads the camera's frames from a `MediaStreamTrackProcessor` stream, runs
 * `lakko_segment.wasm` and the WebGL2 compositor on an `OffscreenCanvas`, and
 * writes only processed frames to the `MediaStreamTrackGenerator` the call
 * sends. A frame the background was not applied to is closed here, never
 * written (`background-engine.ts`).
 *
 * Messages in: `start` (the two streams, the compiled module, the first
 * mode), `mode`, `enabled`, `restart`, `stop`. Out: `state`
 * (`starting` / `working` / `failed`). No picture, no person data and no
 * timing leaves the worker but the state.
 */

import { BackgroundEngine, type EngineState } from "./background-engine";
import { GlBackgroundCompositor } from "./background-gl";
import type { CameraFrame, StageMode } from "./background-mode";
import { SegmentInstance } from "./background-segment-wasm";

export type BackgroundWorkerIn =
  | {
      readonly type: "start";
      readonly readable: ReadableStream<VideoFrame>;
      readonly writable: WritableStream<VideoFrame>;
      readonly module: WebAssembly.Module;
      readonly mode: StageMode | null;
      readonly enabled: boolean;
    }
  | { readonly type: "mode"; readonly mode: StageMode | null }
  | { readonly type: "enabled"; readonly enabled: boolean }
  | { readonly type: "restart" }
  | { readonly type: "stop" };

export type BackgroundWorkerOut =
  | { readonly type: "state"; readonly state: EngineState }
  /** Why it failed: an error message or the watch's verdict, no picture. */
  | { readonly type: "reason"; readonly reason: string };

const scope = self as unknown as DedicatedWorkerGlobalScope;

let engine: BackgroundEngine<CameraFrame, void, StageMode> | null = null;
let compositor: GlBackgroundCompositor | null = null;
let canvas: OffscreenCanvas | null = null;
let pendingMode: StageMode | null = null;
let enabled = true;
let stopped = false;
let said: EngineState | null = null;

function say(state: EngineState): void {
  if (state === said) return;
  said = state;
  scope.postMessage({ type: "state", state } satisfies BackgroundWorkerOut);
}

function closeMode(mode: StageMode | null): void {
  if (mode?.kind === "picture") mode.bitmap.close();
}

async function run(msg: Extract<BackgroundWorkerIn, { type: "start" }>): Promise<void> {
  say("starting");
  pendingMode = msg.mode;
  enabled = msg.enabled;
  const reader = msg.readable.getReader();
  const writer = msg.writable.getWriter();
  try {
    const seg = await SegmentInstance.create(msg.module);
    if (stopped) return;
    canvas = new OffscreenCanvas(1280, 720);
    compositor = new GlBackgroundCompositor(canvas, seg);
    const clock = (): number => performance.now();
    // The model loads here (tens to hundreds of ms); a failed load leaves the
    // watch Failed, so the first frame is dropped and the camera turned off.
    seg.init(clock());
    engine = new BackgroundEngine<CameraFrame, void, StageMode>(seg, compositor, clock);
    engine.setMode(pendingMode);
  } catch (err) {
    scope.postMessage({ type: "reason", reason: err instanceof Error ? err.message : String(err) } satisfies BackgroundWorkerOut);
    say("failed");
    // Keep draining so the camera's frames are closed, never forwarded.
  }
  for (;;) {
    let next: ReadableStreamReadResult<VideoFrame>;
    try {
      next = await reader.read();
    } catch {
      break;
    }
    if (next.done) break;
    const frame = next.value;
    try {
      if (stopped || engine === null || canvas === null || !enabled) continue;
      const outcome = engine.process({
        source: frame,
        width: frame.displayWidth,
        height: frame.displayHeight,
      });
      if (outcome.kind === "stop") {
        if (said !== "failed") {
          scope.postMessage({ type: "reason", reason: engine.failureReason ?? "unknown" } satisfies BackgroundWorkerOut);
        }
        say("failed");
        continue;
      }
      say(engine.state);
      if (outcome.kind !== "send") continue;
      const out = new VideoFrame(canvas, {
        timestamp: frame.timestamp,
        ...(frame.duration === null ? {} : { duration: frame.duration }),
      });
      try {
        await writer.write(out);
      } catch {
        out.close();
        break;
      }
    } finally {
      frame.close();
    }
  }
  try {
    await writer.close();
  } catch {
    // The generator's track was stopped already.
  }
  reader.releaseLock();
}

scope.onmessage = (event: MessageEvent<BackgroundWorkerIn>): void => {
  const msg = event.data;
  switch (msg.type) {
    case "start":
      void run(msg);
      return;
    case "mode": {
      const previous = pendingMode;
      pendingMode = msg.mode;
      engine?.setMode(msg.mode);
      // The compositor keeps its own upload of a picture; the old bitmap is
      // not needed once a new mode is set.
      if (previous !== msg.mode) closeMode(previous);
      return;
    }
    case "enabled":
      enabled = msg.enabled;
      return;
    case "restart":
      engine?.restart();
      said = null;
      if (engine !== null) say(engine.state);
      return;
    case "stop":
      stopped = true;
      closeMode(pendingMode);
      pendingMode = null;
      compositor?.dispose();
      compositor = null;
      engine = null;
      return;
  }
};
