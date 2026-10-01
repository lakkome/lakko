// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.FfiRefusalReason

/**
 * ADR-0045 UX §6 / §12 item 6 — the typed membership-refusal table.
 *
 * What is pinned here is the DECISION — which sentence answers which core
 * refusal, and whether the user may be invited to try again — never the
 * composable.
 *
 * ## Why the retry port is a STATED rule here and the real core elsewhere
 *
 * `membershipRefusalRetryMaySucceed` is a JNA call into
 * `liblakko_ffi_uniffi.so`, which the host JVM does not load — the same
 * host-vs-instrumented split `GroupPushRoutingTest` /
 * `GroupPushRoutingInstrumentedTest` already use. So this suite drives the
 * module through a port that STATES the maintainer's rule, and pins that the
 * module transmits whatever the port says (including when the port is wrong).
 * That agreement with the CORE's own answer — the thing that would catch the
 * rule itself drifting — is proven in [MembershipRefusalInstrumentedTest],
 * which runs where the `.so` exists.
 *
 * Neither half is sufficient alone, which is why both exist: the instrumented
 * test cannot see a module that hard-coded the partition (it would agree with
 * the core on every input), and this one cannot see the rule being restated
 * wrongly.
 *
 * ## What the COMPILER pins that this file cannot
 *
 * `refusalCopy` and `membershipRefusalReason` are `when` **expressions** over
 * an enum and a sealed class with no `else` branch, so a nineteenth
 * `FfiRefusalReason` or a forty-fourth `FfiException` subclass fails the BUILD
 * rather than a test. That is the strongest form of the "a new core reason
 * cannot silently render blank" requirement available on this platform, and it
 * is why neither has an `else`. Do not add one.
 *
 * The discrimination evidence for each rule (each probe was RUN, the failure
 * observed, and the probe reverted) is recorded in `MembershipRefusal.kt`.
 */
class MembershipRefusalTest {

    /**
     * The maintainer's rule, STATED here rather than read from the core:
     * retry when a request was refused because a bounded queue was full,
     * never when it was refused because of its own state.
     *
     * Agreement with the core's own `membershipRefusalRetryMaySucceed` is
     * pinned in [MembershipRefusalInstrumentedTest]; what this suite pins is
     * that the module ASKS rather than deciding.
     */
    private val expectedRetryable = setOf(
        FfiRefusalReason.ROLE_CHANGE_PENDING,
        FfiRefusalReason.PENDING_SET_FULL,
    )

    private val statedRule: (FfiRefusalReason) -> Boolean = { it in expectedRetryable }

    private fun say(reason: FfiRefusalReason): String =
        membershipRefusal(reason, statedRule).message

    // ---- Coverage ---------------------------------------------------------

    @Test
    fun `every core reason has a DECIDED answer, so a new one fails here`() {
        // `FfiRefusalReason.entries` is the CORE's own enumeration, not a list
        // this file keeps, so a reason added to `lakko-mls` and regenerated
        // arrives here automatically.
        // 20 since ADR-0050 appended DEVICE_BINDING_INVALID and DEVICE_LIMIT_REACHED;
        // 23 since ADR-0060 appended the three conversation-kind reasons.
        assertEquals(23, FfiRefusalReason.entries.size)

        for (reason in FfiRefusalReason.entries) {
            assertTrue("$reason rendered an empty sentence", say(reason).isNotEmpty())
            // Every reason is EITHER given its own sentence OR recorded as a
            // deliberate collapse onto the fail-closed row. Falling back is
            // what an unconsidered reason would also do, so what is asserted
            // is the decision, not the output.
            assertTrue(
                "$reason is a core refusal with no decision: give it a sentence, or record " +
                    "the collapse in MEMBERSHIP_REFUSALS_COLLAPSED_TO_FALLBACK",
                membershipRefusalHasOwnCopy(reason) ||
                    reason in MEMBERSHIP_REFUSALS_COLLAPSED_TO_FALLBACK,
            )
        }

        // Disjoint: a reason with its own sentence is not a collapse.
        for (reason in MEMBERSHIP_REFUSALS_COLLAPSED_TO_FALLBACK) {
            assertFalse("$reason is in both halves", membershipRefusalHasOwnCopy(reason))
        }
    }

    // ---- The copy ---------------------------------------------------------

    @Test
    fun `a chat's fixed membership reads the ADR-0060 sentences`() {
        assertEquals(
            "A chat is always the same two people.",
            say(FfiRefusalReason.ONE_TO_ONE_MEMBERSHIP_FIXED),
        )
        assertEquals("A chat has no admins.", say(FfiRefusalReason.ONE_TO_ONE_HAS_NO_ROLES))
        assertEquals(
            "This change can't be made to this conversation.",
            say(FfiRefusalReason.GROUP_KIND_CHANGED),
        )
    }

