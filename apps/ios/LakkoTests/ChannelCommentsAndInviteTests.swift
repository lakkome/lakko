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

/// UX `group-and-channel-privacy.md` 13.2 (defect seen live 2026-11-22: a
/// comment written on another device did not reach a channel open on
/// screen) — what the open channel's tick does for one post's thread. The
/// same table as web `comment-thread.test.ts` and Android
/// `CommentRefreshTest`.
@MainActor
final class ChannelCommentRefreshTests: XCTestCase {
    typealias Refresh = ChannelConversationModel.CommentRefresh

    private func refresh(
        _ firstRead: Bool, _ expanded: Bool, _ morePages: Bool, _ rowsRead: Int,
        _ countAtRead: UInt64?, _ server: UInt64?
    ) -> Refresh {
        ChannelConversationModel.commentRefresh(
            firstRead: firstRead, expanded: expanded, morePages: morePages, rowsRead: rowsRead,
            countAtRead: countAtRead, serverCount: server)
    }

    func testTheSharedTable() {
        XCTAssertEqual(refresh(false, false, false, 0, nil, 3), .none, "nothing before the first read")
        XCTAssertEqual(refresh(true, true, false, 2, nil, nil), .newer, "no count, open, read to the end")
        XCTAssertEqual(refresh(true, false, false, 2, nil, nil), .none, "no count, collapsed")
        XCTAssertEqual(refresh(true, true, true, 20, nil, nil), .none, "no count, open, pages unread")
        XCTAssertEqual(refresh(true, false, false, 2, 2, 2), .none, "count as read")
        XCTAssertEqual(refresh(true, false, false, 1, 1, 2), .newer, "count up, collapsed")
        XCTAssertEqual(refresh(true, true, false, 1, 1, 2), .newer, "count up, open")
        XCTAssertEqual(refresh(true, true, true, 20, 25, 26), .none, "count up, pages unread")
        XCTAssertEqual(refresh(true, false, false, 3, 3, 2), .reread, "count down, collapsed")
        XCTAssertEqual(refresh(true, true, false, 3, 3, 2), .rebase, "count down, open")
        XCTAssertEqual(refresh(true, false, false, 2, nil, 2), .none, "the rows read stand in")
        XCTAssertEqual(refresh(true, false, false, 2, nil, 3), .newer, "one more than the rows read")
    }

    private func comment(_ cursor: UInt64) -> ChannelConversationModel.Comment {
        ChannelConversationModel.Comment(
            cursor: cursor, writerKey: Data(), authorKey: Data([1]), name: "n", text: "t\(cursor)",
            replyTo: nil, serverTsMs: cursor, own: false)
    }

    func testArrivedCommentsJoinTheThreadInOrderWithoutRepeats() {
        let merged = ChannelConversationModel.merged(
            [comment(1), comment(3)], [comment(2), comment(3), comment(4)])
        XCTAssertEqual(merged.map(\.cursor), [1, 2, 3, 4])
    }
}
