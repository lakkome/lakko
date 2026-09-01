// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0045 — ownership transfer on iOS, the owner's way out
/// (`docs/ux/proposal-based-membership.md` §6.1).
///
/// iOS mirror of the web reference `apps/web/src/chat/ownership-transfer.test.ts`.
///
/// ## Two halves, and why the split is where it is
///
/// The **rules** (`exitBlock`, `transferCandidates`, the copy, the
/// last-observed-owner store) are pure, so they are driven directly — the same
/// split `LeaveGroupTests` uses, and for the same reason: a SwiftUI view cannot
/// be asserted on, so every rule §6.1 states lives in `OwnershipTransfer.swift`
/// where it is executable.
///
/// The **action** needs a real group, because the two properties that matter —
/// that a transfer is a *proposal* rather than a commit, and that the C-65
/// collision is refused in **both** directions with the one §6 string — are
/// only observable against a live `Group`. Those run against a real
/// `ChatRepository` over the scripted DS, exactly as `LeaveGroupTests` pins the
/// live half of leaving.
@MainActor
final class OwnershipTransferTests: XCTestCase {

    override func setUp() {
        super.setUp()
        // ABORT on the first failure rather than recording and continuing. The
        // live-MLS half builds state step by step, so a later assertion running
        // against a fixture that already failed reports a second, derived
        // failure that hides the first — and a wedged `await` after a recorded
        // failure hangs the suite instead of ending it.
        continueAfterFailure = false
    }

    // MARK: - Fixtures for the pure rules

    private static let ownerKey = "b3duZXItLS0tLS0tLS0tLS0tLS0tLS0tLS0="
    private static let aliceKey = "YWxpY2UtLS0tLS0tLS0tLS0tLS0tLS0tLS0="
    private static let bobKey = "Ym9iLS0tLS0tLS0tLS0tLS0tLS0tLS0tLS0="

    private func roster() -> [RosterEntryView] {
        [
            RosterEntryView(leafIndex: 0, signatureKeyB64: Self.ownerKey, label: "Owner"),
            RosterEntryView(leafIndex: 1, signatureKeyB64: Self.aliceKey, label: "Alice"),
            RosterEntryView(leafIndex: 2, signatureKeyB64: Self.bobKey, label: "b0b1c2d3"),
        ]
    }

    private func exitInput(
        usesProposalPath: Bool = true,
        isOwner: Bool = false,
        ownLeafKnown: Bool = true,
        candidateCount: Int = 2,
        waitingRoleChange: WaitingRoleChange? = nil,
        ownRemovalPending: Bool = false
    ) -> ExitBlockInput {
        ExitBlockInput(
            usesProposalPath: usesProposalPath,
            isOwner: isOwner,
            ownLeafKnown: ownLeafKnown,
            candidateCount: candidateCount,
            waitingRoleChange: waitingRoleChange,
            ownRemovalPending: ownRemovalPending)
    }

    /// Narrow to the owner arm, failing rather than returning `nil`, so a test
    /// that lands on the wrong arm says so instead of silently skipping.
    private func ownerArm(
        _ block: ExitBlock, file: StaticString = #filePath, line: UInt = #line
    ) throws -> (transfer: GuardedControl, leave: GuardedControl) {
        guard case .owner(let transfer, let leave) = block else {
            XCTFail("expected the owner arm of the exit block, got \(block)", file: file, line: line)
            throw XCTSkip("wrong arm")
        }
        return (transfer, leave)
    }

    private func memberArm(
        _ block: ExitBlock, file: StaticString = #filePath, line: UInt = #line
    ) throws -> GuardedControl {
        guard case .member(let leave) = block else {
            XCTFail("expected the member arm of the exit block, got \(block)", file: file, line: line)
            throw XCTSkip("wrong arm")
        }
        return leave
    }

    // MARK: - §6.1.1 the exit block, two role-disjoint controls

    /// The reversal §6.1.1 argues for: the owner's `Leave group` is present and
    /// DISABLED, never hidden. An owner looking for the exit must not open the
    /// members panel and find nothing at all about leaving; the reason names the
    /// action sitting directly above it, which is what makes it a signposted
    /// route rather than a control that always refuses.
    func testTheOwnerSeesTransferLiveAndLeaveDisabledWithItsReason() throws {
        let arm = try ownerArm(exitBlock(exitInput(isOwner: true)))
        XCTAssertTrue(arm.transfer.enabled)
        XCTAssertNil(arm.transfer.disabledReason)
        XCTAssertFalse(
            arm.leave.enabled,
            "an owner is still the owner: the core refuses their departure fail-closed")
        XCTAssertEqual(arm.leave.disabledReason, leaveBlockedByOwnershipMessage)
        XCTAssertEqual(
            arm.leave.disabledReason,
            "Transfer ownership first — a group cannot be left without an owner.")
    }

    /// A non-owner's transfer is refused by the core and by every peer, and
    /// nothing a member could do would make it work — that is the case where "a
    /// control that always refuses is worse than no control" STILL holds, and it
    /// is the same sentence the owner's `Leave group` no longer satisfies. The
    /// `member` arm therefore carries no transfer control at all.
    func testANonOwnerSeesLeaveAloneAndNoTransferControl() throws {
        let block = exitBlock(exitInput(isOwner: false))
        let leave = try memberArm(block)
        XCTAssertTrue(leave.enabled)
        XCTAssertNil(leave.disabledReason)
        if case .owner = block {
            XCTFail("a member must never be handed a transfer control")
        }
    }