    @Test
    fun `the device cap reads the ADR-0050 sentence`() {
        assertEquals(
            "Up to 8 devices per account. Remove one first.",
            say(FfiRefusalReason.DEVICE_LIMIT_REACHED),
        )
    }

    @Test
    fun `it states the spec-approved rows verbatim`() {
        assertEquals(
            "Only the group owner or an admin can add members.",
            say(FfiRefusalReason.ADD_NOT_OWNER_OR_ADMIN),
        )
        assertEquals(
            "Only the group owner or an admin can remove members.",
            say(FfiRefusalReason.REMOVE_NOT_OWNER_OR_ADMIN),
        )
        assertEquals("The group owner cannot be removed.", say(FfiRefusalReason.OWNER_UNREMOVABLE))
        assertEquals(
            "The group owner cannot leave the group.",
            say(FfiRefusalReason.OWNER_CANNOT_LEAVE),
        )
        assertEquals(
            "A role change is already waiting to apply. Wait for it to land, then try again.",
            say(FfiRefusalReason.ROLE_CHANGE_PENDING),
        )
        // §6 row 7, the fail-closed fallback.
        assertEquals(
            "This change could not be requested.",
            say(FfiRefusalReason.UNSUPPORTED_PROPOSAL),
        )

        // Asserted against the exported constants too, so renaming a constant
        // cannot silently change a sentence.
        assertEquals("Only the group owner or an admin can add members.", ADD_NOT_OWNER_OR_ADMIN)
        assertEquals("This change could not be requested.", MEMBERSHIP_REFUSAL_FALLBACK)
    }

    @Test
    fun `it shares one vocabulary with the local pre-checks`() {
        // §6: "One string table, two entry points, no second vocabulary."
        // `LeaveGroup.kt` refuses locally without ever calling the core;
        // `ChatRepository` refuses a second role change the same way. A user
        // must not be able to tell which layer answered, so the pre-checks
        // read from the same constants rather than restating them.
        assertEquals(
            say(FfiRefusalReason.OWNER_CANNOT_LEAVE),
            LeaveGroupRefusal.OWNER_CANNOT_LEAVE.message,
        )
        assertEquals(MEMBERSHIP_REFUSAL_FALLBACK, LeaveGroupRefusal.NOT_REQUESTABLE.message)
        assertEquals(say(FfiRefusalReason.ROLE_CHANGE_PENDING), ROLE_CHANGE_ALREADY_PENDING)
    }

    @Test
    fun `it says something true for each reason the spec never named`() {
        // §7.1 flags this one by name as needing the typed table.
        assertEquals(
            "Too many changes are already waiting to apply. Wait for them to land, then try again.",
            say(FfiRefusalReason.PENDING_SET_FULL),
        )
        // Reachable from the §6.1 transfer picker when the chosen member left.
        assertEquals(
            "Ownership can only be held by a current member of this group.",
            say(FfiRefusalReason.OWNER_NOT_A_MEMBER),
        )
        // Reachable from a stale Approve / Reject tap.
        assertEquals(
            "That join request is no longer waiting.",
            say(FfiRefusalReason.NO_SUCH_JOIN_REQUEST),
        )
        assertEquals(
            "That person has already joined this group.",
            say(FfiRefusalReason.JOINER_ALREADY_A_MEMBER),
        )
        // §6 row 5, widened: the core gives ONE reason to a role change and an
        // ownership transfer, because a transfer IS a role-table rewrite. A
        // string naming only admins would be wrong for half of what it covers.
        assertEquals(
            "Only the group owner can change admins or transfer ownership.",
            say(FfiRefusalReason.NOT_OWNER),
        )
    }

    @Test
    fun `it collapses to the fail-closed row for everything a user cannot act on`() {
        assertEquals(
            setOf(
                FfiRefusalReason.NOT_A_MEMBER,
                FfiRefusalReason.LEAF_UPDATE_REFUSED,
                FfiRefusalReason.UNSUPPORTED_PROPOSAL,
                FfiRefusalReason.NO_ROLE_TABLE,
                FfiRefusalReason.ROLE_TABLE_WOULD_VANISH,
                FfiRefusalReason.NOT_A_COMMITTER,
                FfiRefusalReason.PROPOSALS_PENDING,
                FfiRefusalReason.SWEEP_ALREADY_STAGED,
            ),
            MEMBERSHIP_REFUSALS_COLLAPSED_TO_FALLBACK,
        )
        for (reason in MEMBERSHIP_REFUSALS_COLLAPSED_TO_FALLBACK) {
            assertEquals("$reason", MEMBERSHIP_REFUSAL_FALLBACK, say(reason))
        }
    }

