// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * ADR-0045 — `Leave group` on Android: the decision rule that gates the control.
 *
 * ## Why the rule and not the action
 *
 * `ChatRepository` needs an Android `Context` and the FFI `Group` needs the JNI
 * native library, which a JVM unit test cannot load — the same constraint that
 * put [runSplitPoll], [prepareCrashSafeCommit] and `runApproveExternalJoin` behind
 * injected steps. The **live-MLS half** — that a member's leave really does
 * produce a proposal rather than a commit, that the owner's is really refused
 * before anything reaches the wire, that the roster really keeps the leaving
 * member's row — is pinned once on iOS (`LakkoTests/LeaveGroupTests.swift`),
 * whose simulator target CAN link the FFI. Both platforms drive the identical
 * repository call (`removeMemberFromGroup(groupIdHex, ownLeaf)`), so pinning the
 * core's behaviour once is enough; what has to hold on BOTH is this rule, and it
 * is what this file covers.
 *
 * ## Discrimination (measured by RUNNING each break, quoted verbatim — not
 * predicted)
 *
 * **Break 1 — [leaveGroupAffordance] stubbed to `return LeaveGroupAffordance.Enabled`**
 * (its pre-implementation shape). 5 of 8 fail, e.g.
 *   - `the owner is not offered the control`:
 *     `expected:<Hidden(reason=OWNER_CANNOT_LEAVE)> but was:<Enabled>`
 *   - alongside `a legacy group with no role table is not offered the control`,
 *     `an unresolvable own leaf is not offered the control`,
 *     `an already requested leave disables the control`,
 *     `a busy panel disables the control`.
 *
 * **Break 2 — the owner arm alone deleted** (`if (false)`). Exactly 1 fails:
 *   - `the owner is not offered the control`:
 *     `expected:<Hidden(reason=OWNER_CANNOT_LEAVE)> but was:<Enabled>`.
 *
 * **Break 3 — the disabled check keyed on the first pending request instead of
 * our own leaf** (`requests.firstOrNull()?.targetLeaf ?: ownLeaf`). Exactly 1
 * fails:
 *   - `another members pending removal does not disable our leave`:
 *     `expected:<Enabled> but was:<Disabled>`.
 */
class LeaveGroupTest {
    init {
        EnglishForTests.install()
    }


    // ---- fixtures ---------------------------------------------------------

    /** A pending request as the fold produces one. Constructed directly rather
     *  than folded, so each test states only the field it is about. */
    private fun request(
        key: String,
        kind: PendingRequestKind,
        targetLeaf: UInt?,
        senderLeaf: UInt?,
        own: Boolean,
        state: RequestState = RequestState.WAITING,
    ) = PendingRequest(
        key = key,
        kind = kind,
        addSignatureKeyB64 = null,
        targetLeaf = targetLeaf,
        senderLeaf = senderLeaf,
        observedAtMs = 0L,
        own = own,
        state = state,
    )

    /** Our own leave, already published and past the quiet window. */
    private fun ownLeave(leaf: UInt) = request(
        key = "remove:$leaf",
        kind = PendingRequestKind.REMOVE,
        targetLeaf = leaf,
        senderLeaf = leaf,
        own = true,
    )

    // ---- (1) who is offered the control -----------------------------------

    /**
     * The ordinary case, and the whole point of the feature: a plain member of a
     * role-tabled group may ask to leave. The core carries the matching
     * exception — "Removing exactly your own leaf is leaving, and needs no
     * role" — so no role gate applies here.
     */
    @Test
    fun `a plain member on the proposal path is offered the control`() {
        assertEquals(
            "a plain member naming their own leaf needs no role — that is what leaving IS",
            LeaveGroupAffordance.Enabled,
            leaveGroupAffordance(
                usesProposalPath = true,
                isOwner = false,
                ownLeaf = 3u,
                busy = false,
                requests = emptyList(),
            ),
        )
    }

    /**
     * The owner cannot leave *while owner*: `set_group_roles` is owner-only, so
     * an ownerless group could never appoint another admin again. The core
     * refuses it fail-closed; the UI must not offer a control that always
     * refuses (UX §6), so this is hidden, not merely disabled.
     */
    @Test
    fun `the owner is not offered the control`() {
        assertEquals(
            "the owner cannot leave while owner: set_group_roles is owner-only, so an " +
                "ownerless group could never appoint another admin",
            LeaveGroupAffordance.Hidden(LeaveGroupRefusal.OWNER_CANNOT_LEAVE),
            leaveGroupAffordance(
                usesProposalPath = true,
                isOwner = true,
                ownLeaf = 0u,
                busy = false,
                requests = emptyList(),
            ),
        )
        assertEquals(
            "the copy is the UX §6 refusal table verbatim",
            "The group owner cannot leave the group.",
            LeaveGroupRefusal.OWNER_CANNOT_LEAVE.message.en,
        )
    }

