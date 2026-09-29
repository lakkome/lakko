// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.FfiRefusalReason

/*
 * ADR-0045 UX §6 / §12 item 6 — the typed membership-refusal table.
 *
 * ## What this is for
 *
 * §12 item 6 has been open since the spec was written: *"Prose-matching
 * `Error::Group` strings across three clients would work and would be wrong."*
 * The clients did the defensible thing and built no table, so a core refusal
 * reached the user as the core's own developer sentence — on Android, as
 * `e.message ?: e.toString()` straight into a red `Text`, which for a member
 * who tapped `Add` read `proposeAddMemberFromWire failed`, or, one layer
 * further in, `add proposal refused: proposer is not owner or admin`.
 *
 * The core now carries a machine-readable `RefusalReason` beside that
 * sentence. This module is the client half: reason in, sentence out.
 * **Nothing here looks at a message.**
 *
 * ## Why there is a variant -> reason bridge, and why it is not prose matching
 *
 * `#[uniffi(flat_error)]` lowers ONLY a variant's discriminant and its
 * `Display` string, dropping associated data — so the reason cannot ride as a
 * field on `FfiException`, and the variant is the one zero-parse channel the
 * type has. [membershipRefusalReason] is therefore a mechanical map from
 * `FfiException.MembershipX` to `FfiRefusalReason.X`. It is compiler-checked
 * and exhaustive, which is exactly what a `message?.contains(...)` is not: a
 * renamed variant fails the build, a reworded sentence changes nothing.
 *
 * ## The `else` branches that are deliberately absent
 *
 * Both `when`s below are **expressions** with no `else`, so a nineteenth
 * `FfiRefusalReason` or a forty-fourth `FfiException` subclass fails the
 * BUILD. That is the whole "a new core reason cannot silently render blank"
 * guarantee on this platform, and it evaporates the moment somebody adds an
 * `else ->` to quiet the compiler. Do not.
 *
 * ## The retry affordance is the CORE's answer
 *
 * Whether waiting helps is asked of `membershipRefusalRetryMaySucceed` and
 * never restated here. The rule is narrow and easy to get wrong in a way
 * nothing catches: it is **not** the complement of "permitted" — most
 * non-permission refusals are still not retryable — and a client inferring it
 * from a constant's name would loop a user on `PROPOSALS_PENDING` (which
 * sounds transient and is not) or tell them they may not do something they may
 * on `PENDING_SET_FULL`.
 *
 * So the invitation to try again is **appended** rather than stored, and only
 * when the port says so. A stored sentence would agree with the core by
 * coincidence and keep agreeing after the core changed its mind.
 *
 * ## Which rules here actually discriminate (MEASURED — each probe was run)
 *
 * Each was introduced as a deliberate regression, `./gradlew
 * :app:testDebugUnitTest` was run, the failure below observed, and the probe
 * reverted.
 *
 * 1. **`retryMaySucceed` is computed from a local list** instead of from the
 *    port — passes every real-core test, fails only the inverted one:
 *
 *        it follows the port even when the port is WRONG
 *        java.lang.AssertionError: ROLE_CHANGE_PENDING
 *
 * 2. **The invitation is stored in the sentence instead of appended**:
 *
 *        the invitation is appended only when the port says so
 *        expected:<A role change is already waiting to apply.> but was:<A role
 *        change is already waiting to apply. Wait for it to land, then try
 *        again.>
 *
 * 3. **The bridge reads only the outermost throwable, not `ChatError.cause`**
 *    — which is the whole production path:
 *
 *        it finds the reason through the wrapper the repository throws
 *        expected:<ADD_NOT_OWNER_OR_ADMIN> but was:<null>
 *
 * 4. **Two bridge arms transposed** (`OWNER_UNREMOVABLE` <->
 *    `OWNER_CANNOT_LEAVE`):
 *
 *        every refusal variant bridges to its reason
 *        expected:<OWNER_UNREMOVABLE> but was:<OWNER_CANNOT_LEAVE>
 *
 * 5. **[externalJoinAffordance] ignores `retained`**:
 *
 *        an unretained join request is never actionable
 *        expected:<Informational(...)> but was:<Actionable>
 */

// ---------------------------------------------------------------------------
// Copy
// ---------------------------------------------------------------------------

/**
 * §6 row 7 — the fail-closed fallback, and the default for **anything** this
 * build does not recognise, a reason from a future core included.
 *
 * "It must exist, must never leak an internal reason, and must never be
 * reached in normal use." The third clause is the one worth holding onto: if
 * this string starts appearing, something is wrong that is not a copy problem.
 */
