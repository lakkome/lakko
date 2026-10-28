// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The cross-device sync state, written to this device's disk.
 *
 * Drafts, pin / mute / archive, read cursors, the chat order and the chat
 * folders travel between a person's devices over the DeviceGroup journal.
 * This client resumes the journal from a persisted cursor, so the rows that
 * carried a pin or a folder are behind it and are never read again: without a
 * stored copy a reload would lose them.
 *
 * What is stored is the folded state itself, as `SyncedState.toBytes()`
 * writes it (ADR-0058). The fold lives in Rust (`lakko_sync::SyncedState`),
 * shared with iOS and Android, so the web no longer keeps a codec of its own
 * for the four maps - the bytes are opaque here. Every register inside keeps
 * the stamp it was written with, so a sibling device's later write wins over
 * a restored one exactly as it would over a live one.
 *
 * Beside it, the folder edits this device made that have not reached the
 * journal yet ({@link StampedEdit}): an edit applies here at once and travels
 * later (UX spec section 6), and a reload in between must not drop it on the
 * floor for the other devices.
 *
 * Pure encode / decode, no I/O - `sync-state-idb.ts` owns the database and
 * the encryption. The payload is JSON so the service worker can read the mute
 * flag out of it without the app's modules (`public/sw-push-decrypt.js`
 * `mutedFromSyncStateRow`).
 *
 * **Drafts and folder names are personal.** The row is encrypted under the
 * device root key like every other local store.
 */

import type { ConversationRef, RefKind, StampedEdit } from "../../chat/folders/folder-model";

/** Payload schema version this client writes. There is no migration: an
 *  older row is dropped and the state refills from the journal and from the
 *  growth re-send of the other devices (no production data exists). */
export const SYNC_STATE_VERSION = 2;

/** One stored row. */
export interface SyncStateRow {
  /** `SyncedState.toBytes()`; empty for a device that holds nothing yet. */
  readonly synced: Uint8Array;
  /** Folder edits not yet published, oldest first. One entry per change
   *  set, so each is re-sent as the one message it was. */
  readonly pending: readonly (readonly StampedEdit[])[];
}

// ---------- wire shape -------------------------------------------------

interface WireRef {
  readonly k: number;
  readonly i: string;
}
interface WireEdit {
  readonly t: string;
  readonly h: string;
  readonly f?: string;
  readonly n?: string;
  readonly ik?: number;
  readonly xf?: number;
  readonly r?: WireRef;
  readonly s?: number;
  readonly p?: readonly WireRef[];
  readonly o?: readonly string[];
  /** ADR-0065 saved: message id, saved-at, author, excerpt, kind, blob ids. */
  readonly m?: string;
  readonly at?: string;
  readonly st?: string;
  readonly a?: string;
  readonly x?: string;
  readonly ak?: string;
  readonly b?: readonly string[];
}
interface WirePayload {
  readonly v: number;
  readonly s: string;
  readonly p: readonly (readonly WireEdit[])[];
}

export function b64(bytes: Uint8Array): string {
  let s = "";
  for (const byte of bytes) s += String.fromCharCode(byte);
  return btoa(s);
}

