// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0045 UX §6.2 — the end of your own membership.
///
/// iOS mirror of the web reference `apps/web/src/chat/membership-ended.ts`.
///
/// **Since ADR-0087 section 9 (2026-10-31) this presentation is what is left
/// for a device whose own leaf was retired while its account stays in the
/// group.** A person who is out of a group has none of it: the row, the history
/// and the MLS group go (``MembershipEndRules``, `ChatRepository+MembershipEnd`).
/// "Keep the history" below is about that retired leaf.
///
/// ## What this is for
///
/// The commit that removes your own leaf arrives on an ordinary poll, merges,
/// and the group goes permanently dead in the MLS core. Before this module the
/// client carried on as though nothing had happened: the chat stayed in the
/// list, the composer stayed enabled, the members sheet kept offering
/// `Leave group` on a group already left, and the first thing the user learned
/// was that sending failed.
///
/// ## Why this is a plain module and not part of `ChatScreen.swift`
///
/// A SwiftUI view cannot be asserted on, so every rule §6.2 states lives here,
/// where it is executable, and the screen renders what this module decides —
/// the same split `PendingRequests.swift`, `OwnershipTransfer.swift`,
/// `LeaveGroup.swift` and `CommitterBeacon.swift` already use.
///
/// ## The one asymmetry the whole design falls out of
///
/// After the removal the client can still **read** everything about the group
/// and can no longer **do** anything with it. `members()`, `epoch()`,
/// `memberCount()`, `ownLeafIndex()` and `groupRoles()` are plain reads of the
/// public group with no state gate, while `createApplicationMessage`,
/// `exportSecret` and every further decrypt raise `UseAfterEviction`. That is
/// not a limitation to work around — it is precisely the shape of the correct
/// UI, and it is why the answer to *keep the history?* is yes and the answer to
/// *keep the composer?* is no.
///
/// Two consequences worth stating because they are easy to get wrong:
///
///   * **The state is terminal and survives a restart.** OpenMLS calls
///     `Inactive` "a terminal state that the group can not exit from", and
///     `write_group_state` runs before the merge, so a snapshot taken
///     afterwards restores an inactive group. There is no transient window to
///     debounce and nothing to re-check later.
///   * **The local view is frozen for good.** `unprotect_message` refuses on
///     `!is_active()` before it looks at the message at all, and the refusal is
///     NOT epoch-scoped — so rows still sitting on the DS are unreadable
///     forever, even ones sealed at an epoch this device still holds keys for.
///     The roster, the epoch and the role table stop at the values that commit
///     produced and never move again.
///
/// One more, because iOS has to reason about it and the reflex is wrong:
/// `epoch()` reports N+1 while the epoch secrets are still the epoch-N object
/// the merge's `PublicState` branch never replaced. **Never infer membership
/// from the epoch.** ``readIsActive(_:)`` is the only signal.
///
/// ## Which rules here actually discriminate (MEASURED — 22 probes were run)
///
/// Each rule below was re-broken as a deliberate regression AFTER the feature
/// was green, `xcodebuild test -only-testing:LakkoTests/MembershipEndedTests`
/// was run, the failure quoted was observed, and the probe reverted. Three of
/// the probes target the arms this feature grafts onto modules that already
/// existed, and they are recorded here rather than there because they are one
/// design and the reader looking for the evidence starts here.
///
/// 1. **``membershipState(isActive:leaveRequestedAtMs:)`` always returns
///    `.active`** — the eviction is never detected. **14 of 39 tests fail**
///    across five sections, because the composer, the teardown, the terminal
///    row, the chat-list line and the leave record all hang off this one
///    predicate:
///
///        testEvictionIsDetectedOnceTheRemovingCommitHasBeenApplied :
///          XCTAssertEqual failed: ("active") is not equal to ("ended(left: false)")
///        testTheComposerIsReplacedNotDisabled : failed - expected the withdrawn arm
///        testEveryStepIsOwedExactlyOnceAndTheSecondPassIsANoOp :
///          XCTAssertEqual failed: ("[]") is not equal to
///          ("[…retirePushSubscription, …stopBeaconTicker]")
///
/// 2. **An unknown `isActive` is treated as EVICTED** (the fail-closed reflex,
///    which is the wrong direction here) — 5 failures:
///
///        testATornReadIsNotEvidenceOfAnEviction :
///          XCTAssertEqual failed: ("ended(left: true)") is not equal to ("active")
///
///    A torn handle is not evidence of anything. Asserting `You are no longer a
///    member of this group.` on it is a false statement the user cannot check,
///    where leaving the chat as it is today is merely the status quo.
///
/// 3. **The leave record is read BEFORE `isActive`** — *asked to leave* is
///    treated as *left*, which is §6.2.11's forbidden optimistic transition:
///
///        testAnActiveGroupWithALeaveRequestOutstandingIsStillActive :
///          XCTAssertEqual failed: ("ended(left: true)") is not equal to ("active")
///
/// 4. **The FALLBACK string is the LEFT one** (the narrowing and the neutral
///    case swapped) — a device that lost the record would tell a user they left
///    a group they were thrown out of:
///
///        testAnAbsentRecordDegradesToTheRemovedCopyWhichIsStillTrue :
///          XCTAssertEqual failed: ("You left this group.") is not equal to
///          ("You are no longer a member of this group.")
///
/// 5. **``LeaveRequestStore/remember(accountIdB64:groupIdB64:atMs:)`` overwrites
///    an existing stamp:**
///
///        testTheFirstLeaveRecordWinsAndAReRequestNeverReStampsIt :
///          XCTAssertEqual failed: ("Optional(9000)") is not equal to ("Optional(1000)")
///
/// 6. **``pendingTeardownSteps(_:alreadyRun:)`` ignores `alreadyRun`** — 4
///    failures. Re-firing `retirePushSubscription` is a network call per
///    re-run, and a teardown that cannot say it is finished is one nothing can
///    be sequenced after:
///
///        testEveryStepIsOwedExactlyOnceAndTheSecondPassIsANoOp :
///          XCTAssertEqual failed: ("[…stopBeaconTicker, …retirePushSubscription]")
///          is not equal to ("[]")
///
/// 7. **``claimTeardownSteps(_:alreadyRun:)`` does not claim what it hands
///    out** — the defect web found by building it the other way first:
///
///        testClaimingHandsOutEachStepOnceSoAConcurrentReEntryGetsNothing :
///          XCTAssertEqual failed: ("[…stopBeaconTicker, …retirePushSubscription]")
///          is not equal to ("[]")
///
/// 8. **``composerState(_:draft:)`` keeps the composer once membership has
///    ended** — 3 failures:
///
///        testTheComposerIsReplacedNotDisabled : failed - expected the withdrawn arm
///
/// 9. **A whitespace-only draft is preserved** — an `Unsent draft` heading over
///    three spaces is furniture pretending to be data:
///
///        testTheComposerRendersNoDraftBlockWhenTheDraftWasEmpty :
///          XCTAssertNil failed: "   " - a blank draft is not worth a block
///
/// 10. **The preserved draft is TRIMMED** rather than kept verbatim. It is the
///     user's text, and this decides whether to show it, not what it should
///     have been:
///
///        testTheComposerPreservesAnUnsentDraftVerbatimLabelled :
///          XCTAssertEqual failed: ("Optional("see you tomorrow")") is not equal
///          to ("Optional("  see you tomorrow  ")")
///
/// 11. **``releaseTeardownStep(_:_:)`` does nothing** — a failed step is never
///     retried:
///
///        testAReleasedStepIsRetriedAndOnlyThatOne :
///          XCTAssertEqual failed: ("[]") is not equal to ("[…retirePushSubscription]")
///
/// 12. **A THROWING `isActive()` reads as EVICTED** rather than as unknown:
///
///        testAThrowingReadReadsAsUnknownNeverAsEvicted : XCTAssertNil failed: "false"
///
/// 13. **`membershipRotationDecision`'s `selfEvicted` arm runs AFTER the
///     membership diff** (`PushGroupRouting.swift`):
///
///        testTheEvictionBeatsTheUnknownMembershipArmToo :
///          XCTAssertEqual failed: ("skipUnknownMembership") is not equal to
///          ("skipSelfEvicted")
///
/// 14. **… or is absent entirely** — 3 failures, including the finding itself,
///     an evicted admin rotating because its role table still names it admin:
///
///        testAnEvictedAdminDoesNotRotateEvenThoughTheRoleTableStillNamesItAdmin :
///          XCTAssertEqual failed: ("rotate") is not equal to ("skipSelfEvicted")
///
///     **… or is MOVED to last**, just before `.rotate`, which is the
///     placement that looks harmless and is not:
///
///        testTheEvictionBeatsEveryOtherVerdictIncludingTheAuthorityCheck :
///          XCTAssertEqual failed: ("skipNoShrink") is not equal to ("skipSelfEvicted")
///
/// 15. **`exitBlock`'s `ended` arm runs AFTER the `hidden` gate**
///     (`OwnershipTransfer.swift`) — which would withhold the one control that
///     is still honest from exactly the legacy groups that have no other exit:
///
///        testTheEndedArmBeatsEveryOtherArmIncludingAGroupWithNoRoleTable :
///          XCTAssertEqual failed: ("hidden") is not equal to ("ended")
///
/// 16. **… or skips the OWNER**, leaving an ex-owner a live
///     `Transfer ownership` over a group they are not in:
///
///        testTheEndedArmBeatsEveryOtherArmIncludingAGroupWithNoRoleTable :
///          XCTAssertEqual failed: ("owner(transfer: …enabled: true…)") is not
///          equal to ("ended")
///
/// 17. **`withoutGroup` reports a payload for an ABSENT group**
///     (`MessageLogStore.swift`), so the caller re-encrypts and rewrites the
///     whole per-account envelope for a no-op:
///
///        testWithoutGroupReportsAnAbsentGroupSoTheCallerCanSkipTheWrite :
///          XCTAssertNil failed: "MessageLogPayload(version: 1, groups: […])"
///
/// 18. **``endedTeardownSteps`` ships one of the two steps** — the
///     four-of-five hazard §14 names, caught by name rather than by count:
///
///        testTheTeardownListNamesThePushSubscriptionAndTheBeaconTicker :
///          XCTAssertTrue failed
///
/// 19. **The store key drops the GROUP component**, so one group's record
///     answers for every group:
///
///        testTheLeaveRecordRoundTripsPerAccountAndGroup :
///          XCTAssertNil failed: "1700000" - another group of the same account
///          must not inherit the record
///
/// 20. **``LeaveRequestStore/clearForAccount(accountIdB64:)`` is not scoped**
///     to the account:
///
///        testTheLeaveRecordIsDiscardedWithTheGroupAndWithTheAccount :
///          XCTAssertEqual failed: ("nil") is not equal to ("Optional(3000)") -
///          wiping one account must not disturb another's record
///
/// 21. **``chatListLine(_:)`` leaks the left/removed distinction** into the
///     glance surface, where §6.2.4 gives it one string:
///
///        testTheChatListLineSaysTheSameThingWhetherYouLeftOrWereRemoved :
///          XCTAssertEqual failed: ("Optional("You left this group.")") is not
///          equal to ("Optional("You are no longer a member.")")
///
/// 22. **``messageActionsState(_:)`` always returns `.available`** — the defect
///     it closes, the per-message menu still offered on a dead group:
///
///        testThePerMessageMenuIsWithdrawnOnceTheMembershipHasEnded :
///          XCTAssertEqual failed: ("available") is not equal to ("withdrawn")
///        testTheMenuAnswersTheComposersQuestionSoTheTwoCannotDrift :
///          XCTAssertEqual failed: ("true") is not equal to ("false") -
///          disagreed about ended(left: false)
///
///     The second is the one that matters: it is the equivalence with
///     ``composerState(_:draft:)``, so a menu that stops agreeing with the
///     composer fails whichever of the two was changed.