internal const val MEMBERSHIP_REFUSAL_FALLBACK: String = "This change could not be requested."

/** §6 row 1, verbatim. */
internal const val ADD_NOT_OWNER_OR_ADMIN: String =
    "Only the group owner or an admin can add members."

/** §6 row 2, verbatim. */
internal const val REMOVE_NOT_OWNER_OR_ADMIN: String =
    "Only the group owner or an admin can remove members."

/**
 * §6 row 3, verbatim.
 *
 * It also answers an owner who tapped their own `Leave group`: the core checks
 * *is this leaf the owner's* before it checks anything about roles, so an
 * owner's self-removal reports this and not `OWNER_CANNOT_LEAVE`. §6.1.1
 * disables that control, but the string is right if it is reached anyway.
 */
internal const val OWNER_UNREMOVABLE: String = "The group owner cannot be removed."

/**
 * §6 row 4, verbatim.
 *
 * **Nothing can reach this today, and it keeps its sentence anyway.** The
 * reason fires only for a `Proposal::SelfRemove` authored by an owner, and
 * `SelfRemove` is unreachable in Lakko at the **wire-format** level, not
 * merely absent from the FFI: RFC 9420 requires it to be sent as a
 * `PublicMessage`, `Group::create` leaves the OpenMLS policy at its
 * pure-ciphertext default, and OpenMLS therefore refuses to craft one
 * (`CannotSelfRemoveWithPureCiphertext`). So no peer can send one either.
 * `Leave group` calls `proposeRemoveMember` with its own leaf, which the core
 * classifies as a `Remove` and — for an owner — answers with
 * [OWNER_UNREMOVABLE], since the owner check runs before the role check. What
 * an owner actually sees is the §6.1.1 pre-check, on a control that never
 * calls the core.
 *
 * Collapsing it onto the fallback would have been defensible on reachability
 * alone and is still the wrong trade, for a reason the core supplies:
 * `self_removal_is_gated_for_its_own_sender_and_refused_for_the_owner` exists
 * precisely to fail the day somebody changes that wire-format policy, and that
 * change is wire-visible (it stops encrypting handshake messages to the
 * zero-trust DS). The day it fires, this reason becomes reachable and the copy
 * is already right. A collapse would instead degrade a precise refusal into the
 * fail-closed row nobody is supposed to see, silently.
 */
internal const val OWNER_CANNOT_LEAVE: String = "The group owner cannot leave the group."

/**
 * §6 row 5, **widened** — the one existing row this table does not take
 * verbatim, and the reason is in the core rather than in taste.
 *
 * §6 says `Only the group owner can change admins.` The core raises ONE reason
 * (`NOT_OWNER`) for a role-table change *and* for an ownership transfer,
 * because a transfer **is** a role-table rewrite — not a separate commit type,
 * just the owner slot moved. One reason takes one string, and a string naming
 * only admins would be plainly wrong for half of what the reason covers.
 */
internal const val NOT_OWNER: String =
    "Only the group owner can change admins or transfer ownership."

/**
 * §6 row 6, its **fact** half. The invitation is [RETRY_WHEN_IT_LANDS] and is
 * appended only when the core says waiting helps; joined, the two are §6 row 6
 * verbatim, which a test asserts.
 *
 * §6 argues at length that this is the one *availability* row beside the
 * authorisation ones, and that getting the distinction backwards is a real
 * defect in both directions. Splitting the sentence is how that argument
 * becomes mechanical instead of remembered.
 */
internal const val ROLE_CHANGE_PENDING: String = "A role change is already waiting to apply."

/**
 * NEW — §7.1 names this refusal and says plainly that turning it into fixed
 * copy "needs the typed refusal table §12 already lists as unbuilt".
 *
 * It deliberately states **no number.** `MAX_PENDING_PROPOSALS` is
 * `pub(crate)` and reaches neither FFI surface; §7.1 records that a
 * client-side literal is the stale-drift class this repo has already been
 * bitten by, and that it fails in the dangerous direction — reading as *room
 * left* when there is none.
 */
internal const val PENDING_SET_FULL: String = "Too many changes are already waiting to apply."