    @Test
    fun `it uses no protocol vocabulary`() {
        // §1: user-facing copy says `request` and `change`; never proposal,
        // commit, epoch, sweep, committer or leaf.
        val forbidden = listOf("proposal", "commit", "epoch", "sweep", "committer", "leaf")
        for (reason in FfiRefusalReason.entries) {
            val text = say(reason).lowercase()
            for (word in forbidden) {
                assertFalse("$reason: `$word` leaked into $text", text.contains(word))
            }
        }
    }

    // ---- The retry affordance ---------------------------------------------

    @Test
    fun `the retryable set is exactly the two full queues`() {
        // The maintainer's rule: retry when a request was refused because a
        // queue was full, never when it was refused because of its own state.
        // Written as a literal expectation rather than derived from the port,
        // so a reason that changes disposition in the core has to move here.
        val retryable = FfiRefusalReason.entries.filter {
            membershipRefusal(it, statedRule).retryMaySucceed
        }
        assertEquals(
            listOf(FfiRefusalReason.ROLE_CHANGE_PENDING, FfiRefusalReason.PENDING_SET_FULL),
            retryable,
        )
    }

    @Test
    fun `it follows the port even when the port is WRONG`() {
        // The test that catches a client which hard-coded the partition. Such
        // a client agrees with the real core on every input, so no amount of
        // real-core testing would ever notice. The port is therefore inverted.
        val inverted: (FfiRefusalReason) -> Boolean = { !statedRule(it) }

        for (reason in listOf(
            FfiRefusalReason.ROLE_CHANGE_PENDING,
            FfiRefusalReason.PENDING_SET_FULL,
        )) {
            val refusal = membershipRefusal(reason, inverted)
            assertFalse("$reason", refusal.retryMaySucceed)
            assertFalse("$reason", refusal.message.contains("try again"))
        }
        assertTrue(
            membershipRefusal(FfiRefusalReason.OWNER_UNREMOVABLE, inverted).retryMaySucceed,
        )
    }

    @Test
    fun `the invitation is appended only when the port says so`() {
        // The invitation is not baked into the sentence: it is appended
        // because the CORE said waiting helps. With the port forced false the
        // fact survives and the invitation does not, which proves the sentence
        // is assembled from the core's answer rather than stored whole.
        val never: (FfiRefusalReason) -> Boolean = { false }
        assertEquals(
            ROLE_CHANGE_PENDING,
            membershipRefusal(FfiRefusalReason.ROLE_CHANGE_PENDING, never).message,
        )
        assertEquals(
            PENDING_SET_FULL,
            membershipRefusal(FfiRefusalReason.PENDING_SET_FULL, never).message,
        )
        // Concatenation, not two unrelated strings: joined they are §6 row 6.
        assertEquals("$ROLE_CHANGE_PENDING $RETRY_WHEN_IT_LANDS", ROLE_CHANGE_ALREADY_PENDING)
    }

    // ---- Recognising a refusal at all -------------------------------------

