// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The DS message ids this PAGE published into each group, shared by every
 * `ChatClient` instance.
 *
 * OpenMLS cannot decrypt a device's own application message
 * (`CannotDecryptOwnMessage`), so a poll must drop its own echoes. Each
 * `ChatClient` has always tracked the ids IT published. ADR-0048 adds a
 * publisher that is not the chat route's client: a profile is published into a
 * group right after a Welcome is opened by the mailbox loop, before any chat
 * route exists. Without a shared registry the chat route's first poll would
 * hand that row to OpenMLS and log a refusal for it.
 *
 * Keyed by group, because a DS message id is only unique within one group's
 * journal. In memory only — after a reload the echo is logged and skipped as
 * it always was.
 *
 * ## This is the ONLY registry
 *
 * `ChatClient` used to additionally keep a group-BLIND `Set<number>` of its own,
 * checked first with an `||`, which is a message-loss hazard rather than a
 * redundancy: one `ChatClient` serves every group (the background receive loop
 * polls them all through a single instance), so "row N is my own echo" became
 * true for every journal at once. A sweep commit at row N in chat A then made
 * chat B's row N vanish — dropped inside the batch, after the cursor had
 * already been moved to it, so no later poll ever fetches it again. Worse in
 * both directions: B's poll then CONSUMED the entry, so A's own echo later
 * reached OpenMLS and was refused as `CannotDecryptOwnMessage` with a phantom
 * system row. Every mark and every lookup therefore goes through here, and
 * every one of them names its group.
 */

const published = new Map<string, Set<number>>();

/** Record that this page published `messageId` into `groupIdB64`. */
export function markOwnPublished(groupIdB64: string, messageId: number): void {
  let ids = published.get(groupIdB64);
  if (ids === undefined) {
    ids = new Set();
    published.set(groupIdB64, ids);
  }
  ids.add(messageId);
}

/** Whether this page published `messageId` into `groupIdB64`. */
export function isOwnPublished(groupIdB64: string, messageId: number): boolean {
  return published.get(groupIdB64)?.has(messageId) === true;
}

/** Forget every id of a group gone from this device (ADR-0087 section 9). */
export function forgetOwnPublished(groupIdB64: string): void {
  published.delete(groupIdB64);
}

/** Forget an id once the cursor has moved past it. */
export function consumeOwnPublished(groupIdB64: string, messageId: number): void {
  const ids = published.get(groupIdB64);
  if (ids === undefined) return;
  ids.delete(messageId);
  if (ids.size === 0) published.delete(groupIdB64);
}