/**
 * NEW — reachable from the §6.1 transfer picker when the chosen member has
 * left the group between the picker rendering and the tap.
 *
 * Phrased as the standing rule rather than as *that person is gone*, because
 * the reason also covers a role-table rewrite whose owner slot names a
 * non-member, where there is no "that person" the user chose.
 */
internal const val OWNER_NOT_A_MEMBER: String =
    "Ownership can only be held by a current member of this group."

/**
 * NEW — a stale-view refusal, and the one an admin hits after tapping an
 * affordance that should no longer have been on screen. Not phrased as an
 * error, because nothing went wrong.
 */
internal const val NO_SUCH_JOIN_REQUEST: String = "That join request is no longer waiting."

/**
 * NEW — two admins approving the same joiner is ordinary. Nothing is wrong and
 * nothing needs doing.
 */
internal const val JOINER_ALREADY_A_MEMBER: String = "That person has already joined this group."

/** §6 row 6's second half — singular, for the one pending role change. */
/** ADR-0050 — the per-account device cap (`MAX_LEAVES_PER_ACCOUNT`). The one
 *  per-device refusal a user is shown (docs/ux/second-device.md section 5). */
internal const val DEVICE_LIMIT_REACHED: String = "Up to 8 devices per account. Remove one first."

/** ADR-0050 — a device whose binding the group refuses: a client bug, given the
 *  same plain sentence as web rather than a screen. */
internal const val DEVICE_BINDING_INVALID: String =
    "Something is wrong with this device's key. Remove it from Devices and add it again."

internal const val RETRY_WHEN_IT_LANDS: String = "Wait for it to land, then try again."

/**
 * [PENDING_SET_FULL]'s second half — plural, for a queue of them. Two
 * invitations rather than one because "wait for it to land" after "too many
 * changes" has no referent.
 */
internal const val RETRY_WHEN_THEY_LAND: String = "Wait for them to land, then try again."

/**
 * NEW — the state §7.1 recorded as unsurfaceable.
 *
 * "At 64 retained external joins `retain_external_join` silently stops
 * retaining, and nothing distinguishes that from a request that never
 * arrived." The core's `retained` flag **is** that distinction, so the state is
 * statable now and silence stopped being the honest option. Naming no number,
 * for the [PENDING_SET_FULL] reason.
 */
internal const val JOIN_REQUEST_NOT_KEPT: String =
    "A join request arrived while too many were already waiting, so it was not kept. " +
        "Ask the person to send it again later."

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/**
 * The reasons that deliberately share §6's fail-closed row.
 *
 * **Why this is a set rather than an absence.** Falling back is what the copy
 * `when` does for a reason with no sentence, so a reason nobody wrote copy for
 * renders a correct-looking string either way. Recording the collapse turns
 * that into an assertion: this set plus the reasons with their own sentence
 * must be exactly `FfiRefusalReason.entries`.
 *
 * The rule behind every entry is the same — **a reason with no user-initiated
 * entry point gets no sentence** — and each still earns its place separately:
 *
 * - `NOT_A_MEMBER`, `UNSUPPORTED_PROPOSAL`, `NO_ROLE_TABLE` — §6 row 7 names
 *   these three explicitly ("unsupported / unresolvable / no role table"). No
 *   change; the spec already decided them.
 * - `LEAF_UPDATE_REFUSED` — the core states it is reachable from no
 *   client-facing propose API; it exists so a *log* can tell it apart from
 *   `UNSUPPORTED_PROPOSAL`. To a user the two are the same fact, and the
 *   distinction survives where it is useful, in the wrapped message.
 * - `ROLE_TABLE_WOULD_VANISH` — a role-table rewrite leaving no readable table
 *   behind, which no client can compose from its own UI. A peer-gate refusal
 *   on somebody else's commit.
 * - `NOT_A_COMMITTER`, `SWEEP_ALREADY_STAGED` — sweep-side. §8: the committer
 *   sees "in the steady state, nothing", and `ChatRepository` swallows sweep
 *   failures by design ("Never surfaced as a user-facing error: nothing the
 *   user did failed"). A sentence here would have no reader.
 * - `PROPOSALS_PENDING` — refuses a **direct** commit, and a client takes the
 *   direct path only in a group with no role table, where the pending set is
 *   permanently empty. Unreachable by construction rather than by policy.
 */
