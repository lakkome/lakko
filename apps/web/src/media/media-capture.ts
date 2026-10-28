// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 §2 — browser capture for voice messages and video notes.
 *
 * Browser-bound (getUserMedia, MediaRecorder, Web Audio, canvas), so it is not
 * unit-tested; every decision it could get wrong lives in a pure module that
 * is: the gesture (`recording-machine.ts`), the container choice and the crop
 * (`capture-format.ts`), and the waveform fold (`waveform.ts`).
 *
 * ## Voice
 *
 * The microphone stream goes straight into a `MediaRecorder` with the MP4/AAC
 * type `pickRecordingMime` chose. A parallel `AnalyserNode` reads the level
 * every `LEVEL_SAMPLE_MS`: that single series drives both the live meter and
 * the envelope waveform, so there is no second decode of the finished file.
 *
 * ## Video note
 *
 * The camera frame is drawn, centre-cropped to a square and scaled to at most
 * 480 px, onto a canvas; the recorder records the canvas's video track plus
 * the microphone's audio track, as H.264 + AAC in MP4. Drawing through a
 * canvas is what makes the output square whatever the camera's aspect ratio
 * is, and it is also what lets "flip camera" swap the source mid-take without
 * restarting the recorder. The poster frame is taken from the same canvas
 * shortly after the first frames land and JPEG-encoded under the 256 KiB thumb
 * bound.
 *
 * ### The recorder is never handed a canvas nobody has drawn on
 *
 * THE DEFECT this shape exists for: the camera was attached to a `<video>`, the
 * element was asked to play without waiting, a draw loop that returns early
 * while the source has no frame was armed, and `canvas.captureStream()` was
 * handed to a fresh `MediaRecorder` — all in one synchronous breath, so the
 * canvas could not possibly have been drawn on yet. `getUserMedia` resolving
 * means the DEVICE is open; the first frame arrives some time after that (tens
 * of milliseconds on a fake camera, hundreds on a real one). The audio covered
 * the whole take and the picture covered the take minus that window, silently
 * sliding the two against each other by exactly the camera's opening time.
 *
 * Two things now hold, and they answer different halves:
 *
 *   * `acquireStream` does not resolve for a video note until the source has a
 *     frame that can be DRAWN. That happens while the machine is still in
 *     `requesting` — so the wait lands under the bar's "Starting…", where a
 *     person is told the device is opening, rather than under a clock and a red
 *     dot that would be claiming to record it. `start()` then draws that frame
 *     synchronously, before the recorder exists.
 *   * the canvas is captured in EXPLICIT-FRAME mode (`captureStream(0)` plus
 *     `requestFrame()`), so a frame is emitted only ever right after a
 *     successful `drawImage`. That is structural rather than timing: even if the
 *     wait above times out on a camera that never produces anything, the
 *     encoder cannot receive the blank canvas.
 *
 * Duration is measured with the monotonic clock from start to stop: the MP4
 * a `MediaRecorder` emits may carry no usable duration.
 */

import {
  ATTACHMENT_MIME,
  MAX_THUMB_BYTES,
  POSTER_SIDE,
  pickRecordingMime,
  squareCrop,
} from "./capture-format";
import type { CaptureKind } from "./recording-machine";
import { downsampleWaveform, rmsLevel } from "./waveform";

/** How often the level meter / waveform series is sampled. */
const LEVEL_SAMPLE_MS = 50;
/** Frame rate the video-note canvas is redrawn and captured at. */
const VIDEO_FPS = 30;
/** When the poster frame is taken, after the first frame is drawn. */
const POSTER_DELAY_MS = 300;
/**
 * How long a video note waits for the camera's FIRST frame before going ahead
 * without one.
 *
 * A bound rather than an open wait: a camera that opens and then never produces
 * anything must not hold a press down forever. Going ahead is safe because the
 * capture is explicit-frame — nothing reaches the encoder until something has
 * been drawn — so the cost of the timeout is a take whose picture starts late,
 * which is what happened on every take before this existed.
 */
