// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The wallet-address QR of the donations page (`docs/ux/donations.md`
 * section 2.4): the coin mark the signed statement names, the shared artwork,
 * error correction H, and that the code still scans with the mark on it.
 *
 * The scan check rasterizes the plan (dark square modules on white, quiet
 * zone 4) and paints the whole white circle as an opaque coloured disc, the
 * worst case for any artwork inside it, then reads it back with jsQR.
 */
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import jsQR from "jsqr";

import {
  DONATE_QR_QUIET,
  coinMarkFor,
  donateQrSvg,
  markCircleSpan,
  planDonateQr,
  type DonateQrPlan,
} from "./donate-qr";
import type { DonateIcon } from "./client";

const ADDRESSES: readonly string[] = [
  // bech32 P2WSH, the longest Bitcoin form (62 characters).
  "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3",
  "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq",
  "0x52908400098527886E0F7030069857D2E4169EE7",
  "EQDtFpEwcFAEcRe5mLVh2N6C0x-_hJEM7W61_JLnSF74p4q2",
  "TQn9Y2khEsLJW1ChVWFMSMeRDow5KcbLSE",
];

function plan(address: string, icon: DonateIcon): DonateQrPlan {
  return planDonateQr(address, icon);
}

/** Draw `p` at `px` pixels per module: white page, dark modules, and the
 *  white circle (if any) filled with an opaque mid-grey disc. */
function rasterize(p: DonateQrPlan, px: number): { data: Uint8ClampedArray; side: number } {
  const n = p.size + 2 * DONATE_QR_QUIET;
  const side = n * px;
  const data = new Uint8ClampedArray(side * side * 4).fill(255);
  const set = (x: number, y: number, v: number): void => {
    const i = (y * side + x) * 4;
    data[i] = v;
    data[i + 1] = v;
    data[i + 2] = v;
  };
  for (let y = 0; y < p.size; y++) {
    for (let x = 0; x < p.size; x++) {
      if (p.dark[y * p.size + x] === 0) continue;
      for (let dy = 0; dy < px; dy++) {
        for (let dx = 0; dx < px; dx++) set((x + DONATE_QR_QUIET) * px + dx, (y + DONATE_QR_QUIET) * px + dy, 26);
      }
    }
  }
  const c = p.circle;
  if (c !== null) {
    const cx = (c.cx + DONATE_QR_QUIET) * px;
    const cy = (c.cy + DONATE_QR_QUIET) * px;
    const r = c.r * px;
    for (let y = 0; y < side; y++) {
      for (let x = 0; x < side; x++) {
        if ((x + 0.5 - cx) ** 2 + (y + 0.5 - cy) ** 2 <= r * r) set(x, y, 120);
      }
    }
  }
  return { data, side };
}

describe("coin marks", () => {
  it("draws the mark the statement names, and none for none", () => {
    expect(coinMarkFor("none")).toBeNull();
    for (const icon of ["btc", "eth", "ton"] as const) expect(coinMarkFor(icon)).not.toBeNull();
    expect(coinMarkFor("btc")?.body).toContain("#F7931A");
    expect(coinMarkFor("eth")?.body).toContain("#627EEA");
    expect(coinMarkFor("ton")?.body).toContain("#0098EA");
  });

  it("is the shared artwork of apps/shared/donation-marks, verbatim", () => {
    for (const icon of ["btc", "eth", "ton"] as const) {
      const path = fileURLToPath(new URL(`../../../shared/donation-marks/${icon}.svg`, import.meta.url));
      const file = readFileSync(path, "utf8");
      const m = /<svg[^>]*viewBox="0 0 (\d+) \1"[^>]*>([\s\S]*)<\/svg>/.exec(file);
      expect(m).not.toBeNull();
      const mark = coinMarkFor(icon);
      expect(mark?.viewBox).toBe(Number(m?.[1]));
      expect(mark?.body).toBe(m?.[2]?.trim());
    }
  });
});

describe("planDonateQr", () => {
  it("encodes at level H and clears a centre circle only when there is a mark", () => {
    for (const address of ADDRESSES) {
      const plain = plan(address, "none");
      expect(plain.circle).toBeNull();
      expect(plain.mark).toBeNull();
      const marked = plan(address, "btc");
      expect(marked.size).toBe(plain.size);
      expect(marked.circle?.r).toBe(markCircleSpan(marked.size) / 2);
      let cleared = 0;
      for (let i = 0; i < plain.dark.length; i++) {
        if (plain.dark[i] !== 0 && marked.dark[i] === 0) cleared++;
        // The mark only removes modules; it never adds one.
        expect(marked.dark[i] !== 0 && plain.dark[i] === 0).toBe(false);
      }
      expect(cleared).toBeGreaterThan(0);
      // Far below what level H restores (about 30% of the codewords).
      expect(cleared / (plain.size * plain.size)).toBeLessThan(0.08);
    }
  });

  it("scans with each mark and without one", () => {
    for (const address of ADDRESSES) {
      for (const icon of ["none", "btc", "eth", "ton"] as const) {
        for (const px of [4, 6]) {
          const img = rasterize(plan(address, icon), px);
          const got = jsQR(img.data, img.side, img.side, { inversionAttempts: "dontInvert" })?.data ?? null;
          expect(got, `${icon} ${address} at ${px} px`).toBe(address);
        }
      }
    }
  });
});

describe("donateQrSvg", () => {
  it("is a white card with the coin artwork and neither the address text nor the Lakko logo", () => {
    const address = ADDRESSES[0] ?? "";
    const svg = donateQrSvg(plan(address, "ton"), "QR code");
    expect(svg.startsWith("<svg")).toBe(true);
    expect(svg).toContain('fill="#ffffff"');
    expect(svg).toContain("#0098EA");
    expect(svg).not.toContain(address);
    expect(svg).not.toContain("icon-192");
    expect(svg).not.toContain("<image");
    expect(svg).toContain('aria-label="QR code"');
  });

  it("escapes the label and draws no centre for a plain code", () => {
    const svg = donateQrSvg(plan("TQn9Y2khEsLJW1ChVWFMSMeRDow5KcbLSE", "none"), 'a "b" <c>');
    expect(svg).toContain('aria-label="a &quot;b&quot; &lt;c&gt;"');
    expect(svg).not.toContain("<circle");
  });
});
