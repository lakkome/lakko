// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Pure rules for the Contacts list pane (docs/ux/client-redesign.md section
 * 5.3): the letter groups over the alphabetical contact list, the order of
 * incoming requests, the search match, and which sent requests are still
 * waiting for an answer. No Solid, no storage, no wasm; tested in
 * `contact-list-model.test.ts`.
 */

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
  /** The canonical username the request was addressed to. */
  readonly recipientUsername: string;
  /** The recipient's 16-byte account id as lower-case hex; the key the
   *  accepted list is matched on once their acceptance lands. */
  readonly recipientAccountIdHex: string;
  /** When this device sent it (ms since epoch). */
  readonly sentAtMs: number;
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
 *  accepted list, newest first. */
export function waitingSentRequests(
  sent: readonly SentRequestRecord[],
  acceptedAccountIdHexes: ReadonlySet<string>,
): SentRequestRecord[] {
  return sent
    .filter((r) => !acceptedAccountIdHexes.has(r.recipientAccountIdHex))
    .sort((a, b) => b.sentAtMs - a.sentAtMs);
}