const FIRST_FRAME_TIMEOUT_MS = 2000;
/** How often the first-frame wait re-checks, for engines with no
 *  `requestVideoFrameCallback` (Firefox) and as a floor under it. */
const FRAME_POLL_MS = 16;

export interface CapturedPoster {
  readonly blob: Blob;
  readonly mime: string;
  readonly width: number;
  readonly height: number;
}

export interface CaptureResult {
  readonly kind: CaptureKind;
  readonly blob: Blob;
  /** The attachment MIME type (`audio/mp4` / `video/mp4`), no parameters. */
  readonly mime: string;
  readonly durationMs: number;
  readonly waveform: Uint8Array | undefined;
  readonly width: number | undefined;
  readonly height: number | undefined;
  readonly poster: CapturedPoster | undefined;
}

export type PermissionState = "granted" | "prompt" | "denied" | "unknown";

/** Whether this browser can record `kind` in the one allowed format. */
export function recordingSupported(kind: CaptureKind): boolean {
  if (typeof MediaRecorder === "undefined" || navigator.mediaDevices?.getUserMedia === undefined) {
    return false;
  }
  return pickRecordingMime(kind, (m) => MediaRecorder.isTypeSupported(m)) !== null;
}

/** The Permissions API's answer for everything `kind` needs; `unknown` when
 *  the browser cannot say (Firefox and Safari for camera / microphone). */
export async function queryPermission(kind: CaptureKind): Promise<PermissionState> {
  const names = kind === "voice" ? ["microphone"] : ["microphone", "camera"];
  const permissions = navigator.permissions;
  if (permissions === undefined) return "unknown";
  try {
    const states = await Promise.all(
      names.map((name) => permissions.query({ name: name as PermissionName }).then((s) => s.state)),
    );
    if (states.includes("denied")) return "denied";
    if (states.every((s) => s === "granted")) return "granted";
    return "prompt";
  } catch {
    return "unknown";
  }
}

/** `true` / `false` when the device list says whether a camera exists;
 *  `null` when it cannot be read. */
export async function hasCamera(): Promise<boolean | null> {
  try {
    const devices = await navigator.mediaDevices.enumerateDevices();
    return devices.some((d) => d.kind === "videoinput");
  } catch {
    return null;
  }
}

export type AcquireFailure = "denied" | "no-device" | "failed";

export class AcquireError extends Error {
  override name = "AcquireError";
  constructor(public readonly reason: AcquireFailure, cause: unknown) {
    super(`media acquire failed: ${reason}`, { cause });
  }
}

function classify(err: unknown): AcquireFailure {
  const name = err instanceof DOMException ? err.name : "";
  if (name === "NotAllowedError" || name === "SecurityError") return "denied";
  if (name === "NotFoundError" || name === "OverconstrainedError") return "no-device";
  return "failed";
}

/**
 * The `<video>` a video-note stream has already been decoded into, ready to be
 * drawn from synchronously.
 *
 * Keyed by the stream it decodes, so it travels with that stream from
 * `acquireStream` to whichever `MediaTake` ends up recording it — the take is
 * constructed from the stream alone, and a `<video>` created inside `start()`
 * cannot have a frame yet however the camera is behaving. Released by
 * `stopStream`, which is the one place a stream's life ends.
 */
const preparedSources = new WeakMap<MediaStream, HTMLVideoElement>();

/** A muted, playing `<video>` fed by `stream`. */
function sourceVideoFor(stream: MediaStream): HTMLVideoElement {
  const video = document.createElement("video");
  video.muted = true;
  video.playsInline = true;
  video.srcObject = stream;
  void video.play().catch(() => undefined);
  return video;
}

/**
 * Whether `video` has a frame `drawImage` would draw.
 *
 * Both halves are load-bearing: the dimensions because the square crop is
 * computed from them (`squareCrop` refuses a frame with no usable size), and
 * `HAVE_CURRENT_DATA` because that is the platform's own statement that a frame
 * for the current position exists.
 */
