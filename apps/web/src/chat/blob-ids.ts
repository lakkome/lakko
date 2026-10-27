// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 — the storage blobs a message names: each attachment's file and,
 * for a photo, video or video note, its poster thumbnail. What a delete for
 * everyone removes, what a save records, and what a pin keeps.
 */

import type { AttachmentRefBytes } from "./attachments";
import type { PersistedGroup, PersistedMessage } from "./message-log-store";

export function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

function b64ToHex(b64: string): string | null {
  try {
    const bin = atob(b64);
    if (bin.length !== 32) return null;
    let out = "";
    for (let i = 0; i < bin.length; i++) out += bin.charCodeAt(i).toString(16).padStart(2, "0");
    return out;
  } catch {
    return null;
  }
}

function dedup(ids: readonly string[]): string[] {
  return [...new Set(ids)];
}

/** The blob ids (lower-case hex) of attachment refs: files and thumbs. */
export function blobIdsOfRefs(refs: readonly AttachmentRefBytes[]): string[] {
  const out: string[] = [];
  for (const r of refs) {
    if (r.blobId.length === 32) out.push(bytesToHex(r.blobId));
    if (r.thumb !== undefined && r.thumb.blobId.length === 32) out.push(bytesToHex(r.thumb.blobId));
  }
  return dedup(out);
}

/** The blob ids of a stored message. */
export function blobIdsOfPersisted(message: PersistedMessage): string[] {
  const out: string[] = [];
  for (const a of message.attachments) {
    const id = b64ToHex(a.blobIdB64);
    if (id !== null) out.push(id);
    const thumb = a.thumb ?? null;
    if (thumb !== null) {
      const t = b64ToHex(thumb.blobIdB64);
      if (t !== null) out.push(t);
    }
  }
  return dedup(out);
}

/** The blob ids of the messages of `group` whose ids are in `messageIds`
 *  (the pinned ones), in the order the log holds them. */
export function blobIdsOfMessages(group: PersistedGroup | undefined, messageIds: ReadonlySet<number>): string[] {
  if (group === undefined || messageIds.size === 0) return [];
  const out: string[] = [];
  for (const m of group.messages) {
    const id = Number(m.messageId);
    if (!messageIds.has(id)) continue;
    out.push(...blobIdsOfPersisted(m));
  }
  return dedup(out);
}

/** Flat-concatenated 32-byte ids (the `savedBlobIds` / `addSaved` form) to
 *  hex, and back. */
export function splitBlobIds(flat: Uint8Array): string[] {
  const out: string[] = [];
  for (let i = 0; i + 32 <= flat.length; i += 32) out.push(bytesToHex(flat.subarray(i, i + 32)));
  return out;
}

export function joinBlobIds(idsHex: readonly string[]): Uint8Array {
  const out = new Uint8Array(idsHex.length * 32);
  idsHex.forEach((hex, i) => {
    for (let j = 0; j < 32; j++) out[i * 32 + j] = Number.parseInt(hex.slice(j * 2, j * 2 + 2), 16);
  });
  return out;
}

export function hexToBytes32(hex: string): Uint8Array | null {
  if (!/^[0-9a-f]{64}$/.test(hex)) return null;
  const out = new Uint8Array(32);
  for (let j = 0; j < 32; j++) out[j] = Number.parseInt(hex.slice(j * 2, j * 2 + 2), 16);
  return out;
}
