// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which folder tab is open (`docs/ux/chat-folders.md` 2.5).
 *
 * A per-DEVICE fact, never synced: a phone left on "Family" must not flip the
 * laptop to "Family". Kept in `localStorage` per account - it is a random
 * folder id and nothing else, so there is nothing in it to encrypt. A folder
 * deleted since (on any device) falls back to All chats.
 */
import { createSignal } from "solid-js";

/** The fixed first tab. */
export const ALL_CHATS = "all";

const KEY_PREFIX = "lakko.folderTab.";

const [selected, setSelected] = createSignal<string>(ALL_CHATS);
let loadedFor: string | null = null;

/** The tab to show: the stored one if that folder still exists, else All
 *  chats. Pure. */
export function resolveTab(stored: string | null, folderIds: readonly string[]): string {
  if (stored === null || stored === ALL_CHATS) return ALL_CHATS;
  return folderIds.includes(stored) ? stored : ALL_CHATS;
}

function read(accountId: string): string | null {
  try {
    return globalThis.localStorage?.getItem(KEY_PREFIX + accountId) ?? null;
  } catch {
    return null;
  }
}

/** Load the stored tab for `accountId` (call from an effect when the
 *  account is known; a repeat for the same account is a no-op). */
export function loadSelectedTab(accountId: string | null): void {
  if (accountId === null || loadedFor === accountId) return;
  loadedFor = accountId;
  setSelected(read(accountId) ?? ALL_CHATS);
}

/** The selected tab, as a signal read (not yet checked against the live
 *  folders - see {@link resolveTab}). */
export function selectedTab(): string {
  return selected();
}

/** Select a tab on this device. */
export function selectTab(accountId: string | null, tab: string): void {
  setSelected(tab);
  if (accountId === null) return;
  loadedFor = accountId;
  try {
    globalThis.localStorage?.setItem(KEY_PREFIX + accountId, tab);
  } catch {
    // A browser that refuses storage keeps the tab for this page only.
  }
}
