// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Pure rules for the Contacts list pane (docs/ux/client-redesign.md section
 * 5.3): the letter groups over the alphabetical contact list, the order of
 * incoming requests, the search match, and which sent requests are still
 * waiting for an answer. No Solid, no storage, no wasm; tested in
 * `contact-list-model.test.ts`.
 */
import { badgeText } from "../feed/feed-marks";


/** The group every label that does not start with a letter falls into. */
export const OTHER_LETTER = "#";

/** The section letter for a label: its first letter, upper-cased, in any
 *  script; a leading `@` is ignored; digits and punctuation go under `#`. */
export function letterOf(label: string): string {
  const first = label.replace(/^@/, "").trim().charAt(0);
  if (first.length === 0) return OTHER_LETTER;
  const upper = first.toLocaleUpperCase();
  return /\p{L}/u.test(upper) ? upper : OTHER_LETTER;
}

export interface LetterGroup<T> {
  readonly letter: string;
  readonly items: readonly T[];
}

function bareLabel(label: string): string {
  return label.replace(/^@/, "").trim();
}

/** Alphabetical order, case- and accent-insensitive, numbers in natural
 *  order; every `#` label sorts after every lettered one. */
export function compareContactLabels(a: string, b: string): number {
  const aOther = letterOf(a) === OTHER_LETTER;
  const bOther = letterOf(b) === OTHER_LETTER;
  if (aOther !== bOther) return aOther ? 1 : -1;
  return bareLabel(a).localeCompare(bareLabel(b), undefined, {
    sensitivity: "base",
    numeric: true,
  });
}

/** Sort the items by label and cut them into letter groups, `#` last. */
export function groupByLetter<T>(
  items: readonly T[],
  labelOf: (item: T) => string,
): LetterGroup<T>[] {
  const sorted = [...items].sort((a, b) => compareContactLabels(labelOf(a), labelOf(b)));
  const groups: { letter: string; items: T[] }[] = [];
  for (const item of sorted) {
    const letter = letterOf(labelOf(item));
    const last = groups[groups.length - 1];
    if (last !== undefined && last.letter === letter) {
      last.items.push(item);
    } else {
      groups.push({ letter, items: [item] });
    }
  }
  return groups;
}

/** A search query matches a contact by name or by username, ignoring case
 *  and a typed `@`. An empty query matches everything. */
export function matchesContactQuery(
  query: string,
  name: string,
  username: string | null,
): boolean {
  const q = query.trim().replace(/^@/, "").toLocaleLowerCase();
  if (q.length === 0) return true;
  if (name.toLocaleLowerCase().includes(q)) return true;
  return username !== null && username.replace(/^@/, "").toLocaleLowerCase().includes(q);
}

/** Incoming requests newest first. The delivery id is monotonic per
 *  mailbox, so it orders arrivals without parsing a timestamp. */
export function sortIncomingRequests<T extends { readonly messageId: number }>(
  requests: readonly T[],
): T[] {
  return [...requests].sort((a, b) => b.messageId - a.messageId);
}

/** A contact request this account sent and has not seen answered. */
export interface SentRequestRecord {
  /** The canonical username the request was addressed to; empty for a person
   *  found by name alone. */
  readonly recipientUsername: string;
  /** The name the person was found under. Absent in older records. */
  readonly recipientName?: string;
  /** The recipient's 16-byte account id as lower-case hex; the key the
   *  accepted list is matched on once their acceptance lands. */
  readonly recipientAccountIdHex: string;
  /** When this device sent it (ms since epoch). */
  readonly sentAtMs: number;
  /** ADR-0084 - the recipient's signature key (standard base64), for their
   *  photo once this device holds their profile. Absent in older records. */
  readonly recipientKeyB64?: string;
}

/** Add or refresh a sent record; one row per recipient, the newest send
 *  wins. */
export function upsertSentRequest(
  list: readonly SentRequestRecord[],
  record: SentRequestRecord,
): SentRequestRecord[] {
  return [
    ...list.filter((r) => r.recipientAccountIdHex !== record.recipientAccountIdHex),
    record,
  ];
}

/** The sent requests still waiting: those whose recipient is not yet in the
 *  accepted list, newest first. The list is matched on the account id and on
 *  the key: a row a Welcome made before the person's profile arrived names
 *  the key alone. */
export function waitingSentRequests(
  sent: readonly SentRequestRecord[],
  acceptedAccountIdHexes: ReadonlySet<string>,
  acceptedKeysB64: ReadonlySet<string> = new Set(),
): SentRequestRecord[] {
  return sent
    .filter(
      (r) =>
        !acceptedAccountIdHexes.has(r.recipientAccountIdHex) &&
        !(r.recipientKeyB64 !== undefined && acceptedKeysB64.has(r.recipientKeyB64)),
    )
    .sort((a, b) => b.sentAtMs - a.sentAtMs);
}

/** What a Sent row shows: the name as found, and `@username` under it. A
 *  person found by their username alone is named by it, once. */
export function sentRowLines(record: SentRequestRecord): {
  readonly title: string;
  readonly username: string | null;
} {
  const handle = record.recipientUsername.length > 0 ? `@${record.recipientUsername}` : null;
  const name = record.recipientName ?? handle ?? "";
  return { title: name, username: handle === name ? null : handle };
}

/** What the Contacts item of the rail shows for the requests waiting: the
 *  Chats badge's text, or nothing at zero. */
export function contactsBadge(waiting: number): string | null {
  return waiting > 0 ? badgeText(waiting) : null;
}
