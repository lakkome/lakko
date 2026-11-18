// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The QR code of a crypto wallet address on the donations page
 * (`docs/ux/donations.md` section 2.4): drawn by the client from the verified
 * address, error correction H, a quiet zone of 4 modules, dark square modules
 * on a white card, and the coin mark the signed statement names in the
 * centre, on a white circle that clears the modules beneath it.
 *
 * This is NOT the branded QR of the design system (`ui/branded-qr.ts`): that
 * one carries the Lakko logo, which a wallet address must never carry.
 *
 * The coin marks are the shared artwork in `apps/shared/donation-marks/`
 * (`btc.svg`, `eth.svg`, `ton.svg`), copied here verbatim as inline SVG so
 * iOS, Android and the web draw the same geometry; nothing is downloaded.
 */
import QRCode from "qrcode";

import type { DonateIcon } from "./client";

/** A coin mark: the inner markup and the view box of its shared SVG file. */
interface CoinMark {
  readonly viewBox: number;
  readonly body: string;
}

/** `apps/shared/donation-marks/*.svg`, inner markup verbatim. */
const COIN_MARKS: Readonly<Record<Exclude<DonateIcon, "none">, CoinMark>> = {
  btc: {
    viewBox: 32,
    body:
      '<circle cx="16" cy="16" r="16" fill="#F7931A"/><path fill="#FFFFFF" d="M23.189 14.02c.314-2.096-1.283-3.223-3.465-3.975l.708-2.84-1.728-.43-.69 2.765c-.454-.114-.92-.22-1.385-.326l.695-2.783L15.596 6l-.708 2.839c-.376-.086-.746-.17-1.104-.26l.002-.009-2.384-.595-.46 1.846s1.283.294 1.256.312c.7.175.826.638.805 1.006l-.806 3.235c.048.012.11.03.18.057l-.183-.045-1.13 4.532c-.086.212-.303.531-.793.41.018.025-1.256-.313-1.256-.313l-.858 1.978 2.25.561c.418.105.828.215 1.231.318l-.715 2.872 1.727.43.708-2.84c.472.127.93.245 1.378.357l-.706 2.828 1.728.43.715-2.866c2.948.558 5.164.333 6.097-2.333.752-2.146-.037-3.385-1.588-4.192 1.13-.26 1.98-1.003 2.207-2.538zm-3.95 5.538c-.533 2.147-4.148.986-5.32.695l.95-3.805c1.172.293 4.929.872 4.37 3.11zm.535-5.569c-.487 1.953-3.495.96-4.47.717l.86-3.45c.975.243 4.118.696 3.61 2.733z"/>',
  },
  eth: {
    viewBox: 32,
    body:
      '<circle cx="16" cy="16" r="16" fill="#627EEA"/><g fill="#FFFFFF"><path fill-opacity=".602" d="M16.498 4v8.87l7.497 3.35z"/><path d="M16.498 4L9 16.22l7.498-3.35z"/><path fill-opacity=".602" d="M16.498 21.968v6.027L24 17.616z"/><path d="M16.498 27.995v-6.028L9 17.616z"/><path fill-opacity=".2" d="M16.498 20.573l7.497-4.353-7.497-3.348z"/><path fill-opacity=".602" d="M9 16.22l7.498 4.353v-7.701z"/></g>',
  },
  ton: {
    viewBox: 56,
    body:
      '<circle cx="28" cy="28" r="28" fill="#0098EA"/><path fill="#FFFFFF" d="M37.5603 15.6277H18.4386C14.9228 15.6277 12.6944 19.4202 14.4632 22.4861L26.2644 42.9409C27.0345 44.2765 28.9644 44.2765 29.7345 42.9409L41.5381 22.4861C43.3045 19.4251 41.0761 15.6277 37.5627 15.6277H37.5603ZM26.2548 36.8068L23.6847 31.8327L17.4833 20.7414C17.0742 20.0315 17.5795 19.1218 18.4362 19.1218H26.2524V36.8092L26.2548 36.8068ZM38.5108 20.739L32.3118 31.8351L29.7417 36.8068V19.1194H37.5579C38.4146 19.1194 38.9199 20.0291 38.5108 20.739Z"/>',
  },
};

/** The coin mark drawn for `icon`, or null for a plain QR. */
export function coinMarkFor(icon: DonateIcon): CoinMark | null {
  return icon === "none" ? null : COIN_MARKS[icon];
}

