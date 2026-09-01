// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §6.2.4 — the `Delete this chat?` confirmation.
 *
 * ## Why this module is thin on purpose
 *
 * The title and body already live in `membership-ended.ts` and are already
 * pinned by its tests. Restating them here would give them a second home to
 * drift in, which is the failure mode that split exists to prevent — so this
 * re-exports them through the request rather than re-declaring them.
 *
 * What it adds is the two things §6.2.4 leaves to the widget and
 * `window.confirm` could not express at all: the destructive action's LABEL,
 * which names the chat, and the tone that colours it.
 *
 * ## Why this one matters more than the rest
 *
 * It is the ONE control in the whole design that destroys user data on
 * request. Everything else in §6.2 removes a claim the UI should not have
 * been making; this removes a transcript that exists nowhere else, on one
 * confirmation, with no undo and no export. §6.2.4: *"the confirmation says
 * so in as many words, and that is the only protection there is."*
 */

import { type ConfirmRequest, namedAction } from "../ui/confirm-dialog";
import { DELETE_CHAT_BODY, DELETE_CHAT_TITLE } from "./membership-ended";

/** §6.2.4 — the destructive action's verb. The chat's name is appended by
 *  {@link namedAction}. */
export const DELETE_CHAT_CONFIRM_LABEL = "Delete";

/** §6.2.4 — the exit. */
export const DELETE_CHAT_CANCEL_LABEL = "Cancel";

/**
 * The confirmation for deleting a chat whose membership has ended.
 *
 * Reachable only from the exit block's `ended` arm, so it never removes a
 * live conversation.
 */
export function deleteChatConfirmation(input: {
  /** The chat's roster label, or `null` when it has none. */
  readonly chatLabel: string | null | undefined;
}): ConfirmRequest {
  return {
    title: DELETE_CHAT_TITLE,
    body: DELETE_CHAT_BODY,
    confirmLabel: namedAction(DELETE_CHAT_CONFIRM_LABEL, input.chatLabel),
    cancelLabel: DELETE_CHAT_CANCEL_LABEL,
    tone: "danger",
  };
}
