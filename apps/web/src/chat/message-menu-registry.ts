// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * How something outside a bubble opens that bubble's message menu: the
 * article reader's `…` button (UX attachments 11.3). Each bubble on screen
 * registers its opener under (conversation, message id); a caller asks by
 * the same pair. No event is synthesised and no element is looked up.
 */

const openers = new Map<string, () => void>();

function keyOf(conversationId: string, messageId: number): string {
  return `${conversationId}|${messageId}`;
}

/** Register a bubble's menu opener; returns the unregister (which removes
 *  only this registration, so a re-mounted bubble keeps its own). */
export function registerMessageMenu(conversationId: string, messageId: number, open: () => void): () => void {
  const key = keyOf(conversationId, messageId);
  openers.set(key, open);
  return () => {
    if (openers.get(key) === open) openers.delete(key);
  };
}

/** Open the menu of a message on screen; `false` when it is not mounted. */
export function openMessageMenu(conversationId: string, messageId: number): boolean {
  const open = openers.get(keyOf(conversationId, messageId));
  if (open === undefined) return false;
  open();
  return true;
}
