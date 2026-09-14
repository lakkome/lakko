// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `AttachmentRefBytes` ↔ the message log's `PersistedAttachment`.
 *
 * Lives outside `routes/Chat.tsx` so the round trip is executable under
 * vitest (the route file cannot be imported by a `node`-environment test).
 *
 * ADR-0047 adds media metadata to both shapes. The log is this device's own
 * encrypted store, but a row read back is still checked against the same
 * bounds the core applies to a received envelope — a field that fails its
 * bound is dropped and the attachment survives, exactly the envelope's rule —
 * so a corrupt or older row can never hand a player an out-of-range value.
 */

import type { AttachmentRefBytes, ThumbRefBytes } from "./attachments";
import type { PersistedAttachment, PersistedThumb } from "./message-log-store";

/** Mirrors `lakko-attachments` — see ADR-0047 §1. */
export const MAX_VOICE_DURATION_MS = 15 * 60 * 1000;
export const MAX_VIDEO_NOTE_DURATION_MS = 60 * 1000;
export const MAX_MEDIA_DIMENSION = 4096;
export const MAX_WAVEFORM_BYTES = 128;
export const MAX_THUMB_BYTES = 256 * 1024;
const BLOB_ID_LEN = 32;
const KEY_LEN = 32;
const NONCE_LEN = 12;

export function b64UrlEncode(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i += 1) {
    binary += String.fromCharCode(bytes[i] as number);
  }
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function b64UrlDecode(s: string): Uint8Array {
  const padded = s + "===".slice((s.length + 3) % 4);
  const std = padded.replace(/-/g, "+").replace(/_/g, "/");
  const binary = atob(std);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    out[i] = binary.charCodeAt(i);
  }
  return out;
}

function tryDecode(s: unknown): Uint8Array | undefined {
  if (typeof s !== "string") return undefined;
  try {
    return b64UrlDecode(s);
  } catch {
    return undefined;
  }
}

function dimension(v: unknown): number | undefined {
  return typeof v === "number" && Number.isInteger(v) && v >= 1 && v <= MAX_MEDIA_DIMENSION
    ? v
    : undefined;
}

/** The core's per-kind duration bound: only a voice or video-note ref
 *  carries a duration at all. */
export function boundedDuration(kind: string | undefined, v: unknown): number | undefined {
  if (typeof v !== "number" || !Number.isInteger(v) || v < 0) return undefined;
  if (kind === "voice") return v <= MAX_VOICE_DURATION_MS ? v : undefined;
  if (kind === "video_note") return v <= MAX_VIDEO_NOTE_DURATION_MS ? v : undefined;
  return undefined;
}

export function persistedThumb(t: ThumbRefBytes): PersistedThumb {
  return {
    blobIdB64: b64UrlEncode(t.blobId),
    keyB64: b64UrlEncode(t.key),
    nonceB64: b64UrlEncode(t.nonce),
    mime: t.mime ?? null,
    sizeStr: String(t.size),
    width: t.width ?? null,
    height: t.height ?? null,
  };
}

export function thumbFromPersisted(t: unknown): ThumbRefBytes | undefined {
  if (t === null || typeof t !== "object") return undefined;
  const p = t as Partial<PersistedThumb>;
  const blobId = tryDecode(p.blobIdB64);
  const key = tryDecode(p.keyB64);
  const nonce = tryDecode(p.nonceB64);
  const size = Number(p.sizeStr);
  if (
    blobId?.length !== BLOB_ID_LEN ||
    key?.length !== KEY_LEN ||
    nonce?.length !== NONCE_LEN ||
    !Number.isInteger(size) ||
    size < 1 ||
    size > MAX_THUMB_BYTES
  ) {
    return undefined;
  }
  return {
    blobId,
    key,
    nonce,
    mime: typeof p.mime === "string" ? p.mime : undefined,
    size,
    width: dimension(p.width),
    height: dimension(p.height),
  };
}

export function persistedFromAttachmentRef(
  a: AttachmentRefBytes,
  played: boolean = false,
): PersistedAttachment {
  return {
    blobIdB64: b64UrlEncode(a.blobId),
    keyB64: b64UrlEncode(a.key),
    nonceB64: b64UrlEncode(a.nonce),
    mime: a.mime ?? null,
    sizeStr: String(a.size),
    name: a.name ?? null,
    ...(a.kind === undefined ? {} : { kind: a.kind }),
    ...(a.durationMs === undefined ? {} : { durationMs: a.durationMs }),
    ...(a.waveform === undefined ? {} : { waveformB64: b64UrlEncode(a.waveform) }),
    ...(a.width === undefined ? {} : { width: a.width }),
    ...(a.height === undefined ? {} : { height: a.height }),
    ...(a.thumb === undefined ? {} : { thumb: persistedThumb(a.thumb) }),
    ...(played ? { played: true } : {}),
  };
}

export interface RestoredAttachment {
  readonly ref: AttachmentRefBytes;
  readonly played: boolean;
}

export function attachmentRefFromPersisted(a: PersistedAttachment): RestoredAttachment {
  const kind = typeof a.kind === "string" ? a.kind : undefined;
  const waveform = tryDecode(a.waveformB64);
  return {
    ref: {
      blobId: b64UrlDecode(a.blobIdB64),
      key: b64UrlDecode(a.keyB64),
      nonce: b64UrlDecode(a.nonceB64),
      mime: a.mime ?? undefined,
      size: Number(a.sizeStr),
      name: a.name ?? undefined,
      kind,
      durationMs: boundedDuration(kind, a.durationMs),
      waveform:
        waveform !== undefined && waveform.length >= 1 && waveform.length <= MAX_WAVEFORM_BYTES
          ? waveform
          : undefined,
      width: dimension(a.width),
      height: dimension(a.height),
      thumb: thumbFromPersisted(a.thumb),
    },
    played: a.played === true,
  };
}
