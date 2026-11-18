// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 3 — a picked photo is decoded on the device
 * and re-encoded as JPEG with its longest side at most the core's
 * `photoMaxLongestSidePx` (2560, chosen). Drawing the decoded pixels onto a
 * fresh canvas carries no EXIF (location, camera, time) by construction.
 */

/** JPEG quality, chosen. */
export const PHOTO_JPEG_QUALITY = 0.85;

/** The size `width x height` scales to so its longest side is at most `max`;
 *  never enlarged, never below 1 px. */
export function scaledSize(width: number, height: number, max: number): { width: number; height: number } {
  const longest = Math.max(width, height);
  if (longest <= max || longest <= 0) return { width: Math.max(1, Math.round(width)), height: Math.max(1, Math.round(height)) };
  const scale = max / longest;
  return { width: Math.max(1, Math.round(width * scale)), height: Math.max(1, Math.round(height * scale)) };
}

/** `holiday.HEIC` -> `holiday.jpg`. */
export function jpegName(name: string): string {
  const dot = name.lastIndexOf(".");
  const base = dot > 0 ? name.slice(0, dot) : name.length > 0 ? name : "photo";
  return `${base}.jpg`;
}

async function canvasToJpeg(width: number, height: number, draw: (ctx: CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D) => void): Promise<Blob> {
  if (typeof OffscreenCanvas !== "undefined") {
    const canvas = new OffscreenCanvas(width, height);
    const ctx = canvas.getContext("2d");
    if (ctx === null) throw new Error("no 2d context");
    draw(ctx);
    return canvas.convertToBlob({ type: "image/jpeg", quality: PHOTO_JPEG_QUALITY });
  }
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  if (ctx === null) throw new Error("no 2d context");
  draw(ctx);
  return new Promise<Blob>((resolve, reject) => {
    canvas.toBlob(
      (b) => (b === null ? reject(new Error("jpeg encode failed")) : resolve(b)),
      "image/jpeg",
      PHOTO_JPEG_QUALITY,
    );
  });
}

/** Decode, scale and re-encode `file` as JPEG. Throws when it cannot be
 *  decoded (the caller shows `attach.preview.photo_unreadable`). */
export async function reencodePhoto(file: Blob & { name?: string }, maxLongestSide: number): Promise<File> {
  const bitmap = await createImageBitmap(file, { imageOrientation: "from-image" });
  try {
    const size = scaledSize(bitmap.width, bitmap.height, maxLongestSide);
    const blob = await canvasToJpeg(size.width, size.height, (ctx) => {
      ctx.drawImage(bitmap, 0, 0, size.width, size.height);
    });
    return new File([blob], jpegName(file.name ?? "photo"), { type: "image/jpeg" });
  } finally {
    bitmap.close();
  }
}
