// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// MARK: - ADR-0045 UX §6 / §12 item 6 the typed membership-refusal table
//
// ## What this is for
//
// §12 item 6 has been open since the spec was written: *"Prose-matching
// `Error::Group` strings across three clients would work and would be wrong."*
// The clients did the defensible thing and built no table, so a core refusal
// reached the user as the core's own developer sentence — on iOS, wrapped
// twice over, as `proposeAddMemberFromWire failed: Group(message: "add
// proposal refused: proposer is not owner or admin")`.
//
// The core now carries a machine-readable `RefusalReason` beside that
// sentence. This module is the client half: reason in, sentence out.
// **Nothing here looks at a message.**
//
// ## Why there is a variant -> reason bridge, and why it is not prose matching
//
// `#[uniffi(flat_error)]` lowers ONLY a variant's discriminant and its
// `Display` string, dropping associated data — so the reason cannot ride as a
// field on `FfiError`, and the variant is the one zero-parse channel the type
// has. ``membershipRefusalReason(of:)`` is therefore a mechanical map from
// `FfiError.MembershipX` to `FfiRefusalReason.x`. It is compiler-checked and
// exhaustive, which is exactly what a substring match on `.localizedDescription`
// is not: a renamed variant fails the build, a reworded sentence changes
// nothing.
//
// ## The `default` arms that are deliberately absent
//
// Both switches below are exhaustive with **no wildcard**, so a nineteenth
// `FfiRefusalReason` or a fifty-seventh `FfiError` case fails the BUILD. That
// is the whole "a new core reason cannot silently render blank" guarantee on
// this platform, and it evaporates the moment somebody adds a `default:` to
// quiet the compiler. Do not.
//
// ## The retry affordance is the CORE's answer
//
// Whether waiting helps is asked of `membershipRefusalRetryMaySucceed` and
// never restated here. The rule is narrow and easy to get wrong in a way
// nothing catches: it is **not** the complement of "permitted" — most
// non-permission refusals are still not retryable — and a client inferring it
// from a case name would loop a user on `proposalsPending` (which sounds
// transient and is not) or tell them they may not do something they may on
// `pendingSetFull`.
//
// So the invitation to try again is **appended** rather than stored, and only
// when the port says so. A stored sentence would agree with the core by
// coincidence and keep agreeing after the core changed its mind.
//
// ## Which rules here actually discriminate (MEASURED each probe was run)
//
// Each was introduced as a deliberate regression, `xcodebuild test` was run,
// the failure below observed, and the probe reverted.
//
// 1. **The invitation is stored in the sentence instead of appended from the
//    port** i.e. the module decides retryability itself:
//
//        testItFollowsThePortEvenWhenThePortIsWrong : XCTAssertFalse failed -
//        roleChangePending
//
// 2. **`retryMaySucceed` is computed from a local list** instead of from the
//    port — passes every real-core test, fails only the inverted one:
//
//        testItFollowsThePortEvenWhenThePortIsWrong : XCTAssertFalse failed -
//        roleChangePending
//
// 3. **The bridge unwraps only `FfiError` and not `ChatError.wrapped`** —
//    which is the whole production path:
//
//        testItFindsTheReasonThroughTheWrapperTheRepositoryThrows :
//        XCTAssertEqual failed: ("nil") is not equal to
//        ("Optional(addNotOwnerOrAdmin)")
//
// 4. **Two bridge arms transposed** (`ownerUnremovable` <-> `ownerCannotLeave`):
//
//        testEveryRefusalVariantBridgesToItsReason : XCTAssertEqual failed:
//        ("Optional(ownerCannotLeave)") is not equal to
//        ("Optional(ownerUnremovable)")
//
// 5. **`externalJoinAffordance` ignores `retained`**:
//
//        testAnUnretainedJoinRequestIsNeverActionable : XCTAssertEqual failed:
//        ("actionable") is not equal to ("informational(...)")

// MARK: - Copy

/// §6 row 7 — the fail-closed fallback, and the default for **anything** this
/// build does not recognise, a reason from a future core included.
///
/// "It must exist, must never leak an internal reason, and must never be
/// reached in normal use." The third clause is the one worth holding onto: if
/// this string starts appearing, something is wrong that is not a copy problem.
public var membershipRefusalFallbackMessage: String { Localization.text("chat.refusal.fallback") }

