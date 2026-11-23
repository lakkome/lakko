// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * When the chat list and an open conversation may be drawn
 * (`docs/ux/navigation.md` section 2.4, maintainer decision 2026-11-23).
 *
 * Nothing of a conversation is drawn before what names and orders it has
 * been read from this device's stores. Drawn earlier, a one-to-one read
 * `Member` in the list and a key fingerprint in its header, every row stood
 * in creation order and then jumped, and an open chat with a long history
 * said `No messages yet` for two seconds (seen after a reload, 2026-11-23).
 *
 * Pure: the caller passes what each store reports, so a test can run it.
 */

/** What the stores report for the signed-in account. */
export interface StoresRead {
  /** The signed-in account, or `null` with no session. */
  readonly account: string | null;
  /** The account whose received profiles have been read. */
  readonly profilesLoadedFor: string | null;
  /** The account whose group names, photos and `@name`s have been read. */
  readonly groupNamesLoadedFor: string | null;
}

/** Whether the stores that name a conversation are read for this account. */
export function namesKnown(input: StoresRead): boolean {
  return (
    input.account !== null &&
    input.profilesLoadedFor === input.account &&
    input.groupNamesLoadedFor === input.account
  );
}

/**
 * Whether the merged conversation list may draw its rows, or its empty
 * state: the names are known, the message log was read (it orders the rows
 * and fills their second lines), and the subscribed channels were read
 * (their rows are ordered among the chats).
 */
export function chatListReady(
  input: StoresRead & { readonly logRead: boolean; readonly channelsRead: boolean },
): boolean {
  return namesKnown(input) && input.logRead && input.channelsRead;
}

/**
 * Whether an open conversation may draw itself: its title is known, and its
 * stored messages were read, so an empty timeline means nothing was said.
 */
export function conversationReady(input: StoresRead & { readonly logRead: boolean }): boolean {
  return namesKnown(input) && input.logRead;
}
