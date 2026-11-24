// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where this device keeps the two per-group sets of section 8.1 of
 * `docs/ux/group-and-channel-privacy.md` (`join-request-notices.ts`): the
 * requests already announced and the requests not yet shown, by the joiner's
 * signature key. Local, never synced.
 *
 * One `localStorage` row per account, `{ groupIdB64: { a: [...], u: [...] } }`.
 * The key names the account, so the account store gate
 * (`auth/account-store-gate.ts`) refuses a late write after Forget device;
 * the wipe removes the row (`auth/wipe-account.ts`, step
 * `join-request-notices`) and an ended membership drops its group
 * (`chat/membership-wipe.ts`).
 *
 * Reads are reactive (a Solid signal bumped on every write in this tab and on
 * a `storage` event from another tab), so the chat-list row follows the
 * unshown count.
 */
import { createSignal } from "solid-js";

import { EMPTY_JOIN_NOTICES, sameJoinNotices, type JoinNoticeSets } from "./join-request-notices";

const PREFIX = "lakko.chat.join-request-notices.v1.";

/** A joiner key, base64 of 32 bytes. */
const KEY_RE = /^[A-Za-z0-9+/]{43}=$/;

/** At most this many keys per set: the members panel stops at 64. */
const MAX_KEYS = 128;

const [version, setVersion] = createSignal(0);

if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => {
    if (e.key === null || e.key.startsWith(PREFIX)) setVersion((v) => v + 1);
  });
}

/** The row's key for an account. */
export function joinRequestNoticesKey(accountIdB64: string): string {
  return `${PREFIX}${accountIdB64}`;
}

function storage(): Storage | undefined {
  try {
    return globalThis.localStorage ?? undefined;
  } catch {
    return undefined;
  }
}

type Row = Record<string, { readonly a: readonly string[]; readonly u: readonly string[] }>;

function keys(v: unknown): string[] {
  if (!Array.isArray(v)) return [];
  const out: string[] = [];
  for (const k of v) {
    if (typeof k === "string" && KEY_RE.test(k) && !out.includes(k)) out.push(k);
    if (out.length >= MAX_KEYS) break;
  }
  return out;
}

function readRow(accountIdB64: string, store: Storage | undefined): Row {
  try {
    const raw = store?.getItem(joinRequestNoticesKey(accountIdB64));
    if (raw === null || raw === undefined) return {};
    const parsed = JSON.parse(raw) as unknown;
    if (typeof parsed !== "object" || parsed === null) return {};
    const out: Row = {};
    for (const [g, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof v !== "object" || v === null) continue;
      const entry = v as { a?: unknown; u?: unknown };
      out[g] = { a: keys(entry.a), u: keys(entry.u) };
    }
    return out;
  } catch {
    return {};
  }
}

function writeRow(accountIdB64: string, row: Row, store: Storage | undefined): void {
  try {
    if (Object.keys(row).length === 0) store?.removeItem(joinRequestNoticesKey(accountIdB64));
    else store?.setItem(joinRequestNoticesKey(accountIdB64), JSON.stringify(row));
  } catch {
    // Storage blocked or the account sealed: nothing is kept, so a request
    // may be announced again later. Never a wrong state.
  }
  setVersion((v) => v + 1);
}

/** One group's sets; empty when none is kept. Reactive. */
export function readJoinNotices(
  accountIdB64: string,
  groupIdB64: string,
  store: Storage | undefined = storage(),
): JoinNoticeSets {
  version();
  const entry = readRow(accountIdB64, store)[groupIdB64];
  return entry === undefined ? EMPTY_JOIN_NOTICES : { announced: entry.a, unshown: entry.u };
}

/** How many requests of the group this device has not shown yet. Reactive. */
export function unshownJoinRequestCount(
  accountIdB64: string | null,
  groupIdB64: string,
  store: Storage | undefined = storage(),
): number {
  if (accountIdB64 === null) return 0;
  return readJoinNotices(accountIdB64, groupIdB64, store).unshown.length;
}

/** Replace one group's sets by `next(current)`. Writes only on a change. */
export function updateJoinNotices(
  accountIdB64: string,
  groupIdB64: string,
  next: (current: JoinNoticeSets) => JoinNoticeSets,
  store: Storage | undefined = storage(),
): JoinNoticeSets {
  const row = readRow(accountIdB64, store);
  const entry = row[groupIdB64];
  const current = entry === undefined ? EMPTY_JOIN_NOTICES : { announced: entry.a, unshown: entry.u };
  const updated = next(current);
  if (sameJoinNotices(current, updated)) return current;
  const out: Row = { ...row };
  if (updated.announced.length === 0 && updated.unshown.length === 0) delete out[groupIdB64];
  else out[groupIdB64] = { a: updated.announced.slice(-MAX_KEYS), u: updated.unshown.slice(-MAX_KEYS) };
  writeRow(accountIdB64, out, store);
  return updated;
}

/** Drop one group's sets (its membership ended). */
export function clearJoinNoticesForGroup(
  accountIdB64: string,
  groupIdB64: string,
  store: Storage | undefined = storage(),
): void {
  const row = readRow(accountIdB64, store);
  if (row[groupIdB64] === undefined) return;
  const out: Row = { ...row };
  delete out[groupIdB64];
  writeRow(accountIdB64, out, store);
}

/** Remove the account's row. Called by the Forget-device wipe. */
export function clearJoinNoticesForAccount(accountIdB64: string, store: Storage | undefined = storage()): void {
  try {
    store?.removeItem(joinRequestNoticesKey(accountIdB64));
  } catch {
    // best-effort
  }
  setVersion((v) => v + 1);
}
