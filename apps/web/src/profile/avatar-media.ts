// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0084 / docs/ux/profile-screen.md section 2.2 — producing the profile
 * photo in the browser: the square JPEG still, and the square H.264 MP4
 * without sound for a profile video.
 *
 * Browser-bound (canvas, `<video>`, `MediaRecorder`), so not unit-tested;
 * the arithmetic it uses (the crop, the size loop, the start clamp, the
 * no-re-encode fallback) is in `avatar-crop.ts`, which is.
 *
 * A video is re-encoded by drawing the chosen square of the playing source
 * onto a canvas and recording the canvas's video track alone, so no sound
 * can reach the output whatever the source carries. A browser whose
 * `MediaRecorder` cannot write H.264 MP4 (Firefox today) keeps a picked
 * video only when it is already usable as it is (`usableWithoutReencode`).
 */

import {
  STILL_MAX_SIDE,
  VIDEO_MAX_BYTES,
  VIDEO_MAX_SIDE,
} from "./avatar-model";
import {
  encodeUnderLimit,
  keptVideoDurationMs,
  outputSide,
  usableWithoutReencode,
  type SourceSquare,
} from "./avatar-crop";

/** The cropped result the profile screen holds until Save. */
export interface LocalAvatar {
  readonly still: Blob;
  readonly stillSide: number;
  readonly video: { readonly blob: Blob; readonly side: number; readonly durationMs: number } | null;
  /** Object URLs for the preview; revoked by `releaseLocalAvatar`. */
  readonly stillUrl: string;
  readonly videoUrl: string | null;
}

export function releaseLocalAvatar(a: LocalAvatar | null | undefined): void {
  if (a === null || a === undefined) return;
  URL.revokeObjectURL(a.stillUrl);
  if (a.videoUrl !== null) URL.revokeObjectURL(a.videoUrl);
}

/** Most specific first; no audio codec: the output has no sound. */
const VIDEO_MIME_CANDIDATES: readonly string[] = [
  "video/mp4;codecs=avc1.42E01E",
  "video/mp4;codecs=avc1",
  "video/mp4",
];

/** The `MediaRecorder` type a profile video is written with, or `null`
 *  when this browser cannot write H.264 MP4. */
export function avatarVideoMime(): string | null {
  if (typeof MediaRecorder === "undefined") return null;
  for (const m of VIDEO_MIME_CANDIDATES) {
    try {
      if (MediaRecorder.isTypeSupported(m)) return m;
    } catch {
      /* unsupported */
    }
  }
  return null;
}

/** Load a picked or captured image. */
export async function loadImage(blob: Blob): Promise<HTMLImageElement> {
  const url = URL.createObjectURL(blob);
  try {
    const img = new Image();
    img.decoding = "async";
    img.src = url;
    await img.decode();
    return img;
  } finally {
    // The decoded image keeps its pixels; the URL is no longer needed.
    setTimeout(() => URL.revokeObjectURL(url), 0);
  }
}

/** Load a picked or captured video, muted, its metadata read. */
export function loadVideo(blob: Blob): Promise<{ readonly video: HTMLVideoElement; readonly url: string }> {
  const url = URL.createObjectURL(blob);
  return new Promise((resolve, reject) => {
    const v = document.createElement("video");
    v.muted = true;
    v.playsInline = true;
    v.preload = "auto";
    v.onloadedmetadata = (): void => {
      if (v.videoWidth > 0 && v.videoHeight > 0) resolve({ video: v, url });
      else reject(new Error("video has no picture"));
    };
    v.onerror = (): void => {
      URL.revokeObjectURL(url);
      reject(new Error("video does not load"));
    };
    v.src = url;
  });
}

/** Seek `video` to `ms` and wait for the frame. */
export function seekTo(video: HTMLVideoElement, ms: number): Promise<void> {
  return new Promise((resolve) => {
    const target = Math.max(0, ms / 1000);
    if (Math.abs(video.currentTime - target) < 0.01 && video.readyState >= 2) {
      resolve();
      return;
    }
    const done = (): void => {
      video.removeEventListener("seeked", done);
      clearTimeout(timer);
      resolve();
    };
    const timer = setTimeout(done, 3000);
    video.addEventListener("seeked", done);
    video.currentTime = target;
  });
}

function canvasBlob(canvas: HTMLCanvasElement, type: string, quality?: number): Promise<Blob | null> {
  return new Promise((resolve) => canvas.toBlob(resolve, type, quality));
}

/** The square still: the chosen square of `source`, JPEG, at most 640 px
 *  and 256 KiB. `null` when it cannot be made to fit. */
export async function encodeStill(
  source: CanvasImageSource,
  square: SourceSquare,
  maxSide: number = STILL_MAX_SIDE,
): Promise<{ readonly blob: Blob; readonly side: number } | null> {
  return encodeUnderLimit(outputSide(square.side, maxSide), async (side, quality) => {
    const canvas = document.createElement("canvas");
    canvas.width = side;
    canvas.height = side;
    const g = canvas.getContext("2d");
    if (g === null) return null;
    g.drawImage(source, square.sx, square.sy, square.side, square.side, 0, 0, side, side);
    return canvasBlob(canvas, "image/jpeg", quality);
  });
}

/** Whether a `<video>` carries sound: `null` when the browser cannot say. */
function hasAudioTrack(video: HTMLVideoElement): boolean | null {
  const v = video as HTMLVideoElement & {
    audioTracks?: { length: number };
    mozHasAudio?: boolean;
  };
  if (v.audioTracks !== undefined) return v.audioTracks.length > 0;
  if (typeof v.mozHasAudio === "boolean") return v.mozHasAudio;
  return null;
}