function hasFrame(video: HTMLVideoElement): boolean {
  return video.videoWidth > 0 && video.videoHeight > 0 && video.readyState >= 2;
}

/** Wait for `video` to have a drawable frame. Resolves either way — `false` on
 *  the timeout — because every draw is guarded regardless. */
function waitForFrame(video: HTMLVideoElement, timeoutMs: number): Promise<boolean> {
  if (hasFrame(video)) return Promise.resolve(true);
  return new Promise((resolve) => {
    let settled = false;
    const finish = (ok: boolean): void => {
      if (settled) return;
      settled = true;
      clearTimeout(deadline);
      clearInterval(poll);
      resolve(ok);
    };
    const deadline = setTimeout(() => finish(false), timeoutMs);
    // A PRESENTED frame is the strongest signal, where the engine offers it.
    const rvfc =
      typeof video.requestVideoFrameCallback === "function"
        ? video.requestVideoFrameCallback.bind(video)
        : null;
    const onPresented = (): void => {
      if (hasFrame(video)) finish(true);
      else if (!settled) rvfc?.(onPresented);
    };
    rvfc?.(onPresented);
    // And a poll under it: Firefox has no `requestVideoFrameCallback`, and a
    // rejected `play()` presents nothing while the metadata and the first
    // decoded frame still arrive.
    const poll = setInterval(() => {
      if (hasFrame(video)) finish(true);
    }, FRAME_POLL_MS);
  });
}

/**
 * Open the stream `kind` records from. Rejects with [`AcquireError`].
 *
 * For a video note this also waits for the camera's first frame (see the class
 * doc) — so the returned stream is one a take can draw from at once, and the
 * wait is spent in the machine's `requesting` phase.
 *
 * One consequence, stated because it is not obvious: `requesting` is also what
 * `PROMPT_INFERENCE_MS` is measured against, and on a browser that will not
 * report a camera permission (Firefox, Safari) a grant slower than a second is
 * read as "a prompt was shown", which does not record. The camera's warm-up now
 * counts towards that second. It is additive to a `getUserMedia` that can
 * already exceed it on a cold camera, so this narrows an existing margin rather
 * than opening a new failure — and the alternative, recording a canvas with
 * nothing on it, is the defect.
 */
export async function acquireStream(
  kind: CaptureKind,
  facing: "user" | "environment" = "user",
  /** The profile camera (docs/ux/profile-screen.md section 2.3) asks for the
   *  camera alone: a profile photo or video never keeps sound. */
  options: { readonly audio?: boolean; readonly idealSide?: number } = {},
): Promise<MediaStream> {
  let stream: MediaStream;
  try {
    stream = await navigator.mediaDevices.getUserMedia(
      kind === "voice"
        ? { audio: true }
        : {
            audio: options.audio ?? true,
            video: {
              facingMode: facing,
              width: { ideal: options.idealSide ?? 640 },
              height: { ideal: options.idealSide ?? 640 },
            },
          },
    );
  } catch (err) {
    throw new AcquireError(classify(err), err);
  }
  if (kind === "video_note") {
    const video = sourceVideoFor(stream);
    preparedSources.set(stream, video);
    await waitForFrame(video, FIRST_FRAME_TIMEOUT_MS);
  }
  return stream;
}

export function stopStream(stream: MediaStream | null): void {
  if (stream !== null) {
    const prepared = preparedSources.get(stream);
    if (prepared !== undefined) {
      preparedSources.delete(stream);
      prepared.srcObject = null;
    }
  }
  for (const track of stream?.getTracks() ?? []) track.stop();
}

/** One take. Create with an acquired stream, `start`, then `stop` or
 *  `discard`. Owns the stream from construction on. */
