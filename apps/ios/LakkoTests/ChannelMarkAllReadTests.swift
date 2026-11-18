// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// Mark all read in Chats reads the tab's channels too (follow-up to base
/// scenario 29, 2026-11-13; web and Android do the same): each is read
/// through the newest post this device holds, and a marker never moves back.
final class ChannelMarkAllReadTests: XCTestCase {
    private let a = String(repeating: "aa", count: 16)
    private let b = String(repeating: "bb", count: 16)

    func testEachChannelIsReadThroughItsNewestHeldPost() {
        var reading = FeedReading()
        reading.channelRead = [a: 3, b: 9]
        let slots = [
            a: ChannelPostSlot(cursor: 7, posts: []),
            b: ChannelPostSlot(cursor: 2, posts: []),
        ]
        let next = reading.markingChannelsRead([a, b], slots: slots)
        XCTAssertEqual(next.channelRead[a], 7)
        XCTAssertEqual(next.channelRead[b], 9, "never backwards")
        XCTAssertEqual(reading.markingChannelsRead([a], slots: [:]), reading, "no slot, no change")
    }

    func testOnlyChannelRowsWithNewPostsAreMarked() {
        let rows = [
            ConversationListRow(id: a, kind: .channel, title: "A", preview: "", lastActivityMs: 1, unread: 2),
            ConversationListRow(id: b, kind: .channel, title: "B", preview: "", lastActivityMs: 1, unread: 0),
            ConversationListRow(
                id: String(repeating: "cc", count: 32), kind: .group, title: "G", preview: "",
                lastActivityMs: 1, unread: 4),
        ]
        XCTAssertEqual(ChatListScreen.channelIdsToMarkRead(rows), [a])
    }
}
