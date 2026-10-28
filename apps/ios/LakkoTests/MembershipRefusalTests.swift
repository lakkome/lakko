// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0045 UX §6 / §12 item 6 — the typed membership-refusal table.
///
/// The test target drives rules, not views, so what is pinned here is the
/// DECISION — which sentence answers which core refusal, and whether the user
/// may be invited to try again — never the widget.
///
/// ## What the COMPILER pins that this file cannot
///
/// `membershipRefusalCopy(_:)` and `membershipRefusalReason(of:)` are
/// exhaustive `switch`es with **no `default` arm**, so a nineteenth
/// `FfiRefusalReason` or a forty-fourth `FfiError` case fails the BUILD rather
/// than a test. That is the strongest form of the "a new core reason cannot
/// silently render blank" requirement available on this platform, and it is
/// why neither function is written with a wildcard. Do not add one.
///
/// The discrimination evidence for each rule (each probe was RUN, the failure
/// observed, and the probe reverted) is recorded in `MembershipRefusal.swift`.
@MainActor
final class MembershipRefusalTests: XCTestCase {

    override func setUp() {
        super.setUp()
        // A failed assertion must ABORT this test rather than let the rest of
        // it run against a state the failure already invalidated.
        continueAfterFailure = false
    }

    /// The real core port — what production passes.
    private let real = membershipRefusalRetryMaySucceed

    private func say(_ reason: FfiRefusalReason) -> String {
        membershipRefusal(reason: reason, retryMaySucceed: real).message.withoutIsolates
    }

    // MARK: - Coverage

    /// ADR-0081 amendment (2026-10-19) — the direct-commit path is gone: a
    /// membership operation on a group with no role table is refused with the
    /// core's typed `NoRoleTable`, never committed directly. No client can
    /// make such a group (and `Group.joinFromWelcome` refuses its Welcome),
    /// so the rule is driven over an injected role-table read.
    func testAMembershipOperationOnAGroupWithNoRoleTableIsRefusedTyped() throws {
        XCTAssertThrowsError(try ChatRepository.requireRoleTable(op: "add") { nil }) { error in
            XCTAssertEqual(membershipRefusalReason(of: error), .noRoleTable, "\(error)")
        }
        XCTAssertThrowsError(try ChatRepository.requireRoleTable(op: "remove") { throw CocoaError(.fileReadUnknown) }) {
            XCTAssertEqual(membershipRefusalReason(of: $0), .noRoleTable, "a torn read refuses too")
        }
        XCTAssertNoThrow(try ChatRepository.requireRoleTable(op: "add") {
            RoleTable(owner: Data(repeating: 0x01, count: 32), admins: [])
        })
    }

    func testEveryReasonHasADecidedAnswer() {
        // `allMembershipRefusalReasons` is hand-listed, so it can fall out of
        // step with the enum — nothing in Swift checks that, and every
        // property test over the set is vacuous for a case that is missing.
        // So it is checked by asking each entry to find itself, and by count.
        XCTAssertEqual(allMembershipRefusalReasons.count, 23)
        XCTAssertEqual(Set(allMembershipRefusalReasons).count, 23)

        for reason in allMembershipRefusalReasons {
            let refusal = membershipRefusal(reason: reason, retryMaySucceed: real)
            XCTAssertFalse(
                refusal.message.isEmpty, "\(reason) rendered an empty sentence")
            // Every reason is EITHER given its own sentence OR recorded as a
            // deliberate collapse onto the fail-closed row. Falling back is
            // what an unconsidered reason would also do, so the decision is
            // what is asserted, not the output.
            XCTAssertTrue(
                membershipRefusalHasOwnCopy(reason)
                    || membershipRefusalsCollapsedToFallback.contains(reason),
                "\(reason) is a core refusal with no decision: give it a sentence, or record "
                    + "the collapse in `membershipRefusalsCollapsedToFallback`")
        }

        // Disjoint: a reason with its own sentence is not a collapse.
        for reason in membershipRefusalsCollapsedToFallback {
            XCTAssertFalse(membershipRefusalHasOwnCopy(reason), "\(reason) is in both halves")
        }
    }