    /// No role table means no owner, so there is nobody to transfer to and no
    /// self-remove route in the core either.
    func testATableLessGroupShowsNoExitBlockAtAll() {
        XCTAssertEqual(exitBlock(exitInput(usesProposalPath: false)), .hidden)
        XCTAssertEqual(
            exitBlock(exitInput(usesProposalPath: false, isOwner: true)), .hidden,
            "a group with no role table has no owner, so the owner arm is unreachable there")
    }

    /// Fail-closed: without our own leaf we cannot name ourselves in a
    /// self-remove, so `Leave group` has nothing to act on.
    func testAnUnresolvableOwnLeafShowsNoExitBlock() {
        XCTAssertEqual(exitBlock(exitInput(ownLeafKnown: false)), .hidden)
    }

    /// §6.1.7 — `Leave group` must NOT turn live one instant early. The gate is
    /// the role table, never the local record of having asked: an owner is still
    /// the owner while their transfer is pending, and a `Leave group` that went
    /// live on publication would offer an action the core refuses and the
    /// cryptography does not support.
    func testLeaveGroupDoesNotTurnLiveOneInstantEarly() throws {
        let pending = try ownerArm(
            exitBlock(
                exitInput(
                    isOwner: true,
                    waitingRoleChange: WaitingRoleChange(
                        ownTransfer: true, successorLabel: "Alice"))))
        XCTAssertFalse(
            pending.leave.enabled,
            "publication is not application — the table still names us owner")

        // …and the moment the table names someone else, the same control in the
        // same slot is simply live. Nothing has to announce the transition.
        let landed = try memberArm(
            exitBlock(
                exitInput(
                    isOwner: false,
                    waitingRoleChange: WaitingRoleChange(
                        ownTransfer: true, successorLabel: "Alice"))))
        XCTAssertTrue(landed.enabled)
    }

    /// §5.2's existing row annotation, reused as the control's reason rather
    /// than a new string: the member's own row already carries it, and the
    /// control should say the same thing the roster says.
    func testAMemberWithTheirOwnLeavePendingKeepsLeaveVisibleButDisabled() throws {
        let leave = try memberArm(exitBlock(exitInput(ownRemovalPending: true)))
        XCTAssertFalse(leave.enabled)
        XCTAssertEqual(leave.disabledReason, "Waiting to leave.")
    }

    // MARK: - §6.1.3 / §6.1.6 / §6.1.8 why Transfer ownership is disabled

    /// The honest terminal state of a one-person group. There is no design that
    /// produces a successor out of an empty roster, and the owner of a
    /// one-person group is not being kept anywhere against their will.
    func testAGroupWithNoOtherMemberDisablesTransferWithItsOwnTerminalReason() throws {
        let arm = try ownerArm(exitBlock(exitInput(isOwner: true, candidateCount: 0)))
        XCTAssertFalse(arm.transfer.enabled)
        XCTAssertEqual(arm.transfer.disabledReason, noTransferCandidatesMessage)
        XCTAssertEqual(
            arm.transfer.disabledReason,
            "There is no one else in this group to transfer it to.")
    }

    /// §6.1.8 — the table stays at SEVEN strings. This is the seventh, verbatim,
    /// and it is the one AVAILABILITY refusal in it: the only row that ends by
    /// telling the user to try again, where every other row is an authorisation
    /// refusal stating a fact.
    func testAWaitingRoleChangeDisablesTransferWithTheSeventhString() throws {
        let arm = try ownerArm(
            exitBlock(
                exitInput(
                    isOwner: true,
                    waitingRoleChange: WaitingRoleChange(
                        ownTransfer: false, successorLabel: nil))))
        XCTAssertFalse(arm.transfer.enabled)
        XCTAssertEqual(arm.transfer.disabledReason, roleChangeAlreadyPendingMessage)
        XCTAssertEqual(
            arm.transfer.disabledReason,
            "A role change is already waiting to apply. Wait for it to land, then try again.")
    }

    /// UX §10's non-negotiable own-request exception, satisfied on the control
    /// the user touched rather than as a row annotation (§6.1.6: a role change
    /// carries no target, so only the device that asked could render one).
    func testOurOwnWaitingTransferNamesTheSuccessorInstead() throws {
        let arm = try ownerArm(
            exitBlock(
                exitInput(
                    isOwner: true,
                    waitingRoleChange: WaitingRoleChange(
                        ownTransfer: true, successorLabel: "Alice"))))
        XCTAssertFalse(arm.transfer.enabled)
        XCTAssertEqual(arm.transfer.disabledReason, "Waiting to make Alice the owner.")
    }

    /// The successor is resolved through the LIVE roster at render time, so one
    /// who has since left degrades to the unnamed variant rather than to a stale
    /// name — the rule `PendingRequests` already states for a removal's target,
    /// and the reason the durable record stores the KEY rather than the label.
    func testOurOwnWaitingTransferWhoseSuccessorNoLongerResolvesFallsBack() throws {
        let arm = try ownerArm(
            exitBlock(
                exitInput(
                    isOwner: true,
                    waitingRoleChange: WaitingRoleChange(
                        ownTransfer: true, successorLabel: nil))))
        XCTAssertEqual(arm.transfer.disabledReason, "Ownership transfer waiting to apply.")
    }

