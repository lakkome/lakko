// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which feed the Feed screen opens on (`docs/ux/feed.md` section 4): a
 * preset key, `discover`, or `custom:<id>` for one of the person's saved
 * feeds.
 *
 * A per-DEVICE convenience, never synced, kept in `localStorage` per account
 * like the open folder tab (`chat/folders/selected-tab.ts`): a saved feed's id
 * is the account's, so the row goes with Forget device and Delete account
 * (`auth/wipe-account.ts`). Nothing in it needs encrypting.
 */

const KEY_PREFIX = "lakko.feed.selection.";

/** The stored selection for `accountId`, or `null`. */
export function readFeedSelection(accountId: string | null): string | null {
  if (accountId === null) return null;
  try {
    return globalThis.localStorage?.getItem(KEY_PREFIX + accountId) ?? null;
  } catch {
    return null;
  }
}

/** Store the selection for `accountId`. A convenience only: a refusing
 *  browser keeps the choice for this page. */
export function writeFeedSelection(accountId: string | null, value: string): void {
  if (accountId === null) return;
  try {
    globalThis.localStorage?.setItem(KEY_PREFIX + accountId, value);
  } catch {
    // The feed works without it.
  }
}

/** Forget the stored selection of `accountId` (Forget device, Delete
 *  account). */
export function clearFeedSelection(accountId: string): void {
  try {
    globalThis.localStorage?.removeItem(KEY_PREFIX + accountId);
  } catch {
    // Storage blocked: nothing was stored either.
  }
}
