// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 section 6 — the sender's micro thumbnail: a JPEG whose longest
 * side is at most 96 px and which fits 4096 bytes, the quality lowered until
 * it fits, none when it cannot.
 */
import { describe, expect, it } from "vitest";

import {
  INLINE_THUMB_LONGEST_SIDE,
  INLINE_THUMB_MAX_BYTES,
  INLINE_THUMB_QUALITY_STEPS,
  inlineThumbDataUrl,
  inlineThumbDimensions,
  isInlineThumbJpeg,
  pickInlineThumb,
} from "./visual-media";

/** A JPEG-shaped byte string of `len` bytes. */
function jpeg(len: number): Uint8Array {
  const b = new Uint8Array(len);
  b.set([0xff, 0xd8, 0xff], 0);
  return b;
}

describe("inline thumb size bound", () => {
  it("renders at most 96 px on the longest side, never upscaled", () => {
    expect(inlineThumbDimensions(4000, 3000)).toEqual({ width: INLINE_THUMB_LONGEST_SIDE, height: 72 });
    expect(inlineThumbDimensions(1080, 1920)).toEqual({ width: 54, height: 96 });
    expect(inlineThumbDimensions(40, 30)).toEqual({ width: 40, height: 30 });
    expect(inlineThumbDimensions(0, 30)).toBeNull();
  });

  it("accepts a JPEG of up to 4096 bytes and nothing else", () => {
    expect(isInlineThumbJpeg(jpeg(INLINE_THUMB_MAX_BYTES))).toBe(true);
    expect(isInlineThumbJpeg(jpeg(3))).toBe(true);
    expect(isInlineThumbJpeg(jpeg(INLINE_THUMB_MAX_BYTES + 1))).toBe(false);
    expect(isInlineThumbJpeg(new Uint8Array([0x89, 0x50, 0x4e, 0x47]))).toBe(false);
    expect(isInlineThumbJpeg(new Uint8Array([0xff, 0xd8]))).toBe(false);
    expect(isInlineThumbJpeg(undefined)).toBe(false);
  });

  it("takes the first quality whose JPEG fits", async () => {
    const tried: number[] = [];
    const out = await pickInlineThumb(async (q) => {
      tried.push(q);
      return jpeg(q >= 0.6 ? INLINE_THUMB_MAX_BYTES + 500 : 3000);
    });
    expect(out?.length).toBe(3000);
    expect(tried).toEqual(INLINE_THUMB_QUALITY_STEPS.filter((q) => q >= 0.5));
  });

  it("lowers the quality to the last step and gives up when nothing fits", async () => {
    const tried: number[] = [];
    const out = await pickInlineThumb(async (q) => {
      tried.push(q);
      return jpeg(INLINE_THUMB_MAX_BYTES + 1);
    });
    expect(out).toBeNull();
    expect(tried).toEqual([...INLINE_THUMB_QUALITY_STEPS]);
  });

  it("never takes what the encoder gave that is not a JPEG", async () => {
    const out = await pickInlineThumb(async () => new Uint8Array([1, 2, 3]));
    expect(out).toBeNull();
    expect(await pickInlineThumb(async () => null)).toBeNull();
  });

  it("turns only a valid micro thumbnail into a data URL", () => {
    const url = inlineThumbDataUrl(new Uint8Array([0xff, 0xd8, 0xff, 0xe0]));
    expect(url).toBe("data:image/jpeg;base64,/9j/4A==");
    expect(inlineThumbDataUrl(jpeg(INLINE_THUMB_MAX_BYTES + 1))).toBeNull();
    expect(inlineThumbDataUrl(new Uint8Array([0x3c, 0x73, 0x76, 0x67]))).toBeNull();
  });
});
