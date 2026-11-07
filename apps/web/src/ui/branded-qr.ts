// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The branded QR code (design system section 3, "QR code"): the geometry,
 * written once for the web client and drawn by a painter, so the canvas the
 * screens show and the rasterizer the decode test uses run the same rules.
 *
 * The encoded text is never changed by the styling; only the error
 * correction level depends on whether a logo is drawn.
 */
import QRCode from "qrcode";

/** A logo is drawn only for a text of at most this many UTF-8 bytes: it then
 *  fits QR version 15 at level H in byte mode on every encoder. */
export const QR_LOGO_MAX_BYTES = 220;
/** The quiet zone, in modules, on every side of the code. */
export const QR_QUIET_MODULES = 4;
/** The smallest card side for a code with a logo, and for one without. */
export const QR_MIN_SIDE = 240;
export const QR_MIN_SIDE_NO_LOGO = 288;
/** The card's corner radius. */
export const QR_CARD_RADIUS = 12;

/** Fixed colours: the card is always light, whatever the theme. */
export const QR_CARD_COLOR = "#ffffff";
export const QR_DATA_INK = "#312e81";
export const QR_FINDER_INK = "#4338ca";

/** A data module's dot, as a fraction of a module. A full module (the dots
 *  of neighbouring modules touch): measured 2026-11-06 with jsQR over 120
 *  largest pairing payloads, 0.9 left gaps that failed 12, 13 and 32 decodes
 *  at 288, 576 and 864 px; 1.0 with {@link FINDER_RADIUS} fails none. */
const DOT_DIAMETER = 1.0;
/** The finder's corner radii, in modules (outer ring; the inner two take
 *  half). Was 2: the rounder ring cost decodes at the smallest side. */
const FINDER_RADIUS = 1;
/** The badge's corner radius and the mark's side, as fractions of the badge. */
const BADGE_RADIUS = 0.22;
const MARK_SIDE = 0.76;

export interface BrandedQrPlan {
  /** Modules per side, without the quiet zone. */
  readonly size: number;
  /** One byte per module, row-major; non-zero = dark. */
  readonly dark: Uint8Array;
  readonly level: "H" | "M";
  /** The badge, in modules: first row / column covered and the span. */
  readonly logo: { readonly from: number; readonly span: number } | null;
}

/** The badge span for a code of `size` modules: the largest odd number not
 *  above 20% of the side, or 0 when that is under 5 (too small for a mark). */
export function logoSpan(size: number): number {
  let k = Math.floor(size * 0.2);
  if (k % 2 === 0) k -= 1;
  return k >= 5 ? k : 0;
}

function utf8Length(text: string): number {
  return new TextEncoder().encode(text).length;
}

/** Encode `text` and decide the level and the logo. */
export function planBrandedQr(text: string): BrandedQrPlan {
  const wantsLogo = utf8Length(text) <= QR_LOGO_MAX_BYTES;
  const level = wantsLogo ? "H" : "M";
  const qr = QRCode.create(text, { errorCorrectionLevel: level });
  const size = qr.modules.size;
  const dark = Uint8Array.from(qr.modules.data);
  const span = wantsLogo ? logoSpan(size) : 0;
  const logo = span === 0 ? null : { from: (size - span) / 2, span };
  return { size, dark, level, logo };
}

/** The card side a caller's `requested` side becomes. */
export function brandedQrSide(plan: BrandedQrPlan, requested: number): number {
  return Math.max(requested, plan.logo === null ? QR_MIN_SIDE_NO_LOGO : QR_MIN_SIDE);
}

/** What a renderer must draw; coordinates in the card's own units. */
export interface QrPainter {
  roundRect(x: number, y: number, w: number, h: number, r: number, color: string): void;
  circle(cx: number, cy: number, r: number, color: string): void;
  /** The brand mark, a square of side `side` at `x`, `y`. */
  logo(x: number, y: number, side: number): void;
}

function inFinder(x: number, y: number, n: number): boolean {
  return (x < 7 && y < 7) || (x >= n - 7 && y < 7) || (x < 7 && y >= n - 7);
}

/** Draw `plan` on a card of side `side`. */
export function paintBrandedQr(plan: BrandedQrPlan, side: number, p: QrPainter): void {
  const n = plan.size;
  const m = side / (n + 2 * QR_QUIET_MODULES);
  const at = (module: number): number => (module + QR_QUIET_MODULES) * m;

  p.roundRect(0, 0, side, side, QR_CARD_RADIUS, QR_CARD_COLOR);

  for (const [fx, fy] of [
    [0, 0],
    [n - 7, 0],
    [0, n - 7],
  ] as const) {
    const ox = at(fx);
    const oy = at(fy);
    p.roundRect(ox, oy, 7 * m, 7 * m, FINDER_RADIUS * m, QR_FINDER_INK);
    p.roundRect(ox + m, oy + m, 5 * m, 5 * m, (FINDER_RADIUS / 2) * m, QR_CARD_COLOR);
    p.roundRect(ox + 2 * m, oy + 2 * m, 3 * m, 3 * m, (FINDER_RADIUS / 2) * m, QR_FINDER_INK);
  }

  const logo = plan.logo;
  const underLogo = (x: number, y: number): boolean =>
    logo !== null &&
    x >= logo.from &&
    x < logo.from + logo.span &&
    y >= logo.from &&
    y < logo.from + logo.span;

  const r = (DOT_DIAMETER * m) / 2;
  for (let y = 0; y < n; y++) {
    for (let x = 0; x < n; x++) {
      if (plan.dark[y * n + x] === 0 || inFinder(x, y, n) || underLogo(x, y)) continue;
      p.circle(at(x) + m / 2, at(y) + m / 2, r, QR_DATA_INK);
    }
  }

  if (logo !== null) {
    const bx = at(logo.from);
    const badge = logo.span * m;
    p.roundRect(bx, bx, badge, badge, BADGE_RADIUS * badge, QR_CARD_COLOR);
    const mark = MARK_SIDE * badge;
    p.logo(bx + (badge - mark) / 2, bx + (badge - mark) / 2, mark);
  }
}
