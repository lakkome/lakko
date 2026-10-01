// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0061 section 6 — a chat envelope from a newer Lakko (an unknown wire
 * version) never shows the core's English "[unsupported vN message]". The
 * REAL wasm core classifies it; this client keeps it as a message and draws
 * `chat.message.unsupported` in the timeline and the chat list preview.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init from "../../wasm/lakko_ffi_wasm";
import { parseBodyEnvelope, parseEnvelope } from "./attachments";
import { eventToPersistedMessage, summarizeGroupSlot } from "./background-log";
import type { IncomingChatEvent } from "./client";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const NEWER = new TextEncoder().encode(JSON.stringify({ v: 99, text: "from the future" }));
const ENGLISH = "This message needs a newer version of Lakko.";

describe("a message from a newer Lakko", () => {
  it("is classified with its version, with no core prose", async () => {
    const body = await parseBodyEnvelope(NEWER);
    expect(body).toEqual({ kind: "unsupported", version: 99 });
  });

  it("never yields the core's English placeholder from the legacy parser", async () => {
    const parsed = await parseEnvelope(NEWER);
    expect(parsed.text).toBe("");
    expect(parsed.attachments).toEqual([]);
  });

  it("is persisted as such and previewed in the catalog's words", () => {
    const event: Extract<IncomingChatEvent, { kind: "application" }> = {
      kind: "application",
      messageId: 7,
      text: "",
      plaintext: "",
      attachments: [],
      senderLeaf: 1,
      senderKey: undefined,
      createdAt: "2026-10-01T10:00:00Z",
      unsupportedVersion: 99,
    };
    const persisted = eventToPersistedMessage(event);
    expect(persisted.unsupportedVersion).toBe(99);
    expect(persisted.text).toBe("");
    const summary = summarizeGroupSlot({ cursor: "7", messages: [persisted] }, null);
    expect(summary.preview).toBe(ENGLISH);
  });

  it("leaves an ordinary message's preview alone", () => {
    const persisted = eventToPersistedMessage({
      kind: "application",
      messageId: 8,
      text: "[unsupported v99 message]",
      plaintext: "[unsupported v99 message]",
      attachments: [],
      senderLeaf: 1,
      senderKey: undefined,
      createdAt: "2026-10-01T10:00:00Z",
    });
    expect(persisted.unsupportedVersion).toBeUndefined();
    // A person who typed those words sees what they typed.
    expect(summarizeGroupSlot({ cursor: "8", messages: [persisted] }, null).preview).toBe(
      "[unsupported v99 message]",
    );
  });
});