    /// `ProposalKind.roleChange` carries no payload, so the successor key on our
    /// own observation record is the ONLY thing that tells a transfer from a
    /// promote. A promote reporting itself as `Ownership transfer waiting to
    /// apply.` would be a plain falsehood; the seventh string is true of it.
    func testOurOwnPromoteIsNotOurOwnTransferAndGetsTheSeventhString() throws {
        let arm = try ownerArm(
            exitBlock(
                exitInput(
                    isOwner: true,
                    waitingRoleChange: WaitingRoleChange(
                        ownTransfer: false, successorLabel: nil))))
        XCTAssertEqual(arm.transfer.disabledReason, roleChangeAlreadyPendingMessage)
    }

    /// §6 draws the distinction and it decides the precedence: an authorisation
    /// refusal states a fact with no invitation to retry; an availability
    /// refusal ends by telling the user to try again. An empty roster is
    /// PERMANENT, so it must not be masked by a string that promises a retry
    /// which can never succeed.
    func testAPermanentRefusalOutranksTheRetryInvitingOne() throws {
        let arm = try ownerArm(
            exitBlock(
                exitInput(
                    isOwner: true,
                    candidateCount: 0,
                    waitingRoleChange: WaitingRoleChange(
                        ownTransfer: false, successorLabel: nil))))
        XCTAssertEqual(arm.transfer.disabledReason, noTransferCandidatesMessage)
    }

    /// …and our own request outranks even the permanent one, because it is UX
    /// §10's own-request exception and the most specific thing this device
    /// knows.
    func testOurOwnRequestOutranksEvenThePermanentOne() throws {
        let arm = try ownerArm(
            exitBlock(
                exitInput(
                    isOwner: true,
                    candidateCount: 0,
                    waitingRoleChange: WaitingRoleChange(
                        ownTransfer: true, successorLabel: "Alice"))))
        XCTAssertEqual(arm.transfer.disabledReason, "Waiting to make Alice the owner.")
    }

    /// §6.1.6 — below the quiet window the control stays ENABLED and a second
    /// tap is refused by the C-65 pre-check. `waitingRoleChange` is `nil` for a
    /// request that has not matured, so this is the caller's `state == .waiting`
    /// filter arriving here as an absence: a request that is not yet rendered
    /// must not silently disable a control with nothing to explain it.
    func testARoleChangeInsideTheQuietWindowLeavesTheControlEnabled() throws {
        let arm = try ownerArm(exitBlock(exitInput(isOwner: true, waitingRoleChange: nil)))
        XCTAssertTrue(arm.transfer.enabled)
    }

    // MARK: - §6.1.3 the picker offers every member, marked not filtered

    /// The reason this is not an admins-only picker is a dead end rather than a
    /// preference: in the commonest small group — owner plus one member, no
    /// admins ever appointed — an admins-only picker is EMPTY and the owner is
    /// stuck again, which is the exact dead end this feature exists to close.
    func testThePickerExcludesTheOwnerAndIncludesANonAdminMember() {
        let rows = transferCandidates(
            roster(), ownerKeyB64: Self.ownerKey, adminKeysB64: [Self.aliceKey])
        XCTAssertEqual(rows.map(\.signatureKeyB64), [Self.aliceKey, Self.bobKey])
        XCTAssertEqual(
            rows.first { $0.signatureKeyB64 == Self.bobKey }?.isAdmin, false,
            "a plain member is a candidate: the core admits any member")
    }

    /// MARK, do not filter, and do not sort — hoisting admins to the top would
    /// be a soft version of the filter §6.1.3 rejects.
    func testThePickerMarksAnAdminRatherThanSortingOrFilteringThem() {
        let rows = transferCandidates(
            roster(), ownerKeyB64: Self.ownerKey, adminKeysB64: [Self.bobKey])
        XCTAssertEqual(rows.map(\.isAdmin), [false, true])
        XCTAssertEqual(
            rows.map(\.label), ["Alice", "b0b1c2d3"],
            "roster order, unchanged")
    }

    /// OpenMLS hands a vacated leaf to the next Add, so a leaf-keyed selection
    /// can silently come to name someone else. The selection VALUE is the
    /// 32-byte signature key, which is also what `proposeOwnershipTransfer`
    /// takes and what the role table keys on.
    func testThePickerIsKeyedOnTheSignatureKeyNeverTheLeafIndex() {
        let rows = transferCandidates(roster(), ownerKeyB64: Self.ownerKey, adminKeysB64: [])
        XCTAssertEqual(rows.map(\.id), [Self.aliceKey, Self.bobKey])
        XCTAssertEqual(rows.map(\.leafIndex), [1, 2])
    }

    func testAOnePersonGroupYieldsNoCandidates() {
        let rows = transferCandidates(
            [RosterEntryView(leafIndex: 0, signatureKeyB64: Self.ownerKey, label: "Owner")],
            ownerKeyB64: Self.ownerKey,
            adminKeysB64: [])
        XCTAssertTrue(rows.isEmpty)
    }

    /// The label the user reads in the picker and the label they read on the
    /// roster must be produced by the same function, because whom they are
    /// naming is the one thing they will cross-check.
    func testThePickerReusesTheRostersOwnLabelVerbatim() {
        let rows = transferCandidates(roster(), ownerKeyB64: Self.ownerKey, adminKeysB64: [])
        XCTAssertEqual(rows.last?.label, "b0b1c2d3")
    }

