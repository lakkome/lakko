// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 — the message log keeps media metadata across a reload, keeps the
 * local-only "played" marker, and re-applies the envelope's bounds on read.
 */
import { describe, expect, it } from "vitest";

import type { AttachmentRefBytes } from "./attachments";
import {
  attachmentRefFromPersisted,
  persistedFromAttachmentRef,
} from "./persisted-attachment";
import type { PersistedAttachment } from "./message-log-store";

function bytes(n: number, fill: number): Uint8Array {
  return new Uint8Array(n).fill(fill);
}

const VIDEO_NOTE: AttachmentRefBytes = {
  blobId: bytes(32, 1),
  key: bytes(32, 2),
  nonce: bytes(12, 3),
  mime: "video/mp4",
  size: 123_456,
  name: "video-1.mp4",
  kind: "video_note",
  durationMs: 9_000,
  waveform: new Uint8Array([0, 128, 255]),
  width: 480,
  height: 480,
  thumb: {
    blobId: bytes(32, 4),
    key: bytes(32, 5),
    nonce: bytes(12, 6),
    mime: "image/jpeg",
    size: 20_000,
    width: 240,
    height: 240,
  },
};

describe("round trip", () => {
  it("keeps every media field and the played marker", () => {
    const persisted = JSON.parse(JSON.stringify(persistedFromAttachmentRef(VIDEO_NOTE, true))) as PersistedAttachment;
    const { ref, played } = attachmentRefFromPersisted(persisted);
    expect(played).toBe(true);
    expect(ref).toEqual(VIDEO_NOTE);
  });

  it("a plain file writes no media keys and reads back unplayed", () => {
    const file: AttachmentRefBytes = {
      blobId: bytes(32, 9),
      key: bytes(32, 9),
      nonce: bytes(12, 9),
      mime: undefined,
      size: 5,
      name: undefined,
    };
    const persisted = persistedFromAttachmentRef(file);
    expect(Object.keys(persisted).sort()).toEqual(["blobIdB64", "keyB64", "mime", "name", "nonceB64", "sizeStr"]);
    const { ref, played } = attachmentRefFromPersisted(persisted);
    expect(played).toBe(false);
    expect(ref.kind).toBeUndefined();
    expect(ref.thumb).toBeUndefined();
    expect(ref.blobId).toEqual(file.blobId);
  });

  it("a row written before the media fields existed reads as a file", () => {
    const legacy = {
      blobIdB64: persistedFromAttachmentRef(VIDEO_NOTE).blobIdB64,
      keyB64: persistedFromAttachmentRef(VIDEO_NOTE).keyB64,
      nonceB64: persistedFromAttachmentRef(VIDEO_NOTE).nonceB64,
      mime: "audio/mp4",
      sizeStr: "10",
      name: "voice.m4a",
    } satisfies PersistedAttachment;
    const { ref } = attachmentRefFromPersisted(legacy);
    expect(ref.kind).toBeUndefined();
    expect(ref.durationMs).toBeUndefined();
  });
});

describe("bounds on read", () => {
  const base = persistedFromAttachmentRef(VIDEO_NOTE);

  it("drops an out-of-range field and keeps the attachment", () => {
    const { ref } = attachmentRefFromPersisted({
      ...base,
      durationMs: 60_001,
      width: 0,
      height: 4097,
      waveformB64: persistedFromAttachmentRef({ ...VIDEO_NOTE, waveform: bytes(129, 1) }).waveformB64 ?? null,
    });
    expect(ref.durationMs).toBeUndefined();
    expect(ref.width).toBeUndefined();
    expect(ref.height).toBeUndefined();
    expect(ref.waveform).toBeUndefined();
    expect(ref.kind).toBe("video_note");
    expect(ref.thumb).toBeDefined();
  });

  it("a duration only survives on a kind that has one, within that kind's cap", () => {
    expect(attachmentRefFromPersisted({ ...base, kind: "voice", durationMs: 15 * 60 * 1000 }).ref.durationMs).toBe(900_000);
    expect(attachmentRefFromPersisted({ ...base, kind: "voice", durationMs: 900_001 }).ref.durationMs).toBeUndefined();
    expect(attachmentRefFromPersisted({ ...base, kind: "gif", durationMs: 1000 }).ref.durationMs).toBeUndefined();
  });

  it("drops a thumb with a wrong-length key or an oversize body", () => {
    const thumb = base.thumb!;
    expect(attachmentRefFromPersisted({ ...base, thumb: { ...thumb, keyB64: "AAAA" } }).ref.thumb).toBeUndefined();
    expect(attachmentRefFromPersisted({ ...base, thumb: { ...thumb, sizeStr: String(256 * 1024 + 1) } }).ref.thumb).toBeUndefined();
    expect(attachmentRefFromPersisted({ ...base, thumb: null }).ref.thumb).toBeUndefined();
  });

  it("a non-boolean played value is not a played marker", () => {
    expect(attachmentRefFromPersisted({ ...base, played: "yes" as unknown as boolean }).played).toBe(false);
  });
});
