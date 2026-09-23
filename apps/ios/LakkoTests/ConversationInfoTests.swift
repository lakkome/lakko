// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/conversation-info.md` — one way into a conversation's info, three
/// things behind it, and a conversation between two people showing none of a
/// group's machinery.
///
/// The defect these pin is section 0: one panel did three jobs, so tapping a
/// person's name showed `Members (2)`, a group name, an invite link,
/// `Make admin`, `Transfer ownership` and `Leave group`.
final class ConversationInfoTests: XCTestCase {

    // MARK: - Section 1: which info opens

    func testAConversationBetweenTwoPeopleOpensThePerson() {
        XCTAssertEqual(
            ConversationInfoDestination.of(kind: .chat, contactMessageId: 42),
            .person(contactMessageId: 42))
    }

    func testAGroupOpensTheGroup() {
        XCTAssertEqual(
            ConversationInfoDestination.of(kind: .group, contactMessageId: nil),
            .group)
    }

    /// A group is a group whatever else is known: a contact on the other side
    /// of one of its conversations never turns it into a person.
    func testAGroupOpensTheGroupEvenWithAContactToHand() {
        XCTAssertEqual(
            ConversationInfoDestination.of(kind: .group, contactMessageId: 42),
            .group)
    }

    /// A conversation can arrive before the contact does. That is a person
    /// this device knows less about, not a group.
    func testAConversationWithNoContactStillOpensThePerson() {
        XCTAssertEqual(
            ConversationInfoDestination.of(kind: .chat, contactMessageId: nil),
            .person(contactMessageId: nil))
    }

    func testEachDestinationSaysWhatItOpens() {
        XCTAssertEqual(
            ConversationInfoDestination.person(contactMessageId: 1).accessibilityHint,
            "Opens contact info")
        XCTAssertEqual(ConversationInfoDestination.group.accessibilityHint, "Opens group info")
        XCTAssertEqual(
            ConversationInfoDestination.channel(channelId: "c").accessibilityHint,
            "Opens channel info")
    }

    // MARK: - Section 2.1: what a person never shows

    /// The requirement, stated as one assertion: no group name, no invite
    /// link, no roles, no ownership, no `Leave group`.
    func testAPersonCarriesNothingSectionTwoOneForbids() {
        for destination in [
            ConversationInfoDestination.person(contactMessageId: 7),
            ConversationInfoDestination.person(contactMessageId: nil),
        ] {
            let content = ConversationInfoContent.of(destination)
            XCTAssertFalse(
                content.carriesAnythingForbiddenForOneToOne,
                "a conversation between two people showed group machinery")
        }
    }

    /// The same list again, one flag at a time, so a failure names which one
    /// came back.
    func testAPersonShowsNoGroupNameLinkRolesOwnershipOrLeave() {
        let content = ConversationInfoContent.of(.person(contactMessageId: 7))
        XCTAssertFalse(content.sharedName)
        XCTAssertFalse(content.inviteLink)
        XCTAssertFalse(content.roles)
        XCTAssertFalse(content.ownership)
        XCTAssertFalse(content.leaveGroup)
        XCTAssertFalse(content.memberRoster, "a roster of two is not a person")
    }

    /// The one security thing a person acts on belongs in front of them.
    func testAPersonShowsTheSafetyNumber() {
        XCTAssertTrue(ConversationInfoContent.of(.person(contactMessageId: 7)).safetyNumber)
        XCTAssertTrue(ConversationInfoContent.of(.person(contactMessageId: nil)).safetyNumber)
    }

    // MARK: - Section 2.2: what a group keeps

    func testAGroupKeepsEveryCapabilityItHadBefore() {
        let content = ConversationInfoContent.of(.group)
        XCTAssertTrue(content.sharedName)
        XCTAssertTrue(content.memberRoster)
        XCTAssertTrue(content.inviteLink)
        XCTAssertTrue(content.roles)
        XCTAssertTrue(content.ownership)
        XCTAssertTrue(content.leaveGroup)
    }

    /// The safety number is between two people, so it sits on each of them
    /// rather than on the group they share.
    func testAGroupDoesNotShowASafetyNumber() {
        XCTAssertFalse(ConversationInfoContent.of(.group).safetyNumber)
    }

    // MARK: - Section 2.3: a channel

    func testAChannelShowsItsNameAndUnsubscribeAndNoGroupMachinery() {
        let content = ConversationInfoContent.of(.channel(channelId: "c"))
        XCTAssertTrue(content.sharedName)
        XCTAssertTrue(content.unsubscribe)
        XCTAssertFalse(content.memberRoster)
        XCTAssertFalse(content.roles)
        XCTAssertFalse(content.ownership)
        XCTAssertFalse(content.leaveGroup)
    }

    func testOnlyAChannelOffersUnsubscribe() {
        XCTAssertFalse(ConversationInfoContent.of(.group).unsubscribe)
        XCTAssertFalse(ConversationInfoContent.of(.person(contactMessageId: 1)).unsubscribe)
    }

    // MARK: - The three read as one product

    /// Section 2 asks for a common shape, not three overlapping ones: no two
    /// destinations hold the same set of sections, and none is empty.
    func testTheThreeDestinationsAreDistinctAndNoneIsEmpty() {
        let person = ConversationInfoContent.of(.person(contactMessageId: 1))
        let group = ConversationInfoContent.of(.group)
        let channel = ConversationInfoContent.of(.channel(channelId: "c"))
        XCTAssertNotEqual(person, group)
        XCTAssertNotEqual(person, channel)
        XCTAssertNotEqual(group, channel)
        for content in [person, group, channel] {
            XCTAssertTrue(
                content.sharedName || content.memberRoster || content.safetyNumber
                    || content.unsubscribe,
                "a destination with nothing on it is a tap that reads as nothing happening")
        }
    }
}
