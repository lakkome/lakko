// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import type { ShareableContact } from "./types";

/** Contacts matching `query` by name or username, case-insensitive. */
export function filterContacts(contacts: readonly ShareableContact[], query: string): ShareableContact[] {
  const q = query.trim().toLocaleLowerCase();
  if (q.length === 0) return [...contacts];
  const bare = q.startsWith("@") ? q.slice(1) : q;
  return contacts.filter(
    (c) => c.name.toLocaleLowerCase().includes(q) || (c.username !== undefined && c.username.includes(bare)),
  );
}
