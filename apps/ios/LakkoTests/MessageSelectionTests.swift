// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// `docs/ux/message-actions.md` section 8 — Select: Delete and Forward in
/// bulk. The rules of the mode are a value (`MessageSelection`), so they are
/// asserted directly; the screen that draws them is not here.
final class MessageSelectionTests: XCTestCase {

    // MARK: - Entering, toggling, the limit

    /// Section 8, "Entering" — the message the card was opened on is already
    /// selected, and the count reads "1 selected".
    func testEnteringSelectsTheMessageTheCardWasOpenedOn() {
        let selection = MessageSelection(initial: 7)
        XCTAssertTrue(selection.contains(7))
        XCTAssertEqual(selection.count, 1)
        XCTAssertEqual(selection.countLabel, "1 selected")
    }

    /// A tap toggles: on, then off; the count follows.
    func testToggleAddsThenRemoves() {
        var selection = MessageSelection(initial: 1)
        XCTAssertTrue(selection.toggle(2))
        XCTAssertEqual(selection.countLabel, "2 selected")
        XCTAssertTrue(selection.toggle(2))
        XCTAssertFalse(selection.contains(2))
        XCTAssertEqual(selection.countLabel, "1 selected")
    }

    /// Section 8 — "at most 100 messages can be selected; at the limit further
    /// taps do nothing and the count reads '100 selected, the limit'". A tap
    /// that REMOVES still works at the limit, so 100 is never a trap.
    func testTheLimitIsOneHundredAndFurtherAddsDoNothing() {
        var selection = MessageSelection(initial: 1)
        for id in 2...100 { XCTAssertTrue(selection.toggle(Int64(id))) }
        XCTAssertEqual(selection.count, 100)
        XCTAssertTrue(selection.isAtLimit)
        XCTAssertEqual(selection.countLabel, "100 selected, the limit")

        XCTAssertFalse(selection.toggle(101), "a 101st add changed the selection")
        XCTAssertFalse(selection.contains(101))
        XCTAssertEqual(selection.count, 100)

        XCTAssertTrue(selection.toggle(50), "a remove at the limit was refused")
        XCTAssertEqual(selection.count, 99)
        XCTAssertEqual(selection.countLabel, "99 selected")
        XCTAssertTrue(selection.toggle(101), "after a remove the slot was not reusable")
    }

    /// Deselecting everything leaves an empty selection (the mode stays; the
    /// bar's buttons disable).
    func testDeselectingTheLastMessageLeavesAnEmptySelection() {
        var selection = MessageSelection(initial: 3)
        selection.toggle(3)
        XCTAssertTrue(selection.isEmpty)
        XCTAssertEqual(selection.countLabel, "0 selected")
        XCTAssertFalse(selection.forwardEnabled(holdsInvitation: false))
        XCTAssertFalse(selection.saveEnabled)
        XCTAssertFalse(selection.deleteEnabled(allOwn: true))
    }

    /// A selected message the timeline stopped offering (deleted elsewhere)
    /// falls out of the selection.
    func testRetainDropsIdsTheTimelineNoLongerOffers() {
        var selection = MessageSelection(remaining: [1, 2, 3])
        selection.retain(onlyIn: [2, 3, 9])
        XCTAssertEqual(selection.ids, [2, 3])
    }

    // MARK: - The action bar's gates and copy

    /// Section 8 — Forward is enabled when the selection is not empty,
    /// whoever wrote the messages.
    func testForwardIsEnabledOnAnyNonEmptySelection() {
        XCTAssertTrue(MessageSelection(initial: 1).forwardEnabled(holdsInvitation: false))
        XCTAssertFalse(MessageSelection(remaining: []).forwardEnabled(holdsInvitation: false))
    }

    /// Maintainer, 2026-11-13 — a message carrying a channel invitation card
    /// is never forwarded: a selection holding one does not offer Forward;
    /// Save stays.
    func testForwardIsDisabledWhileTheSelectionHoldsAnInvitationCard() {
        let selection = MessageSelection(initial: 1)
        XCTAssertFalse(selection.forwardEnabled(holdsInvitation: true))
        XCTAssertTrue(selection.saveEnabled)
        XCTAssertTrue(selection.deleteEnabled(allOwn: true))
    }

    /// Section 8 — Delete is enabled when the selection is not empty and every
    /// selected message is the person's own; otherwise disabled with the
    /// helper line "Only your own messages can be deleted."
    func testDeleteNeedsEveryMessageToBeOwnAndSaysSoOtherwise() {
        let selection = MessageSelection(initial: 1)
        XCTAssertTrue(selection.deleteEnabled(allOwn: true))
        XCTAssertNil(selection.deleteHelper(allOwn: true))

        XCTAssertFalse(selection.deleteEnabled(allOwn: false))
        XCTAssertEqual(selection.deleteHelper(allOwn: false), "Only your own messages can be deleted.")

        // An empty selection has nothing to explain.
        let empty = MessageSelection(remaining: [])
        XCTAssertFalse(empty.deleteEnabled(allOwn: false))
        XCTAssertNil(empty.deleteHelper(allOwn: false))
    }

    /// Section 8 — "Delete 12 messages?" / "Delete 1 message?".
    func testConfirmDeleteCopy() {
        XCTAssertEqual(MessageSelection.confirmDeleteLabel(12), "Delete 12 messages?")
        XCTAssertEqual(MessageSelection.confirmDeleteLabel(1), "Delete 1 message?")
        XCTAssertEqual(MessageSelection.confirmDeleteLabel(2), "Delete 2 messages?")
    }

