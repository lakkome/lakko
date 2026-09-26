// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0052 decision 1 / `docs/ux/message-actions.md` section 3.1 — the two
/// pure rules of a reply's quote: what is snapshotted when Reply is chosen,
/// and what the block draws from whatever snapshot arrived.
///
/// Pure functions on purpose: the strip and the bubble on iOS, and their twins
/// on the other clients, all show the same two strings, and a rule that lives
/// in one view drifts from the copy in the other.
public enum ReplySnapshot {

    /// The excerpt the block falls back to when a reply carries no snapshot at
    /// all (only the target id). Section 3.1: "the name line empty and the
    /// excerpt Message".
    public static let missingExcerpt = "Message"

    /// Take the snapshot for a reply to `message`, at the moment Reply is
    /// chosen.
    ///
    /// `authorLabel` is whatever the timeline already calls that person
    /// (section 3.1): the caller resolves it, because the name is the
    /// timeline's business and this helper knows nothing about contacts.
    ///
    /// The excerpt is the message's text trimmed to one line; a message with
    /// no text takes the kind of its first attachment, decided by the same
    /// rule the timeline decides the bubble shape with
    /// (``ChatListFormatting/mediaLabel(kind:mime:allowlist:)`` over
    /// ``InlineMedia``), so a quote never calls something a photo that the
    /// bubble drew as a file. A message with neither is snapshotted with no
    /// excerpt, which the block later reads as ``missingExcerpt``.
    ///
    /// Nothing is length-bounded here: the core truncates an over-long
    /// snapshot on a character boundary when the envelope is composed.
    public static func replySnapshot(for message: ChatMessage, authorLabel: String) -> ReplyRef {
        let line = oneLine(message.text)
        let excerpt: String?
        if !line.isEmpty {
            excerpt = line
        } else if let first = message.attachments.first {
            excerpt = ChatListFormatting.mediaLabel(kind: first.kind, mime: first.mime)
        } else {
            excerpt = nil
        }
        let author = authorLabel.trimmingCharacters(in: .whitespacesAndNewlines)
        return ReplyRef(
            target: message.messageId,
            author: author.isEmpty ? nil : author,
            excerpt: excerpt)
    }

    /// The two lines the quote block draws — in the strip above the composer
    /// and in the bubble alike — from the snapshot a reply carries.
    ///
    /// A missing or blank author draws an empty name line; a missing or blank
    /// excerpt draws ``missingExcerpt``. Both are collapsed to one line again
    /// here because the snapshot came off the wire and a peer may have sent a
    /// line break the composer here never would.
    public static func replyQuoteLines(_ reply: ReplyRef) -> (name: String, excerpt: String) {
        let name = oneLine(reply.author ?? "")
        let excerpt = oneLine(reply.excerpt ?? "")
        return (name: name, excerpt: excerpt.isEmpty ? missingExcerpt : excerpt)
    }

    /// Trim, then fold every line break and run of whitespace into one space.
    static func oneLine(_ text: String) -> String {
        text.split(whereSeparator: { $0.isWhitespace || $0.isNewline })
            .joined(separator: " ")
    }
}
