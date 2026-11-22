// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The table of `apps/web/src/chat/member-row-actions.test.ts`, row for row.
final class MemberRowActionsTests: XCTestCase {
    private func row(
        _ viewer: MemberRole?, _ rowRole: MemberRole, ownRow: Bool = false, ended: Bool = false
    ) -> MemberRowActions {
        MemberRowActions.of(viewerRole: viewer, rowRole: rowRole, ownRow: ownRow, membershipEnded: ended)
    }

    func testNeverOffersAnythingOnTheOwnersRow() {
        for viewer in MemberRole.allCases.map(Optional.some) + [nil] {
            let r = row(viewer, .owner)
            XCTAssertEqual(r.remove, viewer == nil)
            XCTAssertNil(r.adminToggle)
        }
    }

    /// Seen live 2026-11-22: an admin had `Remove` on the owner's row.
    func testAnAdminRemovesPlainMembersOnlyAndNeverChangesRoles() {
        XCTAssertEqual(row(.admin, .owner), .init(mark: .owner, remove: false, adminToggle: nil))
        XCTAssertEqual(row(.admin, .admin), .init(mark: .admin, remove: false, adminToggle: nil))
        XCTAssertEqual(row(.admin, .member), .init(mark: nil, remove: true, adminToggle: nil))
    }

    func testTheOwnerRemovesAnyoneElseAndMakesOrTakesBackAdmins() {
        XCTAssertEqual(row(.owner, .admin), .init(mark: .admin, remove: true, adminToggle: .take))
        XCTAssertEqual(row(.owner, .member), .init(mark: nil, remove: true, adminToggle: .make))
    }

    func testAPlainMemberActsOnNoRowButSeesTheMarks() {
        for r in MemberRole.allCases {
            let out = row(.member, r)
            XCTAssertFalse(out.remove)
            XCTAssertNil(out.adminToggle)
            XCTAssertEqual(out.mark, r == .member ? nil : r)
        }
    }

    func testNobodyActsOnTheirOwnRowAndTheOwnRowKeepsItsMark() {
        for viewer in MemberRole.allCases {
            let out = row(viewer, viewer, ownRow: true)
            XCTAssertFalse(out.remove)
            XCTAssertNil(out.adminToggle)
            XCTAssertEqual(out.mark, viewer == .member ? nil : viewer)
        }
        XCTAssertFalse(row(nil, .member, ownRow: true).remove)
    }

    func testAGroupWithNoRoleTableLetsAnyoneRemoveAnyoneElse() {
        for r in MemberRole.allCases {
            XCTAssertEqual(row(nil, r), .init(mark: nil, remove: true, adminToggle: nil))
        }
    }

    func testAnEndedMembershipShowsAReadOnlyRosterWithoutMarks() {
        for viewer in MemberRole.allCases.map(Optional.some) + [nil] {
            for r in MemberRole.allCases {
                XCTAssertEqual(row(viewer, r, ended: true), .init(mark: nil, remove: false, adminToggle: nil))
            }
        }
    }

    func testRowRoleReadsTheOwnerTheAdminsAndEveryoneElse() {
        let a = Data([1]), b = Data([2]), c = Data([3])
        XCTAssertEqual(MemberRole.of(person: a, owner: a, admins: [b]), .owner)
        XCTAssertEqual(MemberRole.of(person: b, owner: a, admins: [b]), .admin)
        XCTAssertEqual(MemberRole.of(person: c, owner: a, admins: [b]), .member)
        XCTAssertEqual(MemberRole.of(person: a, owner: nil, admins: [b]), .member)
    }
}