export class MediaTake {
  private recorder: MediaRecorder | null = null;
  private readonly chunks: Blob[] = [];
  private audioContext: AudioContext | null = null;
  private levelTimer: ReturnType<typeof setInterval> | null = null;
  private readonly levels: number[] = [];
  private startedAt = 0;
  private currentLevel = 0;
  /** The same reading as `currentLevel` before the meter's ×3 display gain.
   *  `microphone-silence.ts` compares against a floor shared with iOS and
   *  Android, so it must see the scale those two see, not this one's. */
  private rawLevel = 0;
  /** Whether the analyser has ever produced a reading. A take whose meter never
   *  started (no audio track, no `AudioContext`, or the catch below) has MADE no
   *  measurement, and a silence warning drawn from an absent measurement would
   *  accuse a microphone that may be working. */
  private levelSampled = false;
  private endedHandler: (() => void) | null = null;
  // Video-note pipeline.
  private sourceVideo: HTMLVideoElement | null = null;
  private canvas: HTMLCanvasElement | null = null;
  private canvasStream: MediaStream | null = null;
  /** The capture track, when it is in explicit-frame mode: the one thing that
   *  emits a frame, and only ever right after a draw. */
  private canvasTrack: CanvasCaptureMediaStreamTrack | null = null;
  private drawTimer: ReturnType<typeof setInterval> | null = null;
  private poster: CapturedPoster | undefined;
  private posterTimer: ReturnType<typeof setTimeout> | null = null;
  private posterArmed = false;
  private facing: "user" | "environment" = "user";
  private stopped = false;

  constructor(
    readonly kind: CaptureKind,
    private stream: MediaStream,
  ) {}

  /** The live stream the round preview shows (the camera, uncropped). */
  get previewStream(): MediaStream {
    return this.stream;
  }

  get facingMode(): "user" | "environment" {
    return this.facing;
  }

  /** Current level, 0..1, for the meter. */
  level(): number {
    return this.currentLevel;
  }

  /**
   * The newest reading on the raw 0..1 scale the three clients share, or `null`
   * when the meter has produced nothing to read.
   *
   * `null` and `0` are deliberately different answers: `0` is the meter saying
   * it heard nothing, `null` is there being no meter to say it. UX spec §2's
   * warning must follow only the first.
   */
  measuredLevel(): number | null {
    return this.levelSampled ? this.rawLevel : null;
  }

  /** Called once if any captured track ends on its own — permission revoked,
   *  device unplugged. */
  onTrackEnded(handler: () => void): void {
    this.endedHandler = handler;
    for (const track of this.stream.getTracks()) this.watchTrack(track);
  }

  private watchTrack(track: MediaStreamTrack): void {
    track.addEventListener("ended", () => {
      if (!this.stopped) this.endedHandler?.();
    });
  }

  start(): void {
    const mimeType = pickRecordingMime(this.kind, (m) => MediaRecorder.isTypeSupported(m));
    if (mimeType === null) throw new Error("recording format unsupported");
    const recordStream = this.kind === "voice" ? this.stream : this.startVideoPipeline();
    const recorder = new MediaRecorder(recordStream, { mimeType });
    recorder.addEventListener("dataavailable", (e) => {
      if (e.data.size > 0) this.chunks.push(e.data);
    });
    this.recorder = recorder;
    this.startLevelSampling();
    this.startedAt = performance.now();
    recorder.start(1000);
    // The take's first picture, pushed once the encoder is running and from a
    // canvas that already holds a camera frame — so the video track starts
    // where the audio track does. The draw loop pushes the rest.
    this.canvasTrack?.requestFrame();
  }

  private startLevelSampling(): void {
    const tracks = this.stream.getAudioTracks();
    if (tracks.length === 0 || typeof AudioContext === "undefined") return;
    try {
      const ctx = new AudioContext();
      const source = ctx.createMediaStreamSource(new MediaStream(tracks));
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 1024;
      source.connect(analyser);
      this.audioContext = ctx;
      const buffer = new Float32Array(analyser.fftSize);
      this.levelTimer = setInterval(() => {
        analyser.getFloatTimeDomainData(buffer);
        const level = rmsLevel(buffer);
        this.currentLevel = Math.min(1, level * 3);
        this.rawLevel = level;
        this.levelSampled = true;
        this.levels.push(level);
      }, LEVEL_SAMPLE_MS);
    } catch {
      // No meter and no waveform; the recording itself is unaffected.
      this.audioContext = null;
    }
  }