// MARK: - Copy (§6.2) — one definition each, so the view cannot drift from it
//
// From the catalog (ADR-0061), read at every render so the text follows a
// live language switch.

/// §6.2.3 — the narrowing, applied only on positive evidence.
///
/// True of exactly one of the two cases, which is why it needs the durable
/// record and why ``endedRemovedMessage`` is the fallback rather than this.
public var endedLeftMessage: String { Localization.text("chat.ended.left") }

/// §6.2.3 — the neutral string, and the FALLBACK.
///
/// This inverts the usual reflex to treat the neutral case as the special one.
/// "You are no longer a member of this group" is true of someone who left as
/// well as someone who was removed; "you left this group" is true of only one.
/// So a device that lost the record — the ADR-0040 crash window, a reinstall, a
/// restore — degrades to a sentence that is still correct rather than to one
/// that is wrong.
///
/// And nothing here says **who**. Not because the information is unobtainable
/// (the removing proposal's author leaf is in hand inside the core's own gate)
/// but because a client that names a remover it inferred is one bad inference
/// away from a serious accusation, and because §5.1's discipline — state
/// inaction, name nobody, blame nobody — is the same discipline whether the
/// news is a wait or an ending.
public var endedRemovedMessage: String { Localization.text("chat.ended.no_longer_member") }

