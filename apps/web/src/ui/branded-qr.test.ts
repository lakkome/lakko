// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The branded QR code scans (design system section 3, "QR code").
 *
 * The geometry of `paintBrandedQr` is drawn by a small software rasterizer
 * (there is no canvas in vitest) with the real brand tile (`/icon-192.png`,
 * decoded here) in the badge, then read back with jsQR, the decoder the web
 * client already ships for scanning. Payloads: the largest device pairing
 * payload (a real key package and a device card of maximal length, through
 * the real wasm encoder), an invite link, the authenticator setup code, and a
 * text of exactly the logo threshold.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { deflateSync, inflateSync } from "node:zlib";
import jsQR from "jsqr";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64UrlEncodeNoPad } from "../auth/account-id";
import {
  QR_LOGO_MAX_BYTES,
  QR_MIN_SIDE,
  QR_MIN_SIDE_NO_LOGO,
  brandedQrSide,
  logoSpan,
  paintBrandedQr,
  planBrandedQr,
  type QrPainter,
} from "./branded-qr";

interface Rgba {
  readonly width: number;
  readonly height: number;
  readonly data: Uint8ClampedArray;
}

/** A minimal PNG reader: 8-bit RGBA, non-interlaced (the brand tile). */
function readPng(bytes: Uint8Array): Rgba {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let off = 8;
  let width = 0;
  let height = 0;
  const idat: Uint8Array[] = [];
  while (off < bytes.length) {
    const len = view.getUint32(off);
    const type = String.fromCharCode(...bytes.subarray(off + 4, off + 8));
    const body = bytes.subarray(off + 8, off + 8 + len);
    if (type === "IHDR") {
      width = view.getUint32(off + 8);
      height = view.getUint32(off + 12);
      if (body[8] !== 8 || body[9] !== 6 || body[12] !== 0) throw new Error("unsupported png");
    } else if (type === "IDAT") {
      idat.push(body);
    }
    off += 12 + len;
  }
  const raw = inflateSync(Buffer.concat(idat));
  const stride = width * 4;
  const out = new Uint8ClampedArray(width * height * 4);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)]!;
    const row = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let x = 0; x < stride; x++) {
      const a = x >= 4 ? out[y * stride + x - 4]! : 0;
      const b = y > 0 ? out[(y - 1) * stride + x]! : 0;
      const c = x >= 4 && y > 0 ? out[(y - 1) * stride + x - 4]! : 0;
      let v = row[x]!;
      if (filter === 1) v += a;
      else if (filter === 2) v += b;
      else if (filter === 3) v += (a + b) >> 1;
      else if (filter === 4) {
        const p = a + b - c;
        const pa = Math.abs(p - a);
        const pb = Math.abs(p - b);
        const pc = Math.abs(p - c);
        v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      out[y * stride + x] = v & 0xff;
    }
  }
  return { width, height, data: out };
}

function hex(color: string): [number, number, number] {
  const n = Number.parseInt(color.slice(1), 16);
  return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
}