    // MARK: - §6.1.4 the outgoing-owner choice

    /// Pre-selected for the core's stated reason and NOT because it preserves a
    /// way back — it does not. It is the recoverable side of an unrecoverable
    /// action: shedding admin later is one ordinary role change the new owner
    /// can make.
    func testStayAnAdminIsPreSelected() {
        XCTAssertEqual(preselectedOutgoingOwner, .admin)
        let preselected = outgoingOwnerOptions.filter(\.preSelected)
        XCTAssertEqual(preselected.count, 1, "a single-select control has exactly one default row")
        XCTAssertEqual(preselected.first?.value, .admin)
        XCTAssertEqual(preselected.first?.label, "Stay an admin")
    }

    /// Each option states its own consequence, because either silent choice is
    /// wrong in one direction and the UI must not undo with a layout choice what
    /// the core protected with a type.
    func testEachOutgoingOwnerOptionCarriesItsOwnConsequence() {
        XCTAssertEqual(outgoingOwnerOptions.map(\.value), [.admin, .member])
        XCTAssertEqual(
            outgoingOwnerOptions.first?.consequence,
            "You can still add and remove members. You can give this up later by asking the new owner.")
        XCTAssertEqual(outgoingOwnerOptions.last?.label, "Become a regular member")
        XCTAssertEqual(
            outgoingOwnerOptions.last?.consequence,
            "You give up every group power immediately. Only the new owner can give any of it back.")
    }

    // MARK: - §6.1.5 the step IS the confirmation

    /// There is no default successor and no path to the action that skips the
    /// picker. The unselected label is the control's own name, which asserts
    /// nothing.
    func testTheActionIsDisabledUntilASuccessorIsPicked() {
        let idle = transferActionLabel(selected: nil)
        XCTAssertFalse(idle.enabled)
        XCTAssertEqual(idle.label, "Transfer ownership")
    }

    /// Not a re-formatted or re-shortened variant: that is the string the user
    /// cross-checks against the row they tapped.
    func testTheActionNamesThePickedPersonUsingTheRowsLabelVerbatim() {
        let picked = transferActionLabel(
            selected: TransferCandidate(
                leafIndex: 2, signatureKeyB64: Self.bobKey, label: "b0b1c2d3", isAdmin: false))
        XCTAssertTrue(picked.enabled)
        XCTAssertEqual(picked.label, "Make b0b1c2d3 the owner")
    }

    /// BOUNDARY GUARD — this fails when the wording drifts, which is the point:
    /// all three clauses are literally true and none is rhetorical.
    func testTheIrreversibilityCopyStatesThreeSpecificLossesNotAnAdjective() {
        XCTAssertEqual(transferIrreversibleHeadline, "This cannot be undone.")
        XCTAssertEqual(
            transferIrreversibleBody,
            "Only an owner can change roles. Once you transfer this group you cannot "
                + "take it back, you cannot restore your own owner role, and you cannot "
                + "change your own role again unless the new owner does it for you.")
    }

    /// Note APPLIED, not NOW: asserting the exit is open before the commit lands
    /// would be the optimistic update §3 forbids.
    func testTheTrailingLineSaysAppliedNeverNow() {
        XCTAssertEqual(
            transferUnlocksLeaveMessage,
            "Once the change is applied, you can leave the group like any other member.")
        XCTAssertFalse(transferUnlocksLeaveMessage.contains("now"))
    }

    // MARK: - §6.1.7 what the transferor sees afterwards

    /// The swept forms name the transferor's OWN new role, because half the
    /// panel changes under them in one tap: an owner who chose `Become a regular
    /// member` loses the add form, the invite block, every `Remove` and every
    /// admin toggle at once.
    func testASweptTransferNamesTheTransferorsOwnNewRole() {
        XCTAssertEqual(
            transferOutcomeNotice(outcome: .committed, successorLabel: "Alice", outgoing: .admin),
            "Alice is now the group owner. You are still an admin.")
        XCTAssertEqual(
            transferOutcomeNotice(outcome: .committed, successorLabel: "Alice", outgoing: .member),
            "Alice is now the group owner. You are now a regular member.")
    }

    /// Nothing has changed yet; appending "you are now an admin" to a request
    /// that has not landed would be the optimistic-update lie in its purest form.
    func testAPublishedButUnsweptTransferNamesNoRole() {
        let notice = transferOutcomeNotice(
            outcome: .proposed, successorLabel: "Alice", outgoing: .admin)
        XCTAssertEqual(
            notice,
            "Ownership transfer requested. It applies when an owner or an admin applies the change.")
        XCTAssertFalse(notice.contains("admin."))
        XCTAssertEqual(
            transferOutcomeNotice(outcome: .proposed, successorLabel: "Alice", outgoing: .member),
            notice,
            "the unswept form names no role in EITHER direction")
    }

    // MARK: - §6.1.6 the pending label resolves through the live roster

    func testThePendingLabelNamesTheSuccessorWhenTheyStillResolve() {
        XCTAssertEqual(
            transferPendingLabel(successorLabel: "Alice"), "Waiting to make Alice the owner.")
    }