/// §6.2.5 line 2 — the standing consequence, identical in both cases.
///
/// Written to claim exactly what is claimed and no more. A device that was
/// offline across its own removal loses the tail of its history silently and
/// permanently (§6.2.6): the decrypt refusal is not epoch-scoped, so rows still
/// on the DS at the moment the commit is applied are unreachable forever. The
/// design deliberately does not mention that — the user cannot tell how much
/// they are missing and no client can tell them — and *the messages you already
/// received are still here* stays true either way.
public var endedConsequenceMessage: String { Localization.text("chat.ended.consequence") }

/// §6.2.4 — the chat-list row's one muted secondary line. Deliberately neutral
/// between left and removed: the list is a glance surface and the distinction
/// belongs in the transcript.
public var chatListEndedLine: String { Localization.text("chats.row.ended") }

/// §6.2.5 — the static bar that REPLACES the composer, in the same slot so the
/// layout does not jump.
public var composerWithdrawnMessage: String { Localization.text("chat.ended.composer") }

/// §6.2.5 — the muted label above the preserved draft.
public var unsentDraftLabel: String { Localization.text("chat.ended.unsent_draft") }

/// UX 9.4 — the row of a group this device asked to leave, until it lands.
public var waitingToLeaveLine: String { Localization.text("chat.members.pending.waiting_leave") }

