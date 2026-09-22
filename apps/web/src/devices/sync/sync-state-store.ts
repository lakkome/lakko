// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The cross-device sync state, written to this device's disk.
 *
 * Drafts, pin / mute / archive, read cursors and the chat order travel
 * between a person's devices over the DeviceGroup journal, and until this
 * existed they travelled nowhere else: the four maps were reactive signals
 * and nothing more. A reload started them empty, and the journal could not
 * refill them - this client resumes from a persisted cursor, so the rows that
 * carried them are behind it and are never read again. A chat pinned and a
 * draft half-written were gone, permanently, for a reload.
 *
 * The natives reach the same place by the opposite route: they poll from
 * cursor 0 on every launch, but they restore the DeviceGroup snapshot first,
 * so the rows they re-fetch are ones their MLS state has already consumed and
 * cannot decrypt again.
 *
 * What is stored is a SNAPSHOT of the folded state, not the journal. That is
 * what makes restoring it safe: every entry carries the HLC of the payload
 * that produced it, the poll folds newer rows on top through the same
 * `shouldReplace` comparison it always used, and a sibling device's later
 * write therefore wins over a restored one exactly as it would over a live
 * one. Nothing here is authoritative; it is a head start.
 *
 * Pure encode / decode, no I/O - `sync-state-idb.ts` owns the database and
 * the encryption, and vitest can run this without either.
 *
 * **Drafts are message content.** The blob is encrypted under the device root
 * key like every other local store (`read-watermark-idb.ts`,
 * `message-log-store.ts`); a draft is a message the person has not sent yet,
 * and it is the one piece of this state that would be worth reading.
 */

import {
  type DraftState,
  type FlagsState,
  type OrderState,
  type ReadCursorState,
} from "./state";

/** Payload schema version this client writes. */
export const SYNC_STATE_VERSION = 1;

/** The four maps, as one snapshot. */
export interface SyncStateSnapshot {
  readonly drafts: Readonly<Record<string, DraftState>>;
  readonly flags: Readonly<Record<string, FlagsState>>;
  readonly readCursors: Readonly<Record<string, ReadCursorState>>;
  readonly order: OrderState | null;
}

// ---------- wire shape -------------------------------------------
//
// `Uint8Array` and `bigint` are not JSON, so bytes ride as base64 and every
// HLC as a decimal string. Short keys because this row is rewritten on every
// keystroke-driven draft change.

interface WireDraft {
  readonly t: string;
  readonly r: string;
  readonly h: string;
}
interface WireFlags {
  readonly p: boolean;
  readonly m: boolean;
  readonly a: boolean;
  readonly u: boolean;
  readonly h: string;
}
interface WireCursor {
  readonly i: string;
  readonly r: string;
  readonly h: string;
}
interface WireOrder {
  readonly g: readonly string[];
  readonly h: string;
}
interface WirePayload {
  readonly v: number;
  readonly d: Record<string, WireDraft>;
  readonly f: Record<string, WireFlags>;
  readonly c: Record<string, WireCursor>;
  readonly o: WireOrder | null;
}

function b64(bytes: Uint8Array): string {
  let s = "";
  for (const byte of bytes) s += String.fromCharCode(byte);
  return btoa(s);
}

