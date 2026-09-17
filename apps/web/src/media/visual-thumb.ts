// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX spec `docs/ux/media-messages.md` section 10, sending side — read a picked
 * photo's or video's pixel size and render its JPEG thumb (a photo's scaled
 * copy, a video's first frame), before the attachment is uploaded.
 *
 * Browser-only (canvas, `createImageBitmap`, `<video>`); the arithmetic lives
 * in `visual-media.ts`. Nothing here throws: a decoder that cannot read the
 * file yields no dimensions and no thumb, and the attachment is sent without
 * them. A preview never blocks a send.
 */

import {
  THUMB_HARD_CAP_BYTES,
  fitWithin,
  pickThumbQuality,
  thumbDimensions,
  type Size,
} from "./visual-media";

export interface VisualPreview {
  /** Pixel size, scaled to fit 4096. Absent when the decoder failed. */
  readonly size: Size | null;
  /** The JPEG thumb, or `null` when none could be produced. */
  readonly thumb: { readonly blob: Blob; readonly width: number; readonly height: number } | null;
}

const NONE: VisualPreview = { size: null, thumb: null };
const VIDEO_TIMEOUT_MS = 8000;

type Drawable = CanvasImageSource & { readonly width?: number };

async function encodeJpeg(source: Drawable, sw: number, sh: number): Promise<VisualPreview["thumb"]> {
  const dims = thumbDimensions(sw, sh);
  if (dims === null) return null;
  const canvas = document.createElement("canvas");
  canvas.width = dims.width;
  canvas.height = dims.height;
  const ctx = canvas.getContext("2d");
  if (ctx === null) return null;
  // JPEG has no alpha: a transparent PNG would otherwise turn black.
  ctx.fillStyle = "#ffffff";
  ctx.fillRect(0, 0, dims.width, dims.height);
  ctx.drawImage(source, 0, 0, dims.width, dims.height);
  const picked = await pickThumbQuality(async (quality) => {
    const blob = await new Promise<Blob | null>((resolve) => {
      canvas.toBlob((b) => resolve(b), "image/jpeg", quality);
    });
    return { bytes: blob?.size ?? 0, value: blob };
  });
  const blob = picked?.value ?? null;
  if (blob === null || blob.size === 0 || blob.size > THUMB_HARD_CAP_BYTES) return null;
  return { blob, width: dims.width, height: dims.height };
}

/** Pixel size and thumb of a picked image. */
export async function describeImage(file: Blob): Promise<VisualPreview> {
  if (typeof createImageBitmap !== "function") return NONE;
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file, { imageOrientation: "from-image" });
  } catch {
    return NONE;
  }
  try {
    const size = fitWithin(bitmap.width, bitmap.height);
    let thumb: VisualPreview["thumb"] = null;
    try {
      thumb = await encodeJpeg(bitmap, bitmap.width, bitmap.height);
    } catch {
      thumb = null;
    }
    return { size, thumb };
  } finally {
    bitmap.close();
  }
}

function waitFor(el: HTMLVideoElement, event: string, timeoutMs: number): Promise<boolean> {
  return new Promise((resolve) => {
    const done = (ok: boolean): void => {
      clearTimeout(timer);
      el.removeEventListener(event, onEvent);
      el.removeEventListener("error", onError);
      resolve(ok);
    };
    const onEvent = (): void => done(true);
    const onError = (): void => done(false);
    const timer = setTimeout(() => done(false), timeoutMs);
    el.addEventListener(event, onEvent);
    el.addEventListener("error", onError);
  });
}

/** Pixel size of the first video track and a poster of its first frame. */
export async function describeVideo(file: Blob): Promise<VisualPreview> {
  if (typeof document === "undefined") return NONE;
  const url = URL.createObjectURL(file);
  const video = document.createElement("video");
  video.muted = true;
  video.playsInline = true;
  video.preload = "auto";
  try {
    const loaded = waitFor(video, "loadeddata", VIDEO_TIMEOUT_MS);
    video.src = url;
    if (!(await loaded)) return NONE;
    const size = fitWithin(video.videoWidth, video.videoHeight);
    if (size === null) return NONE;
    // Some engines paint nothing for the frame at 0 until a seek completes.
    const seeked = waitFor(video, "seeked", 2000);
    try {
      video.currentTime = Math.min(0.05, Number.isFinite(video.duration) ? video.duration / 2 : 0.05);
    } catch {
      // A non-seekable stream: draw whatever is current.
    }
    await seeked;
    let thumb: VisualPreview["thumb"] = null;
    try {
      thumb = await encodeJpeg(video, video.videoWidth, video.videoHeight);
    } catch {
      thumb = null;
    }
    return { size, thumb };
  } catch {
    return NONE;
  } finally {
    video.removeAttribute("src");
    video.load();
    URL.revokeObjectURL(url);
  }
}