    func testThePendingLabelFallsBackWhenTheSuccessorNoLongerResolves() {
        XCTAssertEqual(
            transferPendingLabel(successorLabel: nil), "Ownership transfer waiting to apply.")
    }

    // MARK: - §6.1.9 telling the new owner, fail-open

    /// Structurally unable to fire on a device that first observes the table
    /// already naming it owner — a fresh device, a restore, a first sync. So the
    /// rule is fail-open: no remembered previous owner means say nothing.
    func testTheNoticeSaysNothingWhenNoPreviousOwnerWasEverRecorded() {
        XCTAssertNil(
            newOwnerNotice(
                lastObservedOwnerB64: nil,
                currentOwnerB64: Self.aliceKey,
                ownSignatureKeyB64: Self.aliceKey,
                previousOwnerLabel: "Owner"))
    }

    func testTheNoticeDoesNotFireOnAFirstObservationOfAnUnchangedTable() {
        XCTAssertNil(
            newOwnerNotice(
                lastObservedOwnerB64: Self.aliceKey,
                currentOwnerB64: Self.aliceKey,
                ownSignatureKeyB64: Self.aliceKey,
                previousOwnerLabel: "Owner"))
    }

    /// Naming the previous owner is sound rather than a guess: a role change is
    /// owner-only and `owner_survives` (C-63) forbids naming a non-member, so
    /// the previous owner did author it.
    func testTheNoticeFiresNamingThePreviousOwnerWhenTheTableChangedToNameUs() {
        let notice = newOwnerNotice(
            lastObservedOwnerB64: Self.ownerKey,
            currentOwnerB64: Self.aliceKey,
            ownSignatureKeyB64: Self.aliceKey,
            previousOwnerLabel: "Owner")
        XCTAssertEqual(notice?.headline, "Owner made you the owner of this group.")
        XCTAssertEqual(notice?.body, newOwnerNoticeBody)
        XCTAssertEqual(
            notice?.body,
            "You can add and remove members, change admins, and transfer ownership to someone else.")
        XCTAssertEqual(notice?.dismiss, newOwnerNoticeDismiss)
    }

    func testTheNoticeOmitsTheNameWhenThePreviousOwnerNoLongerResolves() {
        let notice = newOwnerNotice(
            lastObservedOwnerB64: Self.ownerKey,
            currentOwnerB64: Self.aliceKey,
            ownSignatureKeyB64: Self.aliceKey,
            previousOwnerLabel: nil)
        XCTAssertEqual(notice?.headline, "You are now the owner of this group.")
    }

    /// Including the TRANSFEROR: their last observed owner was themselves and
    /// the table now names someone else, which is a change — but not one that
    /// makes them the owner.
    func testTheNoticeSaysNothingToSomeoneWhoIsNotTheOwner() {
        XCTAssertNil(
            newOwnerNotice(
                lastObservedOwnerB64: Self.ownerKey,
                currentOwnerB64: Self.aliceKey,
                ownSignatureKeyB64: Self.bobKey,
                previousOwnerLabel: "Owner"))
    }

    func testTheNoticeSaysNothingWithNoRoleTableOrAnUnknownOwnKey() {
        XCTAssertNil(
            newOwnerNotice(
                lastObservedOwnerB64: Self.ownerKey,
                currentOwnerB64: nil,
                ownSignatureKeyB64: Self.aliceKey,
                previousOwnerLabel: "Owner"))
        XCTAssertNil(
            newOwnerNotice(
                lastObservedOwnerB64: Self.ownerKey,
                currentOwnerB64: Self.aliceKey,
                ownSignatureKeyB64: nil,
                previousOwnerLabel: "Owner"))
    }

    // MARK: - §6.1.10 the last-observed-owner store

