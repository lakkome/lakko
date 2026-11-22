// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §6 — the `Leave this group?` confirmation.
 *
 * ## Why the confirmation is the LAST exit
 *
 * §3: once published, a request is in every member's proposal store and dies
 * only when some commit lands. The core has no withdraw and MLS offers no
 * mechanism for one, so a `Cancel` afterwards could not be honoured and must
 * not be built.
 *
 * The copy is `docs/ux/group-and-channel-privacy.md` section 9.4, and lives
 * here rather than in `routes/Chat.tsx` so the one exit from an unwithdrawable
 * action has an assertion somewhere.
 */

import { type ConfirmRequest, namedActionKey } from "../ui/confirm-dialog";
import { t } from "../l10n";

/** §6 — the title. */
export const LEAVE_GROUP_TITLE = (): string => t("chat.leave.title");

/**
 * The body: what the person loses (amended 2026-11-22). That they stay in the
 * group until the leave lands is shown by the row's `Waiting to leave.` line
 * and the notice after Leave, not narrated here.
 */
export const LEAVE_GROUP_BODY = (): string => t("chat.leave.body");

/** §6 — the destructive action's verb. The group's name is appended by
 *  {@link namedAction}. */
export const LEAVE_GROUP_CONFIRM_LABEL = (): string => t("chat.leave.confirm");

/** §6 — the exit. */
export const LEAVE_GROUP_CANCEL_LABEL = (): string => t("common.cancel");

/**
 * The confirmation for leaving a group.
 *
 * Never absent: there is no code path on which a departure is published on a
 * single tap.
 */
export function leaveGroupConfirmation(input: {
  /** The chat's roster label, or `null` when it has none. */
  readonly groupLabel: string | null | undefined;
}): ConfirmRequest {
  return {
    title: LEAVE_GROUP_TITLE(),
    body: LEAVE_GROUP_BODY(),
    confirmLabel: namedActionKey("chat.leave.confirm", "chat.leave.confirm_named", input.groupLabel),
    cancelLabel: LEAVE_GROUP_CANCEL_LABEL(),
    tone: "danger",
  };
}
