// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// `docs/ux/conversation-info.md` — one way into a conversation's info, three
/// things behind it, and a conversation between two people showing none of a
/// group's machinery.
///
/// These run the SHARED rule (`crates/lakko-ui-rules`, over the FFI) rather
/// than an iOS copy of it. iOS is the one client whose unit tests link
/// `LakkoCore`, so it is the one place the rule the three clients agreed on
/// can be asserted across the boundary it actually crosses at runtime.
///
/// The defects they pin are the ones the split copies produced: one panel did
/// three jobs, so tapping a person's name showed `Members (2)`, a group name,
/// an invite link, `Make admin`, `Transfer ownership` and `Leave group`; an
/// ended one-to-one carried no way to close it; and a channel offered no way
/// to stop following it.
final class ConversationInfoTests: XCTestCase {

    private func person(ended: Bool, contact: Bool = true) -> PersonInfoCapabilities {
        PersonInfoCapabilities(membershipEnded: ended, isContact: contact)
    }

    // MARK: - Section 1: which info opens, and what iOS routes on

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

    func testAChannelOpensTheChannelAndCarriesItsId() {
        XCTAssertEqual(
            ConversationInfoDestination.ofChannel(channelId: "c0ffee"),
            .channel(channelId: "c0ffee"))
    }

    /// The destination iOS routes on must be the one the shared rule chose,
    /// not a second opinion about the same conversation.
    func testTheDestinationAgreesWithTheSharedRule() {
        XCTAssertEqual(infoTarget(kind: ConversationKind.chat.sharedKind), .person)
        XCTAssertEqual(infoTarget(kind: ConversationKind.group.sharedKind), .group)
        XCTAssertEqual(infoTarget(kind: .channel), .channel)
    }

    func testEachDestinationSaysWhatItOpens() {
        XCTAssertEqual(
            ConversationInfoDestination.person(contactMessageId: 1).accessibilityHint,
            "Contact info")
        XCTAssertEqual(ConversationInfoDestination.group.accessibilityHint, "Group info")
        XCTAssertEqual(
            ConversationInfoDestination.channel(channelId: "c").accessibilityHint,
            "Channel info")
    }

    // MARK: - Section 2.1: what a person never shows

    /// Section 2.1's `Never` list, under EVERY combination rather than for one
    /// example. This is the defect the whole shared module exists to close, so
    /// it is checked exhaustively, and through the FFI, because that is the
    /// path the app itself takes.
    func testAPersonNeverCarriesAnythingGroupShaped() {
        let groupShaped: [InfoSection] = [
            .groupName, .renameGroup, .memberList, .addMember,
            .inviteLink, .roles, .transferOwnership, .leaveGroup,
        ]
        for ended in [false, true] {
            for contact in [false, true] {
                let sections = personInfoSections(caps: person(ended: ended, contact: contact))
                for forbidden in groupShaped {
                    XCTAssertFalse(
                        sections.contains(forbidden),
                        "ended=\(ended) contact=\(contact) showed \(forbidden)")
                }
            }
        }
    }

    func testAContactGetsTheNumberTheNameAndTheWayOutOfTheList() {
        let sections = personInfoSections(caps: person(ended: false))
        XCTAssertTrue(sections.contains(.safetyNumber))
        XCTAssertTrue(sections.contains(.renameContact))
        XCTAssertTrue(sections.contains(.removeContact))
        XCTAssertFalse(sections.contains(.addContact))
    }

    /// Amended 2026-10-24: a person the viewer removed keeps the conversation,
    /// and its card puts them back instead of offering rename and remove.
    func testARemovedPersonIsOfferedAddToContactsInsteadOfRenameAndRemove() {
        let sections = personInfoSections(caps: person(ended: false, contact: false))
        XCTAssertTrue(sections.contains(.addContact))
        XCTAssertTrue(sections.contains(.safetyNumber))
        XCTAssertTrue(sections.contains(.call))
        XCTAssertTrue(sections.contains(.message))
        XCTAssertFalse(sections.contains(.renameContact))
        XCTAssertFalse(sections.contains(.removeContact))
    }

