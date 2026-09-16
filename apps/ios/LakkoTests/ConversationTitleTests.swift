// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The pure rules behind the Chats / Groups split and the member-derived
/// group title (docs/ux/client-redesign.md sections 3 and 5.1).
final class ConversationTitleTests: XCTestCase {

    func testTwoPeopleAreAChatMoreAreAGroup() {
        XCTAssertEqual(ConversationKind.of(peopleCount: 2), .chat)
        XCTAssertEqual(ConversationKind.of(peopleCount: 3), .group)
        XCTAssertEqual(ConversationKind.of(peopleCount: 7), .group)
    }

    func testUnreadableOrLonelyRosterListsUnderChats() {
        XCTAssertEqual(ConversationKind.of(peopleCount: nil), .chat)
        XCTAssertEqual(ConversationKind.of(peopleCount: 1), .chat)
        XCTAssertEqual(ConversationKind.of(peopleCount: 0), .chat)
    }

    func testGroupTitleJoinsUpToThreeNames() {
        XCTAssertEqual(
            ConversationTitle.groupTitle(otherMemberNames: ["Anna", "Ivan"]),
            "Anna, Ivan")
        XCTAssertEqual(
            ConversationTitle.groupTitle(otherMemberNames: ["Anna", "Ivan", "Maria"]),
            "Anna, Ivan, Maria")
    }

    func testGroupTitleCountsTheRestPastThree() {
        XCTAssertEqual(
            ConversationTitle.groupTitle(otherMemberNames: ["Anna", "Ivan", "Maria", "Olga", "Petr"]),
            "Anna, Ivan, Maria +2")
    }

    func testUnnamedMembersAreCountedButNeverShown() {
        // A member with no readable name (only a key fingerprint) is `nil`.
        XCTAssertEqual(
            ConversationTitle.groupTitle(otherMemberNames: ["Anna", nil, "  "]),
            "Anna +2")
        XCTAssertEqual(ConversationTitle.groupTitle(otherMemberNames: [nil, nil]), "Group")
        XCTAssertEqual(ConversationTitle.groupTitle(otherMemberNames: []), "Group")
    }

    func testMemberCountLabel() {
        XCTAssertEqual(ConversationTitle.memberCountLabel(1), "1 member")
        XCTAssertEqual(ConversationTitle.memberCountLabel(4), "4 members")
    }

    func testInitials() {
        XCTAssertEqual(InitialsAvatar.initials(of: "Anna Petrova"), "AP")
        XCTAssertEqual(InitialsAvatar.initials(of: "anna"), "A")
        XCTAssertEqual(InitialsAvatar.initials(of: "Anna, Ivan, Maria +2"), "AI")
        XCTAssertEqual(InitialsAvatar.initials(of: "@alice"), "A")
        XCTAssertEqual(InitialsAvatar.initials(of: "..."), "")
    }
}
