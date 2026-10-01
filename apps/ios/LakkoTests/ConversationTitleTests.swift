// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// The pure rules behind the Chats / Groups split and the member-derived
/// group title (docs/ux/client-redesign.md sections 3 and 5.1).
final class ConversationTitleTests: XCTestCase {

    // MARK: - Conversation kind (ADR-0062)

    /// The kind is read from the group, never from a count: what the creator
    /// passed to `Group.create` is what `groupKind()` answers, through the
    /// real FFI boundary.
    func testTheKindIsWhatTheCreatorFixedInTheGroup() throws {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x21, count: 64))
        let oneToOne = try Group.create(identity: identity, kind: .oneToOne)
        let group = try Group.create(identity: identity, kind: .group)
        XCTAssertEqual(try oneToOne.groupKind(), .oneToOne)
        XCTAssertEqual(try group.groupKind(), .group)
        XCTAssertEqual(ConversationKind.of(groupKind: try oneToOne.groupKind()), .chat)
        XCTAssertEqual(
            ConversationKind.of(groupKind: try group.groupKind()), .group,
            "a group is a group while it holds one or two people")
    }

    func testAGroupWithoutAReadableKindCountsAsAGroup() {
        XCTAssertEqual(ConversationKind.of(groupKind: .oneToOne), .chat)
        XCTAssertEqual(ConversationKind.of(groupKind: .group), .group)
        XCTAssertEqual(ConversationKind.of(groupKind: nil), .group)
    }

    /// ADR-0062 section 7 — a group-name envelope in a one-to-one is dropped.
    func testAGroupNameAppliesOnlyOutsideAOneToOne() {
        XCTAssertFalse(ConversationKind.acceptsGroupName(groupKind: .oneToOne))
        XCTAssertTrue(ConversationKind.acceptsGroupName(groupKind: .group))
        XCTAssertTrue(ConversationKind.acceptsGroupName(groupKind: nil))
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
