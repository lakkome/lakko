// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0052 decision 1 / `docs/ux/message-actions.md` section 3.1 — what a
/// reply snapshots when Reply is chosen, and what the quote block draws from
/// whatever snapshot arrived.
final class ReplySnapshotTests: XCTestCase {

    private func message(
        _ text: String,
        id: Int64 = 41,
        direction: Lakko.Direction = .incoming,
        attachments: [AttachmentRef] = []
    ) -> ChatMessage {
        ChatMessage(
            messageId: id,
            direction: direction,
            text: text,
            attachments: attachments,
            createdAt: "2026-09-26T10:00:00Z",
            authorKeyB64: "them")
    }

    private func ref(kind: String?, mime: String) -> AttachmentRef {
        AttachmentRef(
            blobId: Data(repeating: 1, count: 32),
            key: Data(repeating: 2, count: 32),
            nonce: Data(repeating: 3, count: 12),
            mime: mime,
            size: 10,
            name: "a",
            kind: kind)
    }

    // MARK: - The snapshot

    func testTheSnapshotCarriesTheTargetIdTheNameAndTheTextOnOneLine() {
        let snap = ReplySnapshot.replySnapshot(
            for: message("  first line\nsecond   line \n"), authorLabel: "Anna")
        XCTAssertEqual(snap.target, 41)
        XCTAssertEqual(snap.author, "Anna")
        XCTAssertEqual(snap.excerpt, "first line second line")
    }

    /// Section 3.1 — "a message with no text shows the kind of its first
    /// attachment instead", decided the way the timeline decides the bubble.
    func testAMessageWithNoTextIsDescribedByItsFirstAttachmentsKind() {
        let cases: [(String?, String, String)] = [
            ("voice", "audio/mp4", "Voice message"),
            ("video_note", "video/mp4", "Video message"),
            (nil, "image/jpeg", "Photo"),
            (nil, "video/mp4", "Video"),
            ("gif", "image/gif", "GIF"),
            (nil, "application/pdf", "File"),
        ]
        for (kind, mime, expected) in cases {
            let snap = ReplySnapshot.replySnapshot(
                for: message("", attachments: [ref(kind: kind, mime: mime)]), authorLabel: "Anna")
            XCTAssertEqual(snap.excerpt, expected, "kind \(kind ?? "nil") mime \(mime)")
        }
    }

    /// The FIRST attachment, and the text wins over any attachment: a photo
    /// with a caption is quoted by its caption.
    func testTextWinsOverAttachmentsAndOnlyTheFirstAttachmentCounts() {
        let captioned = ReplySnapshot.replySnapshot(
            for: message("look", attachments: [ref(kind: nil, mime: "image/jpeg")]), authorLabel: "A")
        XCTAssertEqual(captioned.excerpt, "look")

        let two = ReplySnapshot.replySnapshot(
            for: message(
                "", attachments: [ref(kind: "voice", mime: "audio/mp4"), ref(kind: nil, mime: "image/jpeg")]),
            authorLabel: "A")
        XCTAssertEqual(two.excerpt, "Voice message")
    }

    /// The kind is sender-chosen; the MIME allowlist decides whether it is
    /// media at all. A `voice` kind on a non-audio MIME is a file in the
    /// timeline, so it is a file in the quote.
    func testAKindTheAllowlistRefusesIsQuotedAsAFile() {
        let snap = ReplySnapshot.replySnapshot(
            for: message("", attachments: [ref(kind: "voice", mime: "text/html")]), authorLabel: "A")
        XCTAssertEqual(snap.excerpt, "File")
    }

    func testAnEmptyOrBlankAuthorLabelIsSnapshottedAsNoName() {
        XCTAssertNil(ReplySnapshot.replySnapshot(for: message("x"), authorLabel: "  ").author)
    }

    func testAMessageWithNeitherTextNorAttachmentsHasNoExcerpt() {
        XCTAssertNil(ReplySnapshot.replySnapshot(for: message(""), authorLabel: "A").excerpt)
    }

    // MARK: - The quote lines

    func testTheQuoteDrawsTheSnapshotAsItCame() {
        let lines = ReplySnapshot.replyQuoteLines(ReplyRef(target: 1, author: "Anna", excerpt: "hello"))
        XCTAssertEqual(lines.name, "Anna")
        XCTAssertEqual(lines.excerpt, "hello")
    }

    /// Section 3.1 — "a reply that arrives with no snapshot at all (only the
    /// target id) draws the block with the name line empty and the excerpt
    /// Message".
    func testAReplyWithNoSnapshotDrawsAnEmptyNameAndMessage() {
        let lines = ReplySnapshot.replyQuoteLines(ReplyRef(target: 1, author: nil, excerpt: nil))
        XCTAssertEqual(lines.name, "")
        XCTAssertEqual(lines.excerpt, "Message")
    }

    /// A blank string from a peer is the same as none, and a line break a
    /// peer sent is folded: both lines are single-line by rule, not by luck.
    func testBlankAndMultilineSnapshotsFromAPeerAreNormalised() {
        let lines = ReplySnapshot.replyQuoteLines(ReplyRef(target: 1, author: " ", excerpt: "a\n\nb"))
        XCTAssertEqual(lines.name, "")
        XCTAssertEqual(lines.excerpt, "a b")
        XCTAssertEqual(
            ReplySnapshot.replyQuoteLines(ReplyRef(target: 1, author: "A", excerpt: "\n ")).excerpt,
            "Message")
    }
}