    /// Section 8 — "Deleted 5 of 12." / "Forwarded 5 of 12."
    func testProgressCopy() {
        XCTAssertEqual(MessageSelection.progressLabel(kind: .delete, done: 5, total: 12).withoutIsolates, "Deleted 5 of 12.")
        XCTAssertEqual(
            MessageSelection.progressLabel(kind: .forward, done: 5, total: 12).withoutIsolates, "Forwarded 5 of 12.")
        XCTAssertEqual(MessageSelection.progressLabel(kind: .delete, done: 0, total: 3).withoutIsolates, "Deleted 0 of 3.")
    }

    // MARK: - Which rows

    /// Section 8 — a system row, a deleted message, a message whose send has
    /// not completed or has failed, and a row with no id are not selectable;
    /// and nothing is once this device can no longer act (ADR-0045 UX 6.2.5).
    func testOnlyLandedNonDeletedNonSystemRowsAreSelectable() {
        XCTAssertTrue(MessageSelection.canSelect(
            actionsAvailable: true, isSystemRow: false, hasId: true, deleted: false, sendComplete: true))
        XCTAssertFalse(MessageSelection.canSelect(
            actionsAvailable: true, isSystemRow: true, hasId: true, deleted: false, sendComplete: true),
            "a system row was selectable")
        XCTAssertFalse(MessageSelection.canSelect(
            actionsAvailable: true, isSystemRow: false, hasId: true, deleted: true, sendComplete: true),
            "a deleted message was selectable")
        XCTAssertFalse(MessageSelection.canSelect(
            actionsAvailable: true, isSystemRow: false, hasId: true, deleted: false, sendComplete: false),
            "a message still sending or failed was selectable")
        XCTAssertFalse(MessageSelection.canSelect(
            actionsAvailable: true, isSystemRow: false, hasId: false, deleted: false, sendComplete: true),
            "a row with no id was selectable")
        XCTAssertFalse(MessageSelection.canSelect(
            actionsAvailable: false, isSystemRow: false, hasId: true, deleted: false, sendComplete: true),
            "a row was selectable after the membership ended")
    }

    /// Section 8 — sends go oldest first, in the timeline's order, whatever
    /// order the person tapped in.
    func testSelectedInOrderFollowsTheTimelineNotTheTaps() {
        var selection = MessageSelection(initial: 30)
        selection.toggle(10)
        selection.toggle(20)
        XCTAssertEqual(selection.selectedInOrder(of: [5, 10, 15, 20, 25, 30]), [10, 20, 30])
        // An id the timeline does not draw is not sent.
        XCTAssertEqual(selection.selectedInOrder(of: [10, 30]), [10, 30])
    }

    // MARK: - Accessibility

    /// Section 8 — the mark is named by the message's text or kind and states
    /// "Selected" or "Not selected".
    func testAccessibilityNameAndValue() {
        XCTAssertEqual(MessageSelection.accessibilityName(text: "see you at 5", attachmentCount: 0), "see you at 5")
        XCTAssertEqual(MessageSelection.accessibilityName(text: "", attachmentCount: 1), "Media message")
        XCTAssertEqual(MessageSelection.accessibilityName(text: "", attachmentCount: 0), "Message")
        XCTAssertEqual(MessageSelection.accessibilityValue(selected: true), "Selected")
        XCTAssertEqual(MessageSelection.accessibilityValue(selected: false), "Not selected")
    }

    // MARK: - The bulk run

    private struct StepFailed: Error {}

    /// Section 8 — every step runs, in order, awaited; full success leaves
    /// nothing remaining.
    func testRunVisitsEveryItemInOrderOnSuccess() async {
        var visited: [Int] = []
        let outcome = await MessageSelection.run(over: [3, 1, 2]) { item in
            visited.append(item)
        }
        XCTAssertEqual(visited, [3, 1, 2])
        XCTAssertEqual(outcome, MessageSelection.BulkOutcome(done: 3, total: 3, remaining: []))
        XCTAssertTrue(outcome.succeeded)
    }

    /// Section 8 — "on a failure it stops, keeps the unsent messages selected,
    /// and says 'Deleted 5 of 12.'": the failed item and every one after it
    /// remain, nothing after the failure is attempted.
    func testRunStopsAtTheFirstFailureAndKeepsTheRest() async {
        var visited: [Int] = []
        let outcome = await MessageSelection.run(over: [1, 2, 3, 4, 5]) { item in
            visited.append(item)
            if item == 3 { throw StepFailed() }
        }
        XCTAssertEqual(visited, [1, 2, 3], "a step after the failure was attempted")
        XCTAssertEqual(outcome.done, 2)
        XCTAssertEqual(outcome.total, 5)
        XCTAssertEqual(outcome.remaining, [3, 4, 5])
        XCTAssertFalse(outcome.succeeded)
        XCTAssertEqual(
            MessageSelection.progressLabel(kind: .delete, done: outcome.done, total: outcome.total)
                .withoutIsolates,
            "Deleted 2 of 5.")
        // What the person is left with is exactly the unsent ones.
        XCTAssertEqual(MessageSelection(remaining: outcome.remaining.map(Int64.init)).ids, [3, 4, 5])
    }

    /// A failure on the very first item leaves everything selected and says 0.
    func testRunFailingAtOnceKeepsEverything() async {
        let outcome = await MessageSelection.run(over: [9, 8]) { _ in throw StepFailed() }
        XCTAssertEqual(outcome.done, 0)
        XCTAssertEqual(outcome.remaining, [9, 8])
    }

    /// An empty run succeeds trivially (the bar never offers it, but the loop
    /// must not misreport).
    func testRunOverNothingSucceeds() async {
        let outcome = await MessageSelection.run(over: [Int]()) { _ in }
        XCTAssertTrue(outcome.succeeded)
        XCTAssertEqual(outcome.total, 0)
    }
}
