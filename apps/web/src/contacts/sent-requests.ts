// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The contact requests this account has sent, per account, in
 * `localStorage`, so the Contacts pane can show them under "Sent" with a
 * waiting label (docs/ux/client-redesign.md section 5.3) until the
 * recipient's acceptance lands in the address book. Only the username and
 * the account id are kept; the intro text is not.
 *
 * The rules (dedup, which rows are still waiting) live in
 * `contact-list-model.ts`; this file is the storage.
 */
import {
  upsertSentRequest,
  type SentRequestRecord,
} from "./contact-list-model";

const KEY_PREFIX = "lakko.contacts.sent.v1:";

function keyFor(accountIdB64: string): string {
  return `${KEY_PREFIX}${accountIdB64}`;
}

function isRecord(value: unknown): value is SentRequestRecord {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Record<string, unknown>;
  return (
    typeof v["recipientUsername"] === "string" &&
    typeof v["recipientAccountIdHex"] === "string" &&
    typeof v["sentAtMs"] === "number"
  );
}

/** Every sent record for the account; an unreadable row reads as none. */
export function loadSentRequests(accountIdB64: string): SentRequestRecord[] {
  if (typeof localStorage === "undefined") return [];
  const raw = localStorage.getItem(keyFor(accountIdB64));
  if (raw === null) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter(isRecord) : [];
  } catch {
    return [];
  }
}

function save(accountIdB64: string, list: readonly SentRequestRecord[]): void {
  if (typeof localStorage === "undefined") return;
  if (list.length === 0) {
    localStorage.removeItem(keyFor(accountIdB64));
    return;
  }
  localStorage.setItem(keyFor(accountIdB64), JSON.stringify(list));
}

/** Remember a request just sent. */
export function recordSentRequest(
  accountIdB64: string,
  record: SentRequestRecord,
): SentRequestRecord[] {
  const next = upsertSentRequest(loadSentRequests(accountIdB64), record);
  save(accountIdB64, next);
  return next;
}

/** Drop a sent record (the user dismissed it from the list). */
export function forgetSentRequest(
  accountIdB64: string,
  recipientAccountIdHex: string,
): SentRequestRecord[] {
  const next = loadSentRequests(accountIdB64).filter(
    (r) => r.recipientAccountIdHex !== recipientAccountIdHex,
  );
  save(accountIdB64, next);
  return next;
}

/** Drop every sent record of the account (Forget device, Delete account). */
export function clearSentRequests(accountIdB64: string): void {
  if (typeof localStorage === "undefined") return;
  localStorage.removeItem(keyFor(accountIdB64));
}