/** Rasterize at `side` pixels with 4 x 4 coverage samples per pixel. */
function rasterize(text: string, side: number, mark: Rgba): Rgba {
  const plan = planBrandedQr(text);
  const data = new Uint8ClampedArray(side * side * 4);
  for (let i = 0; i < data.length; i += 4) {
    // The page behind the card: a dark theme, the worst case.
    data[i] = 15;
    data[i + 1] = 18;
    data[i + 2] = 32;
    data[i + 3] = 255;
  }
  const S = 4;
  const blend = (px: number, py: number, rgb: [number, number, number], alpha: number): void => {
    if (px < 0 || py < 0 || px >= side || py >= side || alpha <= 0) return;
    const i = (py * side + px) * 4;
    for (let k = 0; k < 3; k++) data[i + k] = data[i + k]! * (1 - alpha) + rgb[k]! * alpha;
  };
  const fill = (
    x0: number,
    y0: number,
    x1: number,
    y1: number,
    inside: (x: number, y: number) => boolean,
    rgb: [number, number, number],
  ): void => {
    for (let py = Math.floor(y0); py < Math.ceil(y1); py++) {
      for (let px = Math.floor(x0); px < Math.ceil(x1); px++) {
        let hits = 0;
        for (let sy = 0; sy < S; sy++) {
          for (let sx = 0; sx < S; sx++) {
            if (inside(px + (sx + 0.5) / S, py + (sy + 0.5) / S)) hits++;
          }
        }
        blend(px, py, rgb, hits / (S * S));
      }
    }
  };
  const painter: QrPainter = {
    roundRect(x, y, w, h, r, color) {
      const rr = Math.min(r, w / 2, h / 2);
      fill(x, y, x + w, y + h, (px, py) => {
        if (px < x || py < y || px > x + w || py > y + h) return false;
        const cx = Math.min(Math.max(px, x + rr), x + w - rr);
        const cy = Math.min(Math.max(py, y + rr), y + h - rr);
        return (px - cx) ** 2 + (py - cy) ** 2 <= rr * rr;
      }, hex(color));
    },
    circle(cx, cy, r, color) {
      fill(cx - r, cy - r, cx + r, cy + r, (px, py) => (px - cx) ** 2 + (py - cy) ** 2 <= r * r, hex(color));
    },
    logo(x, y, s) {
      for (let py = Math.floor(y); py < Math.ceil(y + s); py++) {
        for (let px = Math.floor(x); px < Math.ceil(x + s); px++) {
          const u = Math.min(mark.width - 1, Math.max(0, Math.floor(((px + 0.5 - x) / s) * mark.width)));
          const v = Math.min(mark.height - 1, Math.max(0, Math.floor(((py + 0.5 - y) / s) * mark.height)));
          const i = (v * mark.width + u) * 4;
          blend(px, py, [mark.data[i]!, mark.data[i + 1]!, mark.data[i + 2]!], mark.data[i + 3]! / 255);
        }
      }
    },
  };
  paintBrandedQr(plan, side, painter);
  return { width: side, height: side, data };
}

/** With `LAKKO_QR_DUMP_DIR` set, write a rendering as a PNG to look at. */
function dump(name: string, img: Rgba): void {
  const dir = process.env["LAKKO_QR_DUMP_DIR"];
  if (dir === undefined || dir === "") return;
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc = (buf: Uint8Array): number => {
    let c = 0xffffffff;
    for (const b of buf) c = crcTable[(c ^ b) & 0xff]! ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (type: string, body: Uint8Array): Buffer => {
    const head = Buffer.alloc(8);
    head.writeUInt32BE(body.length, 0);
    head.write(type, 4, "ascii");
    const tail = Buffer.alloc(4);
    tail.writeUInt32BE(crc(Buffer.concat([head.subarray(4), body])), 0);
    return Buffer.concat([head, body, tail]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(img.width, 0);
  ihdr.writeUInt32BE(img.height, 4);
  ihdr.set([8, 6, 0, 0, 0], 8);
  const raw = Buffer.alloc((img.width * 4 + 1) * img.height);
  for (let y = 0; y < img.height; y++) {
    raw.set(img.data.subarray(y * img.width * 4, (y + 1) * img.width * 4), y * (img.width * 4 + 1) + 1);
  }
  writeFileSync(
    `${dir}/${name}.png`,
    Buffer.concat([
      Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
      chunk("IHDR", ihdr),
      chunk("IDAT", deflateSync(raw)),
      chunk("IEND", new Uint8Array(0)),
    ]),
  );
}

function decode(img: Rgba): string | null {
  return jsQR(img.data, img.width, img.height, { inversionAttempts: "dontInvert" })?.data ?? null;
}

/** `n` pseudo-random bytes from `seed` (mulberry32): the same every run. */
function fixedBytes(n: number, seed: number): Uint8Array {
  let a = seed >>> 0;
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i++) {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    out[i] = ((t ^ (t >>> 14)) >>> 0) & 0xff;
  }
  return out;
}

let mark: Rgba;
let pairingText: string;

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
  mark = readPng(readFileSync(fileURLToPath(new URL("../../public/icon-192.png", import.meta.url))));
  // The key package carries fresh random keys, so a real one would make the
  // QR's contents (and whether jsQR reads that particular code) differ from
  // run to run. The test needs the largest payload's SIZE, not its keys:
  // fixed pseudo-random bytes of a real key package's length.
  const identity = wasm.Identity.fromSeed(new Uint8Array(32).fill(9));
  const keyPackage = fixedBytes(identity.createKeyPackage().length, 0x5eed);
  identity.free();
  const long = "W".repeat(64);
  const card = new wasm.DeviceCard(1n, 1n, "desktop", long, "x".repeat(32), long, long, "NL");
  const envelope = wasm.composeDeviceCardEnvelope(card);
  const payload = wasm.encodePairingQr(
    new Uint8Array(32).fill(1),
    keyPackage,
    new Uint8Array(32).fill(2),
    envelope,
  );
  pairingText = base64UrlEncodeNoPad(payload);
});

