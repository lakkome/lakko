// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// `docs/ux/proposal-based-membership.md` section 2, "What 'tap, done'
/// shows" (2026-10-29, with the amendment of the same day): decided from the
/// group after the attempt, never from which path committed the change.
@MainActor
final class MembershipActionNoticeTests: XCTestCase {

    /// Owner or admin actions show no notice at all, landed or not: the
    /// member row's annotation (for an approved join: the request row, then
    /// the roster) is the only sign until the change lands.
    func testOwnerAndAdminActionsShowNoNotice() {
        for action in [MembershipActionNotice.Action.add, .approveJoin, .remove, .setAdmin, .transferOwnership] {
            XCTAssertNil(MembershipActionNotice.after(action, inEffect: true), "\(action) in effect")
            XCTAssertNil(MembershipActionNotice.after(action, inEffect: false), "\(action) not yet")
        }
    }

    /// A leave already in effect after the attempt says nothing, whichever
    /// sweep committed it (this screen's or a background one): the group is
    /// what is read, and it shows this device gone.
    func testALeaveAlreadyInEffectShowsNoNotice() {
        XCTAssertNil(MembershipActionNotice.after(.leave, inEffect: true))
    }

    func testALeaveStillWaitingSaysSoAndClearsWhenItLands() {
        let key = MembershipActionNotice.after(.leave, inEffect: false)
        XCTAssertEqual(key, "chat.leave.requested")
        XCTAssertNotEqual(Localization.text(key ?? ""), key, "a catalog sentence")
        XCTAssertFalse(MembershipActionNotice.leaveNoticeClears(shown: true, membershipEnded: false))
        XCTAssertTrue(MembershipActionNotice.leaveNoticeClears(shown: true, membershipEnded: true))
        XCTAssertFalse(
            MembershipActionNotice.leaveNoticeClears(shown: false, membershipEnded: true),
            "another notice on screen is not taken away")
    }

    /// Seen live 2026-10-29: an avatar read "V·" for "VG2 · @vg2". Initials
    /// come from the display name, never from the label carrying the username.
    func testInitialsComeFromTheNameNotTheListLabel() {
        let resolved = resolveDisplayName(
            signatureKey: Data(repeating: 0x44, count: 32),
            localRename: nil,
            profile: ProfileClaim(displayName: "VG2", username: "vg2", updatedAtMs: 1),
            verification: .verified)
        XCTAssertEqual(resolved.listLabel, "VG2 · @vg2")
        XCTAssertEqual(avatarInitials(name: resolved.name), "VG")
        XCTAssertNotEqual(
            avatarInitials(name: resolved.listLabel), avatarInitials(name: resolved.name),
            "the label would give a second initial")
        let twoWords = resolveDisplayName(
            signatureKey: Data(repeating: 0x45, count: 32),
            localRename: nil,
            profile: ProfileClaim(displayName: "Anna Petrova", username: "anna", updatedAtMs: 1),
            verification: .verified)
        XCTAssertEqual(avatarInitials(name: twoWords.name), "AP")
    }
}
