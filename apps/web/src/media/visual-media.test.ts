// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  BUBBLE_MAX_HEIGHT,
  BUBBLE_MAX_WIDTH,
  BUBBLE_MIN_SIDE,
  THUMB_HARD_CAP_BYTES,
  THUMB_QUALITY_STEPS,
  THUMB_TARGET_BYTES,
  bubbleBox,
  fitWithin,
  pickThumbQuality,
  saveNameFor,
  thumbDimensions,
} from "./visual-media";

describe("fitWithin", () => {
  it("leaves a size inside the bound unchanged", () => {
    expect(fitWithin(1200, 900)).toEqual({ width: 1200, height: 900 });
  });

  it("scales a large landscape and portrait image proportionally to 4096", () => {
    expect(fitWithin(8192, 4096)).toEqual({ width: 4096, height: 2048 });
    expect(fitWithin(3000, 12000)).toEqual({ width: 1024, height: 4096 });
  });

  it("never produces a zero side for an extreme aspect", () => {
    expect(fitWithin(100_000, 10)).toEqual({ width: 4096, height: 1 });
  });

  it("refuses a degenerate input", () => {
    expect(fitWithin(0, 100)).toBeNull();
    expect(fitWithin(100, Number.NaN)).toBeNull();
    expect(fitWithin(-5, 100)).toBeNull();
  });
});

describe("thumbDimensions", () => {
  it("puts the longest side at 320", () => {
    expect(thumbDimensions(4000, 3000)).toEqual({ width: 320, height: 240 });
    expect(thumbDimensions(1080, 1920)).toEqual({ width: 180, height: 320 });
  });

  it("does not upscale a small source", () => {
    expect(thumbDimensions(200, 100)).toEqual({ width: 200, height: 100 });
  });
});

describe("pickThumbQuality", () => {
  it("takes the first quality under the target", async () => {
    const sizes = [90_000, 70_000, 50_000, 30_000];
    let i = 0;
    const r = await pickThumbQuality(async (q) => ({ bytes: sizes[i++] ?? 1, value: q }));
    expect(r?.quality).toBe(THUMB_QUALITY_STEPS[2]);
    expect(i).toBe(3);
  });

  it("falls back to the smallest result under the hard cap", async () => {
    const r = await pickThumbQuality(async (q) => ({ bytes: THUMB_TARGET_BYTES + Math.round(q * 1000), value: q }));
    expect(r?.quality).toBe(THUMB_QUALITY_STEPS[THUMB_QUALITY_STEPS.length - 1]);
  });

  it("gives up when nothing fits the hard cap or encoding yields nothing", async () => {
    expect(await pickThumbQuality(async () => ({ bytes: THUMB_HARD_CAP_BYTES + 1, value: 0 }))).toBeNull();
    expect(await pickThumbQuality(async () => ({ bytes: 0, value: 0 }))).toBeNull();
  });
});

describe("bubbleBox", () => {
  it("scales a large photo down to the width bound", () => {
    expect(bubbleBox({ width: 4000, height: 3000 })).toEqual({ width: 320, height: 240 });
  });

  it("scales a tall photo down to the height bound", () => {
    expect(bubbleBox({ width: 1080, height: 1920 })).toEqual({ width: 203, height: 360 });
  });

  it("does not upscale a small photo that has dimensions", () => {
    expect(bubbleBox({ width: 200, height: 150 })).toEqual({ width: 200, height: 150 });
  });

  it("uses the thumb's aspect when the media has no dimensions", () => {
    expect(bubbleBox({}, { width: 180, height: 320 })).toEqual({ width: 203, height: 360 });
    expect(bubbleBox({ width: 100 }, { width: 320, height: 160 })).toEqual({ width: 320, height: 160 });
  });

  it("falls back to 4:3 without dimensions or a thumb", () => {
    expect(bubbleBox({})).toEqual({ width: 320, height: 240 });
  });

  it("clamps an extreme aspect to the minimum side", () => {
    const box = bubbleBox({ width: 4096, height: 10 });
    expect(box.width).toBe(BUBBLE_MAX_WIDTH);
    expect(box.height).toBe(BUBBLE_MIN_SIDE);
    expect(bubbleBox({ width: 10, height: 4096 }).height).toBe(BUBBLE_MAX_HEIGHT);
  });
});

describe("saveNameFor", () => {
  const blobId = new Uint8Array([0xab, 0x01, 0x02, 0x03, 0xff]);
  it("keeps the sender's name", () => {
    expect(saveNameFor({ name: "cat.png", mime: "image/png", blobId }, "photo")).toBe("cat.png");
  });
  it("builds a name from the type and blob id otherwise", () => {
    expect(saveNameFor({ mime: "image/JPEG", blobId }, "photo")).toBe("photo-ab010203.jpg");
    expect(saveNameFor({ name: " ", mime: "video/mp4; codecs=avc1", blobId }, "video")).toBe("video-ab010203.mp4");
    expect(saveNameFor({ mime: undefined, blobId }, "photo")).toBe("photo-ab010203.bin");
  });
});