    // MARK: - The copy

    func testItStatesTheSpecApprovedRowsVerbatim() {
        XCTAssertEqual(say(.addNotOwnerOrAdmin), "Only the group owner or an admin can add members.")
        XCTAssertEqual(
            say(.removeNotOwnerOrAdmin), "Only the group owner or an admin can remove members.")
        XCTAssertEqual(say(.ownerUnremovable), "The group owner cannot be removed.")
        XCTAssertEqual(say(.ownerCannotLeave), "The group owner cannot leave the group.")
        XCTAssertEqual(
            say(.roleChangePending),
            "A role change is already waiting to apply. Wait for it to land, then try again.")
        // §6 row 7, the fail-closed fallback.
        XCTAssertEqual(say(.unsupportedProposal), "This change could not be requested.")

        // Asserted against the exported constants too, so renaming a constant
        // cannot silently change a sentence.
        XCTAssertEqual(
            addNotOwnerOrAdminMessage, "Only the group owner or an admin can add members.")
        XCTAssertEqual(membershipRefusalFallbackMessage, "This change could not be requested.")
    }

    func testItSharesOneVocabularyWithTheLocalPreChecks() {
        // §6: "One string table, two entry points, no second vocabulary."
        // `LeaveGroup.swift` refuses locally without ever calling the core;
        // `ChatRepository` refuses a second role change the same way. A user
        // must not be able to tell which layer answered, so the pre-checks
        // read from the same constants rather than restating them.
        XCTAssertEqual(LeaveGroupRefusal.ownerCannotLeave.message, say(.ownerCannotLeave))
        XCTAssertEqual(LeaveGroupRefusal.notRequestable.message, membershipRefusalFallbackMessage)
        XCTAssertEqual(roleChangeAlreadyPendingMessage.withoutIsolates, say(.roleChangePending))
    }

    func testItSaysSomethingTrueForEachReasonTheSpecNeverNamed() {
        // §7.1 flags this one by name as needing the typed table.
        XCTAssertEqual(
            say(.pendingSetFull),
            "Too many changes are already waiting to apply. Wait for them to land, then try again.")
        // Reachable from the §6.1 transfer picker when the chosen member left.
        XCTAssertEqual(
            say(.ownerNotAMember), "Ownership can only be held by a current member of this group.")
        // Reachable from a stale Approve / Reject tap.
        XCTAssertEqual(say(.noSuchJoinRequest), "That join request is no longer waiting.")
        XCTAssertEqual(say(.joinerAlreadyAMember), "That person has already joined this group.")
        // ADR-0062 — the conversation-kind refusals, the same copy as web and
        // Android.
        XCTAssertEqual(say(.oneToOneMembershipFixed), "A chat is always the same two people.")
        XCTAssertEqual(say(.oneToOneHasNoRoles), "A chat has no admins.")
        XCTAssertEqual(say(.groupKindChanged), "This change can't be made to this conversation.")
        // §6 row 5, widened: the core gives ONE reason to a role change and an
        // ownership transfer, because a transfer IS a role-table rewrite. A
        // string naming only admins would be wrong for half of what it covers.
        XCTAssertEqual(
            say(.notOwner), "Only the group owner can change admins or transfer ownership.")
    }

    func testItCollapsesToTheFailClosedRowForEverythingAUserCannotActOn() {
        XCTAssertEqual(
            Set(membershipRefusalsCollapsedToFallback),
            Set([
                .notAMember, .leafUpdateRefused, .unsupportedProposal, .noRoleTable,
                .roleTableWouldVanish, .notACommitter, .proposalsPending, .sweepAlreadyStaged,
            ] as [FfiRefusalReason]))
        for reason in membershipRefusalsCollapsedToFallback {
            XCTAssertEqual(say(reason), membershipRefusalFallbackMessage, "\(reason)")
        }
    }