internal val MEMBERSHIP_REFUSALS_COLLAPSED_TO_FALLBACK: Set<FfiRefusalReason> = setOf(
    FfiRefusalReason.NOT_A_MEMBER,
    FfiRefusalReason.LEAF_UPDATE_REFUSED,
    FfiRefusalReason.UNSUPPORTED_PROPOSAL,
    FfiRefusalReason.NO_ROLE_TABLE,
    FfiRefusalReason.ROLE_TABLE_WOULD_VANISH,
    FfiRefusalReason.NOT_A_COMMITTER,
    FfiRefusalReason.PROPOSALS_PENDING,
    FfiRefusalReason.SWEEP_ALREADY_STAGED,
)

/**
 * The fact half of a reason's copy, plus the invitation to try again when
 * there is one.
 */
private data class RefusalCopy(
    /** The statement of fact. Always shown. */
    val fact: String,
    /**
     * Appended after a space **only** when the core says a plain retry may
     * succeed. `null` for every reason the core never calls retryable.
     */
    val invitation: String? = null,
)

/**
 * Reason -> copy.
 *
 * **Exhaustive `when` expression, no `else`.** A nineteenth `FfiRefusalReason`
 * fails the build here, which is the guarantee this module exists to provide.
 */
private fun refusalCopy(reason: FfiRefusalReason): RefusalCopy = when (reason) {
    FfiRefusalReason.ADD_NOT_OWNER_OR_ADMIN -> RefusalCopy(ADD_NOT_OWNER_OR_ADMIN)
    FfiRefusalReason.REMOVE_NOT_OWNER_OR_ADMIN -> RefusalCopy(REMOVE_NOT_OWNER_OR_ADMIN)
    FfiRefusalReason.OWNER_UNREMOVABLE -> RefusalCopy(OWNER_UNREMOVABLE)
    FfiRefusalReason.OWNER_CANNOT_LEAVE -> RefusalCopy(OWNER_CANNOT_LEAVE)
    FfiRefusalReason.NOT_OWNER -> RefusalCopy(NOT_OWNER)
    FfiRefusalReason.OWNER_NOT_A_MEMBER -> RefusalCopy(OWNER_NOT_A_MEMBER)
    FfiRefusalReason.NO_SUCH_JOIN_REQUEST -> RefusalCopy(NO_SUCH_JOIN_REQUEST)
    FfiRefusalReason.JOINER_ALREADY_A_MEMBER -> RefusalCopy(JOINER_ALREADY_A_MEMBER)
    FfiRefusalReason.ROLE_CHANGE_PENDING -> RefusalCopy(ROLE_CHANGE_PENDING, RETRY_WHEN_IT_LANDS)
    FfiRefusalReason.PENDING_SET_FULL -> RefusalCopy(PENDING_SET_FULL, RETRY_WHEN_THEY_LAND)
    FfiRefusalReason.DEVICE_LIMIT_REACHED -> RefusalCopy(DEVICE_LIMIT_REACHED)
    FfiRefusalReason.DEVICE_BINDING_INVALID -> RefusalCopy(DEVICE_BINDING_INVALID)

    // The recorded collapses. Listed individually rather than as an `else` so
    // that adding a reason to the core forces a decision here.
    FfiRefusalReason.NOT_A_MEMBER,
    FfiRefusalReason.LEAF_UPDATE_REFUSED,
    FfiRefusalReason.UNSUPPORTED_PROPOSAL,
    FfiRefusalReason.NO_ROLE_TABLE,
    FfiRefusalReason.ROLE_TABLE_WOULD_VANISH,
    FfiRefusalReason.NOT_A_COMMITTER,
    FfiRefusalReason.PROPOSALS_PENDING,
    FfiRefusalReason.SWEEP_ALREADY_STAGED,
    -> RefusalCopy(MEMBERSHIP_REFUSAL_FALLBACK)
}

/**
 * Whether [reason] has a sentence of its own, as opposed to sharing §6's
 * fail-closed row. For the completeness assertion, not for branching on.
 */
internal fun membershipRefusalHasOwnCopy(reason: FfiRefusalReason): Boolean =
    refusalCopy(reason).fact != MEMBERSHIP_REFUSAL_FALLBACK

// ---------------------------------------------------------------------------
// The verdict
// ---------------------------------------------------------------------------

/** A refused membership change, ready to render. */
internal data class MembershipRefusal(
    /** The core's reason. Carry it into a log; never show it. */
    val reason: FfiRefusalReason,
    /** The sentence to show. Never empty. */
    val message: String,
    /**
     * The core's answer to *will the identical call work later*. Drives the
     * retry affordance; never re-derived here.
     */
    val retryMaySucceed: Boolean,
)