/// §6 row 1, verbatim.
public var addNotOwnerOrAdminMessage: String { Localization.text("chat.refusal.add_not_admin") }

/// §6 row 2, verbatim.
public var removeNotOwnerOrAdminMessage: String { Localization.text("chat.refusal.remove_not_admin") }

/// §6 row 3, verbatim.
///
/// It also answers an owner who tapped their own `Leave group`: the core checks
/// *is this leaf the owner's* before it checks anything about roles, so an
/// owner's self-removal reports this and not `ownerCannotLeave`. §6.1.1
/// disables that control, but the string is right if it is reached anyway.
public var ownerUnremovableMessage: String { Localization.text("chat.refusal.owner_unremovable") }

/// §6 row 4, verbatim.
///
/// **Nothing can reach this today, and it keeps its sentence anyway.** The
/// reason fires only for a `Proposal::SelfRemove` authored by an owner, and
/// `SelfRemove` is unreachable in Lakko at the **wire-format** level, not
/// merely absent from the FFI: RFC 9420 requires it to be sent as a
/// `PublicMessage`, `Group::create` leaves the OpenMLS policy at its
/// pure-ciphertext default, and OpenMLS therefore refuses to craft one
/// (`CannotSelfRemoveWithPureCiphertext`). So no peer can send one either.
/// `Leave group` calls `proposeRemoveMember` with its own leaf, which the core
/// classifies as a `Remove` and — for an owner — answers with
/// ``ownerUnremovableMessage``, since the owner check runs before the role
/// check. What an owner actually sees is the §6.1.1 pre-check, on a disabled
/// control that never calls the core.
///
/// Collapsing it onto the fallback would have been defensible on reachability
/// alone and is still the wrong trade, for a reason the core supplies:
/// `self_removal_is_gated_for_its_own_sender_and_refused_for_the_owner` exists
/// precisely to fail the day somebody changes that wire-format policy, and
/// that change is wire-visible (it stops encrypting handshake messages to the
/// zero-trust DS). The day it fires, this reason becomes reachable and the copy
/// is already right. A collapse would instead degrade a precise refusal into
/// the fail-closed row nobody is supposed to see, silently.
public var ownerCannotLeaveMessage: String { Localization.text("chat.refusal.owner_cannot_leave") }

/// §6 row 5, **widened** — the one existing row this table does not take
/// verbatim, and the reason is in the core rather than in taste.
///
/// §6 says `Only the group owner can change admins.` The core raises ONE
/// reason (`notOwner`) for a role-table change *and* for an ownership
/// transfer, because a transfer **is** a role-table rewrite — not a separate
/// commit type, just the owner slot moved. One reason takes one string, and a
/// string naming only admins would be plainly wrong for half of what the
/// reason covers.
public var notOwnerMessage: String { Localization.text("chat.refusal.not_owner") }

/// §6 row 6, its **fact** half. The invitation is ``retryWhenItLandsMessage``
/// and is appended only when the core says waiting helps; joined, the two are
/// §6 row 6 verbatim, which a test asserts.
///
/// §6 argues at length that this is the one *availability* row beside the
/// authorisation ones, and that getting the distinction backwards is a real
/// defect in both directions. Splitting the sentence is how that argument
/// becomes mechanical instead of remembered.
public var roleChangePendingMessage: String { Localization.text("chat.refusal.role_change_pending") }

/// NEW — §7.1 names this refusal and says plainly that turning it into fixed
/// copy "needs the typed refusal table §12 already lists as unbuilt".
///
/// It deliberately states **no number.** `MAX_PENDING_PROPOSALS` is
/// `pub(crate)` and reaches neither FFI surface; §7.1 records that a
/// client-side literal is the stale-drift class this repo has already been
/// bitten by, and that it fails in the dangerous direction — reading as *room
/// left* when there is none.
public var pendingSetFullMessage: String { Localization.text("chat.refusal.pending_full") }

/// NEW — reachable from the §6.1 transfer picker when the chosen member has
/// left the group between the picker rendering and the tap.
///
/// Phrased as the standing rule rather than as *that person is gone*, because
/// the reason also covers a role-table rewrite whose owner slot names a
/// non-member, where there is no "that person" the user chose.
public var ownerNotAMemberMessage: String { Localization.text("chat.refusal.owner_not_member") }