function unb64(text: string): Uint8Array | null {
  try {
    const raw = atob(text);
    const out = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i += 1) out[i] = raw.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

/** A decimal string back to a `bigint`, or `null` when it is not one. */
function bigOf(text: unknown): bigint | null {
  if (typeof text !== "string") return null;
  try {
    return BigInt(text);
  } catch {
    return null;
  }
}

/** Encode the snapshot to the payload JSON. */
export function encodeSyncState(state: SyncStateSnapshot): string {
  const d: Record<string, WireDraft> = {};
  for (const [hex, draft] of Object.entries(state.drafts)) {
    d[hex] = {
      t: draft.text,
      r: b64(draft.replyToId),
      h: draft.updatedAtHlc.toString(),
    };
  }
  const f: Record<string, WireFlags> = {};
  for (const [hex, flags] of Object.entries(state.flags)) {
    f[hex] = {
      p: flags.pinned,
      m: flags.muted,
      a: flags.archived,
      u: flags.unreadOverride,
      h: flags.updatedAtHlc.toString(),
    };
  }
  const c: Record<string, WireCursor> = {};
  for (const [hex, cursor] of Object.entries(state.readCursors)) {
    c[hex] = {
      i: b64(cursor.upToMessageId),
      r: cursor.readAtMs.toString(),
      h: cursor.updatedAtHlc.toString(),
    };
  }
  const o: WireOrder | null =
    state.order === null
      ? null
      : {
          g: state.order.groupIds.map(b64),
          h: state.order.updatedAtHlc.toString(),
        };
  const payload: WirePayload = { v: SYNC_STATE_VERSION, d, f, c, o };
  return JSON.stringify(payload);
}

/**
 * Decode payload JSON.
 *
 * A malformed ENTRY is dropped and the rest of the snapshot is kept: losing
 * one chat's pin is better than losing every chat's, and the poll can refill
 * any single entry from a sibling's next write. A payload that is not this
 * schema at all returns `null`, which the caller treats as "nothing stored"
 * - the same state a first run is in.
 */
export function decodeSyncState(json: string): SyncStateSnapshot | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const obj = parsed as Partial<WirePayload>;
  if (obj.v !== SYNC_STATE_VERSION) return null;

  const drafts: Record<string, DraftState> = {};
  for (const [hex, raw] of Object.entries(obj.d ?? {})) {
    const hlc = bigOf(raw?.h);
    const replyToId = typeof raw?.r === "string" ? unb64(raw.r) : null;
    if (hlc === null || replyToId === null || typeof raw?.t !== "string") continue;
    drafts[hex] = { text: raw.t, replyToId, updatedAtHlc: hlc };
  }

  const flags: Record<string, FlagsState> = {};
  for (const [hex, raw] of Object.entries(obj.f ?? {})) {
    const hlc = bigOf(raw?.h);
    if (
      hlc === null ||
      typeof raw?.p !== "boolean" ||
      typeof raw?.m !== "boolean" ||
      typeof raw?.a !== "boolean" ||
      typeof raw?.u !== "boolean"
    ) {
      continue;
    }
    flags[hex] = {
      pinned: raw.p,
      muted: raw.m,
      archived: raw.a,
      unreadOverride: raw.u,
      updatedAtHlc: hlc,
    };
  }

  const readCursors: Record<string, ReadCursorState> = {};
  for (const [hex, raw] of Object.entries(obj.c ?? {})) {
    const hlc = bigOf(raw?.h);
    const readAtMs = bigOf(raw?.r);
    const upToMessageId = typeof raw?.i === "string" ? unb64(raw.i) : null;
    if (hlc === null || readAtMs === null || upToMessageId === null) continue;
    readCursors[hex] = { upToMessageId, readAtMs, updatedAtHlc: hlc };
  }

  let order: OrderState | null = null;
  const rawOrder = obj.o;
  if (rawOrder !== null && rawOrder !== undefined) {
    const hlc = bigOf(rawOrder.h);
    const ids = Array.isArray(rawOrder.g)
      ? rawOrder.g.map((g) => (typeof g === "string" ? unb64(g) : null))
      : null;
    // The order is one value rather than a map, so a single bad id would
    // reorder the list silently. Either all of it decodes or none does.
    if (hlc !== null && ids !== null && ids.every((g) => g !== null)) {
      order = { groupIds: ids as Uint8Array[], updatedAtHlc: hlc };
    }
  }

  return { drafts, flags, readCursors, order };
}

/** Whether a snapshot holds anything worth writing. */
export function isEmptySyncState(state: SyncStateSnapshot): boolean {
  return (
    Object.keys(state.drafts).length === 0 &&
    Object.keys(state.flags).length === 0 &&
    Object.keys(state.readCursors).length === 0 &&
    state.order === null
  );
}
