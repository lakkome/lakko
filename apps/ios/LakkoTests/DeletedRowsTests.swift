// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// `docs/ux/message-actions.md` section 9 — a deleted message leaves the
/// conversation: never drawn, except a 2-second going when the deletion was
/// applied in front of the person; never a list preview; never unread.
final class DeletedRowsTests: XCTestCase {

    private func message(
        _ id: Int64, deleted: Bool = false, text: String = "hi",
        direction: Direction = .incoming, at createdAt: String = "2026-11-22T10:00:00Z"
    ) -> ChatMessage {
        ChatMessage(messageId: id, direction: direction, text: text, createdAt: createdAt, deleted: deleted)
    }

    // MARK: - The two durations

    func testTheDurationsAreTheSpecsChosenValues() {
        XCTAssertEqual(DeletedRows.holdSeconds, 2.0)
        XCTAssertEqual(DeletedRows.collapseSeconds, 0.25)
    }

    // MARK: - Which deletions are seen going

    func testADeletionAppliedInFrontOfADrawnRowEntersTheHold() {
        let held = DeletedRows.enteringHold(drawnBefore: [1, 2, 3], deletedNow: [2], inFront: true)
        XCTAssertEqual(held, [2])
    }

    func testADeletionAppliedElsewhereShowsNothing() {
        let held = DeletedRows.enteringHold(drawnBefore: [1, 2, 3], deletedNow: [2], inFront: false)
        XCTAssertTrue(held.isEmpty)
    }

    func testAMessageAndItsDeletionInOneBatchShowNothing() {
        // The message was never drawn: it is not among the rows before.
        let before = [message(1)]
        let after = [message(1), message(2, deleted: true)]
        let newly = DeletedRows.newlyDeleted(before: before, after: after)
        XCTAssertTrue(newly.isEmpty)
        XCTAssertTrue(DeletedRows.enteringHold(drawnBefore: [1], deletedNow: newly, inFront: true).isEmpty)
    }

    func testABulkDeleteHoldsEveryRowAndARowAlreadyHeldKeepsItsTimer() {
        let held = DeletedRows.enteringHold(
            drawnBefore: [1, 2, 3, 4], deletedNow: [2, 3, 4], alreadyHeld: [4], inFront: true)
        XCTAssertEqual(held, [2, 3])
    }

    func testNewlyDeletedIsWhatTurnedDeletedBetweenTwoStates() {
        let before = [message(1), message(2), message(3, deleted: true)]
        let after = [message(1, deleted: true), message(2), message(3, deleted: true)]
        XCTAssertEqual(DeletedRows.newlyDeleted(before: before, after: after), [1])
    }

    // MARK: - What the timeline draws

    func testADeletedMessageIsNotDrawnUnlessHeld() {
        let messages = [message(1), message(2, deleted: true), message(3, deleted: true)]
        XCTAssertEqual(DeletedRows.drawn(messages, held: []).map(\.messageId), [1])
        XCTAssertEqual(DeletedRows.drawn(messages, held: [3]).map(\.messageId), [1, 3])
        XCTAssertEqual(DeletedRows.drawnIds(messages, held: [2]), [1, 2])
    }

    func testADateSeparatorWithNoMessageLeftIsNotDrawn() {
        let calendar = Calendar(identifier: .gregorian)
        let messages = [
            message(1, at: "2026-11-20T10:00:00Z"),
            message(2, deleted: true, at: "2026-11-21T10:00:00Z"),
            message(3, at: "2026-11-22T10:00:00Z"),
        ]
        let entries = ChatTimeline.layout(
            messages: DeletedRows.drawn(messages, held: []),
            nowMs: ChatRepository.parseDsTimestampMs("2026-11-22T12:00:00Z") ?? 0,
            calendar: calendar,
            locale: Locale(identifier: "en_US"),
            parseMs: ChatRepository.parseDsTimestampMs)
        XCTAssertEqual(entries.map(\.message.messageId), [1, 3])
        XCTAssertEqual(entries.filter { $0.dateLabel != nil }.count, 2, "the deleted day has no separator")
    }

