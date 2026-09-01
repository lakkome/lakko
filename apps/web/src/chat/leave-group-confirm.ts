// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §6 — the `Leave this group?` confirmation.
 *
 * ## What is new here, and what is not
 *
 * The copy is §6's, verbatim, and was already correct in the app. What was
 * missing is that it lived as a string literal inside `routes/Chat.tsx`'s
 * `onLeaveGroup`, which `vitest.config.ts` cannot load at all (plain `node`,
 * no jsdom, no Solid JSX transform) — so the one exit from an unwithdrawable
 * action had no assertion anywhere. Moving it here is the same split
 * `remove-member-confirm.ts` already uses, for the same reason.
 *
 * ## Why the confirmation is the LAST exit
 *
 * §3: once published, a request is in every member's proposal store and dies
 * only when some commit lands. The core has no withdraw and MLS offers no
 * mechanism for one, so a `Cancel` afterwards could not be honoured and must
 * not be built. As with removing somebody else, that argument does not weaken
 * on the direct-commit path — it sharpens: there the departure lands at once.
 *
 * ## One thing deliberately NOT done here
 *
 * `remove-member-confirm.ts` drops its body on the direct-commit path,
 * because "they will keep receiving messages until an owner or an admin
 * applies the change" is false when the commit lands immediately. The same
 * reasoning would apply to this body's FIRST sentence — and §6 nevertheless
 * writes one unconditional body for leaving, which is what the app has always
 * shown. Splitting it would be inventing copy this design never wrote, in a
 * flow whose second sentence (`You will need a new invite to rejoin.`) is
 * true on both paths. Left as specified, and flagged rather than silently
 * diverged from.
 */

import { type ConfirmRequest, namedAction } from "../ui/confirm-dialog";

/** §6 — the title. */
export const LEAVE_GROUP_TITLE = "Leave this group?";

/**
 * §6 — the body.
 *
 * The first sentence is §3's honesty rule applied to your own departure: you
 * are still cryptographically in the group — still receiving, still able to
 * decrypt — until the commit lands, and the UI must not imply otherwise. The
 * second is the fact that decides whether to press the button at all.
 */
export const LEAVE_GROUP_BODY =
  "You will keep receiving messages until an owner or an admin applies " +
  "your request. You will need a new invite to rejoin.";

/** §6 — the destructive action's verb. The group's name is appended by
 *  {@link namedAction}. */
export const LEAVE_GROUP_CONFIRM_LABEL = "Leave";

/** §6 — the exit. */
export const LEAVE_GROUP_CANCEL_LABEL = "Cancel";

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
    title: LEAVE_GROUP_TITLE,
    body: LEAVE_GROUP_BODY,
    confirmLabel: namedAction(LEAVE_GROUP_CONFIRM_LABEL, input.groupLabel),
    cancelLabel: LEAVE_GROUP_CANCEL_LABEL,
    tone: "danger",
  };
}
