// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The upload of one composer-tray item (`docs/ux/media-messages.md` section
 * 11), written once for its two callers: the tray uploading an item the
 * moment it is added (`tray-store.ts`), and a message's send finishing an
 * item whose upload had not finished when Send was pressed (the outbox,
 * `docs/ux/connection-state.md` section 7.3).
 *
 * A part is the bytes that are sent (a photo after its re-encode) and what
 * the sender attaches besides: the size, the thumb (uploaded as its own
 * blob), the micro thumbnail inside the message, an audio file's tags and
 * cover. A failed thumb or cover sends the item without it.
 */

import {
  encryptAndUploadBytes,
  type AttachmentRefBytes,
  type ThumbRefBytes,
} from "./attachments";
import type { TraySource } from "./compose-tray";
import { StorageClient } from "../storage/client";

/** What an item carries besides its bytes. */
export interface TrayPartMeta {
  readonly source: TraySource;
  readonly size?: { readonly width: number; readonly height: number } | null;
  readonly thumb?: { readonly bytes: Uint8Array; readonly width: number; readonly height: number };
  /** ADR-0047 section 6 — the micro thumbnail; only on the message's carrier. */
  readonly inline?: Uint8Array;
  readonly audio?: {
    readonly title: string;
    readonly artist?: string;
    readonly durationMs?: number;
    readonly cover?: Uint8Array;
  };
}

/** One item ready to be uploaded. */
export interface TrayPart {
  readonly bytes: Uint8Array;
  readonly mime: string | undefined;
  readonly name: string | undefined;
  readonly meta: TrayPartMeta;
}

/** The bytes upload, injectable for a test. */
export type BytesUpload = typeof encryptAndUploadBytes;

/** Upload one part and return the reference the message carries. */
export async function uploadTrayPart(
  part: TrayPart,
  storage: StorageClient = new StorageClient(),
  onProgress?: (fraction: number) => void,
  upload: BytesUpload = encryptAndUploadBytes,
): Promise<AttachmentRefBytes> {
  let out = await upload(part.bytes, { mime: part.mime, name: part.name }, {}, storage, onProgress);
  const meta = part.meta;
  if (meta.size !== undefined && meta.size !== null) {
    out = { ...out, width: meta.size.width, height: meta.size.height };
  }
  if (meta.inline !== undefined) out = { ...out, inlineThumb: meta.inline };
  if (meta.thumb !== undefined) {
    try {
      const up = await upload(meta.thumb.bytes, { mime: "image/jpeg", name: undefined }, {}, storage);
      const thumb: ThumbRefBytes = {
        blobId: up.blobId,
        key: up.key,
        nonce: up.nonce,
        mime: "image/jpeg",
        size: meta.thumb.bytes.length,
        width: meta.thumb.width,
        height: meta.thumb.height,
      };
      out = { ...out, thumb };
    } catch {
      // Sent without a thumb; the receiver lays it out from the dimensions.
    }
  }
  if (meta.source === "audio" && meta.audio !== undefined) {
    const a = meta.audio;
    out = {
      ...out,
      kind: "audio",
      title: a.title,
      ...(a.artist === undefined ? {} : { artist: a.artist }),
      ...(a.durationMs === undefined ? {} : { durationMs: a.durationMs }),
    };
    if (a.cover !== undefined) {
      try {
        const up = await upload(a.cover, { mime: "image/jpeg", name: undefined }, {}, storage);
        out = {
          ...out,
          thumb: {
            blobId: up.blobId,
            key: up.key,
            nonce: up.nonce,
            mime: "image/jpeg",
            size: a.cover.length,
            width: undefined,
            height: undefined,
          },
        };
      } catch {
        // Sent without a cover.
      }
    }
  }
  return out;
}
