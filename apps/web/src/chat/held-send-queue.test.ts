// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.7 case 1 - a held message as stored in the message log, and
 * the rule by which every other writer of the log leaves it alone.
 */
import { describe, expect, it, vi } from "vitest";

vi.mock("../auth/key-store", () => ({ getDeviceKey: async () => null }));

import type { AttachmentRefBytes } from "./attachments";
import { heldFromPersisted, heldToPersisted, type HeldMessage } from "./held-send-queue";
import { carryHeld, type MessageLogPayload, type PersistedHeldSend } from "./message-log-store";

const REF: AttachmentRefBytes = {
  blobId: new Uint8Array(32).fill(1),
  key: new Uint8Array(32).fill(2),
  nonce: new Uint8Array(12).fill(3),
  mime: "audio/mp4",
  size: 1234,
  name: "voice.m4a",
  kind: "voice",
  durationMs: 2000,
} as AttachmentRefBytes;

const CONVERSATION = "ab".repeat(32);

describe("a held message round-trips through its stored form", () => {
  it("text, an uploaded attachment, a reply and a forward attribution", () => {
    const payload: HeldMessage = {
      text: "hello",
      attached: [REF],
      reply: { target: 7, author: "Anna", excerpt: "see you" },
      forward: { author: "Bob", origin: { groupIdB64: "b3JpZ2lu", messageId: 41 } },
    };
    const stored = heldToPersisted(CONVERSATION, { heldAtMs: 99, payload });
    // What is stored is plain JSON (it lives inside the encrypted log).
    const read = heldFromPersisted(JSON.parse(JSON.stringify(stored)));
    expect(read?.conversationHex).toBe(CONVERSATION);
    expect(read?.held.heldAtMs).toBe(99);
    expect(read?.held.payload.text).toBe("hello");
    expect(read?.held.payload.reply).toEqual(payload.reply);
    expect(read?.held.payload.forward).toEqual(payload.forward);
    const a = read?.held.payload.attached[0];
    expect(Array.from(a?.blobId ?? [])).toEqual(Array.from(REF.blobId));
    expect(Array.from(a?.key ?? [])).toEqual(Array.from(REF.key));
    expect(a?.kind).toBe("voice");
    expect(a?.durationMs).toBe(2000);
  });

  it("a plain typed message stores no reply and no forward", () => {
    const stored = heldToPersisted(CONVERSATION, {
      heldAtMs: 1,
      payload: { text: "hi", attached: [], reply: undefined },
    });
    expect(stored).toEqual({ conversationHex: CONVERSATION, heldAtMs: 1, text: "hi", attachments: [] });
    expect(heldFromPersisted(stored)?.held.payload).toEqual({ text: "hi", attached: [], reply: undefined });
  });

  it("junk is dropped, never sent", () => {
    const good: PersistedHeldSend = { conversationHex: CONVERSATION, heldAtMs: 1, text: "x", attachments: [] };
    for (const bad of [
      null,
      "x",
      { ...good, conversationHex: "not hex" },
      { ...good, heldAtMs: "1" },
      { ...good, text: 5 },
      { ...good, attachments: "none" },
      { ...good, text: "" },
    ]) {
      expect(heldFromPersisted(bad)).toBeNull();
    }
  });
});

describe("carryHeld: the held list rides every other write of the log", () => {
  const held: PersistedHeldSend[] = [
    { conversationHex: CONVERSATION, heldAtMs: 1, text: "x", attachments: [] },
  ];
  const disk: MessageLogPayload = { version: 5, groups: {}, held };

  it("a writer that does not mention it keeps what is on disk", () => {
    expect(carryHeld(disk, { version: 5, groups: { g: { cursor: "1", messages: [] } } })).toEqual({
      version: 5,
      groups: { g: { cursor: "1", messages: [] } },
      held,
    });
  });
  it("a writer that names a list replaces it, and an empty one removes the field", () => {
    const other: PersistedHeldSend[] = [{ ...held[0]!, text: "y" }];
    expect(carryHeld(disk, { version: 5, groups: {}, held: other }).held).toEqual(other);
    expect(carryHeld(disk, { version: 5, groups: {}, held: [] })).toEqual({ version: 5, groups: {} });
  });
  it("nothing on disk adds nothing", () => {
    expect(carryHeld(null, { version: 5, groups: {} })).toEqual({ version: 5, groups: {} });
    expect(carryHeld({ version: 5, groups: {} }, { version: 5, groups: {} })).toEqual({ version: 5, groups: {} });
  });
});