    private func scratchOwnerStore() throws -> LastObservedOwnerStore {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "owner-\(UUID().uuidString)"))
        return LastObservedOwnerStore(defaults: defaults)
    }

    func testTheOwnerStoreRoundTripsPerAccountAndGroup() throws {
        let store = try scratchOwnerStore()
        XCTAssertNil(store.load(accountIdB64: "acct-a", groupIdB64: "grp-1"))
        store.remember(accountIdB64: "acct-a", groupIdB64: "grp-1", ownerKeyB64: Self.ownerKey)
        XCTAssertEqual(store.load(accountIdB64: "acct-a", groupIdB64: "grp-1"), Self.ownerKey)
        XCTAssertNil(
            store.load(accountIdB64: "acct-a", groupIdB64: "grp-2"),
            "another group of the same account must not inherit the owner")
        XCTAssertNil(
            store.load(accountIdB64: "acct-b", groupIdB64: "grp-1"),
            "another account must not inherit the owner")
    }

    func testTheOwnerStoreRecordsTheChangeSoASecondReadSeesTheNewOwner() throws {
        let store = try scratchOwnerStore()
        store.remember(accountIdB64: "acct-a", groupIdB64: "grp-1", ownerKeyB64: Self.ownerKey)
        store.remember(accountIdB64: "acct-a", groupIdB64: "grp-1", ownerKeyB64: Self.aliceKey)
        XCTAssertEqual(store.load(accountIdB64: "acct-a", groupIdB64: "grp-1"), Self.aliceKey)
    }

    /// `recover` / `forgetDevice` hygiene, and it must be SCOPED to the account
    /// — the same discipline the commit outbox, the pending-request
    /// observations and the beacon observations all follow.
    func testTheOwnerStoreWipeIsScopedToOneAccount() throws {
        let store = try scratchOwnerStore()
        store.remember(accountIdB64: "acct-a", groupIdB64: "grp-1", ownerKeyB64: Self.ownerKey)
        store.remember(accountIdB64: "acct-b", groupIdB64: "grp-1", ownerKeyB64: Self.ownerKey)
        store.clearForAccount(accountIdB64: "acct-a")
        XCTAssertNil(store.load(accountIdB64: "acct-a", groupIdB64: "grp-1"))
        XCTAssertEqual(
            store.load(accountIdB64: "acct-b", groupIdB64: "grp-1"), Self.ownerKey,
            "wiping one account must not disturb another's memory")
    }

    // MARK: - §6.1.10 the successor key rides the OBSERVATION record

    private func scratchRequestStore() throws -> PendingRequestStore {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "otreq-\(UUID().uuidString)"))
        return PendingRequestStore(defaults: defaults)
    }

    /// It lives on the observation record rather than in a store of its own, and
    /// that is a CORRECTNESS choice rather than tidiness: this store is pruned
    /// against the core's live pending set on every fold, so a swept or
    /// superseded transfer takes its successor key with it.
    func testTheSuccessorKeyRoundTripsOnTheObservationRecord() throws {
        let store = try scratchRequestStore()
        store.record(
            accountIdB64: "a", groupIdB64: "g", key: "role-change", atMs: 10, own: true,
            transferToB64: Self.aliceKey)
        XCTAssertEqual(
            store.load(accountIdB64: "a", groupIdB64: "g")["role-change"]?.transferToB64,
            Self.aliceKey)
    }

    /// First-wins, for the same reason `atMs` is: the successor belongs to the
    /// request that is actually pending, and only the device that published it
    /// can supply one. A LATER observation of the same key without one — the
    /// receive loop seeing our own request land back off the journal — must not
    /// erase it.
    func testALaterObservationWithoutASuccessorDoesNotEraseIt() throws {
        let store = try scratchRequestStore()
        store.record(
            accountIdB64: "a", groupIdB64: "g", key: "role-change", atMs: 10, own: true,
            transferToB64: Self.aliceKey)
        store.record(
            accountIdB64: "a", groupIdB64: "g", key: "role-change", atMs: 99, own: false)
        let seen = store.load(accountIdB64: "a", groupIdB64: "g")["role-change"]
        XCTAssertEqual(seen?.transferToB64, Self.aliceKey)
        XCTAssertEqual(seen?.atMs, 10, "the age is first-wins too")
        XCTAssertEqual(seen?.own, true, "and `own` is upgraded, never downgraded")
    }

    /// A stale successor key surviving the prune would make an unrelated LATER
    /// role change render `Waiting to make {name} the owner.` about somebody
    /// nobody proposed — which is exactly what riding the pruned store prevents.
    func testTheSuccessorKeyIsPrunedWithItsRequest() throws {
        let store = try scratchRequestStore()
        store.record(
            accountIdB64: "a", groupIdB64: "g", key: "role-change", atMs: 10, own: true,
            transferToB64: Self.aliceKey)
        store.prune(accountIdB64: "a", groupIdB64: "g", liveKeys: [])
        XCTAssertNil(store.load(accountIdB64: "a", groupIdB64: "g")["role-change"])
    }

    // MARK: - The action, against a real MLS group

    private struct Fixture {
        let repo: ChatRepository
        let groupIdHex: String
        /// The other two members' signature keys — candidate successors.
        let aliceKey: Data
        let bobKey: Data
        let accountIdB64: String
        let snapshotStore: IdentitySnapshotStore
        let outboxDir: URL
        /// This fixture's private journal on the scripted DS — see the scoping
        /// note on `ScriptedDsURLProtocol`. `settledRowCount()` below still
        /// exists and is still needed: scoping keeps ANOTHER fixture's straggler
        /// out, but the mint this fixture starts itself is its own.
        let dsScope: String
    }

    /// Owner A + plain member B + plain member C, with A (or B, per `asOwner`)
    /// rehydrated into a real repository over the scripted DS.
    private func makeRepo(asOwner: Bool) async throws -> Fixture {
        let dsScope = ScriptedDsURLProtocol.freshScope()

        let ownerSeed = Data(repeating: 0x11, count: 32)
        let aliceSeed = Data(repeating: 0x55, count: 32)
        let bobSeed = Data(repeating: 0x77, count: 32)
        let owner = try Identity.fromSeed(seed: ownerSeed)
        let alice = try Identity.fromSeed(seed: aliceSeed)
        let bob = try Identity.fromSeed(seed: bobSeed)

        // `Group.create` seeds the ADR-0022 role table, which is what routes
        // this group through ADR-0045's proposal path at all — and what gives
        // it an owner to transfer FROM.
        let ownerGroup = try Group.create(identity: owner)
        let addAlice = try ownerGroup.addMemberFromWire(
            identity: owner, keyPackage: try alice.createKeyPackage())
        let aliceGroup = try Group.joinFromWelcome(welcome: addAlice.welcome, identity: alice)
        let addBob = try ownerGroup.addMemberFromWire(
            identity: owner, keyPackage: try bob.createKeyPackage())
        _ = try aliceGroup.processIncomingVerifyingRoles(identity: alice, bytes: addBob.commit)

        let underTest = asOwner ? owner : alice
        let underTestGroup = asOwner ? ownerGroup : aliceGroup
        let underTestSeed = asOwner ? ownerSeed : aliceSeed

        XCTAssertEqual(
            try underTestGroup.epoch(), try ownerGroup.epoch(),
            "the fixture must leave the device under test at the owner's epoch, or its "
                + "proposal would be dropped as wrong-epoch and the suite would pass vacuously")

        let groupIdHex = DsClient.pseudonymHex(try underTestGroup.groupId())
        let snapshotBlob = try underTest.snapshotStorage()
        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            guard let base = $0.baseAddress else { return }
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, base)
        }
        let accountIdB64 = base64Encode(accountIdBytes)

        let snapshotStore = IdentitySnapshotStore()
        snapshotStore.putSnapshot(accountIdB64: accountIdB64, blob: snapshotBlob)
        let suffix = UUID().uuidString
        let knownGroupsStore = KnownGroupsStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "xfer-k-\(suffix)")))
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("xfer-\(suffix)")
        try FileManager.default.createDirectory(at: outboxDir, withIntermediateDirectories: true)

        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [ScriptedDsURLProtocol.self]
        let ds = DsClient(
            baseURL: ScriptedDsURLProtocol.baseURL(dsScope),
            session: URLSession(configuration: config))
        let repo = ChatRepository(
            chatClient: ChatClient(ds: ds),
            dsClient: ds,
            discovery: DiscoveryClient(),
            snapshotStore: snapshotStore,
            knownGroupsStore: knownGroupsStore,
            messageLogStore: MessageLogStore(),
            commitOutbox: CommitOutboxStore(rootDir: outboxDir),
            pushRouting: PushGroupRouting(),
            beaconStore: BeaconObservationStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "xfer-b-\(suffix)"))),
            pendingRequestStore: PendingRequestStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "xfer-p-\(suffix)"))),
            lastObservedOwnerStore: LastObservedOwnerStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "xfer-o-\(suffix)")))
        )
        repo.bootstrap(
            secrets: IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: Data(repeating: 0x22, count: 32),
                accountIdentitySeed: underTestSeed,
                sessionToken: "t",
                sessionExpiresAtMs: 0
            )
        )
        XCTAssertTrue(
            repo.usesProposalPath(groupIdHex: groupIdHex),
            "the group must route membership through ADR-0045 proposals, or none of this applies")
        XCTAssertEqual(
            repo.ownRole(groupIdHex: groupIdHex), asOwner ? .owner : .member,
            "the device under test must hold the role this case is about")

        return Fixture(
            repo: repo,
            groupIdHex: groupIdHex,
            aliceKey: try alice.signaturePublicKey(),
            bobKey: try bob.signaturePublicKey(),
            accountIdB64: accountIdB64,
            snapshotStore: snapshotStore,
            outboxDir: outboxDir,
            dsScope: dsScope)
    }

    private func tearDown(_ fx: Fixture) {
        fx.snapshotStore.deleteSnapshot(accountIdB64: fx.accountIdB64)
        try? FileManager.default.removeItem(at: fx.outboxDir)
        ScriptedDsURLProtocol.reset()
    }

    /// The journal row count once the fixture's own asynchronous publishes have
    /// stopped, so a "nothing reached the wire" assertion measures the action
    /// under test rather than a background task that happened to land during it.
    ///
    /// **This is not defensive padding — it was MEASURED.** Probing the count at
    /// each step showed `bootstrap()` fires ADR-0042 `ensurePushRouting` for
    /// every restored group as a detached task, and an OWNER holding no routing
    /// secret yet mints and BROADCASTS one, which is a real DS publish landing
    /// roughly 200 ms after the call that started it. Read naively, a baseline
    /// captured before that lands makes a refusal that published nothing look
    /// like a publish. Waiting for two consecutive quiet samples measures the
    /// thing the test is about.
    ///
    /// `LeaveGroupTests` does not hit it because its device under test is a
    /// plain member, which defers instead of minting.
    private func settledRowCount(_ fx: Fixture) async -> Int {
        var last = ScriptedDsURLProtocol.rowCount(scope: fx.dsScope)
        var quiet = 0
        // Bounded: ~1.5 s worst case, and a still-noisy fixture returns the last
        // reading rather than hanging the suite.
        for _ in 0..<30 {
            try? await Task.sleep(nanoseconds: 50_000_000)
            let now = ScriptedDsURLProtocol.rowCount(scope: fx.dsScope)
            if now == last {
                quiet += 1
                if quiet == 2 { return now }
            } else {
                quiet = 0
                last = now
            }
        }
        return last
    }

    /// THE CASE THE FEATURE EXISTS FOR. A transfer is a PROPOSAL — never the
    /// direct arm — so the epoch must not move, and the successor's key must be
    /// recorded on our own observation record (§6.1.10), which is the only thing
    /// that later tells this transfer from an ordinary promote.
    func testTheOwnersTransferProducesAProposalAndRecordsTheSuccessor() async throws {
        let fx = try await makeRepo(asOwner: true)
        defer { tearDown(fx) }

        let epochBefore = try XCTUnwrap(fx.repo.epoch(groupIdHex: fx.groupIdHex))
        let rowsBefore = await settledRowCount(fx)

        let outcome = try await fx.repo.transferGroupOwnership(
            groupIdHex: fx.groupIdHex, newOwnerSignaturePubkey: fx.aliceKey, outgoing: .admin)

        guard case .proposed = outcome else {
            XCTFail(
                "a transfer in a role-tabled group must be a PROPOSAL — the direct arm is "
                    + "refused while anything is pending and loses a race catastrophically — "
                    + "but got \(outcome)")
            return
        }
        XCTAssertEqual(
            ScriptedDsURLProtocol.rowCount(scope: fx.dsScope), rowsBefore + 1,
            "the proposal must reach the journal, or no committer can ever sweep it")
        XCTAssertEqual(
            try fx.repo.epoch(groupIdHex: fx.groupIdHex), epochBefore,
            "a proposal advances no epoch — that is what lets concurrent intents compose")

        let requests = fx.repo.pendingRequests(groupIdHex: fx.groupIdHex)
        let roleChange = try XCTUnwrap(
            requests.first { $0.kind == .roleChange },
            "the transfer must be retained in this device's own pending set, or the peers' "
                + "by-reference resolution has nothing to find")
        XCTAssertEqual(
            roleChange.transferToB64, base64Encode(fx.aliceKey),
            "without the successor key on our own record, this transfer is indistinguishable "
                + "from a promote and would report itself with the wrong string")
        XCTAssertTrue(roleChange.own)
    }

    /// C-65, direction one: a pending PROMOTE refuses a transfer, with §6's
    /// seventh string and no eighth.
    func testAPendingRoleChangeRefusesATransferWithTheSeventhString() async throws {
        let fx = try await makeRepo(asOwner: true)
        defer { tearDown(fx) }

        _ = try await fx.repo.setMemberAdmin(
            groupIdHex: fx.groupIdHex, memberSignaturePubkey: fx.bobKey, makeAdmin: true)

        let rowsBefore = await settledRowCount(fx)
        do {
            _ = try await fx.repo.transferGroupOwnership(
                groupIdHex: fx.groupIdHex, newOwnerSignaturePubkey: fx.aliceKey, outgoing: .admin)
            XCTFail(
                "RFC 9420 admits one GroupContextExtensions proposal per commit and OpenMLS "
                    + "enforces it as a HARD failure, so a second one must be refused before "
                    + "it is published")
        } catch {
            XCTAssertEqual(
                errorMessage(error), roleChangeAlreadyPendingMessage,
                "the refusal must be §6's seventh string, not a hand-written near-variant")
        }
        XCTAssertEqual(
            ScriptedDsURLProtocol.rowCount(scope: fx.dsScope), rowsBefore,
            "nothing may reach the wire — a pending set holding two role changes cannot be "
                + "swept at all, which wedges the whole group's membership")
    }

    /// C-65, direction two, and the reason §6's table stays at seven: a pending
    /// TRANSFER refuses an ordinary role change, with the identical string. It
    /// is under-informative here on purpose — the owner knows they asked for a
    /// transfer and is told "a role change" — because the blocking proposal may
    /// be one this device no longer remembers after the ADR-0040 crash window.
    func testAPendingTransferRefusesARoleChangeWithTheSameSeventhString() async throws {
        let fx = try await makeRepo(asOwner: true)
        defer { tearDown(fx) }

        _ = try await fx.repo.transferGroupOwnership(
            groupIdHex: fx.groupIdHex, newOwnerSignaturePubkey: fx.aliceKey, outgoing: .admin)

        do {
            _ = try await fx.repo.setMemberAdmin(
                groupIdHex: fx.groupIdHex, memberSignaturePubkey: fx.bobKey, makeAdmin: true)
            XCTFail("a transfer IS a role change, so it collides in this direction too")
        } catch {
            XCTAssertEqual(
                errorMessage(error), roleChangeAlreadyPendingMessage,
                "ONE string, defined once — two hand-written near-variants of one refusal is "
                    + "the drift class UX §12.6 exists to prevent")
        }
    }

    /// A non-owner's transfer is refused, and refused BEFORE anything is
    /// published: a role change is owner-only, so no peer may ever hold a
    /// request that every gate will drop.
    func testANonOwnersTransferIsRefusedAndNothingReachesTheWire() async throws {
        let fx = try await makeRepo(asOwner: false)
        defer { tearDown(fx) }

        let rowsBefore = await settledRowCount(fx)
        do {
            _ = try await fx.repo.transferGroupOwnership(
                groupIdHex: fx.groupIdHex, newOwnerSignaturePubkey: fx.bobKey, outgoing: .admin)
            XCTFail("a role change is owner-only — a member's transfer must be refused")
        } catch {
            // Expected. The message is the CORE's own and is deliberately not
            // matched: mapping core prose to fixed copy is the drift class
            // UX §12.6 flags, which is why the UI never offers the control.
        }
        XCTAssertEqual(
            ScriptedDsURLProtocol.rowCount(scope: fx.dsScope), rowsBefore,
            "the member's transfer must never reach the DS, but the journal grew from "
                + "\(rowsBefore) to \(ScriptedDsURLProtocol.rowCount(scope: fx.dsScope)) rows")
    }

    /// `ChatError.argument` renders its payload through `localizedDescription`;
    /// reading it back is how a test asserts on the exact string a user sees.
    private func errorMessage(_ error: Error) -> String {
        if case ChatError.argument(let message) = error { return message }
        return error.localizedDescription
    }
}