  private startVideoPipeline(): MediaStream {
    // The source `acquireStream` already waited for a frame on, or a fresh one
    // when there is none (a take whose stream came from somewhere else). Taken
    // out of the map here: from now on this take owns it and tears it down.
    const prepared = preparedSources.get(this.stream);
    preparedSources.delete(this.stream);
    this.sourceVideo = prepared ?? sourceVideoFor(this.stream);

    const settings = this.stream.getVideoTracks()[0]?.getSettings();
    const initial = squareCrop(settings?.width ?? 640, settings?.height ?? 480);
    const canvas = document.createElement("canvas");
    canvas.width = initial?.outputSide ?? 480;
    canvas.height = initial?.outputSide ?? 480;
    this.canvas = canvas;
    const g = canvas.getContext("2d");

    // BEFORE the canvas is captured and before the recorder exists: the frame
    // the acquire waited for goes on now, synchronously, so there is no moment
    // at which this canvas is both capturable and blank.
    this.drawFrame(g);
    this.canvasStream = this.captureCanvas(canvas);
    this.drawTimer = setInterval(() => {
      if (this.drawFrame(g)) this.canvasTrack?.requestFrame();
    }, 1000 / VIDEO_FPS);

    const out = new MediaStream();
    for (const t of this.canvasStream.getVideoTracks()) out.addTrack(t);
    for (const t of this.stream.getAudioTracks()) out.addTrack(t);
    return out;
  }

  /**
   * Capture `canvas` in explicit-frame mode, so only a drawn frame is ever
   * emitted.
   *
   * `captureStream(0)` produces nothing until `requestFrame()`, which is the
   * guarantee; a browser whose track has no `requestFrame` would then produce
   * nothing AT ALL, so that one falls back to the timed capture rather than
   * recording a picture-less note. Nothing is lost there that the first-frame
   * wait has not already secured.
   */
  private captureCanvas(canvas: HTMLCanvasElement): MediaStream {
    const onDemand = canvas.captureStream(0);
    const track = onDemand.getVideoTracks()[0] as CanvasCaptureMediaStreamTrack | undefined;
    if (track !== undefined && typeof track.requestFrame === "function") {
      this.canvasTrack = track;
      return onDemand;
    }
    for (const t of onDemand.getTracks()) t.stop();
    return canvas.captureStream(VIDEO_FPS);
  }

  /** Draw the source's current frame, centre-cropped onto the canvas. `false`
   *  when there is no frame to draw — which is what keeps a blank canvas from
   *  being offered to the encoder. */
  private drawFrame(g: CanvasRenderingContext2D | null): boolean {
    const src = this.sourceVideo;
    const canvas = this.canvas;
    if (g === null || src === null || canvas === null || src.videoWidth === 0) return false;
    const crop = squareCrop(src.videoWidth, src.videoHeight, canvas.width);
    if (crop === null) return false;
    g.drawImage(src, crop.sx, crop.sy, crop.sourceSide, crop.sourceSide, 0, 0, canvas.width, canvas.height);
    if (!this.posterArmed) {
      this.posterArmed = true;
      this.posterTimer = setTimeout(() => {
        void this.capturePoster();
      }, POSTER_DELAY_MS);
    }
    return true;
  }

  private async capturePoster(): Promise<void> {
    const canvas = this.canvas;
    if (canvas === null) return;
    const small = document.createElement("canvas");
    small.width = POSTER_SIDE;
    small.height = POSTER_SIDE;
    small.getContext("2d")?.drawImage(canvas, 0, 0, POSTER_SIDE, POSTER_SIDE);
    for (const quality of [0.8, 0.6, 0.4]) {
      const blob = await new Promise<Blob | null>((resolve) => small.toBlob(resolve, "image/jpeg", quality));
      if (blob !== null && blob.size > 0 && blob.size <= MAX_THUMB_BYTES) {
        this.poster = { blob, mime: "image/jpeg", width: POSTER_SIDE, height: POSTER_SIDE };
        return;
      }
    }
    // No poster within the bound: the note is sent without one and the
    // bubble shows a neutral circle (ADR-0047 open question, current answer).
  }