const INVITE = "https://lakko.me/i/AbCdEfGh#0123456789abcdef";
const OTPAUTH =
  "otpauth://totp/Lakko:valentin_goncharov?secret=JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP&issuer=Lakko&algorithm=SHA1&digits=6&period=30";

describe("the branded QR code", () => {
  it("draws the largest pairing payload without a logo, at level M", () => {
    expect(pairingText.length).toBeGreaterThan(800);
    const plan = planBrandedQr(pairingText);
    expect(plan.logo).toBeNull();
    expect(plan.level).toBe("M");
    expect(brandedQrSide(plan, 0)).toBe(QR_MIN_SIDE_NO_LOGO);
  });

  it("draws a logo at level H for an invite link and the setup code", () => {
    for (const text of [INVITE, OTPAUTH]) {
      const plan = planBrandedQr(text);
      expect(plan.level).toBe("H");
      expect(plan.logo).not.toBeNull();
      expect(plan.logo!.span).toBeLessThanOrEqual(plan.size * 0.2);
      expect(brandedQrSide(plan, 0)).toBe(QR_MIN_SIDE);
    }
    expect(planBrandedQr("h".repeat(QR_LOGO_MAX_BYTES)).logo).not.toBeNull();
    expect(planBrandedQr("h".repeat(QR_LOGO_MAX_BYTES + 1)).logo).toBeNull();
  });

  it("gives the badge an odd span of at least 5 and at most a fifth of the side", () => {
    expect(logoSpan(21)).toBe(0);
    expect(logoSpan(33)).toBe(5);
    expect(logoSpan(37)).toBe(7);
    expect(logoSpan(77)).toBe(15);
  });

  const cases: ReadonlyArray<readonly [string, () => string, number]> = [
    ["the largest pairing payload, minimum side", () => pairingText, QR_MIN_SIDE_NO_LOGO],
    ["the largest pairing payload, minimum side on a 2x screen", () => pairingText, QR_MIN_SIDE_NO_LOGO * 2],
    ["an invite link, minimum side", () => INVITE, QR_MIN_SIDE],
    ["the authenticator setup code, minimum side", () => OTPAUTH, QR_MIN_SIDE],
    ["a text at the logo threshold, minimum side", () => "h".repeat(QR_LOGO_MAX_BYTES), QR_MIN_SIDE],
    ["a text at the logo threshold, 2x screen", () => "h".repeat(QR_LOGO_MAX_BYTES), QR_MIN_SIDE * 2],
  ];
  for (const [name, text, side] of cases) {
    it(`decodes back: ${name}`, () => {
      const img = rasterize(text(), side, mark);
      dump(`web-${name.replace(/[^a-z0-9]+/gi, "-")}`, img);
      expect(decode(img)).toBe(text());
    });
  }
});
