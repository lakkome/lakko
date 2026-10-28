// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0045 UX §6 — the `Remove this member?` confirmation.
 *
 * ## What this is for
 *
 * §6 requires the confirmation on all three platforms and it shipped on one.
 * The asymmetry was sharp: **leaving** a group confirms everywhere, while
 * **removing somebody else** — the more consequential act, and the one whose
 * target cannot consent — confirmed on web alone.
 *
 * ## Why a confirmation at all, and why it is the LAST exit
 *
 * §3: once published, a request is in every member's proposal store and dies
 * only when some commit lands. The core has no withdraw and MLS offers no
 * mechanism for one, so a `Cancel` afterwards could not be honoured and must
 * not be built. **The confirmation is therefore the only exit there is**,
 * which is what makes its absence on two clients of three a real defect
 * rather than a polish item.
 *
 * That argument does not weaken on the direct-commit path — it *sharpens*.
 * There is no withdraw there either, and the removal lands at once rather
 * than at the next sweep. So the confirmation is required on both paths, and
 * only the body line differs.
 *
 * ## Why this is a plain module and not part of `routes/Chat.tsx`
 *
 * `vitest.config.ts` collects `src/**\/*.test.ts` in a plain `node`
 * environment with no jsdom and no Solid JSX transform, so a `.tsx` component
 * cannot be tested at all. The rule therefore lives here, where it is
 * executable, and `Chat.tsx` renders what it decides — the same split
 * `membership-ended.ts` / `ownership-transfer.ts` / `pending-requests.ts`
 * already use, and the split the iOS and Android mirrors take too
 * (`RemoveMemberConfirm.swift`, `RemoveMemberConfirm.kt`), so the copy has one
 * definition per platform and three assertions rather than three literals.
 *
 * ## Which rules here actually discriminate (MEASURED — each probe was run)
 *
 * Each was introduced as a deliberate regression, `vitest` was run, the
 * failure below observed, and the probe reverted.
 *
 * 1. **The body is returned on BOTH paths** — i.e. a direct-commit group is
 *    told the removal is deferred when it is immediate:
 *
 *        AssertionError: expected 'They will keep receiving messages until an…' to be null
 *
 * 2. **The direct path keeps the client's old `(leaf #N)` title**:
 *
 *        AssertionError: expected 'Remove this member (leaf #3)? Remove Cancel' not to contain 'leaf'
 *        AssertionError: expected 'Remove this member (leaf #3)?' to be 'Remove this member?'
 *
 *    The second is the one that reads as a copy nit and is not: the two paths
 *    asking two different questions is what let the direct path keep an
 *    internal identifier in user-facing copy for as long as it did.
 */

import { t } from "../l10n";

/**
 * §6 — the title. Identical on both paths: it is true of a removal however it
 * is applied, and a user should not be asked a differently-worded question
 * because of a group's internal role-table state.
 */
export const REMOVE_MEMBER_TITLE = (): string => t("chat.members.remove.title");

/**
 * §6 — the body, on the proposal path only.
 *
 * "That body line is not a nicety. It is the one fact a user removing someone
 * for a safety reason must have, and it is the fact the old immediate-looking
 * UI concealed." It is also the honesty rule of §3(a) applied to a
 * confirmation rather than to a roster: the member is still cryptographically
 * in the group — still receiving, still able to decrypt — until the commit
 * lands, and the UI must not imply otherwise.
 */
export const REMOVE_MEMBER_BODY = (): string => t("chat.members.remove.body");

/** §6 — the destructive action. */
export const REMOVE_MEMBER_CONFIRM_LABEL = (): string => t("common.remove");

/** §6 — the exit. */
export const REMOVE_MEMBER_CANCEL_LABEL = (): string => t("common.cancel");

/** The copy one confirmation needs, whichever widget presents it. */
export interface RemoveMemberConfirmation {
  /** {@link REMOVE_MEMBER_TITLE}. */
  readonly title: string;
  /** {@link REMOVE_MEMBER_BODY}, or `null` when there is nothing true to add. */
  readonly body: string | null;
  /** {@link REMOVE_MEMBER_CONFIRM_LABEL} — destructive. */
  readonly confirmLabel: string;
  /** {@link REMOVE_MEMBER_CANCEL_LABEL}. */
  readonly cancelLabel: string;
}

/**
 * The confirmation for removing another member.
 *
 * **It is never absent**, and that is the whole decision: this returns a
 * confirmation rather than an optional one, so there is no code path on which
 * a removal happens on a single tap.
 *
 * **The body is dropped on the direct path rather than reworded.** A group
 * with no role table used to remove by direct commit, which takes effect the
 * moment it lands — so §6's sentence about the member still receiving
 * messages would be false there. Since the ADR-0081 amendment of 2026-10-19
 * that path is deleted (a removal in such a group is refused and no client
 * reaches one), so the `usesProposalPath: false` arm is kept only as the
 * fail-safe shape: it says less rather than something false.
 */
export function removeMemberConfirmation(input: {
  readonly usesProposalPath: boolean;
}): RemoveMemberConfirmation {
  return {
    title: REMOVE_MEMBER_TITLE(),
    body: input.usesProposalPath ? REMOVE_MEMBER_BODY() : null,
    confirmLabel: REMOVE_MEMBER_CONFIRM_LABEL(),
    cancelLabel: REMOVE_MEMBER_CANCEL_LABEL(),
  };
}