    func testItUsesNoProtocolVocabulary() {
        // §1: user-facing copy says `request` and `change`; never proposal,
        // commit, epoch, sweep, committer or leaf.
        let forbidden = ["proposal", "commit", "epoch", "sweep", "committer", "leaf"]
        for reason in allMembershipRefusalReasons {
            let text = say(reason).lowercased()
            for word in forbidden {
                XCTAssertFalse(text.contains(word), "\(reason): `\(word)` leaked into \(text)")
            }
        }
    }

    // MARK: - The retry affordance

    func testTheRetryableSetIsExactlyTheTwoFullQueues() {
        // The maintainer's rule: retry when a request was refused because a
        // queue was full, never when it was refused because of its own state.
        // Written as a literal expectation rather than derived from the port,
        // so a reason that changes disposition in the core has to move here.
        let retryable = allMembershipRefusalReasons.filter {
            membershipRefusal(reason: $0, retryMaySucceed: real).retryMaySucceed
        }
        XCTAssertEqual(retryable, [.roleChangePending, .pendingSetFull])
    }

    func testItFollowsThePortEvenWhenThePortIsWrong() {
        // The test that catches a client which hard-coded the partition. Such
        // a client agrees with the real core on every input, so no amount of
        // real-core testing would ever notice. The port is therefore inverted.
        let inverted: (FfiRefusalReason) -> Bool = { !membershipRefusalRetryMaySucceed(reason: $0) }

        for reason in [FfiRefusalReason.roleChangePending, .pendingSetFull] {
            let refusal = membershipRefusal(reason: reason, retryMaySucceed: inverted)
            XCTAssertFalse(refusal.retryMaySucceed, "\(reason)")
            XCTAssertFalse(refusal.message.contains("try again"), "\(reason)")
        }
        XCTAssertTrue(
            membershipRefusal(reason: .ownerUnremovable, retryMaySucceed: inverted).retryMaySucceed)
    }

    func testTheInvitationIsAppendedOnlyWhenThePortSaysSo() {
        // The invitation is not baked into the sentence: it is appended
        // because the CORE said waiting helps. With the port forced false the
        // fact survives and the invitation does not, which proves the sentence
        // is assembled from the core's answer rather than stored whole.
        let never: (FfiRefusalReason) -> Bool = { _ in false }
        XCTAssertEqual(
            membershipRefusal(reason: .roleChangePending, retryMaySucceed: never).message,
            roleChangePendingMessage)
        XCTAssertEqual(
            membershipRefusal(reason: .pendingSetFull, retryMaySucceed: never).message,
            pendingSetFullMessage)
        // Concatenation, not two unrelated strings: joined they are §6 row 6.
        XCTAssertEqual(
            "\(roleChangePendingMessage) \(retryWhenItLandsMessage)",
            roleChangeAlreadyPendingMessage.withoutIsolates
        )
    }

    // MARK: - Recognising a refusal at all

