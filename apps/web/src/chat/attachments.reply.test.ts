// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0052 decision 1 — `composeReplyEnvelope` writes the reply reference
 * beside the text and `parseBodyEnvelope` reads it back as an ORDINARY chat
 * body, through the REAL wasm core (so the bounds and the "not one of the
 * exclusive ops" rule under test are `lakko-attachments`').
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init from "../../wasm/lakko_ffi_wasm";
import {
  composeEnvelope,
  composeReplyEnvelope,
  parseBodyEnvelope,
  type AttachmentRefBytes,
} from "./attachments";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const fill = (n: number, v: number): Uint8Array => new Uint8Array(n).fill(v);

const PHOTO: AttachmentRefBytes = {
  blobId: fill(32, 1),
  key: fill(32, 2),
  nonce: fill(12, 3),
  mime: "image/jpeg",
  size: 4096,
  name: "photo.jpg",
  width: 640,
  height: 480,
};

describe("the reply reference on the chat envelope", () => {
  it("a text-only reply parses as a chat body carrying the reference", async () => {
    const bytes = await composeReplyEnvelope("yes, at five", [], {
      target: 42,
      author: "Anna",
      excerpt: "see you at 5?",
    });
    const body = await parseBodyEnvelope(bytes);
    if (body.kind !== "chat") throw new Error("expected chat");
    expect(body.text).toBe("yes, at five");
    expect(body.attachments).toEqual([]);
    expect(body.reply).toEqual({ target: 42, author: "Anna", excerpt: "see you at 5?" });
  });

  it("a reply with attachments carries both the refs and the reference", async () => {
    const body = await parseBodyEnvelope(
      await composeReplyEnvelope("", [PHOTO], { target: 7, author: "You", excerpt: "Photo" }),
    );
    if (body.kind !== "chat") throw new Error("expected chat");
    expect(body.attachments).toHaveLength(1);
    expect(body.attachments[0]?.name).toBe("photo.jpg");
    expect([body.attachments[0]?.width, body.attachments[0]?.height]).toEqual([640, 480]);
    expect(body.reply).toEqual({ target: 7, author: "You", excerpt: "Photo" });
  });

  it("a reply with no snapshot carries only the target", async () => {
    const body = await parseBodyEnvelope(await composeReplyEnvelope("hi", [], { target: 9 }));
    if (body.kind !== "chat") throw new Error("expected chat");
    expect(body.reply).toEqual({ target: 9 });
    expect(Object.keys(body.reply ?? {})).toEqual(["target"]);
  });

  it("the core drops an empty snapshot half rather than carrying an empty string", async () => {
    const body = await parseBodyEnvelope(
      await composeReplyEnvelope("hi", [], { target: 9, author: "", excerpt: "words" }),
    );
    if (body.kind !== "chat") throw new Error("expected chat");
    expect(body.reply).toEqual({ target: 9, excerpt: "words" });
  });

  it("an over-long excerpt is truncated by the core, on a character boundary", async () => {
    const long = "é".repeat(600); // two bytes each; 512 bytes is not a multiple of 2 + 1
    const body = await parseBodyEnvelope(
      await composeReplyEnvelope("hi", [], { target: 9, author: "A", excerpt: long }),
    );
    if (body.kind !== "chat") throw new Error("expected chat");
    const excerpt = body.reply?.excerpt ?? "";
    expect(excerpt.length).toBeLessThan(long.length);
    expect(excerpt.length).toBeGreaterThan(0);
    expect(new TextEncoder().encode(excerpt).length).toBeLessThanOrEqual(512);
    // Every character survived whole.
    expect(excerpt).toBe("é".repeat(excerpt.length));
  });

  it("a plain message carries no reference", async () => {
    const body = await parseBodyEnvelope(await composeEnvelope("hello", []));
    if (body.kind !== "chat") throw new Error("expected chat");
    expect(body.reply).toBeUndefined();
    expect("reply" in body).toBe(false);
  });
});
