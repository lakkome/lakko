// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0045 UX §6.2 — the end of your own membership, on iOS.
///
/// iOS mirror of the web reference `apps/web/src/chat/membership-ended.test.ts`.
///
/// Every rule §6.2 states lives in `MembershipEnded.swift` — plus the three
/// arms it grafts onto modules that already existed (`exitBlock`'s `ended`,
/// `membershipRotationDecision`'s `selfEvicted`, `MessageLogStore.withoutGroup`)
/// — precisely so it is executable here. A SwiftUI view cannot be asserted on,
/// which is the same split `LeaveGroupTests` and `OwnershipTransferTests`
/// already use.
///
/// The measured discrimination evidence for each rule (each probe was RUN, the
/// failure observed, and the probe reverted) is recorded in
/// `MembershipEnded.swift`'s own header, beside the rule it discriminates.
@MainActor
final class MembershipEndedTests: XCTestCase {

    override func setUp() {
        super.setUp()
        // ABORT on the first failure rather than recording and continuing —
        // the same reason `OwnershipTransferTests` gives: a later assertion
        // running against a fixture that already failed reports a second,
        // derived failure that hides the first.
        continueAfterFailure = false
    }

    // MARK: - Detecting the state

    func testAnActiveGroupIsUnaffected() {
        let state = membershipState(isActive: true, leaveRequestedAtMs: nil)
        XCTAssertEqual(state, .active)
        XCTAssertFalse(membershipEnded(state))
        XCTAssertNil(terminalRow(state))
        XCTAssertNil(chatListLine(state))
        XCTAssertEqual(composerState(state, draft: "half a sentence"), .composer)
    }

    /// §6.2.11 — NO optimistic transition. Between publishing a leave request
    /// and the commit merging you are still fully in the group: still
    /// receiving, still decrypting. The row annotation is §5.2's
    /// `Waiting to leave.` and nothing else changes.
    func testAnActiveGroupWithALeaveRequestOutstandingIsStillActive() {
        let state = membershipState(isActive: true, leaveRequestedAtMs: 1_000)
        XCTAssertEqual(state, .active)
        XCTAssertEqual(composerState(state, draft: ""), .composer)
    }

    func testEvictionIsDetectedOnceTheRemovingCommitHasBeenApplied() {
        let state = membershipState(isActive: false, leaveRequestedAtMs: nil)
        XCTAssertEqual(state, .ended(left: false))
        XCTAssertTrue(membershipEnded(state))
    }

    /// An unreadable handle says nothing about membership. Claiming
    /// `You are no longer a member of this group.` on it would be a false
    /// statement the user cannot check; leaving the chat as it is today is
    /// merely the status quo. So `nil` fails toward `active`.
    func testATornReadIsNotEvidenceOfAnEviction() {
        XCTAssertEqual(
            membershipState(isActive: nil, leaveRequestedAtMs: 4_000), .active)
    }

    func testReadIsActiveReadsTheCoresOwnPredicate() {
        XCTAssertEqual(readIsActive { true }, true)
        XCTAssertEqual(readIsActive { false }, false)
    }

    /// `Group.isActive()` is a throwing UniFFI accessor, so a torn handle
    /// surfaces as an error rather than a value — and that must read as
    /// UNKNOWN, never as evicted.
    func testAThrowingReadReadsAsUnknownNeverAsEvicted() {
        struct Torn: Error {}
        XCTAssertNil(readIsActive { throw Torn() })
    }

    // MARK: - §6.2.3 left or removed

    func testTheDurableLeaveRecordSelectsTheLeftCopy() throws {
        let state = membershipState(isActive: false, leaveRequestedAtMs: 99)
        XCTAssertEqual(state, .ended(left: true))
        XCTAssertEqual(try XCTUnwrap(terminalRow(state)).headline, endedLeftMessage)
        XCTAssertEqual(endedLeftMessage, "You left this group.")
    }

    /// The fallback is the NEUTRAL string and the narrowing is what needs
    /// positive evidence — the inverse of the usual reflex. "You are no longer
    /// a member of this group" is true of someone who left as well as someone
    /// who was removed; "you left this group" is true of only one. So a device
    /// that lost the record (the ADR-0040 crash window, a reinstall, a restore)
    /// degrades to a sentence that is still correct.
    func testAnAbsentRecordDegradesToTheRemovedCopyWhichIsStillTrue() throws {
        let state = membershipState(isActive: false, leaveRequestedAtMs: nil)
        XCTAssertEqual(try XCTUnwrap(terminalRow(state)).headline, endedRemovedMessage)
        XCTAssertEqual(endedRemovedMessage, "You are no longer a member of this group.")
    }

    func testTheStandingConsequenceIsTheSameEitherWay() throws {
        let left = membershipState(isActive: false, leaveRequestedAtMs: 1)
        let removed = membershipState(isActive: false, leaveRequestedAtMs: nil)
        XCTAssertEqual(try XCTUnwrap(terminalRow(left)).consequence, endedConsequenceMessage)
        XCTAssertEqual(try XCTUnwrap(terminalRow(removed)).consequence, endedConsequenceMessage)
        // Written to be true whether or not a device was offline across its own
        // removal and lost the tail of its history (§6.2.6): it claims exactly
        // what is claimed and no more.
        XCTAssertEqual(
            endedConsequenceMessage,
            "The messages you already received are still here. No new ones will arrive.")
    }