    func testARunIsComputedOverTheRemainingMessages() {
        // Two messages from one author around a deleted message of another:
        // the second continues the first's run.
        let calendar = Calendar(identifier: .gregorian)
        let messages = [
            ChatMessage(messageId: 1, direction: .incoming, text: "a",
                        createdAt: "2026-11-22T10:00:00Z", authorKeyB64: "anna"),
            ChatMessage(messageId: 2, direction: .incoming, text: "b",
                        createdAt: "2026-11-22T10:00:30Z", authorKeyB64: "ben", deleted: true),
            ChatMessage(messageId: 3, direction: .incoming, text: "c",
                        createdAt: "2026-11-22T10:01:00Z", authorKeyB64: "anna"),
        ]
        let entries = ChatTimeline.layout(
            messages: DeletedRows.drawn(messages, held: []),
            nowMs: ChatRepository.parseDsTimestampMs("2026-11-22T12:00:00Z") ?? 0,
            calendar: calendar,
            locale: Locale(identifier: "en_US"),
            parseMs: ChatRepository.parseDsTimestampMs)
        XCTAssertEqual(entries.map(\.startsRun), [true, false])
    }

    // MARK: - The chat list

    func testThePreviewIsTheNewestMessageNotDeleted() {
        let messages = [message(1, text: "older"), message(2, deleted: true, text: "newest")]
        let row = DeletedRows.listPreviewRow(messages, isPreviewRow: ChatTimeline.isListPreviewRow)
        XCTAssertEqual(row?.messageId, 1)
        XCTAssertEqual(row?.text, "older")
    }

    func testAConversationWhoseMessagesWereAllDeletedPreviewsNothing() {
        let messages = [message(1, deleted: true), message(2, deleted: true)]
        XCTAssertNil(DeletedRows.listPreviewRow(messages, isPreviewRow: ChatTimeline.isListPreviewRow))
    }

    func testThePreviewNeverSaysMessageDeleted() {
        // `preview` no longer takes a deleted flag at all: the caller picks a
        // message that is not deleted.
        let line = ChatListFormatting.preview(
            text: "older", attachmentLabels: [], direction: .incoming,
            isGroup: false, senderName: nil)
        XCTAssertFalse(line.contains(Localization.text("chat.message.deleted")))
    }

    // MARK: - Unread

    func testADeletedMessageIsNeverUnread() {
        let thread = ChatThread(
            groupIdHex: "aa",
            messages: [message(4), message(5, deleted: true), message(6, deleted: true)],
            cursor: 0)
        let cursor = ReadCursorState(upToMessageId: UnreadCounting.cursorBytes(3), readAtMs: 1, updatedAtHlc: 1)
        XCTAssertEqual(ChatListScreen.unreadCount(thread: thread, cursor: cursor), 1)
        let onlyDeleted = ChatThread(groupIdHex: "aa", messages: [message(5, deleted: true)], cursor: 0)
        XCTAssertEqual(ChatListScreen.unreadCount(thread: onlyDeleted, cursor: cursor), 0)
    }

    // MARK: - Channels

    func testADeletedPostLeavesThePostLog() async throws {
        let now: Int64 = 1_800_000_000_000
        let page = ChannelPostLog.Page(posts: [
            (cursor: 1, serverTsMs: now, payloadB64: "one"),
            (cursor: 2, serverTsMs: now, payloadB64: "two"),
            (cursor: 3, serverTsMs: now, payloadB64: "delete-2"),
        ])
        let slot = try await ChannelPostLog.syncSlot(
            nil,
            fetchPage: { after, _ in after == 0 ? page : ChannelPostLog.Page(posts: []) },
            decode: { payload in
                switch payload {
                case "one": return .text("one")
                case "two": return .text("two")
                case "delete-2": return .delete(target: 2)
                default: return .skip
                }
            },
            nowMs: now)
        XCTAssertEqual(slot.posts.map(\.cursor), [1])
        XCTAssertEqual(slot.cursor, 3)

        // And a delete of a post an earlier sync logged.
        let later = ChannelPostLog.Page(posts: [(cursor: 4, serverTsMs: now, payloadB64: "delete-1")])
        let next = try await ChannelPostLog.syncSlot(
            slot,
            fetchPage: { after, _ in after == 3 ? later : ChannelPostLog.Page(posts: []) },
            decode: { $0 == "delete-1" ? .delete(target: 1) : .skip },
            nowMs: now)
        XCTAssertTrue(next.posts.isEmpty)
    }

    func testAChannelFoldNamesTheDeletedPosts() {
        let posts = [
            ChannelDecodedPost(cursor: 1, serverTsMs: 1, epoch: 0,
                               state: .body(.chat(text: "a", attachments: [], forward: nil))),
            ChannelDecodedPost(cursor: 2, serverTsMs: 2, epoch: 0, state: .body(.delete(target: 1))),
        ]
        let folded = ChannelTimeline.fold(posts)
        XCTAssertTrue(folded.items.isEmpty)
        XCTAssertEqual(folded.deleted, [1])
    }
}