/// NEW — a stale-view refusal, and the one an admin hits after tapping an
/// affordance that should no longer have been on screen. Not phrased as an
/// error, because nothing went wrong.
public var noSuchJoinRequestMessage: String { Localization.text("chat.refusal.no_join_request") }

/// NEW — two admins approving the same joiner is ordinary. Nothing is wrong
/// and nothing needs doing.
public var joinerAlreadyAMemberMessage: String { Localization.text("chat.refusal.joiner_member") }

/// §6 row 6's second half — singular, for the one pending role change.
/// ADR-0050 — the per-account device cap (`MAX_LEAVES_PER_ACCOUNT`).
public var deviceLimitReachedMessage: String {
    Localization.text("chat.refusal.device_limit", counts: ["count": 8])
}
/// ADR-0050 — a device whose binding the group refuses (docs/ux/second-device.md
/// section 5: a client bug, one plain sentence rather than a screen).
public var deviceBindingInvalidMessage: String { Localization.text("chat.refusal.device_binding") }

/// ADR-0062 — a chat (one-to-one) is always the same two people. No screen
/// offers add / remove / leave on a chat, so this is reached only by a stale
/// view; stated as the standing rule. Same copy as web and Android.
public var oneToOneMembershipFixedMessage: String { Localization.text("chat.refusal.one_to_one_fixed") }

/// ADR-0062 — a chat has no owner or admins. Same copy as web and Android.
public var oneToOneHasNoRolesMessage: String { Localization.text("chat.refusal.one_to_one_no_roles") }

/// ADR-0062 — a change that would alter or drop the conversation's kind,
/// refused by the peer gate. Same copy as web and Android.
public var groupKindChangedMessage: String { Localization.text("chat.refusal.group_kind_changed") }

public var retryWhenItLandsMessage: String { Localization.text("chat.refusal.retry_one") }

/// ``pendingSetFullMessage``'s second half — plural, for a queue of them. Two
/// invitations rather than one because "wait for it to land" after "too many
/// changes" has no referent.
public var retryWhenTheyLandMessage: String { Localization.text("chat.refusal.retry_many") }

/// NEW — the state §7.1 recorded as unsurfaceable.
///
/// "At 64 retained external joins `retain_external_join` silently stops
/// retaining, and nothing distinguishes that from a request that never
/// arrived." The core's `retained` flag **is** that distinction, so the state
/// is statable now and silence stopped being the honest option. Naming no
/// number, for the ``pendingSetFullMessage`` reason.
public var joinRequestNotKeptMessage: String { Localization.text("chat.refusal.join_not_kept") }

// MARK: - The table

/// Every ``FfiRefusalReason``, in the core's declaration order.
///
/// Hand-listed because a uniffi-generated Swift enum is not `CaseIterable`. It
/// is checked by count and by set-uniqueness in the test, and the *copy* switch
/// below is exhaustive — so a nineteenth reason fails the build there even if
/// somebody forgets to extend this array.
public let allMembershipRefusalReasons: [FfiRefusalReason] = [
    .notAMember,
    .addNotOwnerOrAdmin,
    .removeNotOwnerOrAdmin,
    .ownerUnremovable,
    .ownerCannotLeave,
    .notOwner,
    .leafUpdateRefused,
    .unsupportedProposal,
    .noRoleTable,
    .ownerNotAMember,
    .roleTableWouldVanish,
    .notACommitter,
    .noSuchJoinRequest,
    .joinerAlreadyAMember,
    .roleChangePending,
    .proposalsPending,
    .pendingSetFull,
    .sweepAlreadyStaged,
    .deviceBindingInvalid,
    .deviceLimitReached,
    .oneToOneMembershipFixed,
    .oneToOneHasNoRoles,
    .groupKindChanged,
]