/// §6.2.7 — one line at the top of the frozen roster.
///
/// A statement about the **list**, not about any row in it, and that is
/// load-bearing: the role table is untouched by a `Remove` — a removal rewrites
/// no `GroupContext` — so a removed admin's frozen roster still names them an
/// admin, and nothing in the UI may present that as a live role.
/// *"Stopped updating"* is the literal truth of the frozen local view rather
/// than a euphemism for it.
public var frozenRosterHeader: String { Localization.text("chat.members.frozen") }

/// §6.2.4 — the one control still honest once you are out.
public var deleteChatControl: String { Localization.text("chat.delete.control") }

/// §6.2.4 — confirmation title.
public var deleteChatTitle: String { Localization.text("chat.delete.title") }

/// §6.2.4 — confirmation body.
public var deleteChatBody: String { Localization.text("chat.delete.body") }

// MARK: - The state

/// Whether this device is still in the group, and — if not — how that happened,
/// as far as this device can honestly tell.
public enum MembershipState: Equatable {
    case active
    /// `left` is `true` only on positive local evidence that we asked (§6.2.3).
    case ended(left: Bool)
}

/// Read the core's own membership predicate, or `nil` when the handle cannot be
/// read at all.
///
/// `Group.isActive()` is the variable `createApplicationMessage`,
/// `exportSecret` and `unprotect_message` each consult before refusing, so a
/// composer gated on it is disabled for the reason the send would have failed.
/// The available alternative — *no member record whose signature key is ours* —
/// is sound today and would work, but it rests on a leaf's signature key being
/// the account's signature key, which ADR-0022 names as an open follow-up (the
/// Phase-2b per-device-leaf resolution). The day that lands, the inference
/// breaks silently, in the dangerous direction, in three clients at once.
/// `isActive` is about *this handle* and is immune to it.
///
/// Takes a throwing closure rather than a `Group` so the rule stays testable
/// without an MLS provider — the same shape the sweep's ports use. A throw
/// reads as `nil` rather than as `false`; see ``membershipState(isActive:leaveRequestedAtMs:)``.
public func readIsActive(_ read: () throws -> Bool) -> Bool? {
    try? read()
}

