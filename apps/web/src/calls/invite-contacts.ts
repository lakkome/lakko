// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The list `Invite contacts` shows on the call screen
 * (`docs/ux/call-links.md` section 3).
 *
 * The person's CONTACTS, not their conversations: an invitation is addressed
 * to a person, and a contact the person has never written to is as invitable
 * as one they talk to daily. (The first version listed conversations, so a
 * person with contacts and no chats saw no way to invite anyone; corrected
 * 2026-10-02.) Groups are not in it.
 *
 * Pure: names come in through `nameOf` / `usernameOf`.
 */

import { compareContactLabels, matchesContactQuery } from "../contacts/contact-list-model";

/** One row of the list. */
export interface InviteContactRow {
  /** The contact's identity key, base64: who the invitation goes to. */
  readonly keyB64: string;
  readonly name: string;
  /** `@username`, when one is known for the contact. */
  readonly username: string | null;
}

/** The rows, by name, the way the Contacts list orders people. */
export function inviteContactRows<C extends { readonly signaturePubkeyB64: string }>(
  contacts: readonly C[],
  nameOf: (contact: C) => string,
  usernameOf: (contact: C) => string | null,
): InviteContactRow[] {
  const seen = new Set<string>();
  const rows: InviteContactRow[] = [];
  for (const c of contacts) {
    if (seen.has(c.signaturePubkeyB64)) continue;
    seen.add(c.signaturePubkeyB64);
    rows.push({ keyB64: c.signaturePubkeyB64, name: nameOf(c), username: usernameOf(c) });
  }
  return rows.sort((a, b) => compareContactLabels(a.name, b.name));
}

/** The rows a typed query keeps: by name or by username. */
export function filterInviteContactRows(
  rows: readonly InviteContactRow[],
  query: string,
): InviteContactRow[] {
  return rows.filter((r) => matchesContactQuery(query, r.name, r.username));
}
