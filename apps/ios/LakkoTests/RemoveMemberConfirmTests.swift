// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// ADR-0045 UX §6 — the `Remove this member?` confirmation.
///
/// The test target drives rules, not views, so what is pinned here is the
/// DECISION — is a confirmation required for this action, and what does it say
/// — never the widget. `.confirmationDialog` versus anything else is §14's
/// business and is deliberately not asserted.
///
/// The discrimination evidence for each rule (each probe was RUN, the failure
/// observed, and the probe reverted) is recorded in `RemoveMemberConfirm.swift`.
@MainActor
final class RemoveMemberConfirmTests: XCTestCase {

    override func setUp() {
        super.setUp()
        // A failed assertion must ABORT this test rather than let the rest of
        // it run against a state the failure already invalidated.
        continueAfterFailure = false
    }

    func testItIsRequiredOnBothPathsBecauseNeitherCanBeWithdrawn() {
        // §3: once published, the request is in every member's proposal store
        // and dies only when some commit lands — the core has no withdraw and
        // MLS offers no mechanism for one. On the direct path there is no
        // withdraw either: the commit is published immediately. So the
        // confirmation is the last exit in both cases, and "confirm only on the
        // proposal path" would be exactly backwards — the direct path removes
        // somebody FASTER.
        for usesProposalPath in [true, false] {
            let copy = removeMemberConfirmation(usesProposalPath: usesProposalPath)
            XCTAssertEqual(copy.title, removeMemberTitle)
            XCTAssertEqual(copy.confirmLabel, removeMemberConfirmLabel)
            XCTAssertEqual(copy.cancelLabel, removeMemberCancelLabel)
        }
    }

    func testItStatesTheSpecCopyVerbatimOnTheProposalPath() {
        // "That body line is not a nicety. It is the one fact a user removing
        // someone for a safety reason must have, and it is the fact the old
        // immediate-looking UI concealed."
        let copy = removeMemberConfirmation(usesProposalPath: true)
        XCTAssertEqual(copy.body, removeMemberBody)
        XCTAssertEqual(copy.title, "Remove this member?")
        XCTAssertEqual(
            removeMemberBody,
            "They will keep receiving messages until an owner or an admin applies the change.")
        XCTAssertEqual(removeMemberConfirmLabel, "Remove")
        XCTAssertEqual(removeMemberCancelLabel, "Cancel")
    }

    func testItSaysNothingOnTheDirectPath() {
        // A group with no role table still removes by direct commit, and a
        // direct commit takes effect the moment it lands. "They will keep
        // receiving messages until an owner or an admin applies the change" is
        // then simply untrue, and a security tool that reassures a user with a
        // false sentence about who can still read their messages is worse than
        // one that says less.
        XCTAssertNil(removeMemberConfirmation(usesProposalPath: false).body)
    }

    func testItUsesNoProtocolVocabulary() {
        // §1: user-facing copy says `request` and `change`; it never says
        // proposal, commit, epoch, sweep, committer — or `leaf`, which is the
        // internal identifier the roster is NOT keyed on and which no user can
        // interpret.
        let forbidden = ["leaf", "proposal", "commit", "epoch", "sweep", "committer"]
        for usesProposalPath in [true, false] {
            let copy = removeMemberConfirmation(usesProposalPath: usesProposalPath)
            let text = "\(copy.title) \(copy.body ?? "") \(copy.confirmLabel) \(copy.cancelLabel)"
                .lowercased()
            for word in forbidden {
                XCTAssertFalse(text.contains(word), "`\(word)` leaked into \(text)")
            }
        }
    }

    func testItMatchesTheLeaveConfirmationsShapeButNotItsVerb() {
        // Not a style point. §6 gives `Remove` and `Leave` the same four-part
        // shape — title, one body sentence, a destructive verb, `Cancel` — and
        // the two sit in the same panel, so a user reading one after the other
        // should be answering the same kind of question. The one thing that
        // must NOT match is the verb.
        let copy = removeMemberConfirmation(usesProposalPath: true)
        XCTAssertEqual(copy.cancelLabel, "Cancel")
        XCTAssertNotEqual(copy.confirmLabel, "Leave")
    }
}
