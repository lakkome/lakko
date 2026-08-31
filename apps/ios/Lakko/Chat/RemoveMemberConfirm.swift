// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// MARK: - ADR-0045 UX §6 the `Remove this member?` confirmation

/// ADR-0045 UX §6 — the `Remove this member?` confirmation.
///
/// iOS mirror of `apps/web/src/chat/remove-member-confirm.ts` and
/// `apps/android/.../chat/RemoveMemberConfirm.kt`.
///
/// ## What this is for
///
/// §6 requires the confirmation on all three platforms and it shipped on one.
/// The asymmetry was sharp: **leaving** a group confirmed everywhere (this
/// sheet's own `.confirmationDialog`), while **removing somebody else** — the
/// more consequential act, and the one whose target cannot consent — went
/// through on a single tap here and on Android.
///
/// ## Why a confirmation at all, and why it is the LAST exit
///
/// §3: once published, a request is in every member's proposal store and dies
/// only when some commit lands. The core has no withdraw and MLS offers no
/// mechanism for one, so a `Cancel` afterwards could not be honoured and must
/// not be built — which is why `PendingRequests.swift` deliberately exposes no
/// withdraw verb. **The confirmation is therefore the only exit there is**, and
/// that is what makes its absence a defect rather than a polish item.
///
/// The argument does not weaken on the direct-commit path — it *sharpens*.
/// There is no withdraw there either, and the removal lands at once rather than
/// at the next sweep. So the confirmation is required on both paths, and only
/// the body line differs.
///
/// ## Why a plain module and not part of `ChatScreen.swift`
///
/// The test target drives rules, not views, so the decision lives here where it
/// is executable and `MembersSheet` renders what it decides — the same split
/// `MembershipEnded.swift`, `OwnershipTransfer.swift`, `LeaveGroup.swift` and
/// `PendingRequests.swift` already use.
///
/// ## Which rules here actually discriminate (MEASURED — each probe was run)
///
/// Each was introduced as a deliberate regression, `xcodebuild test` was run,
/// the output below observed, and the probe reverted.
///
/// 1. **The body is returned on BOTH paths** — a direct-commit group is told
///    the removal is deferred when it is immediate:
///
///        testItSaysNothingOnTheDirectPath : XCTAssertNil failed:
///        "They will keep receiving messages until an owner or an admin
///        applies the change."
///
/// 2. **The direct path keeps a leaf index in the title:**
///
///        testItIsRequiredOnBothPathsBecauseNeitherCanBeWithdrawn :
///        XCTAssertEqual failed: ("Remove this member (leaf #3)?") is not
///        equal to ("Remove this member?")
///        testItUsesNoProtocolVocabulary : XCTAssertFalse failed - `leaf`
///        leaked into remove this member (leaf #3)?  remove cancel

/// §6 — the title. Identical on both paths: it is true of a removal however it
/// is applied, and a user should not be asked a differently-worded question
/// because of a group's internal role-table state.
public let removeMemberTitle = "Remove this member?"

/// §6 — the body, on the proposal path only.
///
/// "That body line is not a nicety. It is the one fact a user removing someone
/// for a safety reason must have, and it is the fact the old immediate-looking
/// UI concealed." It is §3(a)'s honesty rule applied to a confirmation rather
/// than to a roster: the member is still cryptographically in the group — still
/// receiving, still able to decrypt — until the commit lands.
public let removeMemberBody =
    "They will keep receiving messages until an owner or an admin applies the change."

/// §6 — the destructive action.
public let removeMemberConfirmLabel = "Remove"

/// §6 — the exit.
public let removeMemberCancelLabel = "Cancel"

/// The copy one confirmation needs, whichever widget presents it.
public struct RemoveMemberConfirmation: Equatable {
    /// ``removeMemberTitle``.
    public let title: String
    /// ``removeMemberBody``, or `nil` when there is nothing true to add.
    public let body: String?
    /// ``removeMemberConfirmLabel`` — carries the destructive role.
    public let confirmLabel: String
    /// ``removeMemberCancelLabel``.
    public let cancelLabel: String
}

/// The confirmation for removing another member.
///
/// **It is never absent**, and that is the whole decision: this returns a
/// confirmation rather than an optional one, so there is no code path on which
/// a removal happens on a single tap.
///
/// **The body is dropped on the direct path rather than reworded.** A group
/// with no role table removes by direct commit, which takes effect the moment
/// it lands — so §6's sentence about the member still receiving messages is
/// false there, and a security tool that reassures a user with a false
/// statement about who can still read their messages is worse than one that
/// says less. §6 authorises exactly one body line and it belongs to one path;
/// inventing a second for the other would be copy this design never wrote.
public func removeMemberConfirmation(usesProposalPath: Bool) -> RemoveMemberConfirmation {
    RemoveMemberConfirmation(
        title: removeMemberTitle,
        body: usesProposalPath ? removeMemberBody : nil,
        confirmLabel: removeMemberConfirmLabel,
        cancelLabel: removeMemberCancelLabel)
}
