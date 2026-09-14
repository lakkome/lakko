// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 §1 — `composeEnvelope` writes the media metadata and both parse
 * paths read it back, through the REAL wasm core (so the bounds under test
 * are `lakko-attachments`', and the `thumb` handle hygiene is exercised).
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init from "../../wasm/lakko_ffi_wasm";
import { composeEnvelope, parseBodyEnvelope, parseEnvelope, type AttachmentRefBytes } from "./attachments";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const fill = (n: number, v: number): Uint8Array => new Uint8Array(n).fill(v);

const VOICE: AttachmentRefBytes = {
  blobId: fill(32, 1),
  key: fill(32, 2),
  nonce: fill(12, 3),
  mime: "audio/mp4",
  size: 4096,
  name: "voice-1.m4a",
  kind: "voice",
  durationMs: 4200,
  waveform: new Uint8Array([10, 200, 30]),
};

describe("media envelope round trip", () => {
  it("carries kind, duration and waveform through both parsers", async () => {
    const bytes = await composeEnvelope("", [VOICE]);
    const body = await parseBodyEnvelope(bytes);
    if (body.kind !== "chat") throw new Error("expected chat");
    for (const attachments of [(await parseEnvelope(bytes)).attachments, body.attachments]) {
      const a = attachments[0]!;
      expect(a.kind).toBe("voice");
      expect(a.durationMs).toBe(4200);
      expect(Array.from(a.waveform ?? [])).toEqual([10, 200, 30]);
      expect(a.thumb).toBeUndefined();
    }
  });

  it("carries a video note's dimensions and poster thumb", async () => {
    const note: AttachmentRefBytes = {
      ...VOICE,
      mime: "video/mp4",
      name: "video-1.mp4",
      kind: "video_note",
      durationMs: 9000,
      waveform: undefined,
      width: 480,
      height: 480,
      thumb: { blobId: fill(32, 7), key: fill(32, 8), nonce: fill(12, 9), mime: "image/jpeg", size: 1234, width: 240, height: 240 },
    };
    const parsed = await parseBodyEnvelope(await composeEnvelope("", [note]));
    if (parsed.kind !== "chat") throw new Error("expected chat");
    const a = parsed.attachments[0]!;
    expect([a.width, a.height]).toEqual([480, 480]);
    expect(a.thumb?.size).toBe(1234);
    expect(a.thumb?.blobId).toEqual(fill(32, 7));
    expect(a.thumb?.mime).toBe("image/jpeg");
  });

  it("an out-of-bound field is dropped and the attachment survives", async () => {
    const parsed = await parseBodyEnvelope(
      await composeEnvelope("", [{ ...VOICE, durationMs: 15 * 60 * 1000 + 1, waveform: fill(129, 5) }]),
    );
    if (parsed.kind !== "chat") throw new Error("expected chat");
    const a = parsed.attachments[0]!;
    expect(a.kind).toBe("voice");
    expect(a.durationMs).toBeUndefined();
    expect(a.waveform).toBeUndefined();
    expect(a.name).toBe("voice-1.m4a");
  });
});
