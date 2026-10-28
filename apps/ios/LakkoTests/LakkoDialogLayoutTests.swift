// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

// Design system section 3, "Dialog": two actions sit in one row, Cancel
// first; three or more stack full width, the confirming action on top and
// Cancel last.
final class LakkoDialogLayoutTests: XCTestCase {
    func testOneOrTwoActionsSitInARow() {
        XCTAssertEqual(LakkoDialogLayout.layout(actionCount: 1), .row)
        XCTAssertEqual(LakkoDialogLayout.layout(actionCount: 2), .row)
    }

    func testThreeOrMoreActionsStack() {
        XCTAssertEqual(LakkoDialogLayout.layout(actionCount: 3), .stacked)
        XCTAssertEqual(LakkoDialogLayout.layout(actionCount: 5), .stacked)
    }

    func testRowPutsCancelFirstWhateverTheCallerOrder() {
        XCTAssertEqual(LakkoDialogLayout.order([.danger, .cancel], layout: .row), [1, 0])
        XCTAssertEqual(LakkoDialogLayout.order([.cancel, .primary], layout: .row), [0, 1])
    }

    func testStackedPutsConfirmingOnTopAndCancelLast() {
        // Save / Discard / Cancel, given in any order.
        XCTAssertEqual(
            LakkoDialogLayout.order([.cancel, .secondary, .primary], layout: .stacked), [2, 1, 0])
        XCTAssertEqual(
            LakkoDialogLayout.order([.primary, .secondary, .cancel], layout: .stacked), [0, 1, 2])
    }

    func testOrderIsAPermutation() {
        let roles: [LakkoDialogAction.Role] = [.secondary, .cancel, .danger, .primary]
        for layout in [LakkoDialogLayout.row, .stacked] {
            XCTAssertEqual(LakkoDialogLayout.order(roles, layout: layout).sorted(), [0, 1, 2, 3])
        }
    }

    func testNoticeWithOneButtonHasNoCancel() {
        XCTAssertEqual(LakkoDialogLayout.order([.primary], layout: .row), [0])
    }
}
