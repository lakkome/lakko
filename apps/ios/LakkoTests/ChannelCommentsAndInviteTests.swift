// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// Base walk 2026-11-13 (scenario 28): `docs/ux/group-and-channel-privacy.md`
/// 13.2 / 13.3 (a collapsed thread never counts a comment this reader hides)
/// and 14.1 / 15.2 (`Invite people` does not offer a person already in the
/// channel).
@MainActor
final class ChannelCommentsAndInviteTests: XCTestCase {
    typealias More = ChannelConversationModel.CollapsedMore

    private func more(
        visible: Int, rowsRead: Int, firstRead: Bool = true, mayHaveMore: Bool = false, server: UInt64?
    ) -> More {
        ChannelConversationModel.collapsedMore(
            visible: visible, rowsRead: rowsRead, firstRead: firstRead, mayHaveMore: mayHaveMore,
            serverCount: server)
    }

    func testABannedAuthorsCommentsTheServiceStillCountsAreNotCounted() {
        // Two rows read, both by a banned author; the service says 2.
        XCTAssertEqual(more(visible: 0, rowsRead: 2, server: 2), .comment)
    }

    func testACommentThatDidNotOpenIsNotCounted() {
        XCTAssertEqual(more(visible: 3, rowsRead: 4, server: 4), .count(1))
    }

    func testNothingIsCountedBeforeTheFirstPageIsRead() {
        XCTAssertEqual(more(visible: 0, rowsRead: 0, firstRead: false, server: 2), .none)
        XCTAssertEqual(more(visible: 0, rowsRead: 0, firstRead: false, server: 0), .comment)
    }

    func testOnlyTheRowsNotReadYetComeFromTheService() {
        XCTAssertEqual(more(visible: 5, rowsRead: 20, mayHaveMore: true, server: 30), .count(13))
        XCTAssertEqual(more(visible: 5, rowsRead: 20, mayHaveMore: true, server: 10), .count(3))
        XCTAssertEqual(more(visible: 5, rowsRead: 20, mayHaveMore: true, server: nil), .unknown)
    }

    private func contact(_ key: Data, previous: [Data] = []) -> AcceptedContact {
        AcceptedContact(
            request: IncomingContactRequest(
                messageId: 1, fromPubkey: key, fromAccountId: Data(repeating: 0x0A, count: 16),
                fromPseudonym: Data(repeating: 0, count: 32), validAfterMs: 1, intro: nil,
                createdAt: "2026-11-13T00:00:00Z"),
            previousKeysB64: previous.isEmpty ? nil : previous.map { $0.base64EncodedString() })
    }

    func testAPublisherIsNotOfferedToInvite() {
        let publisher = Data(repeating: 0x11, count: 32)
        let anna = Data(repeating: 0x33, count: 32)
        let left = ChannelInvitePeopleScreen.candidates(
            [contact(publisher), contact(anna)], inChannel: [publisher, Data(repeating: 0x22, count: 32)])
        XCTAssertEqual(left.map(\.request.fromPubkey), [anna])
    }

    func testAContactIsMatchedByAnEarlierKeyToo() {
        let old = Data(repeating: 0x44, count: 32)
        let left = ChannelInvitePeopleScreen.candidates(
            [contact(Data(repeating: 0x55, count: 32), previous: [old])], inChannel: [old])
        XCTAssertTrue(left.isEmpty)
    }

    func testEveryContactIsOfferedWhenNobodyIsKnownToBeIn() {
        let all = [contact(Data(repeating: 0x66, count: 32))]
        XCTAssertEqual(ChannelInvitePeopleScreen.candidates(all, inChannel: []).count, 1)
    }
}