/// The reasons that deliberately share §6's fail-closed row.
///
/// **Why this is a list rather than an absence.** Falling back is what the
/// copy switch does for a reason with no sentence, so a reason nobody wrote
/// copy for renders a correct-looking string either way. Recording the
/// collapse turns that into an assertion: this set plus the reasons with their
/// own sentence must be exactly the core's set.
///
/// The rule behind every entry is the same — **a reason with no
/// user-initiated entry point gets no sentence** — and each still earns its
/// place separately:
///
/// - `notAMember`, `unsupportedProposal`, `noRoleTable` — §6 row 7 names these
///   three explicitly ("unsupported / unresolvable / no role table"). No
///   change; the spec already decided them.
/// - `leafUpdateRefused` — the core states it is reachable from no
///   client-facing propose API; it exists so a *log* can tell it apart from
///   `unsupportedProposal`. To a user the two are the same fact, and the
///   distinction survives where it is useful, in the wrapped message.
/// - `roleTableWouldVanish` — a role-table rewrite leaving no readable table
///   behind, which no client can compose from its own UI. A peer-gate refusal
///   on somebody else's commit.
/// - `notACommitter`, `sweepAlreadyStaged` — sweep-side. §8: the committer
///   sees "in the steady state, nothing", and `ChatRepository` swallows sweep
///   failures by design. A sentence here would have no reader.
/// - `proposalsPending` — refuses a **direct** commit, and a client takes the
///   direct path only in a group with no role table, where the pending set is
///   permanently empty. Unreachable by construction rather than by policy.
public let membershipRefusalsCollapsedToFallback: Set<FfiRefusalReason> = [
    .notAMember,
    .leafUpdateRefused,
    .unsupportedProposal,
    .noRoleTable,
    .roleTableWouldVanish,
    .notACommitter,
    .proposalsPending,
    .sweepAlreadyStaged,
]

/// The fact half of a reason's copy, plus the invitation to try again when
/// there is one.
private struct RefusalCopy {
    /// The statement of fact. Always shown.
    let fact: String
    /// Appended after a space **only** when the core says a plain retry may
    /// succeed. `nil` for every reason the core never calls retryable.
    let invitation: String?

    init(_ fact: String, invitation: String? = nil) {
        self.fact = fact
        self.invitation = invitation
    }
}

/// Reason -> copy.
///
/// **Exhaustive, with no `default`.** A nineteenth `FfiRefusalReason` fails
/// the build here, which is the guarantee this module exists to provide.
private func refusalCopy(_ reason: FfiRefusalReason) -> RefusalCopy {
    switch reason {
    case .addNotOwnerOrAdmin: return RefusalCopy(addNotOwnerOrAdminMessage)
    case .removeNotOwnerOrAdmin: return RefusalCopy(removeNotOwnerOrAdminMessage)
    case .ownerUnremovable: return RefusalCopy(ownerUnremovableMessage)
    case .ownerCannotLeave: return RefusalCopy(ownerCannotLeaveMessage)
    case .notOwner: return RefusalCopy(notOwnerMessage)
    case .ownerNotAMember: return RefusalCopy(ownerNotAMemberMessage)
    case .noSuchJoinRequest: return RefusalCopy(noSuchJoinRequestMessage)
    case .joinerAlreadyAMember: return RefusalCopy(joinerAlreadyAMemberMessage)
    case .roleChangePending:
        return RefusalCopy(roleChangePendingMessage, invitation: retryWhenItLandsMessage)
    case .pendingSetFull:
        return RefusalCopy(pendingSetFullMessage, invitation: retryWhenTheyLandMessage)
    case .deviceLimitReached: return RefusalCopy(deviceLimitReachedMessage)
    case .deviceBindingInvalid: return RefusalCopy(deviceBindingInvalidMessage)
    case .oneToOneMembershipFixed: return RefusalCopy(oneToOneMembershipFixedMessage)
    case .oneToOneHasNoRoles: return RefusalCopy(oneToOneHasNoRolesMessage)
    case .groupKindChanged: return RefusalCopy(groupKindChangedMessage)

    // The recorded collapses. Listed individually rather than as a `default`
    // so that adding a reason to the core forces a decision here.
    case .notAMember, .leafUpdateRefused, .unsupportedProposal, .noRoleTable,
        .roleTableWouldVanish, .notACommitter, .proposalsPending, .sweepAlreadyStaged:
        return RefusalCopy(membershipRefusalFallbackMessage)
    }
}