/// Decide the membership state.
///
/// **The order of the two reads is the rule.** `isActive` is consulted first
/// and alone decides whether the membership has ended; the leave record only
/// ever *narrows the copy* afterwards. Reading the record first would make
/// *asked to leave* mean *left*, which is §6.2.11's forbidden optimistic
/// transition: between publishing the request and the commit merging you are
/// still fully in the group — still receiving, still decrypting — and §3's
/// argument against an optimistic `Remove` is the same argument pointed at
/// yourself.
///
/// **An unknown `isActive` fails toward `.active`, and that is deliberate even
/// though it is the un-reflexive direction.** Fail-closed is the house rule for
/// a claim about *cryptographic capability*; this is a claim about *fact*. A
/// torn handle is not evidence of an eviction, and asserting `You are no longer
/// a member of this group.` on one is a false statement the user has no way to
/// check. Getting it wrong the other way costs only the status quo: the
/// composer stays and a send fails, which is exactly what happened before this
/// module existed.
public func membershipState(isActive: Bool?, leaveRequestedAtMs: Int64?) -> MembershipState {
    guard isActive == false else { return .active }
    return .ended(left: leaveRequestedAtMs != nil)
}

/// The standing gate every background discipline consults (§6.2.8).
///
/// A gate rather than a one-shot, because three of the five disciplines are
/// things that must *keep not happening*: the poll can be re-entered from a
/// dozen call sites, the rotation decision is re-evaluated per commit batch,
/// and the call buttons are re-rendered on every roster change.
public func membershipEnded(_ state: MembershipState) -> Bool {
    if case .ended = state { return true }
    return false
}

// MARK: - §6.2.5 the terminal row and the composer

/// The two lines of the terminal timeline row.
public struct TerminalRowCopy: Equatable {
    /// ``endedLeftMessage`` or ``endedRemovedMessage``.
    public var headline: String
    /// ``endedConsequenceMessage``.
    public var consequence: String

    public init(headline: String, consequence: String) {
        self.headline = headline
        self.consequence = consequence
    }
}

/// The terminal row, or `nil` while the membership holds.
///
/// It goes in the **timeline**, not in a banner, and §10 already says why:
/// *only outcomes belong in the timeline* — and this is an outcome, one that
/// happened at a definite point in the transcript. A row marks *where* the
/// history stops, survives scrollback and needs no dismissal state.
public func terminalRow(_ state: MembershipState) -> TerminalRowCopy? {
    guard case .ended(let left) = state else { return nil }
    return TerminalRowCopy(
        headline: left ? endedLeftMessage : endedRemovedMessage,
        consequence: endedConsequenceMessage)
}

/// The chat-list row's secondary line, or `nil` while the membership holds.
public func chatListLine(_ state: MembershipState) -> String? {
    membershipEnded(state) ? chatListEndedLine : nil
}

/// What occupies the composer's slot.
public enum ComposerState: Equatable {
    case composer
    /// `notice` is ``composerWithdrawnMessage``. `draft` is the unsent text,
    /// verbatim, or `nil` when there was nothing worth keeping; `draftLabel` is
    /// ``unsentDraftLabel``, or `nil` alongside a `nil` draft.
    case withdrawn(notice: String, draft: String?, draftLabel: String?)
}

