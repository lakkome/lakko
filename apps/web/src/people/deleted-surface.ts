// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0098 / `docs/ux/deleted-account.md` section 3 — what a conversation
 * offers once the other person of a one-to-one deleted their account. The
 * history stays readable; nothing can be sent, called or reacted to, and
 * typing and read markers stop. A group is unaffected (its other members are
 * still there). Pure.
 */

export interface ConversationSurface {
  /** The composer; when `false` it is replaced by one line. */
  readonly composer: boolean;
  readonly callButtons: boolean;
  /** Reply in the message menu. */
  readonly reply: boolean;
  /** React in the message menu (the reaction row). */
  readonly react: boolean;
  /** Typing signals and read receipts are sent. */
  readonly ephemeral: boolean;
}

const OPEN: ConversationSurface = { composer: true, callButtons: true, reply: true, react: true, ephemeral: true };
const CLOSED: ConversationSurface = {
  composer: false,
  callButtons: false,
  reply: false,
  react: false,
  ephemeral: false,
};

export function deletedConversationSurface(isOneToOne: boolean, peerDeleted: boolean): ConversationSurface {
  return isOneToOne && peerDeleted ? CLOSED : OPEN;
}
