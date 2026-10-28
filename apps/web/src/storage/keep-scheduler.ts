// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 section 3 — when this device tells the server that files are
 * still used (`POST /blob/keep`), so files nobody downloads for a year are
 * not removed while they are pinned, saved, or the own profile photo.
 *
 * Each source (a conversation's pinned messages, the saved list, the own
 * profile photo) is kept about once a month. A source seen for the first
 * time is given a due time at a spread offset inside the first month, keyed
 * on its name, so a device holding many conversations does not send all its
 * keeps in one burst; and a tick sends at most {@link MAX_SOURCES_PER_TICK}
 * sources. Pure: the clock, the sources and the persisted state are the
 * caller's.
 */

export const KEEP_INTERVAL_MS = 30 * 24 * 60 * 60 * 1000;
/** Sources one tick sends at most. Chosen. */
export const MAX_SOURCES_PER_TICK = 4;
/** Ids per `POST /blob/keep` (the server's bound). */
export const KEEP_BATCH = 256;

/** One thing whose files are kept. */
export interface KeepSource {
  /** Stable name: `pins:<conversation id hex>`, `saved`, `profile`. */
  readonly key: string;
  /** Blob ids, lower-case hex. */
  readonly idsHex: readonly string[];
}

/** Persisted: when each source is next due, unix ms. */
export interface KeepState {
  readonly v: 1;
  readonly due: Readonly<Record<string, number>>;
}

export const EMPTY_KEEP_STATE: KeepState = { v: 1, due: {} };

/** A stable offset inside one interval for `key`. */
export function spreadOffsetMs(key: string): number {
  let h = 2166136261;
  for (let i = 0; i < key.length; i++) {
    h ^= key.charCodeAt(i);
    h = Math.imul(h, 16777619) >>> 0;
  }
  return h % KEEP_INTERVAL_MS;
}

export interface KeepPlan {
  /** The sources sent now. */
  readonly keys: readonly string[];
  /** Their ids, deduplicated, in batches of at most {@link KEEP_BATCH}. */
  readonly batches: readonly (readonly string[])[];
  /** The state with first-seen sources scheduled and gone sources dropped
   *  (persist it whether or not the batches went out). */
  readonly state: KeepState;
}

/** What this tick sends. A source with no ids is never sent. */
export function planKeeps(state: KeepState, sources: readonly KeepSource[], nowMs: number): KeepPlan {
  const due: Record<string, number> = {};
  const ready: KeepSource[] = [];
  for (const src of sources) {
    if (src.idsHex.length === 0) continue;
    const at = state.due[src.key] ?? nowMs + spreadOffsetMs(src.key);
    due[src.key] = at;
    if (at <= nowMs) ready.push(src);
  }
  ready.sort((a, b) => (due[a.key] ?? 0) - (due[b.key] ?? 0) || (a.key < b.key ? -1 : 1));
  const chosen = ready.slice(0, MAX_SOURCES_PER_TICK);
  const ids: string[] = [];
  const seen = new Set<string>();
  for (const src of chosen) {
    for (const id of src.idsHex) {
      const lower = id.toLowerCase();
      if (seen.has(lower)) continue;
      seen.add(lower);
      ids.push(lower);
    }
  }
  const batches: string[][] = [];
  for (let i = 0; i < ids.length; i += KEEP_BATCH) batches.push(ids.slice(i, i + KEEP_BATCH));
  return { keys: chosen.map((s) => s.key), batches, state: { v: 1, due } };
}

/** The keeps for `keys` went out: each is next due one interval from now. */
export function markKept(state: KeepState, keys: readonly string[], nowMs: number): KeepState {
  const due = { ...state.due };
  for (const k of keys) due[k] = nowMs + KEEP_INTERVAL_MS;
  return { v: 1, due };
}

/** Read a persisted state; anything malformed reads as empty. */
export function parseKeepState(raw: string | null): KeepState {
  if (raw === null) return EMPTY_KEEP_STATE;
  try {
    const o = JSON.parse(raw) as { v?: unknown; due?: unknown };
    if (o.v !== 1 || typeof o.due !== "object" || o.due === null) return EMPTY_KEEP_STATE;
    const due: Record<string, number> = {};
    for (const [k, v] of Object.entries(o.due as Record<string, unknown>)) {
      if (typeof v === "number" && Number.isFinite(v)) due[k] = v;
    }
    return { v: 1, due };
  } catch {
    return EMPTY_KEEP_STATE;
  }
}