/**
 * Build the copy for [reason], asking [retryMaySucceed] whether waiting helps.
 *
 * The port is a parameter rather than a direct call so the tests can prove
 * this *asks* instead of agreeing by coincidence — see the header, probe 1.
 * Production passes `::membershipRefusalRetryMaySucceed`.
 */
internal fun membershipRefusal(
    reason: FfiRefusalReason,
    retryMaySucceed: (FfiRefusalReason) -> Boolean,
): MembershipRefusal {
    val copy = refusalCopy(reason)
    val retry = retryMaySucceed(reason)
    val message =
        if (retry && copy.invitation != null) "${copy.fact} ${copy.invitation}" else copy.fact
    return MembershipRefusal(reason = reason, message = message, retryMaySucceed = retry)
}

// ---------------------------------------------------------------------------
// Reading a refusal off a thrown value
// ---------------------------------------------------------------------------

/**
 * How deep to follow a `cause` chain. Four is well past anything this app
 * builds and keeps a cyclic cause from spinning.
 */
private const val MAX_WRAP_DEPTH = 4

/**
 * The core's refusal reason carried by [error], or `null` when [error] is not
 * a membership refusal.
 *
 * **It follows `cause`, and that is not defensive coding — it is the only
 * production path.** Every membership call leaves `ChatRepository` as a
 * [ChatError] whose `cause` is the `FfiException`, so reading just the
 * outermost throwable would make this whole table dead code on the one route
 * that reaches the UI.
 *
 * The `FfiException` `when` is an exhaustive expression with **no `else`**: a
 * forty-fourth subclass fails the build, which is what stops a future
 * `MembershipX` from silently falling through to "not a refusal".
 */
internal fun membershipRefusalReason(error: Throwable): FfiRefusalReason? {
    var current: Throwable? = error
    repeat(MAX_WRAP_DEPTH) {
        val here = current ?: return null
        if (here is FfiException) {
            return when (here) {
                is FfiException.MembershipNotAMember -> FfiRefusalReason.NOT_A_MEMBER
                is FfiException.MembershipAddNotOwnerOrAdmin ->
                    FfiRefusalReason.ADD_NOT_OWNER_OR_ADMIN
                is FfiException.MembershipRemoveNotOwnerOrAdmin ->
                    FfiRefusalReason.REMOVE_NOT_OWNER_OR_ADMIN
                is FfiException.MembershipOwnerUnremovable -> FfiRefusalReason.OWNER_UNREMOVABLE
                is FfiException.MembershipOwnerCannotLeave -> FfiRefusalReason.OWNER_CANNOT_LEAVE
                is FfiException.MembershipNotOwner -> FfiRefusalReason.NOT_OWNER
                is FfiException.MembershipLeafUpdateRefused -> FfiRefusalReason.LEAF_UPDATE_REFUSED
                is FfiException.MembershipUnsupportedProposal ->
                    FfiRefusalReason.UNSUPPORTED_PROPOSAL
                is FfiException.MembershipNoRoleTable -> FfiRefusalReason.NO_ROLE_TABLE
                is FfiException.MembershipOwnerNotAMember -> FfiRefusalReason.OWNER_NOT_A_MEMBER
                is FfiException.MembershipRoleTableWouldVanish ->
                    FfiRefusalReason.ROLE_TABLE_WOULD_VANISH
                is FfiException.MembershipNotACommitter -> FfiRefusalReason.NOT_A_COMMITTER
                is FfiException.MembershipRoleChangePending -> FfiRefusalReason.ROLE_CHANGE_PENDING
                is FfiException.MembershipProposalsPending -> FfiRefusalReason.PROPOSALS_PENDING
                is FfiException.MembershipPendingSetFull -> FfiRefusalReason.PENDING_SET_FULL
                is FfiException.MembershipNoSuchJoinRequest ->
                    FfiRefusalReason.NO_SUCH_JOIN_REQUEST
                is FfiException.MembershipDeviceBindingInvalid ->
                    FfiRefusalReason.DEVICE_BINDING_INVALID
                is FfiException.MembershipDeviceLimitReached ->
                    FfiRefusalReason.DEVICE_LIMIT_REACHED
                is FfiException.MembershipJoinerAlreadyAMember ->
                    FfiRefusalReason.JOINER_ALREADY_A_MEMBER
                is FfiException.SweepAlreadyStaged -> FfiRefusalReason.SWEEP_ALREADY_STAGED
                // ADR-0051 — a channel metadata record out of bounds; not a
                // membership refusal.
                is FfiException.ChannelMeta -> null
                // ADR-0054 - an anonymous-token issuance refusal; not a
                // membership refusal.
                is FfiException.AnonToken,
                is FfiException.AnonTokenAlreadyFinalized,
                -> null
                // ADR-0049 section 2 (device-link PAKE) and the sealed invite
                // links: not membership refusals.
                is FfiException.DeviceLink,
                is FfiException.DeviceLinkWrongCode,
                is FfiException.InviteLinkDamaged,
                -> null

                // Everything that is NOT a membership refusal. Enumerated
                // rather than defaulted, so a new `FfiException` subclass has
                // to be classified here instead of quietly reading as "some
                // other error".
                is FfiException.Identity,
                is FfiException.KeyPackage,
                is FfiException.Group,
                is FfiException.Message,
                is FfiException.Exporter,
                is FfiException.Codec,
                is FfiException.Unsupported,
                is FfiException.Auth,
                is FfiException.AuthAlreadyFinalized,
                is FfiException.Mnemonic,
                is FfiException.Pow,
                is FfiException.Discovery,
                is FfiException.DiscoveryAlreadyFinalized,
                is FfiException.Contacts,
                is FfiException.Crypto,
                is FfiException.Attachment,
                is FfiException.Shamir,
                is FfiException.Backup,
                is FfiException.BackupHeadStale,
                is FfiException.Channels,
                is FfiException.Call,
                is FfiException.Sfu,
                is FfiException.Transport,
                is FfiException.Release,
                is FfiException.LockPoisoned,
                // ADR-0048 — name-rule refusals (username / display name).
                // Input errors, never membership refusals.
                is FfiException.UsernameEmpty,
                is FfiException.UsernameTooShort,
                is FfiException.UsernameTooLong,
                is FfiException.UsernameInvalidCharacters,
                is FfiException.DisplayNameLineBreak,
                is FfiException.DisplayNameControlOrFormatCharacter,
                is FfiException.DisplayNameTooLong,
                is FfiException.DisplayNameTooManyBytes,
                -> null
            }
        }
        current = here.cause
    }
    return null
}

