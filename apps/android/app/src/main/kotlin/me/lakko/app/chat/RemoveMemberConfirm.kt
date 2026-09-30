// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.UiText

/**
 * ADR-0045 UX §6 — the `Remove this member?` confirmation.
 *
 * Android mirror of `apps/web/src/chat/remove-member-confirm.ts` and
 * `apps/ios/Lakko/Chat/RemoveMemberConfirm.swift`.
 *
 * ## What this is for
 *
 * §6 requires the confirmation on all three platforms and it shipped on one.
 * The asymmetry was sharp: **leaving** a group confirmed everywhere
 * ([LeaveGroupDialog]), while **removing somebody else** — the more
 * consequential act, and the one whose target cannot consent — went through on
 * a single tap here and on iOS.
 *
 * ## Why a confirmation at all, and why it is the LAST exit
 *
 * §3: once published, a request is in every member's proposal store and dies
 * only when some commit lands. The core has no withdraw and MLS offers no
 * mechanism for one, so a `Cancel` afterwards could not be honoured and must
 * not be built — which is why [PendingRequests] deliberately exposes no
 * withdraw verb. **The confirmation is therefore the only exit there is**, and
 * that is what makes its absence a defect rather than a polish item.
 *
 * The argument does not weaken on the direct-commit path — it *sharpens*.
 * There is no withdraw there either, and the removal lands at once rather than
 * at the next sweep. So the confirmation is required on both paths, and only
 * the body line differs.
 *
 * ## Why a plain module and not part of the Compose screen
 *
 * The host unit suite cannot render a composable, so the rule lives here where
 * it is executable and `MembersSheet` renders what it decides — the same split
 * `MembershipEnded.kt`, `OwnershipTransfer.kt` and `PendingRequests.kt`
 * already use.
 *
 * ## Which rules here actually discriminate (MEASURED — each probe was run)
 *
 * Each was introduced as a deliberate regression, `:app:testDebugUnitTest` was
 * run, the output below observed, and the probe reverted.
 *
 * 1. **The body is returned on BOTH paths** — a direct-commit group is told the
 *    removal is deferred when it is immediate:
 *
 *        RemoveMemberConfirmTest > it says NOTHING on the direct path, where
 *          §6's body would be false FAILED
 *          java.lang.AssertionError: expected null, but was:<They will keep
 *          receiving messages until an owner or an admin applies the change.>
 *
 * 2. **The direct path keeps a leaf index in the title:**
 *
 *        RemoveMemberConfirmTest > it is required on BOTH paths, because
 *          neither can be withdrawn FAILED
 *          org.junit.ComparisonFailure: expected:<Remove this member[]?> but
 *          was:<Remove this member[ (leaf #3)]?>
 *        RemoveMemberConfirmTest > it uses no protocol vocabulary FAILED
 *          java.lang.AssertionError: `leaf` leaked into remove this member
 *          (leaf #3)?  remove cancel expected:<false> but was:<true>
 *
 *    The first is the one that reads as a copy nit and is not: the two paths
 *    asking two differently-worded questions is what let the direct path keep
 *    an internal identifier in user-facing copy for as long as it did.
 */

/**
 * §6 — the title. Identical on both paths: it is true of a removal however it
 * is applied, and a user should not be asked a differently-worded question
 * because of a group's internal role-table state.
 */
internal val REMOVE_MEMBER_TITLE: UiText = UiText("chat.members.remove.title")

/**
 * §6 — the body, on the proposal path only.
 *
 * "That body line is not a nicety. It is the one fact a user removing someone
 * for a safety reason must have, and it is the fact the old immediate-looking
 * UI concealed." It is §3(a)'s honesty rule applied to a confirmation rather
 * than to a roster: the member is still cryptographically in the group — still
 * receiving, still able to decrypt — until the commit lands.
 */
internal val REMOVE_MEMBER_BODY: UiText = UiText("chat.members.remove.body")

/** §6 — the destructive action. */
internal val REMOVE_MEMBER_CONFIRM_LABEL: UiText = UiText("common.remove")

/** §6 — the exit. */
internal val REMOVE_MEMBER_CANCEL_LABEL: UiText = UiText("common.cancel")

/** The copy one confirmation needs, whichever widget presents it. */
internal data class RemoveMemberConfirmation(
    /** [REMOVE_MEMBER_TITLE]. */
    val title: UiText,
    /** [REMOVE_MEMBER_BODY], or `null` when there is nothing true to add. */
    val body: UiText?,
    /** [REMOVE_MEMBER_CONFIRM_LABEL] — rendered with the error colour. */
    val confirmLabel: UiText,
    /** [REMOVE_MEMBER_CANCEL_LABEL]. */
    val cancelLabel: UiText,
)

/**
 * The confirmation for removing another member.
 *
 * **It is never absent**, and that is the whole decision: this returns a
 * confirmation rather than a nullable one, so there is no code path on which a
 * removal happens on a single tap.
 *
 * **The body is dropped on the direct path rather than reworded.** A group with
 * no role table removes by direct commit, which takes effect the moment it
 * lands — so §6's sentence about the member still receiving messages is false
 * there, and a security tool that reassures a user with a false statement about
 * who can still read their messages is worse than one that says less. §6
 * authorises exactly one body line and it belongs to one path; inventing a
 * second for the other would be copy this design never wrote.
 */
internal fun removeMemberConfirmation(usesProposalPath: Boolean): RemoveMemberConfirmation =
    RemoveMemberConfirmation(
        title = REMOVE_MEMBER_TITLE,
        body = if (usesProposalPath) REMOVE_MEMBER_BODY else null,
        confirmLabel = REMOVE_MEMBER_CONFIRM_LABEL,
        cancelLabel = REMOVE_MEMBER_CANCEL_LABEL,
    )
