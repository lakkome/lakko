// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/media-messages.md` section 11 — from the shared mosaic rule
 * (`media_mosaic` in `lakko-ui-rules`, `mediaMosaic` on the wasm surface) to
 * the styles the bubble draws.
 *
 * The rule decides every tile in integer pixels for a bubble 320 px wide; this
 * module never re-decides any of it. It only turns those pixels into
 * percentages of the mosaic, so a bubble narrower than 320 px (a phone-width
 * window) shrinks the whole mosaic in proportion instead of overflowing, and
 * it places a tile from the START edge (`inset-inline-start`), which is how the
 * rule measures `x`: a right-to-left page mirrors the mosaic by itself.
 *
 * Pure: the wasm call is passed in, so vitest runs this with no core.
 */

/** Web bubble width of a photo or video (section 10: web 320 px). */
export const MOSAIC_BUBBLE_WIDTH = 320;

/** The size of one item as the rule takes it: `0` for unknown. */
export interface MosaicInput {
  readonly width?: number | undefined;
  readonly height?: number | undefined;
  readonly thumb?: { readonly width?: number | undefined; readonly height?: number | undefined } | undefined;
}

/** The rule's answer, as the wasm `MediaMosaic` exposes it. */
export interface MosaicAnswer {
  readonly width: number;
  readonly height: number;
  /** Four numbers per tile: x (from the start edge), y, width, height. */
  readonly tiles: ArrayLike<number>;
}

export type ComputeMosaic = (bubbleWidth: number, gap: number, widths: Uint32Array, heights: Uint32Array) => MosaicAnswer;

export interface MosaicTileBox {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

export interface MosaicLayout {
  readonly width: number;
  readonly height: number;
  readonly tiles: readonly MosaicTileBox[];
}

function side(n: number | undefined): number {
  return n !== undefined && Number.isFinite(n) && n > 0 ? Math.min(Math.round(n), 0xffff_ffff) : 0;
}

/**
 * The size the rule is given for one item: its own `width` / `height`, else
 * its thumbnail's (section 11: "a client with no width / height but a
 * thumbnail of known size passes the thumbnail's size"), else unknown.
 */
export function mosaicSizeOf(item: MosaicInput): { readonly width: number; readonly height: number } {
  const w = side(item.width);
  const h = side(item.height);
  if (w > 0 && h > 0) return { width: w, height: h };
  const tw = side(item.thumb?.width);
  const th = side(item.thumb?.height);
  if (tw > 0 && th > 0) return { width: tw, height: th };
  return { width: 0, height: 0 };
}

/** Lay out `items` through the shared rule. An answer with no tiles (nothing
 *  to lay out, or a width that cannot hold them) is returned as is. */
export function mosaicLayout(
  items: readonly MosaicInput[],
  compute: ComputeMosaic,
  gap: number,
  bubbleWidth: number = MOSAIC_BUBBLE_WIDTH,
): MosaicLayout {
  const widths = new Uint32Array(items.length);
  const heights = new Uint32Array(items.length);
  items.forEach((item, i) => {
    const s = mosaicSizeOf(item);
    widths[i] = s.width;
    heights[i] = s.height;
  });
  const answer = compute(bubbleWidth, gap, widths, heights);
  const flat = answer.tiles;
  const tiles: MosaicTileBox[] = [];
  for (let i = 0; i + 3 < flat.length; i += 4) {
    tiles.push({
      x: flat[i] ?? 0,
      y: flat[i + 1] ?? 0,
      width: flat[i + 2] ?? 0,
      height: flat[i + 3] ?? 0,
    });
  }
  return { width: answer.width, height: answer.height, tiles };
}

function pct(part: number, whole: number): string {
  if (whole <= 0) return "0%";
  // Four decimals: a 320 px mosaic places every tile within a hundredth of a
  // pixel, so the rule's one-pixel adjustments survive the conversion.
  return `${Number(((part / whole) * 100).toFixed(4))}%`;
}

/** The mosaic box: as wide as the rule says at most, shrinking with the
 *  bubble, its height kept by the aspect ratio. */
export function mosaicBoxStyle(layout: MosaicLayout): Record<string, string> {
  return {
    width: `${layout.width}px`,
    "max-width": "100%",
    "aspect-ratio": `${layout.width} / ${Math.max(1, layout.height)}`,
  };
}

/** One tile's place inside the box, from the start edge. */
export function mosaicTileStyle(tile: MosaicTileBox, layout: MosaicLayout): Record<string, string> {
  return {
    "inset-inline-start": pct(tile.x, layout.width),
    top: pct(tile.y, layout.height),
    width: pct(tile.width, layout.width),
    height: pct(tile.height, layout.height),
  };
}
