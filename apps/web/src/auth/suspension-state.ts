// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether this device is in suspended mode (ADR-0098 step 3, UX
 * `reporting-and-moderation.md` section 4), readable without the auth
 * context.
 *
 * The composers, the call buttons and the add-contact entry points read it to
 * offer nothing that would send, and the Delivery Service client reads it to
 * send nothing at all (the DS does not know accounts, so the client honours
 * the suspension itself). `AuthContext` is the only writer: it sets the flag
 * together with its suspended-mode signal.
 */

import { createSignal, type Accessor } from "solid-js";

const [suspended, setSuspended] = createSignal(false);

/** Reactive: `true` while this device is in suspended mode. */
export const accountSuspended: Accessor<boolean> = suspended;

/** Set by `AuthContext` with every change of suspended mode. */
export function setAccountSuspendedFlag(value: boolean): void {
  setSuspended(value);
}

/**
 * The writes a suspended account still makes to the channels service and to
 * storage (ADR-0098 step 3), as `METHOD path`: buying a file-read pass (files
 * in chats still open) and deleting the backup (with the account). Every
 * other request but GET and HEAD is refused.
 */
export const SUSPENDED_WRITES_ALLOWED: readonly string[] = [
  "POST /api/storage/blob/pass/read",
  "DELETE /api/storage/backup",
];

/** Whether a request may leave while the account is suspended. Pure. */
export function writeAllowedWhileSuspended(method: string, url: string): boolean {
  const m = method.toUpperCase();
  if (m === "GET" || m === "HEAD") return true;
  let path: string;
  try {
    path = new URL(url, "http://local.invalid").pathname;
  } catch {
    return false;
  }
  return SUSPENDED_WRITES_ALLOWED.includes(`${m} ${path}`);
}

/**
 * `fetch` for the services a suspended account must not write to (the
 * channels service, storage). While suspended a request that
 * {@link writeAllowedWhileSuspended} does not allow is refused before it
 * leaves, the way the network refuses one: a `TypeError`, which every caller
 * already treats as "try again later".
 */
export function writeGuardedFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const method = init?.method ?? (input instanceof Request ? input.method : "GET");
  const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
  if (suspended() && !writeAllowedWhileSuspended(method, url)) {
    return Promise.reject(new TypeError("the account is suspended"));
  }
  return globalThis.fetch(input, init);
}