/// Whether `reason` has a sentence of its own, as opposed to sharing §6's
/// fail-closed row. For the completeness assertion, not for branching on.
public func membershipRefusalHasOwnCopy(_ reason: FfiRefusalReason) -> Bool {
    refusalCopy(reason).fact != membershipRefusalFallbackMessage
}

// MARK: - The verdict

/// A refused membership change, ready to render.
public struct MembershipRefusal: Equatable {
    /// The core's reason. Carry it into a log; never show it.
    public let reason: FfiRefusalReason
    /// The sentence to show. Never empty.
    public let message: String
    /// The core's answer to *will the identical call work later*. Drives the
    /// retry affordance; never re-derived here.
    public let retryMaySucceed: Bool
}

/// Build the copy for `reason`, asking `retryMaySucceed` whether waiting helps.
///
/// The port is a parameter rather than a direct call so the tests can prove
/// this *asks* instead of agreeing by coincidence — see the header, probe 2.
/// Production passes ``membershipRefusalRetryMaySucceed``.
public func membershipRefusal(
    reason: FfiRefusalReason,
    retryMaySucceed: (FfiRefusalReason) -> Bool
) -> MembershipRefusal {
    let copy = refusalCopy(reason)
    let retry = retryMaySucceed(reason)
    let message: String
    if retry, let invitation = copy.invitation {
        message = Localization.text(
            "chat.refusal.with_retry", texts: ["fact": copy.fact, "retry": invitation])
    } else {
        message = copy.fact
    }
    return MembershipRefusal(reason: reason, message: message, retryMaySucceed: retry)
}

// MARK: - Reading a refusal off a thrown error

/// How deep to follow a wrapper chain. Four is well past anything this app
/// builds and keeps a pathological nesting from spinning.
private let maxWrapDepth = 4

/// The core's refusal reason carried by `error`, or `nil` when `error` is not
/// a membership refusal.
///
/// **It unwraps ``ChatError/wrapped(_:underlying:)``, and that is not
/// defensive coding — it is the only production path.** Every membership call
/// leaves `ChatRepository` inside that case, so reading just the outermost
/// error would make this whole table dead code on the one route that reaches
/// the UI.
///
/// The `FfiError` switch is exhaustive with **no `default`**: a fifty-seventh
/// variant fails the build, which is what stops a future `MembershipX` case
/// from silently falling through to "not a refusal".
public func membershipRefusalReason(of error: Error) -> FfiRefusalReason? {
    var current = error
    for _ in 0..<maxWrapDepth {
        if let ffi = current as? FfiError {
            switch ffi {
            case .MembershipNotAMember: return .notAMember
            case .MembershipAddNotOwnerOrAdmin: return .addNotOwnerOrAdmin
            case .MembershipRemoveNotOwnerOrAdmin: return .removeNotOwnerOrAdmin
            case .MembershipOwnerUnremovable: return .ownerUnremovable
            case .MembershipOwnerCannotLeave: return .ownerCannotLeave
            case .MembershipNotOwner: return .notOwner
            case .MembershipLeafUpdateRefused: return .leafUpdateRefused
            case .MembershipUnsupportedProposal: return .unsupportedProposal
            case .MembershipNoRoleTable: return .noRoleTable
            case .MembershipOwnerNotAMember: return .ownerNotAMember
            case .MembershipRoleTableWouldVanish: return .roleTableWouldVanish
            case .MembershipNotACommitter: return .notACommitter
            case .MembershipRoleChangePending: return .roleChangePending
            case .MembershipProposalsPending: return .proposalsPending
            case .MembershipPendingSetFull: return .pendingSetFull
            case .MembershipNoSuchJoinRequest: return .noSuchJoinRequest
            case .MembershipJoinerAlreadyAMember: return .joinerAlreadyAMember
            case .SweepAlreadyStaged: return .sweepAlreadyStaged
            case .MembershipDeviceBindingInvalid: return .deviceBindingInvalid
            case .MembershipDeviceLimitReached: return .deviceLimitReached
            // ADR-0062 — the conversation-kind refusals.
            case .MembershipOneToOneMembershipFixed: return .oneToOneMembershipFixed
            case .MembershipOneToOneHasNoRoles: return .oneToOneHasNoRoles
            case .MembershipGroupKindChanged: return .groupKindChanged

            // Everything that is NOT a membership refusal. Enumerated rather
            // than defaulted, so a new `FfiError` variant has to be classified
            // here instead of quietly reading as "some other error".
            case .Identity, .KeyPackage, .Group, .Message, .Exporter, .Codec, .Unsupported,
                .Auth, .AuthAlreadyFinalized, .Mnemonic, .Pow, .Discovery,
                .DiscoveryAlreadyFinalized, .Contacts, .Crypto, .Attachment, .Backup,
                .BackupHeadStale, .Channels, .Call, .Sfu, .Transport, .Release, .LockPoisoned,
                .UsernameEmpty, .UsernameTooShort, .UsernameTooLong, .UsernameInvalidCharacters,
                .DisplayNameLineBreak, .DisplayNameControlOrFormatCharacter,
                .DisplayNameTooLong, .DisplayNameTooManyBytes, .ChannelMeta,
                .AnonToken, .AnonTokenAlreadyFinalized,
                // ADR-0058 synced state; the 2026-09-28 device link and
                // sealed invite links; ADR-0060 scoped discovery.
                .DeviceGroupPayloadRefused, .SyncedState, .DeviceLinkWrongCode, .DeviceLink,
                .InviteLinkDamaged, .Reach, .Introduction, .Visibility, .UsernameCertificate,
                .CommitterDelegation,
                // ADR-0063 feed rules; ADR-0059 channel listings and
                // co-publisher bindings; ADR-0061 interface localization.
                .FeedRule, .ChannelListing, .ChannelCoPublisherBinding, .L10n,
                // The local three-word check and the passkey vault.
                .PhraseCheck, .PasskeyVaultNotOpened, .PasskeyVault,
                // ADR-0065 key-reset attestation (verify only).
                .KeyResetAttestation, .KeyResetAttestationStale, .AccountStatement,
                // ADR-0066 key-rotation envelope and root endorsement.
                .KeyRotation,
                // ADR-0067 the email vault and the recovery-email address.
                .EmailVaultNotOpened, .EmailVault, .RecoveryEmail:
                return nil
            }
        }
        guard case .wrapped(_, let underlying)? = current as? ChatError else { return nil }
        current = underlying
    }
    return nil
}

