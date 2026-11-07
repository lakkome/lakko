// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `BrandedQr`: every QR code the web client draws (design system section 3,
 * "QR code"). The geometry lives in `branded-qr.ts`; this draws it on a
 * canvas at the screen's pixel density.
 */
import { createEffect, type JSX } from "solid-js";

import { t } from "../l10n";
import { brandedQrSide, paintBrandedQr, planBrandedQr, type QrPainter } from "./branded-qr";

/** The brand tile, loaded once; `null` until it has loaded (or if it never
 *  does: the white badge stays, the code still scans at level H). */
let markImage: HTMLImageElement | null = null;
let markLoading: Promise<HTMLImageElement | null> | null = null;

function loadMark(): Promise<HTMLImageElement | null> {
  markLoading ??= new Promise((resolve) => {
    const img = new Image();
    img.onload = () => {
      markImage = img;
      resolve(img);
    };
    img.onerror = () => resolve(null);
    img.src = "/icon-192.png";
  });
  return markLoading;
}

function canvasPainter(ctx: CanvasRenderingContext2D, mark: HTMLImageElement | null): QrPainter {
  return {
    roundRect(x, y, w, h, r, color) {
      const rr = Math.min(r, w / 2, h / 2);
      ctx.fillStyle = color;
      ctx.beginPath();
      ctx.moveTo(x + rr, y);
      ctx.arcTo(x + w, y, x + w, y + h, rr);
      ctx.arcTo(x + w, y + h, x, y + h, rr);
      ctx.arcTo(x, y + h, x, y, rr);
      ctx.arcTo(x, y, x + w, y, rr);
      ctx.closePath();
      ctx.fill();
    },
    circle(cx, cy, r, color) {
      ctx.fillStyle = color;
      ctx.beginPath();
      ctx.arc(cx, cy, r, 0, Math.PI * 2);
      ctx.fill();
    },
    logo(x, y, side) {
      if (mark !== null) ctx.drawImage(mark, x, y, side, side);
    },
  };
}

/** Draw `text` as a branded QR code on `canvas`; `size` is the requested card
 *  side in CSS pixels (raised to the design-system minimum). */
export function drawBrandedQr(canvas: HTMLCanvasElement, text: string, size: number): void {
  const plan = planBrandedQr(text);
  const side = brandedQrSide(plan, size);
  const dpr = typeof window === "undefined" ? 1 : Math.max(1, window.devicePixelRatio || 1);
  canvas.width = Math.round(side * dpr);
  canvas.height = Math.round(side * dpr);
  canvas.style.width = `${side}px`;
  const draw = (mark: HTMLImageElement | null): void => {
    const ctx = canvas.getContext("2d");
    if (ctx === null) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, side, side);
    paintBrandedQr(plan, side, canvasPainter(ctx, mark));
  };
  draw(markImage);
  if (plan.logo !== null && markImage === null) {
    void loadMark().then((img) => {
      if (img !== null && canvas.isConnected) draw(img);
    });
  }
}

export function BrandedQr(props: {
  readonly text: string;
  /** Card side in CSS pixels; raised to the design-system minimum. */
  readonly size?: number;
  readonly label?: string;
}): JSX.Element {
  let canvas: HTMLCanvasElement | undefined;
  createEffect(() => {
    const text = props.text;
    const size = props.size ?? 0;
    if (canvas === undefined || text === "") return;
    try {
      drawBrandedQr(canvas, text, size);
    } catch (err: unknown) {
      console.warn("[qr] render failed", err instanceof Error ? err.name : "");
    }
  });
  return (
    <canvas
      ref={canvas}
      class="lk-qr"
      role="img"
      aria-label={props.label ?? t("common.qr_code")}
    />
  );
}
