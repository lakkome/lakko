// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The group-call records (`group-call-record.ts`) of every chat, held for the
 * whole page instead of inside one chat view.
 *
 * The records used to be a signal inside `routes/Chat.tsx`, fed only by that
 * chat's receive loop. A `GroupCallStart` for a chat that was not open was
 * applied by nobody — and once the background receive consumes that row the
 * journal cursor has moved past it, so opening the chat later would never learn
 * the call exists. Both receive paths now fold group-control bodies into this
 * store (through the call host's single queue), and the chat view reads it.
 *
 * Reactive: `recordsFor` reads a version signal, so a Solid computation that
 * calls it re-runs when any chat's records change. Persisted to tab-scoped
 * storage under the same key the chat view always used, so a reload keeps a
 * call whose start row the persisted cursor is already past.
 */
import { createSignal } from "solid-js";

import {
  deserializeGroupCalls,
  NO_GROUP_CALLS,
  pruneGroupCalls,
  reduceGroupCalls,
  serializeGroupCalls,
  selectLiveGroupCall,
  type GroupCallEvent,
  type GroupCallRecord,
  type GroupCallRecords,
  type GroupCallViewer,
} from "./group-call-record";
import { registerAccountMemory } from "../auth/account-memory";

const records = new Map<string, GroupCallRecords>();
const [version, setVersion] = createSignal(0);

function key(accountId: string, groupIdB64: string): string {
  return `${accountId}|${groupIdB64}`;
}

function storageKey(accountId: string, groupIdB64: string): string {
  return `lakko:group-calls:${accountId}:${groupIdB64}`;
}

function load(accountId: string, groupIdB64: string): GroupCallRecords {
  const k = key(accountId, groupIdB64);
  const cached = records.get(k);
  if (cached !== undefined) return cached;
  let loaded: GroupCallRecords = NO_GROUP_CALLS;
  try {
    loaded = deserializeGroupCalls(
      globalThis.sessionStorage?.getItem(storageKey(accountId, groupIdB64)) ?? null,
    );
  } catch {
    loaded = NO_GROUP_CALLS;
  }
  records.set(k, loaded);
  return loaded;
}

/** The records for one chat. Reactive. */
export function groupCallsFor(accountId: string, groupIdB64: string): GroupCallRecords {
  version();
  return load(accountId, groupIdB64);
}

/** Fold one event into one chat's records and persist the result. */
export function applyGroupCallEventFor(
  accountId: string,
  groupIdB64: string,
  event: GroupCallEvent,
  nowMs: number,
): void {
  const current = load(accountId, groupIdB64);
  const next = pruneGroupCalls(reduceGroupCalls(current, event), nowMs);
  if (next === current) return;
  records.set(key(accountId, groupIdB64), next);
  try {
    globalThis.sessionStorage?.setItem(
      storageKey(accountId, groupIdB64),
      serializeGroupCalls(next),
    );
  } catch {
    // Storage full / blocked: the in-memory record still drives this tab.
  }
  setVersion(version() + 1);
}

/** Every chat of `accountId` that has a live group call at `nowMs`. Reactive
 *  over the chats already loaded into memory in this page. */
export function liveGroupCallsFor(
  accountId: string,
  groupIdB64s: readonly string[],
  nowMs: number,
  viewer?: GroupCallViewer,
): { readonly groupIdB64: string; readonly call: GroupCallRecord }[] {
  version();
  const out: { groupIdB64: string; call: GroupCallRecord }[] = [];
  for (const g of groupIdB64s) {
    const live = selectLiveGroupCall(load(accountId, g), nowMs, viewer);
    if (live !== null) out.push({ groupIdB64: g, call: live });
  }
  return out;
}

/** Sign-out: drop the in-memory copies (tab storage is cleared with the tab). */
export function clearGroupCallStore(): void {
  records.clear();
  setVersion(version() + 1);
}

/** Drop the in-memory records of `accountId` only. */
function dropMemoryFor(accountId: string): void {
  const memoryPrefix = `${accountId}|`;
  for (const k of [...records.keys()]) {
    if (k.startsWith(memoryPrefix)) records.delete(k);
  }
}

// A record folded in after the wipe step (a receive that was in flight) would
// be merged into the next save once the account signs in again on this page.
registerAccountMemory("group-calls", (ids) => {
  for (const id of ids) dropMemoryFor(id);
  setVersion(version() + 1);
});

/** Drop one chat's records, in memory and in this tab's session storage
 *  (ADR-0087 section 9). */
export function clearGroupCallsForGroup(accountId: string, groupIdB64: string): void {
  records.delete(key(accountId, groupIdB64));
  try {
    globalThis.sessionStorage?.removeItem(storageKey(accountId, groupIdB64));
  } catch {
    /* storage blocked: nothing was stored either */
  }
  setVersion(version() + 1);
}

/** Forget device / Delete account: drop the account's records, in memory and
 *  in this tab's session storage. Another account's are left alone. */
export function clearGroupCallsForAccount(accountId: string): void {
  dropMemoryFor(accountId);
  try {
    const store = globalThis.sessionStorage;
    if (store !== undefined) {
      const prefix = `lakko:group-calls:${accountId}:`;
      const victims: string[] = [];
      for (let i = 0; i < store.length; i += 1) {
        const k = store.key(i);
        if (k !== null && k.startsWith(prefix)) victims.push(k);
      }
      for (const k of victims) store.removeItem(k);
    }
  } catch {
    /* storage blocked: nothing was stored either */
  }
  setVersion(version() + 1);
}