/// Decide the composer's slot (§6.2.5).
///
/// **Replaced, not disabled, and the distinction is principled.** §5.3 argues a
/// pending row's `Remove` must stay *visible but disabled*, because hiding it
/// would read as the permission having vanished when it is merely suspended.
/// The permission here has vanished, permanently and by design. So the rule is:
/// **disable while the state is transient, remove when it is terminal** — and a
/// disabled Send button on a group you will never be in again is a control
/// promising a future it does not have.
///
/// **The draft is preserved, and NOT in an input.** The commit lands during a
/// poll, which can be mid-sentence, and silently discarding what the user typed
/// is a small data loss with no upside. But the obvious fix — leave the text
/// field there, disabled — is wrong on iOS specifically: a disabled SwiftUI
/// `TextField` is not selectable, so the user can see their words and cannot
/// get them out. The cross-platform shape is therefore **selectable,
/// non-editable text**, which on iOS is `Text` + `.textSelection(.enabled)` and
/// is never a disabled `TextField`.
///
/// A blank-or-whitespace draft yields no block at all: an `Unsent draft`
/// heading over three spaces is furniture pretending to be data. The draft that
/// IS kept is preserved **verbatim**, leading and trailing whitespace included
/// — it is the user's text, and this decides whether to show it, not what it
/// should have been.
public func composerState(_ state: MembershipState, draft: String) -> ComposerState {
    guard membershipEnded(state) else { return .composer }
    let worthKeeping = !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    return .withdrawn(
        notice: composerWithdrawnMessage,
        draft: worthKeeping ? draft : nil,
        draftLabel: worthKeeping ? unsentDraftLabel : nil)
}

/// Whether the per-message action menu is offered at all.
public enum MessageActions: Equatable {
    case available
    case withdrawn
}

/// Decide the per-message action menu — Edit, Delete, React (§6.2.5's rule,
/// reached from the other end).
///
/// **Withdrawn, not disabled, and it is the composer's own question.** All
/// three actions compose an MLS application message, so all three are refused
/// by `createApplicationMessage`'s `is_operational()` gate — the *same* gate,
/// at the *same* instant, for the *same* permanent reason as Send. §5.3's
/// visible-but-disabled rule is for a permission that is merely *suspended*; a
/// greyed React item on a group that can never become live again is a control
/// promising a future it does not have, which is exactly the shape §6.2.5
/// rejects for the composer.
///
/// **Why a function here and not a `&&` at the call site.** Two gates over one
/// predicate drift when somebody later adds a condition to one of them, and the
/// drift is silent in both directions: an ungated menu offers three actions
/// that cannot work, and an over-gated one hides working controls. Both this
/// and ``composerState(_:draft:)`` therefore route through
/// ``membershipEnded(_:)``, and the test enumerates every state and asserts
/// they agree.
///
/// **iOS had a second, worse shape than the other two clients**, and it is
/// worth naming because it looks like the problem was already solved: the view
/// model's `react` / `beginEdit` / `deleteMessage` each already carried a
/// `guard !membershipHasEnded else { return }`, so the ops did not throw — they
/// silently did *nothing*. A menu that opens, offers three actions and answers
/// none of them is a worse lie than one that fails loudly, because the user has
/// no way to tell a swallowed action from a slow one.
///
/// **What this does NOT withdraw is the information.** A reaction *chip* is
/// rendered content — part of the frozen transcript §6.2.6 keeps whole — so the
/// counts stay. What goes is the affordance that would toggle one.
public func messageActionsState(_ state: MembershipState) -> MessageActions {
    membershipEnded(state) ? .withdrawn : .available
}

// MARK: - §6.2.8 the one-shot half of the teardown

/// The teardown steps that are **effects** rather than gates.
///
/// §6.2.8 lists five disciplines and they are not all the same shape, so they
/// are not modelled as though they were. Three of the five — the poll loop, the
/// shrink rotation and the exporter-backed call buttons — are pure **gates**:
/// there is nothing to dispose, only something that must keep not happening,
/// and ``membershipEnded(_:)`` is what they consult. These two are the ones
/// with an effect to fire, and firing an effect more than once is what
/// ``claimTeardownSteps(_:alreadyRun:)`` exists to prevent.
public enum EndedTeardownStep: String, CaseIterable, Equatable {
    /// The beacon emitter runs on its own `Timer` and composes an application
    /// message, which now throws. A candidate that cannot commit must not claim
    /// it can: a beacon means *able to sweep*, and an evicted device can never
    /// sweep again. (The same rule that keeps a backgrounded app from emitting,
    /// reached from the other direction — there the device could sweep and is
    /// not running; here it is running and never can.) The emit path is gated
    /// too, so invalidating the timer is belt and braces; it is what stops the
    /// wasted wake.
    case stopBeaconTicker