    /**
     * A group with no role table has no committer, so nothing would ever sweep
     * the proposal — and the core's self-remove route does not exist there
     * either. Offering the control would produce a request that can never apply.
     */
    @Test
    fun `a legacy group with no role table is not offered the control`() {
        assertEquals(
            "no role table means no committer, so a leave proposal would never be swept",
            LeaveGroupAffordance.Hidden(LeaveGroupRefusal.NOT_REQUESTABLE),
            leaveGroupAffordance(
                usesProposalPath = false,
                isOwner = false,
                ownLeaf = 3u,
                busy = false,
                requests = emptyList(),
            ),
        )
    }

    /**
     * Fail-closed: a leave names a leaf, so an unresolvable own leaf is not a
     * leave we can compose. The fallback copy must never leak a reason.
     */
    @Test
    fun `an unresolvable own leaf is not offered the control`() {
        assertEquals(
            "a leave names a leaf; without one there is nothing to propose",
            LeaveGroupAffordance.Hidden(LeaveGroupRefusal.NOT_REQUESTABLE),
            leaveGroupAffordance(
                usesProposalPath = true,
                isOwner = false,
                ownLeaf = null,
                busy = false,
                requests = emptyList(),
            ),
        )
        assertEquals(
            "the UX §6 fail-closed fallback, which must never leak an internal reason",
            "This change could not be requested.",
            LeaveGroupRefusal.NOT_REQUESTABLE.message.en,
        )
    }

    // ---- (2) when the control is live -------------------------------------

    /**
     * UX §5.3 — an already-published request keeps its control VISIBLE but
     * disabled, exactly as a pending removal does. Hiding it would read as the
     * permission having vanished, where the row's own "Waiting to leave."
     * annotation explains a disabled control. There is deliberately no `Cancel`:
     * a published request cannot be withdrawn.
     */
    @Test
    fun `an already requested leave disables the control`() {
        assertEquals(
            "asking twice would put two identical Remove proposals in one sweep",
            LeaveGroupAffordance.Disabled,
            leaveGroupAffordance(
                usesProposalPath = true,
                isOwner = false,
                ownLeaf = 3u,
                busy = false,
                requests = listOf(ownLeave(3u)),
            ),
        )
    }

    /** Another membership action is already in flight. */
    @Test
    fun `a busy panel disables the control`() {
        assertEquals(
            LeaveGroupAffordance.Disabled,
            leaveGroupAffordance(
                usesProposalPath = true,
                isOwner = false,
                ownLeaf = 3u,
                busy = true,
                requests = emptyList(),
            ),
        )
    }

    /**
     * The disabled check must be keyed on OUR leaf. A removal aimed at somebody
     * else is the ordinary state of a group between any admin's request and the
     * next sweep, and it says nothing about whether we may ask to leave.
     */
    @Test
    fun `another members pending removal does not disable our leave`() {
        val someoneElse = request(
            key = "remove:7",
            kind = PendingRequestKind.REMOVE,
            targetLeaf = 7u,
            senderLeaf = 0u,
            own = false,
        )
        assertEquals(
            "a removal aimed at somebody else says nothing about whether we may ask to leave",
            LeaveGroupAffordance.Enabled,
            leaveGroupAffordance(
                usesProposalPath = true,
                isOwner = false,
                ownLeaf = 3u,
                busy = false,
                requests = listOf(someoneElse),
            ),
        )
    }

    /**
     * A request still inside the quiet window has not rendered anything yet, so
     * it must not silently disable the control either — the user would be left
     * with a dead button and nothing beside it to explain why.
     */
    @Test
    fun `a request inside the quiet window does not disable the control`() {
        val notYetVisible = request(
            key = "remove:3",
            kind = PendingRequestKind.REMOVE,
            targetLeaf = 3u,
            senderLeaf = 3u,
            own = true,
            state = RequestState.PENDING,
        )
        assertEquals(
            "nothing is rendered inside the quiet window, so nothing may explain a " +
                "disabled control there",
            LeaveGroupAffordance.Enabled,
            leaveGroupAffordance(
                usesProposalPath = true,
                isOwner = false,
                ownLeaf = 3u,
                busy = false,
                requests = listOf(notYetVisible),
            ),
        )
    }
}