/**
 * The sentence to show for [error], or `null` when it is not a membership
 * refusal and its own message should stand.
 *
 * `null` and the fallback are different answers and the difference matters.
 * `null` means *this is some other error* — a dead socket, a bootstrap mistake
 * — and its own message is the best thing to show. The fallback means *this is
 * a refusal whose reason this build has no sentence for*, and showing the
 * core's developer prose there would leak an internal reason §6 row 7 forbids
 * leaking.
 */
internal fun membershipRefusalMessage(
    error: Throwable,
    retryMaySucceed: (FfiRefusalReason) -> Boolean,
): String? {
    val reason = membershipRefusalReason(error) ?: return null
    return membershipRefusal(reason, retryMaySucceed).message
}

// ---------------------------------------------------------------------------
// ADR-0027 — an external-join request the quarantine did not keep
// ---------------------------------------------------------------------------

/** What a client may offer for one external-join request. */
internal sealed interface ExternalJoinAffordance {
    /** Approve / Reject may be offered. */
    data object Actionable : ExternalJoinAffordance

    /** Not actionable. [notice] is the most that may be shown. */
    data class Informational(val notice: String) : ExternalJoinAffordance
}

/**
 * Whether an external-join request may be offered with Approve / Reject.
 *
 * The core fires `ExternalJoinProposal` **even when the quarantine was full
 * and the request was not kept**. Approving such a request has nothing to look
 * up and is refused with `NO_SUCH_JOIN_REQUEST`, so an Approve button for it is
 * an affordance that cannot work — the same class of dishonesty as a cosmetic
 * security badge, which is why the honesty rule reaches a members panel and not
 * only a lock icon.
 *
 * All three clients build their pending list from the core's retained set, so
 * an unretained request is *already* absent and no Approve renders. What was
 * missing is the other half: the request is then invisible, and the joiner is
 * left waiting on an admin who was never told. This returns something to say
 * rather than nothing.
 */
internal fun externalJoinAffordance(retained: Boolean): ExternalJoinAffordance =
    if (retained) {
        ExternalJoinAffordance.Actionable
    } else {
        ExternalJoinAffordance.Informational(JOIN_REQUEST_NOT_KEPT)
    }
