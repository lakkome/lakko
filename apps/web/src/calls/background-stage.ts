// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * One camera's background processing (ADR-0089 section 4): given the raw
 * camera track, makes at once the track the call sends instead, which carries
 * only processed frames, and starts the processing behind it.
 *
 * Two ways, chosen by `background-support.ts`:
 *
 * - **frames**: `MediaStreamTrackProcessor` reads the camera's frames, the
 *   worker (`background-worker.ts`) processes them, and a
 *   `MediaStreamTrackGenerator` is the track sent. Off the main thread; keeps
 *   running in a hidden tab.
 * - **canvas**: a hidden `<video>` of the camera is drawn into a WebGL2
 *   canvas on the main thread, published with `captureStream`; the camera
 *   frame only ever reaches textures, and only the processed picture is drawn
 *   onto the canvas, so nothing else can be captured from it. Where the
 *   browser has `requestFrame()` a frame is captured only once one was drawn.
 *   Its frames follow the video element's callbacks, which a browser may slow
 *   or stop in a hidden tab (the picture then stands still).
 *
 * Both run every camera frame through `BackgroundEngine`, whose only output
 * is the processed frame, so neither has a way to pass the camera through.
 * The 19 MB module is fetched and compiled once per page, the first time a
 * stage is made (the UX's `Preparing background...`). The stage never stops
 * the raw camera track: its owner (`background-controller.ts`) does.
 */

import { BackgroundEngine, type EngineState } from "./background-engine";
import { GlBackgroundCompositor } from "./background-gl";
import type { CameraFrame, StageMode } from "./background-mode";
import { SegmentInstance } from "./background-segment-wasm";
import { detectBackgroundPath } from "./background-support";
import type { BackgroundWorkerIn, BackgroundWorkerOut } from "./background-worker";

export type StageState = EngineState;

/** A running background on one camera track. */
export interface BackgroundStage {
  /** The track to send: processed frames only. */
  readonly track: MediaStreamTrack;
  /** What to draw behind the person; `null` while it is being loaded. The
   *  stage takes ownership of a picture's bitmap. */
  setMode(mode: StageMode | null): void;
  /** The camera was turned off (`false`) or on: nothing is processed while
   *  it is off. */
  setEnabled(on: boolean): void;
  /** Stops the processing and the sent track. Leaves the raw track alone. */
  dispose(): void;
}

/** Makes a stage for `raw`; `onState` hears `starting` at once, then
 *  `working` / `failed`. */
export type StageFactory = (raw: MediaStreamTrack, onState: (state: StageState) => void) => BackgroundStage;

/** How long the module may take to download and compile before the stage
 *  counts as failed; chosen, not measured. The model's own watch takes over
 *  from there (15 s to a first mask). */
const LOAD_DEADLINE_MS = 30_000;

let modulePromise: Promise<WebAssembly.Module> | null = null;

/** Fetches and compiles `lakko_segment.wasm` once per page; a failure is
 *  retried by the next stage. */
function segmentModule(): Promise<WebAssembly.Module> {
  if (modulePromise === null) {
    modulePromise = (async (): Promise<WebAssembly.Module> => {
      const { segmentWasmUrl } = await import("./background-assets");
      const response = await fetch(segmentWasmUrl);
      if (!response.ok) throw new Error(`lakko_segment.wasm: HTTP ${response.status}`);
      return WebAssembly.compile(await response.arrayBuffer());
    })();
    modulePromise.catch(() => {
      modulePromise = null;
    });
  }
  return modulePromise;
}

function closeMode(mode: StageMode | null): void {
  if (mode?.kind === "picture") mode.bitmap.close();
}

/** A track that never carries a frame, for a browser that can run no
 *  background while one is selected. */
function silentTrack(raw: MediaStreamTrack): MediaStreamTrack {
  try {
    const canvas = document.createElement("canvas") as HTMLCanvasElement & {
      captureStream?: (rate?: number) => MediaStream;
    };
    const track = canvas.captureStream?.(0).getVideoTracks()[0];
    if (track !== undefined) return track;
  } catch {
    // fall through
  }
  const ended = raw.clone();
  ended.stop();
  return ended;
}

function reportOnce(onState: (state: StageState) => void): (state: StageState) => void {
  let last: StageState | null = null;
  return (state) => {
    if (last === "failed" || state === last) return;
    last = state;
    onState(state);
  };
}

/** The default {@link StageFactory}. */
export const createBackgroundStage: StageFactory = (raw, onState) => {
  const say = reportOnce(onState);
  const path = detectBackgroundPath();
  if (path === "frames") return framesStage(raw, say);
  if (path === "canvas") return canvasStage(raw, say);
  const track = silentTrack(raw);
  say("starting");
  queueMicrotask(() => say("failed"));
  return {
    track,
    setMode: closeMode,
    setEnabled: (): void => undefined,
    dispose: (): void => track.stop(),
  };
};

interface ProcessorCtor {
  new (init: { track: MediaStreamTrack }): { readable: ReadableStream<VideoFrame> };
}
interface GeneratorCtor {
  new (init: { kind: "video" }): MediaStreamTrack & { writable: WritableStream<VideoFrame> };
}

function framesStage(raw: MediaStreamTrack, say: (state: StageState) => void): BackgroundStage {
  const g = globalThis as unknown as {
    MediaStreamTrackProcessor: ProcessorCtor;
    MediaStreamTrackGenerator: GeneratorCtor;
  };
  const generator = new g.MediaStreamTrackGenerator({ kind: "video" });
  let processor: { readable: ReadableStream<VideoFrame> } | null = null;
  let worker: Worker | null = null;
  let mode: StageMode | null = null;
  let enabled = raw.enabled;
  let disposed = false;
  say("starting");

  const post = (msg: BackgroundWorkerIn): void => {
    worker?.postMessage(msg);
  };
  const onRawEnded = (): void => generator.stop();
  raw.addEventListener("ended", onRawEnded);

  const deadline = setTimeout(() => {
    if (worker === null && !disposed) say("failed");
  }, LOAD_DEADLINE_MS);

  void (async (): Promise<void> => {
    try {
      const module = await segmentModule();
      if (disposed) return;
      const { createBackgroundWorker } = await import("./background-assets");
      if (disposed) return;
      processor = new g.MediaStreamTrackProcessor({ track: raw });
      const w = createBackgroundWorker();
      w.onmessage = (event: MessageEvent<BackgroundWorkerOut>): void => {
        if (disposed) return;
        if (event.data.type === "state") say(event.data.state);
        // eslint-disable-next-line no-console
        else console.info(`[background] stage failed: ${event.data.reason}`);
      };
      w.onerror = (): void => {
        if (!disposed) say("failed");
      };
      worker = w;
      clearTimeout(deadline);
      w.postMessage(
        {
          type: "start",
          readable: processor.readable,
          writable: generator.writable,
          module,
          mode,
          enabled,
        } satisfies BackgroundWorkerIn,
        [processor.readable as unknown as Transferable, generator.writable as unknown as Transferable],
      );
      // The worker holds its own copy of the picture now.
      closeMode(mode);
    } catch (err) {
      if (!disposed) {
        // eslint-disable-next-line no-console
        console.info(`[background] stage could not start: ${err instanceof Error ? err.message : String(err)}`);
        say("failed");
      }
    }
  })();

  return {
    track: generator,
    setMode(next): void {
      if (disposed) {
        closeMode(next);
        return;
      }
      if (worker !== null) {
        post({ type: "mode", mode: next });
        closeMode(next);
        return;
      }
      if (mode !== next) closeMode(mode);
      mode = next;
    },
    setEnabled(on): void {
      enabled = on;
      post({ type: "enabled", enabled: on });
    },
    dispose(): void {
      if (disposed) return;
      disposed = true;
      clearTimeout(deadline);
      raw.removeEventListener("ended", onRawEnded);
      post({ type: "stop" });
      worker?.terminate();
      worker = null;
      closeMode(mode);
      mode = null;
      generator.stop();
    },
  };
}

function canvasStage(raw: MediaStreamTrack, say: (state: StageState) => void): BackgroundStage {
  const video = document.createElement("video");
  video.muted = true;
  video.playsInline = true;
  video.autoplay = true;
  // In the document, invisible: some engines do not advance a video that is
  // not in it.
  video.setAttribute("aria-hidden", "true");
  video.style.cssText =
    "position:fixed;left:-10px;top:-10px;width:1px;height:1px;opacity:0;pointer-events:none";
  video.srcObject = new MediaStream([raw]);
  document.body.appendChild(video);
  void video.play().catch(() => undefined);

  const canvas = document.createElement("canvas") as HTMLCanvasElement & {
    captureStream(rate?: number): MediaStream;
  };
  canvas.width = 1280;
  canvas.height = 720;
  // Where the track can be told when a frame is ready (`requestFrame`, Chrome
  // and Safari), it is captured only then; elsewhere (Firefox) the browser
  // captures the canvas as it changes. Either way only processed pictures are
  // ever drawn onto this canvas, so nothing else can be captured from it.
  const probe = canvas.captureStream(0).getVideoTracks()[0] as
    | (MediaStreamTrack & { requestFrame?: () => void })
    | undefined;
  let track: MediaStreamTrack & { requestFrame?: () => void };
  if (probe !== undefined && typeof probe.requestFrame === "function") {
    track = probe;
  } else {
    probe?.stop();
    track = canvas.captureStream(30).getVideoTracks()[0] as MediaStreamTrack;
  }
  let engine: BackgroundEngine<CameraFrame, void, StageMode> | null = null;
  let compositor: GlBackgroundCompositor | null = null;
  let mode: StageMode | null = null;
  let enabled = raw.enabled;
  let disposed = false;
  let failed = false;
  let handle: number | null = null;
  const v = video as HTMLVideoElement & {
    requestVideoFrameCallback?: (cb: () => void) => number;
    cancelVideoFrameCallback?: (handle: number) => void;
  };
  say("starting");

  const onRawEnded = (): void => track.stop();
  raw.addEventListener("ended", onRawEnded);

  const schedule = (): void => {
    if (disposed) return;
    handle =
      typeof v.requestVideoFrameCallback === "function"
        ? v.requestVideoFrameCallback(tick)
        : requestAnimationFrame(tick);
  };
  const tick = (): void => {
    handle = null;
    if (disposed) return;
    if (engine !== null && !failed && enabled && video.readyState >= 2 && video.videoWidth > 0) {
      const outcome = engine.process({ source: video, width: video.videoWidth, height: video.videoHeight });
      if (outcome.kind === "stop") {
        failed = true;
        // eslint-disable-next-line no-console
        console.info(`[background] canvas stage failed: ${engine.failureReason ?? "unknown"}`);
        say("failed");
      } else {
        say(engine.state);
        if (outcome.kind === "send") track.requestFrame?.();
      }
    }
    schedule();
  };

  const deadline = setTimeout(() => {
    if (engine === null && !disposed) say("failed");
  }, LOAD_DEADLINE_MS);

  void (async (): Promise<void> => {
    try {
      const module = await segmentModule();
      if (disposed) return;
      const seg = await SegmentInstance.create(module);
      if (disposed) return;
      compositor = new GlBackgroundCompositor(canvas, seg, true);
      const clock = (): number => performance.now();
      seg.init(clock());
      engine = new BackgroundEngine<CameraFrame, void, StageMode>(seg, compositor, clock);
      engine.setMode(mode);
      clearTimeout(deadline);
      schedule();
    } catch (err) {
      if (!disposed) {
        failed = true;
        // eslint-disable-next-line no-console
        console.info(`[background] canvas stage could not start: ${err instanceof Error ? err.message : String(err)}`);
        say("failed");
      }
    }
  })();

  return {
    track,
    setMode(next): void {
      if (disposed) {
        closeMode(next);
        return;
      }
      if (mode !== next) closeMode(mode);
      mode = next;
      engine?.setMode(next);
    },
    setEnabled(on): void {
      enabled = on;
    },
    dispose(): void {
      if (disposed) return;
      disposed = true;
      clearTimeout(deadline);
      raw.removeEventListener("ended", onRawEnded);
      if (handle !== null) {
        if (typeof v.cancelVideoFrameCallback === "function") v.cancelVideoFrameCallback(handle);
        else cancelAnimationFrame(handle);
      }
      compositor?.dispose();
      compositor = null;
      engine = null;
      closeMode(mode);
      mode = null;
      track.stop();
      video.srcObject = null;
      video.remove();
    },
  };
}
