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
  bubbleMetaSlot,
  inlineTypeOf,
  renderDecision,
  soleMediaBubble,
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

  it("no kind with an image type is a photo bubble, with video/mp4 a video bubble", () => {
    for (const mime of ["image/jpeg", "image/png", "image/gif", "image/webp"]) {
      expect(renderDecision({ mime }, allowlist)).toEqual({ bubble: "photo", inline: "image" });
    }
    expect(renderDecision({ mime: "video/mp4" }, allowlist)).toEqual({ bubble: "video", inline: "video" });
  });

  it("HEIC, SVG, an absent type and a non-media type stay file chips", () => {
    expect(renderDecision({ mime: "image/heic" }, allowlist).bubble).toBe("file");
    expect(renderDecision({ mime: "image/svg+xml" }, allowlist).bubble).toBe("file");
    expect(renderDecision({ mime: undefined }, allowlist).bubble).toBe("file");
    expect(renderDecision({ mime: "application/pdf" }, allowlist).bubble).toBe("file");
  });

  it("no kind with audio, or an unknown kind, is a file chip that may still preview inline", () => {
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

/**
 * UX spec section 2 / section 3 / section 10 — where a message's time, edited
 * marker and delivery ticks are drawn.
 *
 * THE DEFECT this rule exists for: the chat view carried two overlapping
 * conditions instead (one for the chromeless video note, one for the
 * edge-to-edge photo), they did not agree on whether to read the edited text
 * or the original, and a lone VOICE message fell through both — so its own
 * time landed on a row under the player, one level below the recording's
 * duration, which section 2 says explicitly not to do.
 */
describe("where the meta line goes", () => {
  const voice = { kind: "voice", mime: "audio/mp4" };
  const photo = { kind: undefined, mime: "image/png" };
  const note = { kind: "video_note", mime: "video/mp4" };
  const gif = { kind: "gif", mime: "image/gif" };
  const doc = { kind: undefined, mime: "application/pdf" };
  const sole = (
    over: Partial<Parameters<typeof soleMediaBubble>[0]> = {},
  ): ReturnType<typeof soleMediaBubble> =>
    soleMediaBubble({ deleted: false, text: "", attachments: [voice], ...over }, allowlist);

  it("a lone voice message puts its time on the voice row's own line", () => {
    expect(sole()).toBe("voice");
    expect(bubbleMetaSlot(sole())).toBe("voice-line");
  });

  it("a voice message that is not alone keeps the row below", () => {
    // Three ways to stop being "nothing but one piece of media", and each one
    // gives the bubble a paragraph for the line to sit under.
    expect(sole({ text: "listen to this" })).toBeNull();
    expect(sole({ attachments: [voice, photo] })).toBeNull();
    expect(sole({ deleted: true })).toBeNull();
    for (const s of [{ text: "x" }, { attachments: [voice, photo] }, { deleted: true }]) {
      expect(bubbleMetaSlot(sole(s))).toBe("bubble-row");
    }
  });

  it("the other sole bubbles keep the slots they already had", () => {
    expect(bubbleMetaSlot(sole({ attachments: [photo] }))).toBe("media-pill");
    expect(bubbleMetaSlot(sole({ attachments: [{ mime: "video/mp4" }] }))).toBe("media-pill");
    expect(bubbleMetaSlot(sole({ attachments: [note] }))).toBe("note-pill");
    // A GIF is a bubble WITH chrome, so a pill over it would cover an
    // animation that may be 40px tall; a file chip is an ordinary bubble.
    expect(bubbleMetaSlot(sole({ attachments: [gif] }))).toBe("bubble-row");
    expect(bubbleMetaSlot(sole({ attachments: [doc] }))).toBe("bubble-row");
  });

  it("the slot follows the allowlist, never the sender's `kind` alone", () => {
    // The same rule that picks the bubble picks the slot, so a `kind: voice`
    // carrying a refused MIME type is a file chip with an ordinary row - it
    // never gets a voice row to put a time on.
    expect(sole({ attachments: [{ kind: "voice", mime: "text/html" }] })).toBe("file");
    expect(bubbleMetaSlot(sole({ attachments: [{ kind: "voice", mime: "text/html" }] }))).toBe(
      "bubble-row",
    );
  });

  it("a message with no attachment at all is an ordinary bubble", () => {
    expect(sole({ attachments: [], text: "hello" })).toBeNull();
    expect(bubbleMetaSlot(null)).toBe("bubble-row");
  });
});