    /// The interesting one. ADR-0043 recorded that an evicted member's push-gw
    /// row *"is never deleted — nobody else can delete it and no client has
    /// 'you were removed' handling"*, so it goes **inert** rather than away.
    /// This is that handling: the evicted device can delete its own row, and
    /// `PushGroupRouting.retireOldPushGid` already does exactly the right three
    /// things — derive the gid, unsubscribe this device, drop the reverse-index
    /// entry — bounded and never throwing. It closes the other half of G-36
    /// residual 2: rotation stops the group notifying the old id; this stops
    /// the ex-member being subscribed to anything at all. **The honest bound:**
    /// it needs the evicted device to come online and run it, so it
    /// *complements* rotation and replaces nothing.
    case retirePushSubscription
}

/// The canonical set, so a caller cannot quietly implement one of two.
public let endedTeardownSteps: [EndedTeardownStep] = EndedTeardownStep.allCases

/// Which one-shot teardown steps are owed right now.
///
/// Returns `[]` while the membership holds, and `[]` again once every step has
/// been run — so the caller can drive it from an observer that fires for
/// unrelated reasons without re-firing a network call each time. A
/// partially-completed teardown resumes rather than restarting, which matters
/// because the two steps fail independently: the ticker is invalidated locally
/// and cannot fail, while the push retire is bounded network IO that can.
public func pendingTeardownSteps(
    _ state: MembershipState,
    alreadyRun: Set<EndedTeardownStep>
) -> [EndedTeardownStep] {
    guard membershipEnded(state) else { return [] }
    return endedTeardownSteps.filter { !alreadyRun.contains($0) }
}

/// ``pendingTeardownSteps(_:alreadyRun:)``, but it **claims** what it returns in
/// the same synchronous call.
///
/// This is what the driver must use, and the reason is a defect that
/// `pendingTeardownSteps` alone cannot prevent — web found it by building it
/// the other way first. One of the two steps is async (the push retire is
/// bounded network IO), so marking it done only after the `await` leaves a
/// window in which the driver re-runs — and it re-runs on every roster change,
/// for reasons that have nothing to do with the eviction — reads the step as
/// still owed, and fires a second concurrent unsubscribe. "Exactly once" has to
/// be decided *before* the first suspension point, not after the last one.
///
/// On iOS `@MainActor` makes that nearly free — but only if the claim is
/// genuinely taken before the first `await`, which is why this is a synchronous
/// `inout` mutation rather than something the caller does when its `Task`
/// finishes.
///
/// A step that FAILS is handed back with ``releaseTeardownStep(_:_:)``, so the
/// next pass retries it. Claim-then-release rather than mark-on-success is
/// deliberate: the two orderings differ only under concurrency, and that is
/// precisely the case this exists for.
public func claimTeardownSteps(
    _ state: MembershipState,
    alreadyRun: inout Set<EndedTeardownStep>
) -> [EndedTeardownStep] {
    let owed = pendingTeardownSteps(state, alreadyRun: alreadyRun)
    for step in owed { alreadyRun.insert(step) }
    return owed
}

/// Hand a claimed step back after it failed, so a later pass retries it.
public func releaseTeardownStep(
    _ alreadyRun: inout Set<EndedTeardownStep>,
    _ step: EndedTeardownStep
) {
    alreadyRun.remove(step)
}

// MARK: - §6.2.3 / §9 the durable `leaveRequestedAt` record

