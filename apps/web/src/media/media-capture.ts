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

/** Open the stream `kind` records from. Rejects with [`AcquireError`]. */
export async function acquireStream(
  kind: CaptureKind,
  facing: "user" | "environment" = "user",
): Promise<MediaStream> {
  try {
    return await navigator.mediaDevices.getUserMedia(
      kind === "voice"
        ? { audio: true }
        : {
            audio: true,
            video: { facingMode: facing, width: { ideal: 640 }, height: { ideal: 640 } },
          },
    );
  } catch (err) {
    throw new AcquireError(classify(err), err);
  }
}

export function stopStream(stream: MediaStream | null): void {
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
  private endedHandler: (() => void) | null = null;
  // Video-note pipeline.
  private sourceVideo: HTMLVideoElement | null = null;
  private canvas: HTMLCanvasElement | null = null;
  private canvasStream: MediaStream | null = null;
  private drawTimer: ReturnType<typeof setInterval> | null = null;
  private poster: CapturedPoster | undefined;
  private posterTimer: ReturnType<typeof setTimeout> | null = null;
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
        this.levels.push(level);
      }, LEVEL_SAMPLE_MS);
    } catch {
      // No meter and no waveform; the recording itself is unaffected.
      this.audioContext = null;
    }
  }

  private startVideoPipeline(): MediaStream {
    const video = document.createElement("video");
    video.muted = true;
    video.playsInline = true;
    video.srcObject = this.stream;
    void video.play().catch(() => undefined);
    this.sourceVideo = video;

    const settings = this.stream.getVideoTracks()[0]?.getSettings();
    const initial = squareCrop(settings?.width ?? 640, settings?.height ?? 480);
    const canvas = document.createElement("canvas");
    canvas.width = initial?.outputSide ?? 480;
    canvas.height = initial?.outputSide ?? 480;
    this.canvas = canvas;
    const g = canvas.getContext("2d");

    let firstFrame = true;
    this.drawTimer = setInterval(() => {
      const src = this.sourceVideo;
      if (g === null || src === null || src.videoWidth === 0) return;
      const crop = squareCrop(src.videoWidth, src.videoHeight, canvas.width);
      if (crop === null) return;
      g.drawImage(src, crop.sx, crop.sy, crop.sourceSide, crop.sourceSide, 0, 0, canvas.width, canvas.height);
      if (firstFrame) {
        firstFrame = false;
        this.posterTimer = setTimeout(() => {
          void this.capturePoster();
        }, POSTER_DELAY_MS);
      }
    }, 1000 / VIDEO_FPS);

    const out = new MediaStream();
    const canvasStream = canvas.captureStream(VIDEO_FPS);
    this.canvasStream = canvasStream;
    for (const t of canvasStream.getVideoTracks()) out.addTrack(t);
    for (const t of this.stream.getAudioTracks()) out.addTrack(t);
    return out;
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
