// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether the conversation list shows its empty state
 * (`docs/ux/navigation.md` section 4, "Also with an empty list").
 *
 * The search field is there before the first conversation exists, because
 * finding the first person to talk to is what a new account needs it for. So
 * the empty state stands only while there is nothing to list AND nothing typed;
 * once something is typed it gives way to what the search finds, the people
 * half included (`docs/ux/who-can-find-me.md` section 6.1).
 */
export function showsEmptyState(conversationCount: number, query: string): boolean {
  return conversationCount === 0 && query.trim().length === 0;
}