export type VideoEncodeOutcome =
  | { readonly kind: "ok"; readonly avatar: LocalAvatar }
  /** `This video can't be used as a profile video. …` */
  | { readonly kind: "unusable" };

/**
 * The profile video: the chosen square of `source` from `startMs`, at most
 * 10 s, square H.264 MP4 without sound, at most 480 px and 4 MiB, plus the
 * first-frame still.
 */
export async function encodeAvatarVideo(
  source: HTMLVideoElement,
  original: Blob,
  square: SourceSquare,
  startMs: number,
): Promise<VideoEncodeOutcome> {
  const durationMs = Number.isFinite(source.duration) ? source.duration * 1000 : Number.NaN;
  const keptMs = keptVideoDurationMs(durationMs, startMs);
  if (keptMs === null) return { kind: "unusable" };
  await seekTo(source, startMs);
  const still = await encodeStill(source, square);
  if (still === null) return { kind: "unusable" };

  const mime = avatarVideoMime();
  if (mime === null) {
    // Section 2.2 fallback: no re-encode, so the picked file must already fit.
    const ok = usableWithoutReencode({
      mp4: original.type === "video/mp4",
      width: source.videoWidth,
      height: source.videoHeight,
      bytes: original.size,
      durationMs,
      hasAudio: hasAudioTrack(source),
      maxSide: VIDEO_MAX_SIDE,
      maxBytes: VIDEO_MAX_BYTES,
    });
    if (!ok || startMs > 0) return { kind: "unusable" };
    return {
      kind: "ok",
      avatar: {
        still: still.blob,
        stillSide: still.side,
        video: { blob: original, side: source.videoWidth, durationMs: Math.floor(durationMs) },
        stillUrl: URL.createObjectURL(still.blob),
        videoUrl: URL.createObjectURL(original),
      },
    };
  }

  const side = outputSide(square.side, VIDEO_MAX_SIDE, true);
  for (const bitsPerSecond of [2_000_000, 900_000]) {
    const blob = await recordSquare(source, square, side, startMs, keptMs, mime, bitsPerSecond);
    if (blob !== null && blob.size > 0 && blob.size <= VIDEO_MAX_BYTES) {
      const video = new Blob([blob], { type: "video/mp4" });
      return {
        kind: "ok",
        avatar: {
          still: still.blob,
          stillSide: still.side,
          video: { blob: video, side, durationMs: keptMs },
          stillUrl: URL.createObjectURL(still.blob),
          videoUrl: URL.createObjectURL(video),
        },
      };
    }
  }
  return { kind: "unusable" };
}

/** Play `source` from `startMs` for `keptMs`, drawing the square onto a
 *  canvas whose video track alone is recorded. */
async function recordSquare(
  source: HTMLVideoElement,
  square: SourceSquare,
  side: number,
  startMs: number,
  keptMs: number,
  mime: string,
  bitsPerSecond: number,
): Promise<Blob | null> {
  await seekTo(source, startMs);
  const canvas = document.createElement("canvas");
  canvas.width = side;
  canvas.height = side;
  const g = canvas.getContext("2d");
  if (g === null) return null;
  const draw = (): void => {
    g.drawImage(source, square.sx, square.sy, square.side, square.side, 0, 0, side, side);
  };
  draw();
  const stream = canvas.captureStream(30);
  const recorder = new MediaRecorder(new MediaStream(stream.getVideoTracks()), {
    mimeType: mime,
    videoBitsPerSecond: bitsPerSecond,
  });
  const chunks: Blob[] = [];
  recorder.ondataavailable = (e): void => {
    if (e.data.size > 0) chunks.push(e.data);
  };
  const stopped = new Promise<void>((resolve) => {
    recorder.onstop = (): void => resolve();
  });
  const timer = setInterval(draw, 1000 / 30);
  recorder.start(500);
  source.muted = true;
  try {
    await source.play();
  } catch {
    clearInterval(timer);
    recorder.stop();
    await stopped;
    for (const t of stream.getTracks()) t.stop();
    return null;
  }
  await new Promise<void>((resolve) => {
    const end = startMs / 1000 + keptMs / 1000;
    const check = setInterval(() => {
      if (source.currentTime >= end || source.ended) {
        clearInterval(check);
        resolve();
      }
    }, 20);
  });
  source.pause();
  clearInterval(timer);
  recorder.stop();
  await stopped;
  for (const t of stream.getTracks()) t.stop();
  return new Blob(chunks, { type: "video/mp4" });
}

/** The crop screen's start strip: `count` small frames across the video. */
export async function videoStripFrames(video: HTMLVideoElement, count: number, side = 48): Promise<string[]> {
  const out: string[] = [];
  const durationMs = video.duration * 1000;
  if (!Number.isFinite(durationMs) || durationMs <= 0) return out;
  const canvas = document.createElement("canvas");
  canvas.width = side;
  canvas.height = side;
  const g = canvas.getContext("2d");
  if (g === null) return out;
  const s = Math.min(video.videoWidth, video.videoHeight);
  for (let i = 0; i < count; i++) {
    await seekTo(video, (durationMs * i) / count);
    g.drawImage(video, (video.videoWidth - s) / 2, (video.videoHeight - s) / 2, s, s, 0, 0, side, side);
    out.push(canvas.toDataURL("image/jpeg", 0.6));
  }
  await seekTo(video, 0);
  return out;
}
