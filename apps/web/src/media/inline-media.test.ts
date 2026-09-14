// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 §1 — inline playback is gated on the core's MIME allowlist, never
 * on the sender-chosen `kind`.
 *
 * Driven through the REAL wasm `inlineMediaType`, so the allowlist under test
 * is the one in `lakko-attachments`, not a JS re-statement of it.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  WASM_INLINE_AUDIO,
  WASM_INLINE_IMAGE,
  WASM_INLINE_VIDEO,
  blobTypeFor,
  inlineTypeOf,
  renderDecision,
} from "./inline-media";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const allowlist = (mime: string): number | undefined => wasm.inlineMediaType(mime);

describe("inline gating", () => {
  it("pins the wasm enum values this module does not import", () => {
    expect(wasm.InlineMediaType.Image).toBe(WASM_INLINE_IMAGE);
    expect(wasm.InlineMediaType.Audio).toBe(WASM_INLINE_AUDIO);
    expect(wasm.InlineMediaType.Video).toBe(WASM_INLINE_VIDEO);
  });

  it("maps allowlisted types and refuses the rest", () => {
    expect(inlineTypeOf("audio/mp4", allowlist)).toBe("audio");
    expect(inlineTypeOf("video/mp4;codecs=avc1", allowlist)).toBe("video");
    expect(inlineTypeOf("image/gif", allowlist)).toBe("image");
    expect(inlineTypeOf("image/svg+xml", allowlist)).toBeNull();
    expect(inlineTypeOf("text/html", allowlist)).toBeNull();
    expect(inlineTypeOf(undefined, allowlist)).toBeNull();
    expect(inlineTypeOf("", allowlist)).toBeNull();
  });

  it("a throwing or unknown binding answer fails closed", () => {
    expect(inlineTypeOf("audio/mp4", () => { throw new Error("boom"); })).toBeNull();
    expect(inlineTypeOf("audio/mp4", () => 7)).toBeNull();
  });
});

describe("renderDecision", () => {
  it("a voice kind with an audio type is a voice bubble", () => {
    expect(renderDecision({ kind: "voice", mime: "audio/mp4" }, allowlist)).toEqual({
      bubble: "voice",
      inline: "audio",
    });
  });

  it("a voice kind with an HTML type is a file chip with no inline type", () => {
    expect(renderDecision({ kind: "voice", mime: "text/html" }, allowlist)).toEqual({
      bubble: "file",
      inline: null,
    });
  });

  it("a kind whose media class does not match the MIME type is a file chip", () => {
    expect(renderDecision({ kind: "voice", mime: "video/mp4" }, allowlist).bubble).toBe("file");
    expect(renderDecision({ kind: "video_note", mime: "audio/mp4" }, allowlist).bubble).toBe("file");
    expect(renderDecision({ kind: "gif", mime: "audio/mp4" }, allowlist).bubble).toBe("file");
  });

  it("video notes and GIFs render by kind when the type allows it", () => {
    expect(renderDecision({ kind: "video_note", mime: "video/mp4" }, allowlist).bubble).toBe("video_note");
    expect(renderDecision({ kind: "gif", mime: "image/gif" }, allowlist).bubble).toBe("gif");
    expect(renderDecision({ kind: "gif", mime: "image/webp" }, allowlist).bubble).toBe("gif");
  });

  it("no kind or an unknown kind is a file chip that may still preview inline", () => {
    expect(renderDecision({ mime: "audio/mp4" }, allowlist)).toEqual({ bubble: "file", inline: "audio" });
    expect(renderDecision({ kind: "sticker", mime: "image/png" }, allowlist)).toEqual({
      bubble: "file",
      inline: "image",
    });
  });

  it("stamps object URLs only with allowlisted types", () => {
    expect(blobTypeFor("audio/mp4", allowlist)).toBe("audio/mp4");
    expect(blobTypeFor("image/svg+xml", allowlist)).toBe("application/octet-stream");
    expect(blobTypeFor(undefined, allowlist)).toBe("application/octet-stream");
  });
});