/// The sentence to show for `error`, or `nil` when it is not a membership
/// refusal and its own message should stand.
///
/// `nil` and the fallback are different answers and the difference matters.
/// `nil` means *this is some other error* — a dead socket, a bootstrap mistake
/// — and its own message is the best thing to show. The fallback means *this
/// is a refusal whose reason this build has no sentence for*, and showing the
/// core's developer prose there would leak an internal reason §6 row 7 forbids
/// leaking.
public func membershipRefusalMessage(
    for error: Error,
    retryMaySucceed: (FfiRefusalReason) -> Bool
) -> String? {
    guard let reason = membershipRefusalReason(of: error) else { return nil }
    return membershipRefusal(reason: reason, retryMaySucceed: retryMaySucceed).message
}

// MARK: - ADR-0027 an external-join request the quarantine did not keep

/// What a client may offer for one external-join request.
public enum ExternalJoinAffordance: Equatable {
    /// Approve / Reject may be offered.
    case actionable
    /// Not actionable. The notice is the most that may be shown.
    case informational(String)
}

/// Whether an external-join request may be offered with Approve / Reject.
///
/// The core fires `ExternalJoinProposal` **even when the quarantine was full
/// and the request was not kept**. Approving such a request has nothing to
/// look up and is refused with `noSuchJoinRequest`, so an Approve button for
/// it is an affordance that cannot work — the same class of dishonesty as a
/// cosmetic security badge, which is why the honesty rule reaches a members
/// panel and not only a lock icon.
///
/// All three clients build their pending list from the core's retained set, so
/// an unretained request is *already* absent and no Approve renders. What was
/// missing is the other half: the request is then invisible, and the joiner is
/// left waiting on an admin who was never told. This returns something to say
/// rather than nothing.
public func externalJoinAffordance(retained: Bool) -> ExternalJoinAffordance {
    retained ? .actionable : .informational(joinRequestNotKeptMessage)
}