  /** Swap the camera mid-take (locked video note). Recording continues from
   *  the canvas; only the source changes. A failure keeps the current camera. */
  async flipCamera(): Promise<boolean> {
    if (this.kind !== "video_note" || this.sourceVideo === null) return false;
    const next = this.facing === "user" ? "environment" : "user";
    let fresh: MediaStream;
    try {
      fresh = await navigator.mediaDevices.getUserMedia({
        video: { facingMode: { exact: next }, width: { ideal: 640 }, height: { ideal: 640 } },
      });
    } catch {
      return false;
    }
    const newTrack = fresh.getVideoTracks()[0];
    if (newTrack === undefined || this.stopped) {
      stopStream(fresh);
      return false;
    }
    for (const old of this.stream.getVideoTracks()) {
      this.stream.removeTrack(old);
      old.stop();
    }
    this.stream.addTrack(newTrack);
    this.watchTrack(newTrack);
    this.sourceVideo.srcObject = new MediaStream([newTrack]);
    void this.sourceVideo.play().catch(() => undefined);
    this.facing = next;
    // The same wait as the take's own start, for the same reason: until the new
    // camera has produced a frame there is nothing to draw, and the draw loop
    // would be holding the old camera's last frame. Bounded, and a timeout
    // changes nothing — the loop is guarded either way.
    await waitForFrame(this.sourceVideo, FIRST_FRAME_TIMEOUT_MS);
    return true;
  }

  private teardown(): void {
    this.stopped = true;
    if (this.levelTimer !== null) clearInterval(this.levelTimer);
    if (this.drawTimer !== null) clearInterval(this.drawTimer);
    if (this.posterTimer !== null) clearTimeout(this.posterTimer);
    this.levelTimer = null;
    this.drawTimer = null;
    this.posterTimer = null;
    void this.audioContext?.close().catch(() => undefined);
    this.audioContext = null;
    if (this.sourceVideo !== null) {
      this.sourceVideo.srcObject = null;
      this.sourceVideo = null;
    }
    stopStream(this.canvasStream);
    this.canvasStream = null;
    this.canvasTrack = null;
    stopStream(this.stream);
  }

  /** Finish and hand back the take. */
  async stop(): Promise<CaptureResult> {
    const recorder = this.recorder;
    const durationMs = Math.round(performance.now() - this.startedAt);
    if (recorder === null) {
      this.teardown();
      throw new Error("take was never started");
    }
    if (recorder.state !== "inactive") {
      await new Promise<void>((resolve) => {
        recorder.addEventListener("stop", () => resolve(), { once: true });
        recorder.stop();
      });
    }
    if (this.kind === "video_note" && this.poster === undefined && this.canvas !== null) {
      await this.capturePoster();
    }
    const width = this.kind === "video_note" ? this.canvas?.width : undefined;
    const height = this.kind === "video_note" ? this.canvas?.height : undefined;
    this.teardown();
    const mime = ATTACHMENT_MIME[this.kind];
    const blob = new Blob(this.chunks, { type: mime });
    if (blob.size === 0) throw new Error("empty recording");
    return {
      kind: this.kind,
      blob,
      mime,
      durationMs,
      waveform: this.kind === "voice" ? downsampleWaveform(this.levels) : undefined,
      width,
      height,
      poster: this.poster,
    };
  }

  /** Throw the take away. Nothing is kept. */
  discard(): void {
    const recorder = this.recorder;
    if (recorder !== null && recorder.state !== "inactive") {
      try {
        recorder.stop();
      } catch {
        // Already stopping.
      }
    }
    this.chunks.length = 0;
    this.teardown();
  }
}
