// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The plaintext `(accountId, username)` marker a page load restores from.
 * One device-wide row naming the last account signed in on this browser: Log
 * out keeps it (the next login is cheap), Forget device and Delete account
 * drop it when it names the account being wiped (`wipe-account.ts`).
 */

import { accountStoresWritable } from "./account-store-gate";

const STORAGE_KEY = "lakko.auth.v1";

/** The stored marker. */
export interface PersistedSession {
  /** Schema marker — bump if the storage shape changes so old
   *  rows are recognised and dropped instead of mis-parsed. */
  v: 1;
  accountIdB64: string;
  username: string;
}

/** The marker, or `null` when absent or unreadable. */
export function loadPersistedAccount(): PersistedSession | null {
  // SSR-safety: AuthProvider runs in the browser, but a defensive
  // check costs us nothing.
  if (typeof localStorage === "undefined") return null;
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw === null) return null;
  try {
    const parsed = JSON.parse(raw) as Partial<PersistedSession>;
    if (
      parsed.v === 1 &&
      typeof parsed.accountIdB64 === "string" &&
      typeof parsed.username === "string"
    ) {
      return {
        v: 1,
        accountIdB64: parsed.accountIdB64,
        username: parsed.username,
      };
    }
    return null;
  } catch {
    return null;
  }
}

/** Write the marker. */
export function savePersistedAccount(record: PersistedSession): void {
  if (typeof localStorage === "undefined") return;
  // The marker's key names no account, so the storage gate cannot see whose
  // it is: ask for the account in the record (`account-store-gate.ts`).
  if (!accountStoresWritable(record.accountIdB64)) return;
  localStorage.setItem(STORAGE_KEY, JSON.stringify(record));
}

/** Drop the marker when it names `accountIdB64` (or cannot be read at all);
 *  a marker naming another account is that account's. */
export function clearPersistedAccountFor(accountIdB64: string): void {
  if (typeof localStorage === "undefined") return;
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw === null) return;
  const current = loadPersistedAccount();
  if (current === null || current.accountIdB64 === accountIdB64) {
    localStorage.removeItem(STORAGE_KEY);
  }
}
