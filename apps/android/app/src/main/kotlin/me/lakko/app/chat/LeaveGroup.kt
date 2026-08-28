// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/**
 * ADR-0045 — `Leave group`: the member-facing half of proposal-based
 * membership, and the one rule that decides whether it may be offered.
 *
 * Android mirror of `apps/ios/Lakko/Chat/LeaveGroup.swift` and of the web
 * reference `apps/web/src/routes/Chat.tsx` (`onLeaveGroup` plus the control's
 * `Show` / `disabled` gate). Copy and states come from
 * `docs/ux/proposal-based-membership.md` §5 item 6 / §6.
 *
 * ## Why leaving exists at all, and only now
 *
 * ADR-0022 recorded "v1 has no member self-leave — an admin removes them",
 * because OpenMLS `CannotRemoveSelf` makes a self-removal impossible to
 * *commit*: a member could never publish their own departure. ADR-0045 moved
 * membership onto **proposals**, and a proposal is not a commit — so a member
 * can now name their own leaf and let a committer sweep it. The core's
 * `authorise` carries the matching exception verbatim: "Removing exactly your
 * own leaf is leaving, and needs no role."
 *
 * ## Why a plain module and not three `if`s in the composable
 *
 * The host unit suite cannot render a composable and cannot load the JNI
 * library, so the rule lives here where it is executable — the same split
 * `PendingRequests.kt`, `SplitPoll.kt` and `CommitOutbox.kt` already use. It
 * also has **two consumers that must not disagree**: the control's visibility,
 * and the action's own pre-check in `ChatViewModel.onLeaveGroup`. A control that
 * is merely hidden is not a guarantee — `isOwner` is only as fresh as the last
 * roster refresh — so a role change landing between a refresh and a tap would
 * otherwise carry a request the core refuses into the core, and surface the
 * core's own prose ("removal proposal refused: the group owner cannot be
 * removed") to a user who never asked about proposals.
 *
 * ## Why there is no `Cancel`, and therefore why the confirm matters
 *
 * Once published, the request is in every member's proposal store and dies only
 * when some commit lands. There is no wire withdraw, so a local-only cancel
 * would leave the request live on every peer — a worse lie than showing it
 * (UX §3). The confirmation dialog is the **last exit**, which is why this
 * module exposes no withdraw verb and the panel must not invent one.
 */

/**
 * Why `Leave group` is not being offered.
 *
 * The copy is the UX spec §6 refusal table, verbatim. These strings are produced
 * by a **local** pre-check and are never a translation of a core error — mapping
 * core prose to fixed copy is the drift class §12.6 flags, and this deliberately
 * is not that.
 */
internal enum class LeaveGroupRefusal(val message: String) {
    /**
     * The owner cannot leave *while owner*: `set_group_roles` is owner-only, so
     * an ownerless group could never appoint another admin again. A precondition
     * rather than a dead end — transfer ownership, then leave as an ordinary
     * member — but no client has a transfer affordance yet, so the honest
     * surface is to hide the control rather than offer one that always refuses.
     */
    OWNER_CANNOT_LEAVE("The group owner cannot leave the group."),

    /**
     * No role table (a legacy / open group), or this device's own leaf could not
     * be resolved. The UX §6 fail-closed fallback: it must exist, must never
     * leak an internal reason, and must never be reached in normal use.
     */
    NOT_REQUESTABLE("This change could not be requested."),
}

/** Whether the `Leave group` control renders, and whether it is live. */
internal sealed interface LeaveGroupAffordance {
    /**
     * Not offered at all. The reason carries the copy for the case where the
     * action is reached anyway (a stale gate, a race against a role change).
     */
    data class Hidden(val reason: LeaveGroupRefusal) : LeaveGroupAffordance

    /**
     * Offered, but not actionable right now — another membership action is in
     * flight, or this device has already asked and is waiting.
     */
    data object Disabled : LeaveGroupAffordance

    /** Offered and live. */
    data object Enabled : LeaveGroupAffordance
}

/**
 * The one rule: may this device offer `Leave group`, and is it actionable?
 *
 * Mirrors the web gate `usesProposalPath() && !isOwner() && ownLeaf() !== undefined`
 * for visibility and `busy() || rowAnnotation(...) !== null` for the disabled
 * state, so the three clients agree on when the control appears.
 *
 * The already-requested case is [LeaveGroupAffordance.Disabled] rather than
 * hidden for the same reason UX §5.3 keeps a pending row's `Remove`
 * visible-but-disabled: hiding it would read as the permission having vanished,
 * where the row's own "Waiting to leave." annotation explains a disabled
 * control.
 */
internal fun leaveGroupAffordance(
    usesProposalPath: Boolean,
    isOwner: Boolean,
    ownLeaf: UInt?,
    busy: Boolean,
    requests: List<PendingRequest>,
): LeaveGroupAffordance {
    // The owner arm is first because it is the specific case: a group with no
    // role table has no owner either, so the two can never both apply, and
    // leading with the owner keeps the more useful message when one does.
    if (isOwner) return LeaveGroupAffordance.Hidden(LeaveGroupRefusal.OWNER_CANNOT_LEAVE)
    // No role table means no committer, so a leave proposal would sit in every
    // member's store forever with nobody able to sweep it — and the core has no
    // self-remove route there either.
    if (!usesProposalPath) return LeaveGroupAffordance.Hidden(LeaveGroupRefusal.NOT_REQUESTABLE)
    // A leave names a leaf. Without one there is nothing to compose, so this is
    // fail-closed rather than an optimistic attempt.
    if (ownLeaf == null) return LeaveGroupAffordance.Hidden(LeaveGroupRefusal.NOT_REQUESTABLE)
    if (busy) return LeaveGroupAffordance.Disabled
    // Keyed on OUR leaf, never on "some request is pending": a removal aimed at
    // somebody else is the ordinary state of a group between any admin's request
    // and the next sweep. Reusing `rowAnnotation` rather than re-deriving the
    // predicate keeps the disabled state and the row's own explanation of it in
    // lock-step — including the quiet window, so a request that is not yet
    // rendered cannot silently disable the control with nothing to explain it.
    if (rowAnnotation(requests, ownLeaf, ownLeaf) != null) return LeaveGroupAffordance.Disabled
    return LeaveGroupAffordance.Enabled
}