    /// §6.2.3 — and nothing here says WHO. Not because the information is
    /// unobtainable (the removing proposal's author leaf is in hand inside the
    /// core's own gate) but because a client that names a remover it inferred
    /// is one bad inference away from a serious accusation.
    func testTheTerminalRowNamesNobodyInEitherDirection() throws {
        for at in [nil, Int64(12_345)] {
            let row = try XCTUnwrap(
                terminalRow(membershipState(isActive: false, leaveRequestedAtMs: at)))
            let whole = "\(row.headline) \(row.consequence)".lowercased()
            for accusation in ["removed by", "by an admin", "by the owner"] {
                XCTAssertFalse(
                    whole.contains(accusation),
                    "the terminal row must not name a remover, found \"\(accusation)\"")
            }
        }
    }

    // MARK: - §6.2.3 / §9 the durable record, and the prune that must not touch it

    private func scratchLeaveStore() throws -> LeaveRequestStore {
        LeaveRequestStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "leave-req-\(UUID().uuidString)")))
    }

    private func scratchRequestStore() throws -> PendingRequestStore {
        PendingRequestStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "leave-obs-\(UUID().uuidString)")))
    }

    func testTheLeaveRecordRoundTripsPerAccountAndGroup() throws {
        let store = try scratchLeaveStore()
        store.remember(accountIdB64: "acct-a", groupIdB64: "grp-1", atMs: 1_700_000)
        XCTAssertEqual(store.load(accountIdB64: "acct-a", groupIdB64: "grp-1"), 1_700_000)
        XCTAssertNil(
            store.load(accountIdB64: "acct-a", groupIdB64: "grp-2"),
            "another group of the same account must not inherit the record")
        XCTAssertNil(
            store.load(accountIdB64: "acct-b", groupIdB64: "grp-1"),
            "another account must not inherit the record")
    }

    /// This is the whole reason it is a separate field. The observation for a
    /// leave lives under `remove:{ownLeaf}` and every fold prunes an
    /// observation once the core no longer holds the request — and the sweep
    /// that removes you is exactly what stops the core holding it. So the poll
    /// pass that carries the eviction also destroys the only evidence that you
    /// asked for it, and the order of those two steps is an implementation
    /// detail no test pins.
    func testTheLeaveRecordSurvivesThePendingProposalPruneThatDestroysTheObservation() throws {
        let leaves = try scratchLeaveStore()
        let observations = try scratchRequestStore()
        observations.record(
            accountIdB64: "a", groupIdB64: "g", key: "remove:3", atMs: 1_000, own: true)
        leaves.remember(accountIdB64: "a", groupIdB64: "g", atMs: 1_000)

        // The eviction empties the core's pending set, so the fold prunes
        // everything.
        observations.prune(accountIdB64: "a", groupIdB64: "g", liveKeys: [])

        XCTAssertTrue(observations.load(accountIdB64: "a", groupIdB64: "g").isEmpty)
        XCTAssertEqual(leaves.load(accountIdB64: "a", groupIdB64: "g"), 1_000)
        XCTAssertEqual(
            membershipState(
                isActive: false,
                leaveRequestedAtMs: leaves.load(accountIdB64: "a", groupIdB64: "g")),
            .ended(left: true))
    }

    /// First-wins, for the same reason the observation's `atMs` is: a
    /// re-request is the same intent, not a new one.
    func testTheFirstLeaveRecordWinsAndAReRequestNeverReStampsIt() throws {
        let store = try scratchLeaveStore()
        store.remember(accountIdB64: "a", groupIdB64: "g", atMs: 1_000)
        store.remember(accountIdB64: "a", groupIdB64: "g", atMs: 9_000)
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdB64: "g"), 1_000)
    }

    func testTheLeaveRecordIsDiscardedWithTheGroupAndWithTheAccount() throws {
        let store = try scratchLeaveStore()
        store.remember(accountIdB64: "a", groupIdB64: "g", atMs: 1_000)
        store.remember(accountIdB64: "a", groupIdB64: "other", atMs: 2_000)
        store.clear(accountIdB64: "a", groupIdB64: "g")
        XCTAssertNil(store.load(accountIdB64: "a", groupIdB64: "g"))
        XCTAssertEqual(store.load(accountIdB64: "a", groupIdB64: "other"), 2_000)

        store.remember(accountIdB64: "b", groupIdB64: "g", atMs: 3_000)
        store.clearForAccount(accountIdB64: "a")
        XCTAssertNil(store.load(accountIdB64: "a", groupIdB64: "other"))
        XCTAssertEqual(
            store.load(accountIdB64: "b", groupIdB64: "g"), 3_000,
            "wiping one account must not disturb another's record")
    }

    // MARK: - §6.2.5 the composer and the draft it was holding

    private var ended: MembershipState {
        membershipState(isActive: false, leaveRequestedAtMs: nil)
    }

    /// §5.3 argues a pending row's `Remove` must stay visible-but-disabled,
    /// because hiding it would read as the permission having vanished when it
    /// is merely suspended. The permission here has vanished permanently, so
    /// the rule inverts: disable while the state is transient, REPLACE when it
    /// is terminal. A disabled Send on a group you will never be in again is a
    /// control promising a future it does not have.
    func testTheComposerIsReplacedNotDisabled() throws {
        guard case .withdrawn(let notice, _, _) = composerState(ended, draft: "") else {
            return XCTFail("expected the withdrawn arm")
        }
        XCTAssertEqual(notice, composerWithdrawnMessage)
        XCTAssertEqual(composerWithdrawnMessage, "You can no longer send messages to this group.")
    }

    /// The commit lands during a poll, which can be mid-sentence. Silently
    /// discarding what the user typed is a small data loss with no upside — and
    /// the text is kept VERBATIM, leading and trailing whitespace included: it
    /// is the user's text, and this decides whether to show it, not what it
    /// should have been.
    func testTheComposerPreservesAnUnsentDraftVerbatimLabelled() throws {
        guard case .withdrawn(_, let draft, let label) =
            composerState(ended, draft: "  see you tomorrow  ")
        else {
            return XCTFail("expected the withdrawn arm")
        }
        XCTAssertEqual(draft, "  see you tomorrow  ")
        XCTAssertEqual(label, unsentDraftLabel)
        XCTAssertEqual(unsentDraftLabel, "Unsent draft")
    }

    /// An `Unsent draft` heading over three spaces is furniture pretending to
    /// be data.
    func testTheComposerRendersNoDraftBlockWhenTheDraftWasEmpty() throws {
        for blank in ["", "   ", "\n\t"] {
            guard case .withdrawn(_, let draft, let label) = composerState(ended, draft: blank)
            else {
                return XCTFail("expected the withdrawn arm for \(blank.debugDescription)")
            }
            XCTAssertNil(draft, "a blank draft is not worth a block")
            XCTAssertNil(label)
        }
    }

    // MARK: - §6.2.5 the per-message action menu, on the composer's predicate

    /// §6.2.5's rule reached from the other end. Edit / Delete / React each
    /// compose an application message, and `createApplicationMessage` gates on
    /// `is_operational()`, so on a dead group all three are refused —
    /// permanently, for the same reason and at the same instant as Send. A
    /// greyed React item that can never become live again is the shape §5.3
    /// reserves for a permission that is merely suspended.
    func testThePerMessageMenuIsWithdrawnOnceTheMembershipHasEnded() {
        XCTAssertEqual(messageActionsState(ended), .withdrawn)
        XCTAssertEqual(
            messageActionsState(membershipState(isActive: false, leaveRequestedAtMs: 5)),
            .withdrawn)
        XCTAssertEqual(
            messageActionsState(membershipState(isActive: true, leaveRequestedAtMs: nil)),
            .available)
    }

    /// This is the point of the pair, not a restatement of the case above. The
    /// menu and the composer are refused by the SAME core gate, so they must
    /// never disagree about whether this group is writable — and the way two
    /// gates over one predicate drift is that somebody later adds a condition
    /// to one of them. Enumerating every state and asserting the equivalence is
    /// what catches that; asserting each in isolation would not.
    ///
    /// The equivalence is worth MORE on iOS than on the other two clients,
    /// because iOS already carried a second, quieter version of the defect: the
    /// view model's `react` / `beginEdit` / `deleteMessage` each guarded on
    /// `membershipHasEnded` and returned, so the menu opened, offered three
    /// actions and silently answered none of them.
    func testTheMenuAnswersTheComposersQuestionSoTheTwoCannotDrift() {
        let states: [MembershipState] = [
            membershipState(isActive: true, leaveRequestedAtMs: nil),
            membershipState(isActive: nil, leaveRequestedAtMs: nil),
            membershipState(isActive: false, leaveRequestedAtMs: nil),
            membershipState(isActive: false, leaveRequestedAtMs: 5),
        ]
        for state in states {
            let composerLive: Bool
            if case .composer = composerState(state, draft: "draft") {
                composerLive = true
            } else {
                composerLive = false
            }
            let actionsLive = messageActionsState(state) == .available
            XCTAssertEqual(actionsLive, composerLive, "disagreed about \(state)")
        }
    }

    /// Same direction as ``membershipState(isActive:leaveRequestedAtMs:)``'s
    /// own rule and for the same reason: a torn handle is not evidence of an
    /// eviction, and withdrawing the menu on one would hide working controls on
    /// a group the user is still in.
    func testTheMenuDoesNotFailClosedOnAnUnreadableHandle() {
        XCTAssertEqual(
            messageActionsState(membershipState(isActive: nil, leaveRequestedAtMs: 5)),
            .available)
    }

    // MARK: - §6.2.4 / §6.2.7 the chat row and the frozen roster

    func testTheChatListRowGainsOneMutedLineAndNothingElse() {
        let ended = membershipState(isActive: false, leaveRequestedAtMs: 5)
        XCTAssertEqual(chatListLine(ended), chatListEndedLine)
        XCTAssertEqual(chatListEndedLine, "You are no longer a member.")
    }

    /// The row line is deliberately neutral between the two cases: the list is
    /// a glance surface and the distinction belongs in the transcript.
    func testTheChatListLineSaysTheSameThingWhetherYouLeftOrWereRemoved() {
        XCTAssertEqual(
            chatListLine(membershipState(isActive: false, leaveRequestedAtMs: 5)),
            chatListLine(membershipState(isActive: false, leaveRequestedAtMs: nil)))
    }

    /// The role table is untouched by a `Remove` — a removal rewrites no
    /// `GroupContext` — so a removed admin's frozen roster still names them an
    /// admin. Nothing may present that as a LIVE role, which is why the header
    /// is a statement about the list rather than about any row in it.
    func testTheFrozenRosterHeaderIsAStatementAboutTheListNotARow() {
        XCTAssertEqual(
            frozenRosterHeader, "This list stopped updating when your membership ended.")
    }

    // MARK: - §6.2.8 what must stop, and what must be torn down

    func testNothingIsTornDownWhileTheMembershipHolds() {
        let active = membershipState(isActive: true, leaveRequestedAtMs: nil)
        XCTAssertEqual(pendingTeardownSteps(active, alreadyRun: []), [])
    }

    func testEveryStepIsOwedExactlyOnceAndTheSecondPassIsANoOp() {
        var done: Set<EndedTeardownStep> = []
        let first = pendingTeardownSteps(ended, alreadyRun: done)
        XCTAssertEqual(Set(first), Set(endedTeardownSteps))
        for step in first { done.insert(step) }
        XCTAssertEqual(pendingTeardownSteps(ended, alreadyRun: done), [])
        // Idempotent under repetition, which is what makes it safe to drive
        // from a `@Published` observer that can re-run for unrelated reasons.
        XCTAssertEqual(pendingTeardownSteps(ended, alreadyRun: done), [])
    }

    func testAPartiallyCompletedTeardownResumesRatherThanRestarting() {
        XCTAssertEqual(
            pendingTeardownSteps(ended, alreadyRun: [.stopBeaconTicker]),
            [.retirePushSubscription])
    }

    /// The defect this exists for, and web found it by building it wrong
    /// first: one of the two steps is async, so marking it done AFTER the
    /// await leaves a window in which the driver re-runs — and it re-runs on
    /// every roster change, for reasons unrelated to the eviction — reads the
    /// step as still owed, and fires a second concurrent unsubscribe.
    /// "Exactly once" has to be decided BEFORE the first suspension point.
    func testClaimingHandsOutEachStepOnceSoAConcurrentReEntryGetsNothing() {
        var done: Set<EndedTeardownStep> = []
        let first = claimTeardownSteps(ended, alreadyRun: &done)
        XCTAssertEqual(first.count, endedTeardownSteps.count)
        // The re-entry, synchronously, before anything the first pass started
        // has had a chance to resume.
        XCTAssertEqual(claimTeardownSteps(ended, alreadyRun: &done), [])
    }

    func testAReleasedStepIsRetriedAndOnlyThatOne() {
        var done: Set<EndedTeardownStep> = []
        _ = claimTeardownSteps(ended, alreadyRun: &done)
        releaseTeardownStep(&done, .retirePushSubscription)
        XCTAssertEqual(claimTeardownSteps(ended, alreadyRun: &done), [.retirePushSubscription])
        XCTAssertEqual(claimTeardownSteps(ended, alreadyRun: &done), [])
    }

    func testClaimingTakesNothingWhileTheMembershipHolds() {
        var done: Set<EndedTeardownStep> = []
        let active = membershipState(isActive: true, leaveRequestedAtMs: nil)
        XCTAssertEqual(claimTeardownSteps(active, alreadyRun: &done), [])
        XCTAssertTrue(done.isEmpty)
    }

    /// ADR-0043 recorded the ex-member's push-gw row as never deleted, because
    /// "nobody else can delete it and no client has 'you were removed'
    /// handling". This IS that handling: the evicted device deletes its own
    /// row.
    func testTheTeardownListNamesThePushSubscriptionAndTheBeaconTicker() {
        XCTAssertTrue(endedTeardownSteps.contains(.retirePushSubscription))
        XCTAssertTrue(endedTeardownSteps.contains(.stopBeaconTicker))
        XCTAssertEqual(endedTeardownSteps.count, 2)
    }

    // MARK: - §6.2.8 item 3 the ADR-0043 shrink rotation must be suppressed

    private static let ours = String(repeating: "aa", count: 32)
    private static let theirs = String(repeating: "bb", count: 32)
    private static let other = String(repeating: "cc", count: 32)

    /// `membershipRotationDecision` sees a member depart (us) and reads
    /// `ownRole` from the UNTOUCHED role table — a `Remove` rewrites no
    /// `GroupContext` — so before this an evicted admin returned `.rotate` and
    /// then threw on the broadcast, because `createApplicationMessage` is
    /// refused for an inactive group.
    func testAnEvictedAdminDoesNotRotateEvenThoughTheRoleTableStillNamesItAdmin() {
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: [Self.ours, Self.theirs, Self.other],
                currentMemberKeys: [Self.theirs, Self.other],
                ownRole: .admin,
                selfEvicted: true),
            .skipSelfEvicted)
    }

    /// Evaluated FIRST: a device that is out of the group has no business
    /// rotating whatever its stale role table says, and the answer must not
    /// depend on which of two skip reasons happened to be checked first. The
    /// membership here did NOT shrink and the role is a plain member — both of
    /// the other skip arms would have fired.
    func testTheEvictionBeatsEveryOtherVerdictIncludingTheAuthorityCheck() {
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: [Self.ours, Self.theirs],
                currentMemberKeys: [Self.ours, Self.theirs],
                ownRole: .member,
                selfEvicted: true),
            .skipSelfEvicted)
    }

    /// It must also beat the UNKNOWN-membership arm, which is the one that
    /// fires on a torn roster read — the case most likely to coincide with an
    /// eviction.
    func testTheEvictionBeatsTheUnknownMembershipArmToo() {
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: nil,
                currentMemberKeys: nil,
                ownRole: .owner,
                selfEvicted: true),
            .skipSelfEvicted)
    }

    /// The whole point of ADR-0043: the remaining admin re-keys so the evicted
    /// member's `push_gid` stops being notified. Suppressing that would undo
    /// the ADR.
    func testASurvivorOfSomeoneElsesRemovalStillRotates() {
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: [Self.ours, Self.theirs, Self.other],
                currentMemberKeys: [Self.ours, Self.other],
                ownRole: .admin,
                selfEvicted: false),
            .rotate)
    }

    /// Every existing caller and every existing test passes no flag at all.
    func testAnOmittedFlagBehavesExactlyAsBefore() {
        XCTAssertEqual(
            PushGroupRouting.membershipRotationDecision(
                previousMemberKeys: [Self.ours, Self.theirs],
                currentMemberKeys: [Self.ours],
                ownRole: .owner),
            .rotate)
    }

    // MARK: - §6.2.7 / §5 item 6 the exit block's third state

    private func exitInput(
        usesProposalPath: Bool = true,
        isOwner: Bool = false,
        ownLeafKnown: Bool = true,
        membershipEnded: Bool
    ) -> ExitBlockInput {
        ExitBlockInput(
            usesProposalPath: usesProposalPath,
            isOwner: isOwner,
            ownLeafKnown: ownLeafKnown,
            candidateCount: 3,
            waitingRoleChange: nil,
            ownRemovalPending: false,
            membershipEnded: membershipEnded)
    }

    /// One control names a group you are not in; the other is owner-only and
    /// you are not. By §6.1.1's own argument the exit block is the right home
    /// for the one action still honest: its meaning is *the exit*, and once you
    /// are out the only exit-shaped action left is closing the door behind you.
    func testTheExitBlockHoldsDeleteThisChatAndNeitherOfTheOtherTwoControls() {
        XCTAssertEqual(exitBlock(exitInput(membershipEnded: true)), .ended)
    }

    /// A legacy group with no role table still evicts you by direct commit, so
    /// the ended arm must be reached BEFORE the `hidden` gate — otherwise
    /// exactly the groups with no other exit would be denied the one control
    /// that is still honest.
    func testTheEndedArmBeatsEveryOtherArmIncludingAGroupWithNoRoleTable() {
        XCTAssertEqual(
            exitBlock(exitInput(usesProposalPath: false, membershipEnded: true)), .ended)
        XCTAssertEqual(
            exitBlock(exitInput(ownLeafKnown: false, membershipEnded: true)), .ended)
        XCTAssertEqual(exitBlock(exitInput(isOwner: true, membershipEnded: true)), .ended)
    }

    func testTheLiveExitBlockArmsAreUntouched() {
        XCTAssertEqual(
            exitBlock(exitInput(membershipEnded: false)),
            .member(leave: GuardedControl(enabled: true, disabledReason: nil)))
    }

    /// Precise rather than sweeping, and the precision is load-bearing: the
    /// messages really are gone, and the inert MLS handle is not.
    func testTheDeleteConfirmationStatesExactlyWhatIsDestroyed() {
        XCTAssertEqual(deleteChatControl, "Delete this chat")
        XCTAssertEqual(deleteChatTitle, "Delete this chat?")
        XCTAssertEqual(
            deleteChatBody,
            "This removes the conversation and its messages from this device. "
                + "There is no copy anywhere else and this cannot be undone.")
    }

    // MARK: - §6.2.4 the per-group history delete

    private var logPayload: MessageLogPayload {
        MessageLogPayload(
            version: 1,
            groups: [
                "aaaa": PersistedGroup(cursor: 7, messages: []),
                "bbbb": PersistedGroup(cursor: 9, messages: []),
            ])
    }

    func testWithoutGroupDropsExactlyOneGroupAndKeepsTheRest() throws {
        let next = try XCTUnwrap(withoutGroup(logPayload, groupIdHex: "aaaa"))
        XCTAssertEqual(Array(next.groups.keys), ["bbbb"])
        XCTAssertEqual(next.version, 1)
    }

    /// Reported as absent so the caller can skip the write entirely — not
    /// merely an optimisation: a rewrite re-encrypts and republishes the whole
    /// per-account log, and doing that for a no-op is work with no result.
    func testWithoutGroupReportsAnAbsentGroupSoTheCallerCanSkipTheWrite() {
        XCTAssertNil(withoutGroup(logPayload, groupIdHex: "cccc"))
    }

    func testWithoutGroupDoesNotMutateThePayloadItWasGiven() {
        let payload = logPayload
        _ = withoutGroup(payload, groupIdHex: "aaaa")
        XCTAssertEqual(Set(payload.groups.keys), ["aaaa", "bbbb"])
    }

    // ---------------------------------------------------------------------
    // The whole chain, against a REAL MLS eviction
    // ---------------------------------------------------------------------

    /// Everything above drives the rules on hand-fed booleans. This drives them
    /// on a group OpenMLS actually evicted us from, which is the only way to
    /// pin the two claims the rules rest on but cannot themselves establish:
    /// that `Group.isActive()` really does go false when the removing commit
    /// merges, and that the repository's own gates read it.
    ///
    /// It also pins §6.2.8 item 1 by its CONSEQUENCE rather than by inspection:
    /// a row seeded on the DS after the eviction must still be sitting there,
    /// with the cursor where it was. Without the gate the poll fetches it, the
    /// per-message catch swallows the `UseAfterEviction`, and the cursor
    /// advances — the row is consumed and unreadable forever, which is exactly
    /// the failure the gate exists to prevent.
    private struct EvictedFixture {
        let repo: ChatRepository
        let groupIdHex: String
        let accountIdB64: String
        let outboxDir: URL
        /// This fixture's private journal on the scripted DS. Every row count in
        /// this file is read through it, so a straggling publish from ANOTHER
        /// fixture's fire-and-forget `Task` cannot be counted here — see the
        /// scoping note on `ScriptedDsURLProtocol`.
        let dsScope: String
    }

    /// Owner A + member B. A removes B; B applies the commit and is out.
    private func makeEvictedRepo() throws -> EvictedFixture {
        let dsScope = ScriptedDsURLProtocol.freshScope()

        let ownerSeed = Data(repeating: 0x11, count: 32)
        let memberSeed = Data(repeating: 0x55, count: 32)
        let owner = try Identity.fromSeed(seed: ownerSeed)
        let member = try Identity.fromSeed(seed: memberSeed)

        // `Group.create` seeds the ADR-0022 role table, so this group routes
        // membership through ADR-0045 proposals — the path a real removal takes.
        let ownerGroup = try Group.create(identity: owner)
        let added = try ownerGroup.addMemberFromWire(
            identity: owner, keyPackage: try member.createKeyPackage())
        let memberGroup = try Group.joinFromWelcome(
            welcome: added.welcome, identity: member)

        XCTAssertTrue(
            try memberGroup.isActive(),
            "the fixture must start with a LIVE membership, or the assertions below "
                + "would pass vacuously")

        let memberKey = try member.signaturePublicKey()
        let memberLeaf = try XCTUnwrap(
            try ownerGroup.members().first { $0.signaturePublicKey == memberKey }?.leafIndex)

        // The device under test is promoted to ADMIN before it is removed, and
        // that is the point of the fixture rather than decoration. A `Remove`
        // rewrites no `GroupContext`, so the role table is UNTOUCHED by the
        // eviction and goes on naming this device an admin afterwards — which
        // is precisely why the shrink rotation would have fired and why the
        // beacon would have been emitted. Evicting a plain member instead makes
        // both of those pass for the wrong reason: the rank check and the
        // authority check already answer no.
        let promote = try ownerGroup.proposeGroupRoles(
            identity: owner, owner: try owner.signaturePublicKey(), admins: [memberKey])
        _ = try memberGroup.processIncomingVerifyingRoles(identity: member, bytes: promote)
        let promoted = try XCTUnwrap(try ownerGroup.commitPendingProposals(identity: owner))
        _ = try memberGroup.processIncomingVerifyingRoles(
            identity: member, bytes: promoted.commit)
        XCTAssertEqual(
            try memberGroup.ownRole(identity: member), .admin,
            "the device under test must hold the role whose stale table drives the finding")

        // The owner proposes the removal and sweeps it, exactly as a committer
        // does — not a direct commit, because a role-table group has none.
        //
        // The proposal is DELIVERED to the member before the commit, and that
        // is not fixture ceremony: an ADR-0045 sweep commits proposals BY
        // REFERENCE, and a peer that never stored the proposal has nothing for
        // OpenMLS's resolution to find. Skipping this step is refused by RFC
        // 9420 itself — the first draft of this fixture did, and the member's
        // apply failed with `InvalidCommit(MissingProposal)`.
        let proposal = try ownerGroup.proposeRemoveMember(
            identity: owner, leafIndex: memberLeaf)
        _ = try memberGroup.processIncomingVerifyingRoles(identity: member, bytes: proposal)
        XCTAssertTrue(
            try memberGroup.isActive(),
            "a PROPOSAL advances no state: we are still fully in the group, still "
                + "receiving and still decrypting (§6.2.11 — no optimistic transition)")
        let swept = try XCTUnwrap(
            try ownerGroup.commitPendingProposals(identity: owner),
            "the owner must have produced a commit carrying the removal")

        // And here is the moment the whole section is about: the removed device
        // applies the commit itself, on an ordinary poll, and nothing in the
        // return value says what just happened.
        _ = try memberGroup.processIncomingVerifyingRoles(
            identity: member, bytes: swept.commit)
        XCTAssertFalse(
            try memberGroup.isActive(),
            "OpenMLS must report the group inactive once the self-removing commit merged")

        let groupId = try memberGroup.groupId()
        let groupIdHex = DsClient.pseudonymHex(groupId)

        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let accountIdB64 = base64Encode(accountIdBytes)
        let suffix = UUID().uuidString

        // The snapshot is taken AFTER the merge, which is what makes the state
        // survive a restart: OpenMLS persists `Inactive` before merging.
        let snapshotStore = IdentitySnapshotStore()
        snapshotStore.putSnapshot(
            accountIdB64: accountIdB64, blob: try member.snapshotStorage())
        let knownGroupsStore = KnownGroupsStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "ended-k-\(suffix)")))
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("ended-\(suffix)")
        try FileManager.default.createDirectory(
            at: outboxDir, withIntermediateDirectories: true)

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
                defaults: try XCTUnwrap(UserDefaults(suiteName: "ended-b-\(suffix)"))),
            pendingRequestStore: PendingRequestStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "ended-p-\(suffix)"))),
            lastObservedOwnerStore: LastObservedOwnerStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "ended-o-\(suffix)"))),
            leaveRequestStore: LeaveRequestStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "ended-l-\(suffix)"))))
        // Phase 1 fixture: the chats run on the account identity the snapshot
        // was taken under (ADR-0050 device identities are exercised elsewhere).
        repo.deviceIdentityInstaller = { _, _ in nil }
        repo.bootstrap(
            secrets: IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: Data(repeating: 0x22, count: 32),
                accountIdentitySeed: memberSeed,
                sessionToken: "t",
                sessionExpiresAtMs: 0))

        return EvictedFixture(
            repo: repo, groupIdHex: groupIdHex, accountIdB64: accountIdB64,
            outboxDir: outboxDir, dsScope: dsScope)
    }

    func testARealEvictionIsSeenByTheRepositoryAndNarrowsWithTheLeaveRecord() throws {
        let f = try makeEvictedRepo()
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }

        XCTAssertEqual(
            f.repo.isActive(groupIdHex: f.groupIdHex), false,
            "the repository must read the core's own predicate off the restored handle")
        XCTAssertEqual(
            f.repo.membershipStateNow(groupIdHex: f.groupIdHex), .ended(left: false),
            "with no leave record the copy must be the REMOVED form, which is true either way")

        // And the narrowing, on the one piece of local evidence there is.
        f.repo.noteLeaveRequested(groupIdHex: f.groupIdHex)
        XCTAssertEqual(
            f.repo.membershipStateNow(groupIdHex: f.groupIdHex), .ended(left: true))
    }

    /// §6.2.8 item 1 — the poll STOPS, pinned by its consequence.
    ///
    /// Without the gate the fetch happens, the per-message catch swallows the
    /// `UseAfterEviction`, and the cursor advances past a row that can now
    /// never be read. iOS is the most exposed of the three clients here rather
    /// than the least: the poll is a continuous long poll driven by the
    /// screen's `.task`, so the swallowed-refusal loop would run forever.
    func testThePollStopsForAGroupWeAreNoLongerIn() async throws {
        let f = try makeEvictedRepo()
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }

        let cursorBefore = f.repo.state.threads
            .first { $0.groupIdHex == f.groupIdHex }?.cursor
        ScriptedDsURLProtocol.seed(Data([0xDE, 0xAD, 0xBE, 0xEF]), scope: f.dsScope)

        try await f.repo.pollGroup(groupIdHex: f.groupIdHex)

        XCTAssertEqual(
            ScriptedDsURLProtocol.rowCount(scope: f.dsScope), 1,
            "the row must still be on the DS")
        XCTAssertEqual(
            f.repo.state.threads.first { $0.groupIdHex == f.groupIdHex }?.cursor,
            cursorBefore,
            "the cursor must not advance past a row this device can never read")
    }

    /// §6.2.8 item 2 — and no beacon, whatever the foreground says. A beacon
    /// means *able to sweep*, and this device can never sweep again.
    ///
    /// The fixture's device is an ADMIN, so this does not pass for the wrong
    /// reason: the role table is untouched by the removal, `ownCommitterRank`
    /// still reports a rank, and without the gate the emitter would reach the
    /// publish and only then discover that `createApplicationMessage` refuses.
    func testNoBeaconIsEmittedForAGroupWeAreNoLongerIn() async throws {
        let f = try makeEvictedRepo()
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }

        let verdict = await f.repo.emitCommitterBeaconIfDue(
            groupIdHex: f.groupIdHex, foreground: true)
        XCTAssertEqual(verdict, .notACandidate)
        XCTAssertEqual(
            ScriptedDsURLProtocol.rowCount(scope: f.dsScope), 0,
            "an emitted beacon would be a POST, and there must not be one")
    }

    /// §6.2.4 — the chat-list row's snapshot carries the state, so the list can
    /// render its muted line and drop the unread badge without holding a live
    /// `Group`.
    func testTheThreadSnapshotCarriesTheEndedState() throws {
        let f = try makeEvictedRepo()
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }

        let thread = try XCTUnwrap(
            f.repo.state.threads.first { $0.groupIdHex == f.groupIdHex })
        XCTAssertTrue(thread.membershipEnded)
        XCTAssertEqual(
            chatListLine(membershipState(isActive: !thread.membershipEnded,
                                         leaveRequestedAtMs: nil)),
            chatListEndedLine)
    }

    /// §6.2.4 — `Delete this chat` removes the row, and is refused on a group
    /// this device is still in.
    func testDeleteChatDropsTheRegistryRowAndRefusesOnALiveGroup() throws {
        let f = try makeEvictedRepo()
        defer { try? FileManager.default.removeItem(at: f.outboxDir) }

        f.repo.noteLeaveRequested(groupIdHex: f.groupIdHex)
        XCTAssertTrue(f.repo.deleteChat(groupIdHex: f.groupIdHex))
        XCTAssertTrue(
            f.repo.state.threads.allSatisfy { $0.groupIdHex != f.groupIdHex },
            "the row must be gone from the list the delete was reached from")
        XCTAssertNil(
            f.repo.leaveRequestedAt(groupIdHex: f.groupIdHex),
            "and the durable leave record goes with the group record")
        XCTAssertFalse(
            f.repo.deleteChat(groupIdHex: f.groupIdHex),
            "a second delete has nothing to remove")
    }

    /// The layered half of the same rule: the repository re-checks the state
    /// rather than trusting the control it was drawn behind, so a stale gate
    /// can never delete a LIVE conversation.
    func testDeleteChatRefusesWhileTheMembershipHolds() throws {
        let dsScope = ScriptedDsURLProtocol.freshScope()
        let ownerSeed = Data(repeating: 0x33, count: 32)
        let owner = try Identity.fromSeed(seed: ownerSeed)
        let group = try Group.create(identity: owner)
        let groupIdHex = DsClient.pseudonymHex(try group.groupId())
        XCTAssertTrue(try group.isActive())

        var accountIdBytes = Data(count: 16)
        accountIdBytes.withUnsafeMutableBytes {
            _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        let accountIdB64 = base64Encode(accountIdBytes)
        let suffix = UUID().uuidString
        let snapshotStore = IdentitySnapshotStore()
        snapshotStore.putSnapshot(
            accountIdB64: accountIdB64, blob: try owner.snapshotStorage())
        let knownGroupsStore = KnownGroupsStore(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "live-k-\(suffix)")))
        knownGroupsStore.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        let outboxDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("live-\(suffix)")
        defer { try? FileManager.default.removeItem(at: outboxDir) }
        try FileManager.default.createDirectory(
            at: outboxDir, withIntermediateDirectories: true)

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
                defaults: try XCTUnwrap(UserDefaults(suiteName: "live-b-\(suffix)"))),
            pendingRequestStore: PendingRequestStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "live-p-\(suffix)"))),
            lastObservedOwnerStore: LastObservedOwnerStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "live-o-\(suffix)"))),
            leaveRequestStore: LeaveRequestStore(
                defaults: try XCTUnwrap(UserDefaults(suiteName: "live-l-\(suffix)"))))
        // Phase 1 fixture: the chats run on the account identity the snapshot
        // was taken under (ADR-0050 device identities are exercised elsewhere).
        repo.deviceIdentityInstaller = { _, _ in nil }
        repo.bootstrap(
            secrets: IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: Data(repeating: 0x22, count: 32),
                accountIdentitySeed: ownerSeed,
                sessionToken: "t",
                sessionExpiresAtMs: 0))

        XCTAssertEqual(repo.membershipStateNow(groupIdHex: groupIdHex), .active)
        XCTAssertFalse(
            repo.deleteChat(groupIdHex: groupIdHex),
            "a live conversation must never be deletable through this path")
        XCTAssertTrue(repo.state.threads.contains { $0.groupIdHex == groupIdHex })
    }
}