/// Per-`(account, group)` record of *when this device published a self-targeted
/// leave request*.
///
/// **This has to be its own durable field, and the reason is that the failure is
/// silent.** The client already knows locally whether it asked — a device that
/// requested a leave wrote an observation under `remove:{ownLeaf}` with
/// `own: true` — but that record is pruned by the same pass that makes the
/// question askable. ``PendingRequestStore``'s whole discipline is to drop an
/// observation once the core no longer holds the request, and the sweep that
/// removes you is exactly what empties the core's pending set. So the poll batch
/// that carries your eviction also destroys the only evidence that you asked for
/// it, and the order in which those two happen is an implementation detail no
/// test pins.
///
/// Hence: written at **publish**, never pruned with the pending sweep, and
/// discarded only with the group record itself (`Delete this chat`) or the
/// account (`forgetDevice`).
///
/// `UserDefaults`-backed for the same reason as ``PendingRequestStore`` and
/// `LastObservedOwnerStore`: it is a timestamp about a request this device made
/// itself and already published in plaintext-to-members form on the journal, and
/// losing it costs the *narrowing* — the fallback string is the one that is true
/// either way.
public final class LeaveRequestStore {

    private static let prefix = "lakko.leave-requested.v1"

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    private func storeKey(accountIdB64: String, groupIdB64: String) -> String {
        "\(Self.prefix).\(accountIdB64).\(groupIdB64)"
    }

    /// Read the stamp, or `nil` when absent. A corrupt or absent row degrades
    /// to the removed copy, which is still true.
    public func load(accountIdB64: String, groupIdB64: String) -> Int64? {
        defaults.object(
            forKey: storeKey(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        ).flatMap { ($0 as? NSNumber)?.int64Value }
    }

    /// Record that this device published a self-targeted leave request.
    ///
    /// A timestamp rather than a boolean costs nothing and is strictly more
    /// useful. The FIRST stamp wins, for the same reason ``PendingRequestStore``'s
    /// does: a re-request is the same intent, not a new one.
    public func remember(accountIdB64: String, groupIdB64: String, atMs: Int64) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        let key = storeKey(accountIdB64: accountIdB64, groupIdB64: groupIdB64)
        guard defaults.object(forKey: key) == nil else { return }
        defaults.set(NSNumber(value: atMs), forKey: key)
    }

    /// Discard the record for one group — the `Delete this chat` path, which is
    /// the only thing that discards the group record itself.
    public func clear(accountIdB64: String, groupIdB64: String) {
        defaults.removeObject(
            forKey: storeKey(accountIdB64: accountIdB64, groupIdB64: groupIdB64))
    }

    /// Drop every record for one account — `recover` / `forgetDevice` hygiene,
    /// the same discipline the pending-request observations, the beacon
    /// observations and the last-observed owner all follow.
    public func clearForAccount(accountIdB64: String) {
        let needle = "\(Self.prefix).\(accountIdB64)."
        for key in defaults.dictionaryRepresentation().keys where key.hasPrefix(needle) {
            defaults.removeObject(forKey: key)
        }
    }
}

// MARK: - The notice after a membership action

/// `docs/ux/proposal-based-membership.md` section 2, "What 'tap, done'
/// shows" (2026-10-29, with the maintainer's amendment of the same day).
///
/// Decided from the GROUP after the attempt, never from which path committed
/// the change (a background sweep may have committed it first):
/// - add, approving a join request, remove, make/remove admin and ownership
///   transfer (owner or admin actions) show no notice at all, in effect or not; until such a change
///   lands the member row's own annotation is the only sign;
/// - leave shows `chat.leave.requested` only while this device is still a
///   member, and that notice clears itself the moment the leave lands.
public enum MembershipActionNotice {
    public enum Action: Equatable {
        case add
        /// Approving a join request: an add (ADR-0027 on the ADR-0045 path).
        case approveJoin
        case remove
        case setAdmin
        case transferOwnership
        case leave
    }

    /// The catalog key of the notice to show after `action`, or `nil` for
    /// none. `inEffect`: the group, read after the attempt, already shows the
    /// change (for a leave: this device's membership has ended).
    public static func after(_ action: Action, inEffect: Bool) -> String? {
        switch action {
        case .add, .approveJoin, .remove, .setAdmin, .transferOwnership:
            return nil
        case .leave:
            return inEffect ? nil : "chat.leave.requested"
        }
    }

    /// Whether a shown leave notice goes: the leave landed (this device's
    /// membership ended).
    public static func leaveNoticeClears(shown: Bool, membershipEnded: Bool) -> Bool {
        shown && membershipEnded
    }
}
