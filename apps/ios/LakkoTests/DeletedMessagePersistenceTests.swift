// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0023 — what a deleted message leaves on disk.
///
/// The tombstone is drawn from the `deleted` flag, by the bubble and by the
/// conversation row alike, so the text and the attachments of a deleted
/// message are read by nothing. Storing them anyway kept the words someone
/// chose to delete - and the content keys that open their attachment blobs on
/// the server - on the device for as long as the transcript lived.
final class DeletedMessagePersistenceTests: XCTestCase {

    private let bob: MemberIdentity = "Ym9i"

    private func attachment() -> AttachmentRef {
        AttachmentRef(
            blobId: Data(repeating: 1, count: 32),
            key: Data(repeating: 2, count: 32),
            nonce: Data(repeating: 3, count: 12),
            mime: "image/png",
            size: 10,
            name: "a.png")
    }

    private func message(deleted: Bool) -> ChatMessage {
        ChatMessage(
            messageId: 7,
            direction: .incoming,
            text: "the secret",
            attachments: [attachment()],
            createdAt: "1970-01-01T00:00:00Z",
            authorKeyB64: bob,
            deleted: deleted,
            reactions: [Reaction(reactor: bob, emoji: "👍")])
    }

    func testADeletedMessageIsStoredWithNoTextAndNoAttachments() {
        let stored = PersistedMessage(from: message(deleted: true))
        XCTAssertTrue(stored.deleted)
        XCTAssertEqual(stored.text, "")
        XCTAssertTrue(stored.attachments.isEmpty)
    }

    func testAMessageThatWasNotDeletedKeepsBoth() {
        // The control. Emptying on the wrong condition would silently erase
        // every transcript, which is why this pair runs together.
        let stored = PersistedMessage(from: message(deleted: false))
        XCTAssertEqual(stored.text, "the secret")
        XCTAssertEqual(stored.attachments.count, 1)
    }

    func testTheDeletedMessageStillRestoresAsADeletedMessage() throws {
        // Emptying the row must not make it unreadable, or a deleted message
        // would come back as an ordinary empty bubble.
        let blob = try JSONEncoder().encode(PersistedMessage(from: message(deleted: true)))
        let restored = try XCTUnwrap(
            JSONDecoder().decode(PersistedMessage.self, from: blob).toChatMessage())
        XCTAssertTrue(restored.deleted)
        XCTAssertEqual(restored.text, "")
    }

    func testReactionsSurviveADelete() {
        // The message is gone; who reacted to it is still the group's state,
        // and the tombstone carries the chips.
        let stored = PersistedMessage(from: message(deleted: true))
        XCTAssertEqual(stored.reactions, [PersistedReaction(reactorKeyB64: bob, emoji: "👍")])
    }

    func testTheConversationRowReadsTheTombstoneFromTheFlag() {
        // Which is what makes emptying the row safe: the preview never looked
        // at the text of a deleted message in the first place.
        let line = ChatListFormatting.preview(
            text: "",
            attachmentLabels: [],
            direction: .incoming,
            deleted: true,
            isGroup: false,
            senderName: nil)
        XCTAssertEqual(line, "Message deleted")
    }
}