    /// One card wherever it was opened from: Message is always there; from
    /// the chat it returns to it. Withholding it by origin gave one contact
    /// two cards (seen side by side on 2026-09-26).
    func testMessageIsOfferedWhateverTheCardWasOpenedFrom() {
        XCTAssertTrue(personInfoSections(caps: person(ended: false)).contains(.message))
        XCTAssertTrue(personInfoSections(caps: person(ended: true)).contains(.message))
    }

    // MARK: - The first defect this closes: an ended one-to-one had no exit

    /// `Delete this chat` reached an ended one-to-one on Android only. Here it
    /// stayed in the group panel — which a one-to-one had just stopped opening
    /// — so ending a one-to-one left a conversation that could never be
    /// closed.
    func testAnEndedOneToOneCanBeClosed() {
        XCTAssertTrue(
            personInfoSections(caps: person(ended: true)).contains(.deleteChat),
            "an ended one-to-one had no way out")
    }

    /// The other half of the same state: the per-call key comes from a group
    /// this device is no longer in, so a call control would assert a live
    /// session there is not.
    func testAnEndedOneToOneOffersNoCall() {
        XCTAssertFalse(
            personInfoSections(caps: person(ended: true)).contains(.call),
            "an ended one-to-one still offered a call")
    }

    /// The person is still the person: what ended is the conversation.
    func testAnEndedOneToOneStillShowsEverythingAboutThePerson() {
        let sections = personInfoSections(caps: person(ended: true))
        XCTAssertTrue(sections.contains(.safetyNumber))
        XCTAssertTrue(sections.contains(.renameContact))
        XCTAssertTrue(sections.contains(.removeContact))
    }

    // MARK: - Section 2.2: what a group keeps

    func testAnOwnerKeepsEveryGroupCapability() {
        let sections = groupInfoSections(
            caps: GroupInfoCapabilities(
                canManageMembers: true, canRename: true, canMintInvite: true,
                isOwner: true, canLeave: true, membershipEnded: false))
        for expected: InfoSection in [
            .groupName, .memberList, .renameGroup, .addMember,
            .roles, .inviteLink, .transferOwnership, .leaveGroup,
        ] {
            XCTAssertTrue(sections.contains(expected), "an owner lost \(expected)")
        }
    }

    func testAPlainMemberGetsTheRosterAndTheExitAndNothingElse() {
        let sections = groupInfoSections(
            caps: GroupInfoCapabilities(
                canManageMembers: false, canRename: false, canMintInvite: false,
                isOwner: false, canLeave: true, membershipEnded: false))
        XCTAssertTrue(sections.contains(.groupName))
        XCTAssertTrue(sections.contains(.memberList))
        XCTAssertTrue(sections.contains(.leaveGroup))
        for absent: InfoSection in [
            .renameGroup, .addMember, .roles, .inviteLink, .transferOwnership,
        ] {
            XCTAssertFalse(sections.contains(absent), "a plain member was offered \(absent)")
        }
    }

    /// An ended group leaves the name and the roster readable and takes every
    /// action away — and gives back the same exit an ended one-to-one gets,
    /// in the same state, for the same reason.
    func testAnEndedGroupIsReadOnlyExceptForTheWayOut() {
        let sections = groupInfoSections(
            caps: GroupInfoCapabilities(
                canManageMembers: true, canRename: true, canMintInvite: true,
                isOwner: true, canLeave: true, membershipEnded: true))
        XCTAssertTrue(sections.contains(.groupName))
        XCTAssertTrue(sections.contains(.memberList))
        XCTAssertTrue(sections.contains(.deleteChat))
        for gone: InfoSection in [
            .addMember, .renameGroup, .inviteLink, .transferOwnership, .leaveGroup, .roles,
        ] {
            XCTAssertFalse(sections.contains(gone), "\(gone) survived an ended membership")
        }
    }

