// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/media-messages.md` section 11 — the shared mosaic rule's answer as
 * the web draws it: sizes handed to the rule, tiles read back, and the
 * percentages from the start edge that let the mosaic shrink with a narrow
 * bubble.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { mediaMosaic, mediaMosaicGap } from "../../wasm/lakko_ffi_wasm";
import {
  mosaicBoxStyle,
  mosaicLayout,
  mosaicSizeOf,
  mosaicTileStyle,
  type ComputeMosaic,
} from "./mosaic-layout";

const real: ComputeMosaic = (bubble, gap, widths, heights) => {
  const m = mediaMosaic(bubble, gap, widths, heights);
  try {
    return { width: m.width, height: m.height, tiles: m.tiles };
  } finally {
    m.free();
  }
};

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

describe("mosaicSizeOf", () => {
  it("passes the item's size, else its thumbnail's, else unknown", () => {
    expect(mosaicSizeOf({ width: 1200, height: 800 })).toEqual({ width: 1200, height: 800 });
    expect(mosaicSizeOf({ thumb: { width: 320, height: 240 } })).toEqual({ width: 320, height: 240 });
    expect(mosaicSizeOf({ width: 1200 })).toEqual({ width: 0, height: 0 });
    expect(mosaicSizeOf({})).toEqual({ width: 0, height: 0 });
  });
});

describe("mosaicLayout", () => {
  it("reads the rule's flat tiles back in order", () => {
    const fake: ComputeMosaic = () => ({ width: 320, height: 89, tiles: [0, 0, 159, 89, 161, 0, 159, 89] });
    const l = mosaicLayout([{}, {}], fake, 2);
    expect(l).toEqual({
      width: 320,
      height: 89,
      tiles: [
        { x: 0, y: 0, width: 159, height: 89 },
        { x: 161, y: 0, width: 159, height: 89 },
      ],
    });
  });

  it("hands the rule a 320 px bubble, the gap, and one size per item", () => {
    let seen: { bubble: number; gap: number; widths: number[]; heights: number[] } | null = null;
    const spy: ComputeMosaic = (bubble, gap, widths, heights) => {
      seen = { bubble, gap, widths: Array.from(widths), heights: Array.from(heights) };
      return { width: 0, height: 0, tiles: [] };
    };
    mosaicLayout([{ width: 10, height: 20 }, { thumb: { width: 4, height: 3 } }, {}], spy, 2);
    expect(seen).toEqual({ bubble: 320, gap: 2, widths: [10, 4, 0], heights: [20, 3, 0] });
  });

  it("lays out three photos as one large and two stacked (the real rule)", () => {
    const l = mosaicLayout(
      [
        { width: 1000, height: 1000 },
        { width: 1000, height: 1000 },
        { width: 1000, height: 1000 },
      ],
      real,
      mediaMosaicGap(),
    );
    expect(l.tiles).toHaveLength(3);
    expect(l.width).toBe(320);
    const [big, top, bottom] = l.tiles;
    expect(big!.height).toBe(l.height);
    expect(top!.x).toBe(bottom!.x);
    expect(top!.height + bottom!.height + mediaMosaicGap()).toBe(l.height);
  });

  it("fills every row of five exactly (the real rule)", () => {
    const l = mosaicLayout(Array.from({ length: 5 }, () => ({ width: 1600, height: 900 })), real, mediaMosaicGap());
    expect(l.tiles).toHaveLength(5);
    const rows = new Map<number, number>();
    for (const t of l.tiles) rows.set(t.y, Math.max(rows.get(t.y) ?? 0, t.x + t.width));
    for (const right of rows.values()) expect(right).toBe(320);
  });
});

describe("styles", () => {
  const layout = { width: 320, height: 200, tiles: [{ x: 161, y: 0, width: 159, height: 99 }] };

  it("sizes the box at the rule's width, shrinking with the bubble, keeping its aspect", () => {
    expect(mosaicBoxStyle(layout)).toEqual({ width: "320px", "max-width": "100%", "aspect-ratio": "320 / 200" });
  });

  it("places a tile in percentages from the start edge", () => {
    expect(mosaicTileStyle(layout.tiles[0]!, layout)).toEqual({
      "inset-inline-start": "50.3125%",
      top: "0%",
      width: "49.6875%",
      height: "49.5%",
    });
  });
});