    @Test
    fun `every refusal variant bridges to its reason`() {
        // The mechanical variant -> reason bridge. `#[uniffi(flat_error)]`
        // lowers only the discriminant and the Display string, so the reason
        // CANNOT ride as a field and the variant is the only zero-parse
        // channel. This asserts the bridge is wired the right way round for
        // all eighteen — a transposed pair here would show the wrong sentence
        // for a real refusal, silently.
        val pairs: List<Pair<FfiException, FfiRefusalReason>> = listOf(
            FfiException.MembershipNotAMember("x") to FfiRefusalReason.NOT_A_MEMBER,
            FfiException.MembershipAddNotOwnerOrAdmin("x") to
                FfiRefusalReason.ADD_NOT_OWNER_OR_ADMIN,
            FfiException.MembershipRemoveNotOwnerOrAdmin("x") to
                FfiRefusalReason.REMOVE_NOT_OWNER_OR_ADMIN,
            FfiException.MembershipOwnerUnremovable("x") to FfiRefusalReason.OWNER_UNREMOVABLE,
            FfiException.MembershipOwnerCannotLeave("x") to FfiRefusalReason.OWNER_CANNOT_LEAVE,
            FfiException.MembershipNotOwner("x") to FfiRefusalReason.NOT_OWNER,
            FfiException.MembershipLeafUpdateRefused("x") to FfiRefusalReason.LEAF_UPDATE_REFUSED,
            FfiException.MembershipUnsupportedProposal("x") to
                FfiRefusalReason.UNSUPPORTED_PROPOSAL,
            FfiException.MembershipNoRoleTable("x") to FfiRefusalReason.NO_ROLE_TABLE,
            FfiException.MembershipOwnerNotAMember("x") to FfiRefusalReason.OWNER_NOT_A_MEMBER,
            FfiException.MembershipRoleTableWouldVanish("x") to
                FfiRefusalReason.ROLE_TABLE_WOULD_VANISH,
            FfiException.MembershipNotACommitter("x") to FfiRefusalReason.NOT_A_COMMITTER,
            FfiException.MembershipRoleChangePending("x") to FfiRefusalReason.ROLE_CHANGE_PENDING,
            FfiException.MembershipProposalsPending("x") to FfiRefusalReason.PROPOSALS_PENDING,
            FfiException.MembershipPendingSetFull("x") to FfiRefusalReason.PENDING_SET_FULL,
            FfiException.MembershipNoSuchJoinRequest("x") to FfiRefusalReason.NO_SUCH_JOIN_REQUEST,
            FfiException.MembershipJoinerAlreadyAMember("x") to
                FfiRefusalReason.JOINER_ALREADY_A_MEMBER,
            FfiException.SweepAlreadyStaged("x") to FfiRefusalReason.SWEEP_ALREADY_STAGED,
            FfiException.MembershipDeviceBindingInvalid("x") to
                FfiRefusalReason.DEVICE_BINDING_INVALID,
            FfiException.MembershipDeviceLimitReached("x") to
                FfiRefusalReason.DEVICE_LIMIT_REACHED,
            FfiException.MembershipOneToOneMembershipFixed("x") to
                FfiRefusalReason.ONE_TO_ONE_MEMBERSHIP_FIXED,
            FfiException.MembershipOneToOneHasNoRoles("x") to
                FfiRefusalReason.ONE_TO_ONE_HAS_NO_ROLES,
            FfiException.MembershipGroupKindChanged("x") to
                FfiRefusalReason.GROUP_KIND_CHANGED,
        )
        assertEquals(FfiRefusalReason.entries.size, pairs.size)
        assertEquals(FfiRefusalReason.entries.toSet(), pairs.map { it.second }.toSet())
        for ((error, reason) in pairs) {
            assertEquals("$error", reason, membershipRefusalReason(error))
        }
    }

    @Test
    fun `it finds the reason through the wrapper the repository throws`() {
        // Every membership call leaves `ChatRepository` as a `ChatError` whose
        // `cause` is the `FfiException`. Without this the whole table would be
        // dead code on the one path that reaches the UI.
        val wrapped = ChatError(
            "proposeAddMemberFromWire failed",
            FfiException.MembershipAddNotOwnerOrAdmin("core prose"),
        )
        assertEquals(FfiRefusalReason.ADD_NOT_OWNER_OR_ADMIN, membershipRefusalReason(wrapped))
        assertEquals(ADD_NOT_OWNER_OR_ADMIN, membershipRefusalMessage(wrapped, statedRule))
    }

    @Test
    fun `it leaves a NON-refusal alone`() {
        // A network error, a codec error, a bootstrap mistake: these keep
        // their own message. Turning one into "This change could not be
        // requested." would be a worse lie than the raw text.
        assertNull(membershipRefusalReason(FfiException.Codec("bad bytes")))
        assertNull(membershipRefusalReason(IllegalStateException("no live Group handle")))
        assertNull(membershipRefusalMessage(FfiException.Codec("bad bytes"), statedRule))
        assertNull(
            membershipRefusalReason(ChatError("DS publishGroup failed", java.io.IOException("x"))),
        )
    }

    // ---- ADR-0027 — a request the quarantine did not keep -----------------

    @Test
    fun `an unretained join request is never actionable`() {
        // The core fires `ExternalJoinProposal` even when the quarantine was
        // full and the request was NOT retained; approving it then refuses
        // with NO_SUCH_JOIN_REQUEST. An Approve button for it is an affordance
        // that cannot work.
        assertEquals(
            ExternalJoinAffordance.Informational(JOIN_REQUEST_NOT_KEPT),
            externalJoinAffordance(retained = false),
        )
        assertEquals(ExternalJoinAffordance.Actionable, externalJoinAffordance(retained = true))
    }

    @Test
    fun `it says what happened instead of showing nothing`() {
        // §7.1 recorded this as unsurfaceable: "at 64 retained external joins
        // `retain_external_join` silently stops retaining, and nothing
        // distinguishes that from a request that never arrived". `retained` is
        // exactly that distinction, so silence stopped being honest.
        assertEquals(
            "A join request arrived while too many were already waiting, so it was not kept. " +
                "Ask the person to send it again later.",
            JOIN_REQUEST_NOT_KEPT,
        )
    }
}