export function unb64(text: string): Uint8Array | null {
  try {
    const raw = atob(text);
    const out = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i += 1) out[i] = raw.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

function bigOf(text: unknown): bigint | null {
  if (typeof text !== "string") return null;
  try {
    return BigInt(text);
  } catch {
    return null;
  }
}

function refToWire(r: ConversationRef): WireRef {
  return { k: r.kind, i: b64(r.id) };
}

function refFromWire(raw: unknown): ConversationRef | null {
  if (typeof raw !== "object" || raw === null) return null;
  const w = raw as Partial<WireRef>;
  if (w.k !== 1 && w.k !== 2 && w.k !== 3) return null;
  const id = typeof w.i === "string" ? unb64(w.i) : null;
  if (id === null || id.length === 0) return null;
  return { kind: w.k as RefKind, id };
}

function editToWire(e: StampedEdit): WireEdit {
  const h = e.hlc.toString();
  switch (e.t) {
    case "meta":
      return { t: e.t, h, f: e.folderIdHex, n: e.name, ik: e.includeKinds, xf: e.excludeFlags };
    case "member":
      return { t: e.t, h, f: e.folderIdHex, r: refToWire(e.ref), s: e.state };
    case "pins":
      return { t: e.t, h, f: e.folderIdHex, p: e.pinned.map(refToWire) };
    case "removed":
      return { t: e.t, h, f: e.folderIdHex };
    case "order":
      return { t: e.t, h, o: [...e.folderIdsHex] };
    case "saved":
      return {
        t: e.t,
        h,
        r: refToWire(e.ref),
        m: e.messageId.toString(),
        at: e.savedAtMs.toString(),
        st: e.sentAtMs.toString(),
        a: e.author,
        x: e.excerpt,
        ak: e.attachmentKind,
        b: [...e.blobIdsHex],
      };
    case "saved-removed":
      return { t: e.t, h, r: refToWire(e.ref), m: e.messageId.toString() };
  }
}

function editFromWire(raw: unknown): StampedEdit | null {
  if (typeof raw !== "object" || raw === null) return null;
  const w = raw as WireEdit;
  const hlc = bigOf(w.h);
  if (hlc === null) return null;
  const f = typeof w.f === "string" ? w.f : null;
  switch (w.t) {
    case "meta":
      if (f === null || typeof w.n !== "string" || typeof w.ik !== "number" || typeof w.xf !== "number") {
        return null;
      }
      return { t: "meta", hlc, folderIdHex: f, name: w.n, includeKinds: w.ik, excludeFlags: w.xf };
    case "member": {
      const ref = refFromWire(w.r);
      if (f === null || ref === null || (w.s !== 0 && w.s !== 1 && w.s !== 2)) return null;
      return { t: "member", hlc, folderIdHex: f, ref, state: w.s };
    }
    case "pins": {
      if (f === null || !Array.isArray(w.p)) return null;
      const pinned = w.p.map(refFromWire);
      if (pinned.some((r) => r === null)) return null;
      return { t: "pins", hlc, folderIdHex: f, pinned: pinned as ConversationRef[] };
    }
    case "removed":
      return f === null ? null : { t: "removed", hlc, folderIdHex: f };
    case "order":
      if (!Array.isArray(w.o) || w.o.some((x) => typeof x !== "string")) return null;
      return { t: "order", hlc, folderIdsHex: w.o as string[] };
    case "saved": {
      const ref = refFromWire(w.r);
      const messageId = bigOf(w.m);
      const savedAtMs = bigOf(w.at);
      const sentAtMs = w.st === undefined ? 0n : bigOf(w.st);
      if (
        sentAtMs === null ||
        ref === null ||
        messageId === null ||
        savedAtMs === null ||
        typeof w.a !== "string" ||
        typeof w.x !== "string" ||
        typeof w.ak !== "string" ||
        !Array.isArray(w.b) ||
        w.b.some((id) => typeof id !== "string" || !/^[0-9a-f]{64}$/.test(id))
      ) {
        return null;
      }
      return {
        t: "saved",
        hlc,
        ref,
        messageId,
        savedAtMs,
        sentAtMs,
        author: w.a,
        excerpt: w.x,
        attachmentKind: w.ak,
        blobIdsHex: w.b as string[],
      };
    }
    case "saved-removed": {
      const ref = refFromWire(w.r);
      const messageId = bigOf(w.m);
      if (ref === null || messageId === null) return null;
      return { t: "saved-removed", hlc, ref, messageId };
    }
    default:
      return null;
  }
}

/** Encode the row to the payload JSON. */
export function encodeSyncState(row: SyncStateRow): string {
  const payload: WirePayload = {
    v: SYNC_STATE_VERSION,
    s: b64(row.synced),
    p: row.pending.map((batch) => batch.map(editToWire)),
  };
  return JSON.stringify(payload);
}

/**
 * Decode payload JSON. `null` for anything that is not this schema, which
 * the caller treats as "nothing stored". A pending batch that does not decode
 * whole is dropped whole: half a change set sent as if it were the edit would
 * be a different edit.
 */
export function decodeSyncState(json: string): SyncStateRow | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const obj = parsed as Partial<WirePayload>;
  if (obj.v !== SYNC_STATE_VERSION || typeof obj.s !== "string") return null;
  const synced = unb64(obj.s);
  if (synced === null) return null;
  const pending: StampedEdit[][] = [];
  if (Array.isArray(obj.p)) {
    for (const batch of obj.p) {
      if (!Array.isArray(batch)) continue;
      const edits = batch.map(editFromWire);
      if (edits.length === 0 || edits.some((e) => e === null)) continue;
      pending.push(edits as StampedEdit[]);
    }
  }
  return { synced, pending };
}

/** Whether a row holds anything worth writing. */
export function isEmptySyncState(row: SyncStateRow): boolean {
  return row.synced.length === 0 && row.pending.length === 0;
}
