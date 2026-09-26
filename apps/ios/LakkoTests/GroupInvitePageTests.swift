// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0028 v2 on iOS (`docs/ux/group-invite-page.md`): the link shape with
/// the link key in the fragment, and the card's headline for every preview
/// shape the spec names. Both rules are pure, so this suite runs without the
/// core framework.
final class GroupInvitePageTests: XCTestCase {

    private let key = Data((0..<32).map { UInt8($0) })
    private let pointerLink = "https://lakko.me/g/" + String(repeating: "A", count: 43)

    // MARK: - Link composition (spec section 4)

    func testWithLinkKeyAppendsA43CharBase64UrlFragment() {
        let link = GroupInviteLink.withLinkKey(pointerLink, linkKey: key)
        XCTAssertTrue(link.hasPrefix(pointerLink + "#"))
        let fragment = GroupInviteLink.split(link).fragment
        XCTAssertEqual(fragment?.count, 43)
        XCTAssertEqual(fragment, base64UrlEncodeNoPad(key))
        // base64url, no padding: nothing a URL fragment would need escaping.
        XCTAssertFalse(fragment?.contains("=") ?? true)
        XCTAssertFalse(fragment?.contains("+") ?? true)
        XCTAssertFalse(fragment?.contains("/") ?? true)
    }

    func testWithLinkKeyReplacesAnExistingFragmentRatherThanDoublingIt() {
        let first = GroupInviteLink.withLinkKey(pointerLink, linkKey: key)
        let second = GroupInviteLink.withLinkKey(first, linkKey: Data(repeating: 0xFF, count: 32))
        XCTAssertEqual(second.filter { $0 == "#" }.count, 1)
        XCTAssertEqual(GroupInviteLink.split(second).base, pointerLink)
    }

    func testTheKeyParsesBackOutOfTheLink() {
        let link = GroupInviteLink.withLinkKey(pointerLink, linkKey: key)
        let (base, fragment) = GroupInviteLink.split(link)
        XCTAssertEqual(base, pointerLink)
        XCTAssertEqual(GroupInviteLink.linkKey(fromFragment: fragment), key)
    }

    func testSplitOfALinkWithoutAFragment() {
        let (base, fragment) = GroupInviteLink.split(pointerLink)
        XCTAssertEqual(base, pointerLink)
        XCTAssertNil(fragment)
        XCTAssertNil(GroupInviteLink.split(pointerLink + "#").fragment)
    }

    func testAFragmentThatIsNotA32ByteKeyIsNoKey() {
        XCTAssertNil(GroupInviteLink.linkKey(fromFragment: nil))
        XCTAssertNil(GroupInviteLink.linkKey(fromFragment: ""))
        XCTAssertNil(GroupInviteLink.linkKey(fromFragment: "not base64url!"))
        XCTAssertNil(GroupInviteLink.linkKey(
            fromFragment: base64UrlEncodeNoPad(Data(repeating: 1, count: 31))))
        XCTAssertNil(GroupInviteLink.linkKey(
            fromFragment: base64UrlEncodeNoPad(Data(repeating: 1, count: 33))))
    }

    func testTheSlugParserNeverSeesTheFragment() throws {
        // A 32-byte pointer slug with the key appended: the slug parser reads
        // the part before `#` and classifies it as the pointer it is.
        let blobId = Data(repeating: 0xAB, count: 32)
        let link = GroupInviteLink.withLinkKey(
            try GroupInviteUrl.composePointerGroupInviteUrl(blobId: blobId, origin: "https://lakko.me"),
            linkKey: key)
        let base = GroupInviteLink.split(link).base
        let slug = String(base.dropFirst("https://lakko.me/g/".count))
        XCTAssertEqual(try GroupInviteUrl.parseGroupInviteSlug(slug), .pointer(blobId: blobId))
    }

    // MARK: - Headline (spec section 3)

    func testBothNamesAndSeveralMembers() {
        let headline = GroupInviteHeadline.derive(
            from: GroupInviteFacts(groupName: "Hiking club", inviterName: "Anna", memberCount: 12))
        XCTAssertEqual(headline.lead, "Anna invites you to")
        XCTAssertEqual(headline.groupName, "Hiking club")
        XCTAssertEqual(headline.members, "12 members")
    }

    func testOneMemberIsSingular() {
        let headline = GroupInviteHeadline.derive(
            from: GroupInviteFacts(groupName: "Hiking club", inviterName: "Anna", memberCount: 1))
        XCTAssertEqual(headline.members, "1 member")
    }

    func testAMissingInviterReadsYouAreInvitedTo() {
        let headline = GroupInviteHeadline.derive(
            from: GroupInviteFacts(groupName: "Hiking club", inviterName: nil, memberCount: 3))
        XCTAssertEqual(headline.lead, "You are invited to")
        XCTAssertEqual(headline.groupName, "Hiking club")
        XCTAssertEqual(headline.members, "3 members")
    }

    func testAMissingGroupNameFoldsAGroupIntoTheLeadAndOmitsTheGroupLine() {
        let headline = GroupInviteHeadline.derive(
            from: GroupInviteFacts(groupName: nil, inviterName: "Anna", memberCount: 3))
        XCTAssertEqual(headline.lead, "Anna invites you to a group")
        XCTAssertNil(headline.groupName)
        XCTAssertEqual(headline.members, "3 members")
    }

    func testBothNamesMissingStillCarriesTheCount() {
        let headline = GroupInviteHeadline.derive(
            from: GroupInviteFacts(groupName: nil, inviterName: nil, memberCount: 2))
        XCTAssertEqual(headline.lead, "You are invited to a group")
        XCTAssertNil(headline.groupName)
        XCTAssertEqual(headline.members, "2 members")
    }

    func testBlankNamesCountAsMissing() {
        let headline = GroupInviteHeadline.derive(
            from: GroupInviteFacts(groupName: "  ", inviterName: "\n", memberCount: 2))
        XCTAssertEqual(headline.lead, "You are invited to a group")
        XCTAssertNil(headline.groupName)
    }

    func testNoPreviewInventsNothing() {
        let headline = GroupInviteHeadline.derive(from: nil)
        XCTAssertEqual(headline, .noPreview)
        XCTAssertEqual(headline.lead, "You are invited to a group")
        XCTAssertNil(headline.groupName)
        XCTAssertNil(headline.members)
    }

    func testTheFooterNamesTheFingerprint() {
        XCTAssertEqual(
            GroupInviteHeadline.footer(fingerprint: "12345 67890"),
            "Invited by 12345 67890")
        XCTAssertEqual(groupInviteInvalidMessage, "This invite link is invalid or has expired.")
    }

    // MARK: - Intake keeps the fragment beside the slug

    @MainActor
    func testTheIntakeParksTheFragmentWithTheSlugAndClearsBoth() throws {
        let intake = InviteIntake()
        let url = try XCTUnwrap(URL(string: "https://lakko.me/g/abc#" + base64UrlEncodeNoPad(key)))
        intake.handle(url)
        XCTAssertEqual(intake.pendingGroupJoinSlug, "abc")
        XCTAssertEqual(GroupInviteLink.linkKey(fromFragment: intake.pendingGroupJoinFragment), key)
        intake.clearGroupJoin()
        XCTAssertNil(intake.pendingGroupJoinSlug)
        XCTAssertNil(intake.pendingGroupJoinFragment)

        intake.handle(try XCTUnwrap(URL(string: "https://lakko.me/g/abc")))
        XCTAssertEqual(intake.pendingGroupJoinSlug, "abc")
        XCTAssertNil(intake.pendingGroupJoinFragment)
    }
}
