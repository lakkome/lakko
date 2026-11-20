// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 step 5, `docs/ux/reporting-and-moderation.md` section 3 - an
 * account Lakko's moderators hid from search.
 *
 * Discovery refuses publishing the Everyone record with `403
 * {"error":"hidden-from-search"}` while the account is hidden. Who can find
 * me then says so (`who_can_find_me.hidden_by_moderation`) and Everyone
 * cannot be chosen; the person's other choices stay as they are. A later
 * Everyone publish that succeeds leaves the state. Memory only, per account:
 * a fresh page learns it again from the next refused publish.
 */

import { createSignal } from "solid-js";

/** The code discovery answers a hidden account's Everyone publish. */
export const HIDDEN_FROM_SEARCH = "hidden-from-search";

/** Whether an error of the Everyone publish says the account is hidden from
 *  search. Pure: reads `status` and the `{"error"}` code the discovery
 *  client's error carries. */
export function isHiddenFromSearchError(err: unknown): boolean {
  if (typeof err !== "object" || err === null) return false;
  const e = err as { status?: unknown; errorCode?: unknown };
  return e.status === 403 && e.errorCode === HIDDEN_FROM_SEARCH;
}

const [hiddenAccount, setHiddenAccount] = createSignal<string | null>(null);

/** The Everyone publish for `accountIdB64` was refused as hidden. */
export function noteHiddenFromSearch(accountIdB64: string): void {
  setHiddenAccount(accountIdB64);
}

/** An Everyone publish for `accountIdB64` succeeded: no longer hidden. */
export function noteEveryonePublished(accountIdB64: string): void {
  if (hiddenAccount() === accountIdB64) setHiddenAccount(null);
}

/** Reactive: whether `accountIdB64` is held as hidden from search. */
export function isHiddenFromSearch(accountIdB64: string | null): boolean {
  return accountIdB64 !== null && hiddenAccount() === accountIdB64;
}

/** Sign-out: nothing held. */
export function clearHiddenFromSearch(): void {
  setHiddenAccount(null);
}
