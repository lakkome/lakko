// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * UX spec `docs/ux/media-messages.md` section 10 — the pure geometry of photo
 * and video bubbles, kept free of the DOM so vitest can run it without jsdom:
 *
 *   * the pixel size a sender attaches (`fitWithin`, the 4096 wire bound);
 *   * the thumb a sender renders (`thumbDimensions`, longest side 320);
 *   * the JPEG quality ladder that keeps a thumb under its budget
 *     (`pickThumbQuality`).
 *
 * Where a received photo or video is drawn is the shared mosaic rule
 * (`mosaic-layout.ts`, section 11), one item included.
 */

/** The core drops a `width` / `height` outside 1..=4096 (ADR-0047). */
export const MAX_MEDIA_DIMENSION = 4096;
/** Longest side of a photo thumb or video poster. */
export const THUMB_LONGEST_SIDE = 320;
/** Size the sender aims a thumb under. */
export const THUMB_TARGET_BYTES = 64 * 1024;
/** The core's hard wire cap on a thumb blob. */
export const THUMB_HARD_CAP_BYTES = 256 * 1024;
/** JPEG qualities tried in order until a thumb fits the target. */
export const THUMB_QUALITY_STEPS: readonly number[] = [0.82, 0.72, 0.62, 0.52, 0.42, 0.32];

export interface Size {
  readonly width: number;
  readonly height: number;
}

function validSide(n: number | undefined): n is number {
  return n !== undefined && Number.isFinite(n) && n > 0;
}

/**
 * Scale `(width, height)` down proportionally so neither side exceeds `max`.
 * Never scales up. Returns `null` for a non-positive or non-finite input, so a
 * decoder that reports `0x0` sends no dimensions rather than a bogus box.
 */
export function fitWithin(width: number, height: number, max: number = MAX_MEDIA_DIMENSION): Size | null {
  if (!validSide(width) || !validSide(height) || !validSide(max)) return null;
  const scale = Math.min(1, max / width, max / height);
  return {
    width: Math.max(1, Math.min(max, Math.round(width * scale))),
    height: Math.max(1, Math.min(max, Math.round(height * scale))),
  };
}

/** The size a thumb is rendered at: longest side `THUMB_LONGEST_SIDE`, never
 *  upscaled past the source. */
export function thumbDimensions(width: number, height: number): Size | null {
  return fitWithin(width, height, THUMB_LONGEST_SIDE);
}

/**
 * Walk the quality ladder: `encode(q)` returns the encoded size at quality
 * `q`. The first quality under `THUMB_TARGET_BYTES` wins; if none does, the
 * smallest result under `THUMB_HARD_CAP_BYTES` is used; if even that is over
 * the hard cap, `null` — the attachment goes without a thumb.
 */
export async function pickThumbQuality<T>(
  encode: (quality: number) => Promise<{ readonly bytes: number; readonly value: T }>,
): Promise<{ readonly quality: number; readonly value: T } | null> {
  let best: { quality: number; bytes: number; value: T } | null = null;
  for (const quality of THUMB_QUALITY_STEPS) {
    const r = await encode(quality);
    if (r.bytes > 0 && r.bytes <= THUMB_TARGET_BYTES) return { quality, value: r.value };
    if (r.bytes > 0 && (best === null || r.bytes < best.bytes)) best = { quality, bytes: r.bytes, value: r.value };
  }
  if (best !== null && best.bytes <= THUMB_HARD_CAP_BYTES) return { quality: best.quality, value: best.value };
  return null;
}

const EXTENSIONS: Readonly<Record<string, string>> = {
  "image/jpeg": "jpg",
  "image/png": "png",
  "image/gif": "gif",
  "image/webp": "webp",
  "video/mp4": "mp4",
};

/** The file name the viewer's Save suggests: the sender's name when it has
 *  one, else `<stem>-<first 8 hex of the blob id>.<extension of the type>`. */
export function saveNameFor(
  ref: { readonly name?: string | undefined; readonly mime?: string | undefined; readonly blobId: Uint8Array },
  fallbackStem: string,
): string {
  if (ref.name !== undefined && ref.name.trim().length > 0) return ref.name;
  const essence = (ref.mime ?? "").split(";")[0]?.trim().toLowerCase() ?? "";
  const ext = EXTENSIONS[essence] ?? "bin";
  let hex = "";
  for (const b of ref.blobId.subarray(0, 4)) hex += b.toString(16).padStart(2, "0");
  return `${fallbackStem}-${hex}.${ext}`;
}
