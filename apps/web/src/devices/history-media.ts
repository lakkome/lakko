// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 media metadata ↔ the history-bundle placeholder (version 2).
 *
 * Plain module so the mapping runs under vitest without the wasm build. The
 * core applies the envelope's bounds on both encode and decode
 * (`RawMediaMetadata::bounded`); the message log re-checks them on read, as
 * for any row. The local `played` marker is never transferred.
 */

import {
  b64UrlDecode,
  b64UrlEncode,
  persistedThumb,
  thumbFromPersisted,
} from "../chat/persisted-attachment";
import type { PersistedAttachment } from "../chat/message-log-store";

/** Arguments for `HistoryAttachmentInput.setMedia`. */
export interface HistoryMediaArgs {
  readonly kind: string | undefined;
  readonly durationMs: number | undefined;
  readonly waveform: Uint8Array | undefined;
  readonly width: number | undefined;
  readonly height: number | undefined;
}

/** Arguments for `HistoryAttachmentInput.setThumb`. */
export interface HistoryThumbArgs {
  readonly blobId: Uint8Array;
  readonly key: Uint8Array;
  readonly nonce: Uint8Array;
  readonly size: bigint;
  readonly mime: string | undefined;
  readonly width: number | undefined;
  readonly height: number | undefined;
}

/** The media part of an opened placeholder attachment (the wasm getters). */
export interface OpenedHistoryMedia {
  readonly kind: string | undefined;
  readonly durationMs: number | undefined;
  readonly waveform: Uint8Array | undefined;
  readonly width: number | undefined;
  readonly height: number | undefined;
  readonly thumb:
    | {
        readonly blobId: Uint8Array;
        readonly key: Uint8Array;
        readonly nonce: Uint8Array;
        readonly size: bigint;
        readonly mime: string | undefined;
        readonly width: number | undefined;
        readonly height: number | undefined;
      }
    | undefined;
}

type PersistedMedia = Pick<
  PersistedAttachment,
  "kind" | "durationMs" | "waveformB64" | "width" | "height" | "thumb"
>;

function decodeWaveform(b64: string | null | undefined): Uint8Array | undefined {
  if (typeof b64 !== "string" || b64.length === 0) return undefined;
  try {
    return b64UrlDecode(b64);
  } catch {
    return undefined;
  }
}

/** What to hand the placeholder encoder for one stored attachment. `null`
 *  means the attachment carries none of that part. */
export function historyMediaFromPersisted(a: PersistedAttachment): {
  readonly media: HistoryMediaArgs | null;
  readonly thumb: HistoryThumbArgs | null;
} {
  const media: HistoryMediaArgs = {
    kind: a.kind ?? undefined,
    durationMs: a.durationMs ?? undefined,
    waveform: decodeWaveform(a.waveformB64),
    width: a.width ?? undefined,
    height: a.height ?? undefined,
  };
  const hasMedia = Object.values(media).some((v) => v !== undefined);
  const t = thumbFromPersisted(a.thumb);
  return {
    media: hasMedia ? media : null,
    thumb:
      t === undefined
        ? null
        : {
            blobId: t.blobId,
            key: t.key,
            nonce: t.nonce,
            size: BigInt(t.size),
            mime: t.mime,
            width: t.width,
            height: t.height,
          },
  };
}

/** The stored-row media fields for one opened placeholder attachment. Absent
 *  fields are omitted, so a plain file restores exactly as before. */
export function persistedMediaFromOpened(o: OpenedHistoryMedia): PersistedMedia {
  const out: {
    -readonly [K in keyof PersistedMedia]: PersistedMedia[K];
  } = {};
  if (o.kind !== undefined) out.kind = o.kind;
  if (o.durationMs !== undefined) out.durationMs = o.durationMs;
  if (o.waveform !== undefined && o.waveform.length > 0) {
    out.waveformB64 = b64UrlEncode(o.waveform);
  }
  if (o.width !== undefined) out.width = o.width;
  if (o.height !== undefined) out.height = o.height;
  if (o.thumb !== undefined) {
    const size = Number(o.thumb.size);
    if (Number.isSafeInteger(size)) {
      out.thumb = persistedThumb({
        blobId: o.thumb.blobId,
        key: o.thumb.key,
        nonce: o.thumb.nonce,
        mime: o.thumb.mime,
        size,
        width: o.thumb.width,
        height: o.thumb.height,
      });
    }
  }
  return out;
}
