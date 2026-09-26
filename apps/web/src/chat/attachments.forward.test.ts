// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0052 decision 2 — `composeForwardEnvelope` writes the forward
 * attribution beside the text and `parseBodyEnvelope` reads it back as an
 * ORDINARY chat body, through the REAL wasm core (so the bounds and the
 * both-or-neither origin rule under test are `lakko-attachments`').
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init from "../../wasm/lakko_ffi_wasm";
import {
  composeEnvelope,
  composeForwardEnvelope,
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

const ORIGIN = { groupIdB64: "Z3JvdXA=", messageId: 41 };

describe("the forward attribution on the chat envelope", () => {
  it("a text-only forward parses as a chat body carrying the name and the origin", async () => {
    const body = await parseBodyEnvelope(
      await composeForwardEnvelope("see you at 5?", [], { author: "Anna", origin: ORIGIN }),
    );
    if (body.kind !== "chat") throw new Error("expected chat");
    expect(body.text).toBe("see you at 5?");
    expect(body.attachments).toEqual([]);
    expect(body.forward).toEqual({ author: "Anna", origin: ORIGIN });
    expect("reply" in body).toBe(false);
  });

  it("a forward with attachments carries both the refs and the attribution", async () => {
    const body = await parseBodyEnvelope(
      await composeForwardEnvelope("", [PHOTO], { author: "You", origin: ORIGIN }),
    );
    if (body.kind !== "chat") throw new Error("expected chat");
    expect(body.attachments).toHaveLength(1);
    expect(body.attachments[0]?.name).toBe("photo.jpg");
    expect([body.attachments[0]?.width, body.attachments[0]?.height]).toEqual([640, 480]);
    expect(body.forward).toEqual({ author: "You", origin: ORIGIN });
  });

  it("a name alone and an origin alone each parse as exactly that", async () => {
    const named = await parseBodyEnvelope(await composeForwardEnvelope("hi", [], { author: "Anna" }));
    if (named.kind !== "chat") throw new Error("expected chat");
    expect(named.forward).toEqual({ author: "Anna" });
    expect(Object.keys(named.forward ?? {})).toEqual(["author"]);

    const placed = await parseBodyEnvelope(
      await composeForwardEnvelope("hi", [], { origin: ORIGIN }),
    );
    if (placed.kind !== "chat") throw new Error("expected chat");
    expect(placed.forward).toEqual({ origin: ORIGIN });
  });

  it("the core truncates an over-long name on a character boundary", async () => {
    const long = "é".repeat(300); // two bytes each; 255 is not a multiple of 2
    const body = await parseBodyEnvelope(
      await composeForwardEnvelope("hi", [], { author: long, origin: ORIGIN }),
    );
    if (body.kind !== "chat") throw new Error("expected chat");
    const name = body.forward?.author ?? "";
    expect(name.length).toBeGreaterThan(0);
    expect(name.length).toBeLessThan(long.length);
    expect(new TextEncoder().encode(name).length).toBeLessThanOrEqual(255);
    expect(name).toBe("é".repeat(name.length));
  });

  it("a plain message and a reply carry no attribution", async () => {
    const plain = await parseBodyEnvelope(await composeEnvelope("hello", []));
    if (plain.kind !== "chat") throw new Error("expected chat");
    expect("forward" in plain).toBe(false);
    const reply = await parseBodyEnvelope(
      await composeReplyEnvelope("yes", [], { target: 3, author: "A", excerpt: "x" }),
    );
    if (reply.kind !== "chat") throw new Error("expected chat");
    expect("forward" in reply).toBe(false);
    expect(reply.reply).toEqual({ target: 3, author: "A", excerpt: "x" });
  });
});