/** Quiet zone, in modules, on every side. */
export const DONATE_QR_QUIET = 4;
/** The card is white in both themes (a light-on-dark code does not scan on
 *  every camera); the modules are the light theme's text colour
 *  (`--surface-fg` light), fixed for the same reason. */
export const DONATE_QR_CARD = "#ffffff";
export const DONATE_QR_INK = "#1a1d2b";
/** The white circle under the mark, as a fraction of the code's side
 *  (without the quiet zone), rounded down to an odd number of modules. At
 *  level H the code survives far more lost modules than this clears. */
const MARK_CIRCLE_FRACTION = 0.28;
/** The coin mark's diameter, as a fraction of the white circle. */
const MARK_SIDE = 0.84;

export interface DonateQrPlan {
  /** Modules per side, without the quiet zone. */
  readonly size: number;
  /** One byte per module, row-major; non-zero = dark. */
  readonly dark: Uint8Array;
  /** The white circle under the mark, in modules from the code's top-left
   *  corner (quiet zone excluded); null for a plain code. */
  readonly circle: { readonly cx: number; readonly cy: number; readonly r: number } | null;
  readonly mark: CoinMark | null;
}

/** The white circle's diameter for a code of `size` modules: the largest odd
 *  number not above {@link MARK_CIRCLE_FRACTION} of the side, 0 when under 5. */
export function markCircleSpan(size: number): number {
  let k = Math.floor(size * MARK_CIRCLE_FRACTION);
  if (k % 2 === 0) k -= 1;
  return k >= 5 ? k : 0;
}

/** True when module (`x`, `y`) touches the circle: it is then left undrawn. */
function underCircle(x: number, y: number, c: NonNullable<DonateQrPlan["circle"]>): boolean {
  const nx = Math.min(Math.max(c.cx, x), x + 1);
  const ny = Math.min(Math.max(c.cy, y), y + 1);
  return (nx - c.cx) ** 2 + (ny - c.cy) ** 2 < c.r * c.r;
}

/** Encode `address` at level H and place the mark `icon` names. */
export function planDonateQr(address: string, icon: DonateIcon): DonateQrPlan {
  const qr = QRCode.create(address, { errorCorrectionLevel: "H" });
  const size = qr.modules.size;
  const dark = Uint8Array.from(qr.modules.data);
  const mark = coinMarkFor(icon);
  const span = mark === null ? 0 : markCircleSpan(size);
  const circle = span === 0 ? null : { cx: size / 2, cy: size / 2, r: span / 2 };
  if (circle !== null) {
    for (let y = 0; y < size; y++) {
      for (let x = 0; x < size; x++) {
        if (underCircle(x, y, circle)) dark[y * size + x] = 0;
      }
    }
  }
  return { size, dark, circle, mark: circle === null ? null : mark };
}

/** The plan as one SVG document (units: modules, quiet zone included), to be
 *  shown at any pixel size. Everything in it comes from the plan and the
 *  constants above; the address only decides which modules are dark. */
export function donateQrSvg(plan: DonateQrPlan, label: string): string {
  const q = DONATE_QR_QUIET;
  const side = plan.size + 2 * q;
  const radius = (12 / 200) * side;
  let d = "";
  for (let y = 0; y < plan.size; y++) {
    for (let x = 0; x < plan.size; x++) {
      if (plan.dark[y * plan.size + x] !== 0) d += `M${x + q} ${y + q}h1v1h-1z`;
    }
  }
  let centre = "";
  if (plan.circle !== null && plan.mark !== null) {
    const cx = plan.circle.cx + q;
    const cy = plan.circle.cy + q;
    const r = plan.circle.r;
    const m = 2 * r * MARK_SIDE;
    centre =
      `<circle cx="${cx}" cy="${cy}" r="${r}" fill="${DONATE_QR_CARD}"/>` +
      `<svg x="${cx - m / 2}" y="${cy - m / 2}" width="${m}" height="${m}" ` +
      `viewBox="0 0 ${plan.mark.viewBox} ${plan.mark.viewBox}">${plan.mark.body}</svg>`;
  }
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${side} ${side}" role="img" aria-label="${escapeAttr(label)}">` +
    `<rect width="${side}" height="${side}" rx="${radius}" fill="${DONATE_QR_CARD}"/>` +
    `<path d="${d}" fill="${DONATE_QR_INK}" shape-rendering="crispEdges"/>` +
    centre +
    `</svg>`
  );
}

function escapeAttr(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