    /// A group carrying no ADR-0022 role table has no ADR-0045 proposal path,
    /// so a member cannot commit their own removal and there is nothing for
    /// `Leave group` to do. Web and Android both already refused to draw it
    /// there, against a rule that offered it — a rule nobody was following,
    /// which is how `canLeave` came to exist.
    func testAGroupNobodyCanLeaveIsNotOfferedAWayOut() {
        let sections = groupInfoSections(
            caps: GroupInfoCapabilities(
                canManageMembers: false, canRename: false, canMintInvite: false,
                isOwner: false, canLeave: false, membershipEnded: false))
        XCTAssertFalse(sections.contains(.leaveGroup))
        // Still a group: everything readable about it stays readable.
        XCTAssertTrue(sections.contains(.groupName))
        XCTAssertTrue(sections.contains(.memberList))
    }

    // MARK: - The second defect this closes: a channel could not be unfollowed

    /// `Unsubscribe` was built on web and on neither native, while both
    /// natives already held the two functions it is made of.
    func testASubscriberCanStopFollowingAChannel() {
        let sections = channelInfoSections(
            caps: ChannelInfoCapabilities(
                isPublisher: false, isSubscriber: true, subscriberCountKnown: false))
        XCTAssertTrue(sections.contains(.unsubscribe))
        XCTAssertFalse(sections.contains(.manageChannel))
        XCTAssertFalse(
            sections.contains(.subscriberCount),
            "a reader cannot know a subscriber count, so it must not show one")
    }

    /// A publisher reads its own channel from its own keys, so there is no
    /// subscription to end and a control offering to end one would end
    /// nothing.
    func testAPublisherIsNotOfferedAWayToStopFollowing() {
        let sections = channelInfoSections(
            caps: ChannelInfoCapabilities(
                isPublisher: true, isSubscriber: true, subscriberCountKnown: true))
        XCTAssertFalse(sections.contains(.unsubscribe))
        XCTAssertTrue(sections.contains(.manageChannel))
        XCTAssertTrue(sections.contains(.subscriberCount))
    }

    /// A channel found by handle and not yet subscribed to: what it is, and
    /// nothing to act on.
    func testAChannelNobodyHereReadsYetShowsWhatItIsAndNoActions() {
        let sections = channelInfoSections(
            caps: ChannelInfoCapabilities(
                isPublisher: false, isSubscriber: false, subscriberCountKnown: false))
        XCTAssertEqual(sections, [.channelHandle, .channelDescription])
    }

    /// Every channel says what it is, whoever is reading.
    func testEveryChannelShowsItsHandleAndDescription() {
        for isPublisher in [false, true] {
            for isSubscriber in [false, true] {
                let sections = channelInfoSections(
                    caps: ChannelInfoCapabilities(
                        isPublisher: isPublisher,
                        isSubscriber: isSubscriber,
                        subscriberCountKnown: isPublisher))
                XCTAssertTrue(sections.contains(.channelHandle))
                XCTAssertTrue(sections.contains(.channelDescription))
            }
        }
    }

    // MARK: - The three read as one product

    /// Section 2 asks for a common shape, not three overlapping ones: no
    /// destination's sections are empty, and a person's and a group's never
    /// coincide.
    func testTheThreeDestinationsAreDistinctAndNoneIsEmpty() {
        let personSections = personInfoSections(caps: person(ended: false))
        let groupSections = groupInfoSections(
            caps: GroupInfoCapabilities(
                canManageMembers: false, canRename: false, canMintInvite: false,
                isOwner: false, canLeave: true, membershipEnded: false))
        let channelSections = channelInfoSections(
            caps: ChannelInfoCapabilities(
                isPublisher: false, isSubscriber: true, subscriberCountKnown: false))
        XCTAssertNotEqual(personSections, groupSections)
        XCTAssertNotEqual(personSections, channelSections)
        XCTAssertNotEqual(groupSections, channelSections)
        for sections in [personSections, groupSections, channelSections] {
            XCTAssertFalse(
                sections.isEmpty,
                "a destination with nothing on it is a tap that reads as nothing happening")
        }
    }
}