    func testEveryRefusalVariantBridgesToItsReason() {
        // The mechanical variant -> reason bridge. `#[uniffi(flat_error)]`
        // lowers only the discriminant and the Display string, so the reason
        // CANNOT ride as a field and the variant is the only zero-parse
        // channel. This asserts the bridge is wired the right way round for
        // all eighteen — a transposed pair here would show the wrong sentence
        // for a real refusal, silently.
        let pairs: [(FfiError, FfiRefusalReason)] = [
            (.MembershipNotAMember(message: "x"), .notAMember),
            (.MembershipAddNotOwnerOrAdmin(message: "x"), .addNotOwnerOrAdmin),
            (.MembershipRemoveNotOwnerOrAdmin(message: "x"), .removeNotOwnerOrAdmin),
            (.MembershipOwnerUnremovable(message: "x"), .ownerUnremovable),
            (.MembershipOwnerCannotLeave(message: "x"), .ownerCannotLeave),
            (.MembershipNotOwner(message: "x"), .notOwner),
            (.MembershipLeafUpdateRefused(message: "x"), .leafUpdateRefused),
            (.MembershipUnsupportedProposal(message: "x"), .unsupportedProposal),
            (.MembershipNoRoleTable(message: "x"), .noRoleTable),
            (.MembershipOwnerNotAMember(message: "x"), .ownerNotAMember),
            (.MembershipRoleTableWouldVanish(message: "x"), .roleTableWouldVanish),
            (.MembershipNotACommitter(message: "x"), .notACommitter),
            (.MembershipRoleChangePending(message: "x"), .roleChangePending),
            (.MembershipProposalsPending(message: "x"), .proposalsPending),
            (.MembershipPendingSetFull(message: "x"), .pendingSetFull),
            (.MembershipNoSuchJoinRequest(message: "x"), .noSuchJoinRequest),
            (.MembershipJoinerAlreadyAMember(message: "x"), .joinerAlreadyAMember),
            (.SweepAlreadyStaged(message: "x"), .sweepAlreadyStaged),
            (.MembershipDeviceBindingInvalid(message: "x"), .deviceBindingInvalid),
            (.MembershipDeviceLimitReached(message: "x"), .deviceLimitReached),
            (.MembershipOneToOneMembershipFixed(message: "x"), .oneToOneMembershipFixed),
            (.MembershipOneToOneHasNoRoles(message: "x"), .oneToOneHasNoRoles),
            (.MembershipGroupKindChanged(message: "x"), .groupKindChanged),
        ]
        XCTAssertEqual(pairs.count, allMembershipRefusalReasons.count)
        XCTAssertEqual(Set(pairs.map(\.1)), Set(allMembershipRefusalReasons))
        for (error, reason) in pairs {
            XCTAssertEqual(membershipRefusalReason(of: error), reason, "\(error)")
        }
    }

    func testItFindsTheReasonThroughTheWrapperTheRepositoryThrows() {
        // Every membership call leaves `ChatRepository` as
        // `ChatError.wrapped(_, underlying:)`, which is what reaches the view
        // model. Without this the whole table would be dead code on the one
        // path that reaches the UI.
        let wrapped = ChatError.wrapped(
            "proposeAddMemberFromWire failed",
            underlying: FfiError.MembershipAddNotOwnerOrAdmin(message: "core prose"))
        XCTAssertEqual(membershipRefusalReason(of: wrapped), .addNotOwnerOrAdmin)
        XCTAssertEqual(
            membershipRefusalMessage(for: wrapped, retryMaySucceed: real), addNotOwnerOrAdminMessage)
    }

    func testItLeavesANonRefusalAlone() {
        // A network error, a codec error, a bootstrap mistake: these keep
        // their own message. Turning one into "This change could not be
        // requested." would be a worse lie than the raw text.
        XCTAssertNil(membershipRefusalReason(of: FfiError.Codec(message: "bad bytes")))
        XCTAssertNil(membershipRefusalReason(of: ChatError.argument("no live Group handle")))
        XCTAssertNil(
            membershipRefusalMessage(
                for: FfiError.Codec(message: "bad bytes"), retryMaySucceed: real))
        struct Unrelated: Error {}
        XCTAssertNil(membershipRefusalReason(of: Unrelated()))
    }

    // MARK: - ADR-0027 — a request the quarantine did not keep

    func testAnUnretainedJoinRequestIsNeverActionable() {
        // The core fires `ExternalJoinProposal` even when the quarantine was
        // full and the request was NOT retained; approving it then refuses
        // with `noSuchJoinRequest`. An Approve button for it is an affordance
        // that cannot work.
        XCTAssertEqual(
            externalJoinAffordance(retained: false), .informational(joinRequestNotKeptMessage))
        XCTAssertEqual(externalJoinAffordance(retained: true), .actionable)
    }

    func testItSaysWhatHappenedInsteadOfShowingNothing() {
        // §7.1 recorded this as unsurfaceable: "at 64 retained external joins
        // `retain_external_join` silently stops retaining, and nothing
        // distinguishes that from a request that never arrived". `retained` is
        // exactly that distinction, so silence stopped being honest.
        XCTAssertEqual(
            joinRequestNotKeptMessage,
            "A join request arrived while too many were already waiting, so it was not kept. "
                + "Ask the person to send it again later.")
    }
}
