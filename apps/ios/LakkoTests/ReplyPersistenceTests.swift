// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0052 — a reply's reference and snapshot survive the message log, so a
/// relaunch draws the quote exactly as it was drawn on arrival; a row written
/// without one reads back as an ordinary message.
final class ReplyPersistenceTests: XCTestCase {

    private let reply = ReplyRef(target: 12, author: "Anna", excerpt: "see you at 5")

    private func message(deleted: Bool = false, reply: ReplyRef?) -> ChatMessage {
        ChatMessage(
            messageId: 40,
            direction: .incoming,
            text: "sure",
            createdAt: "2026-09-26T10:00:00Z",
            authorKeyB64: "Ym9i",
            deleted: deleted,
            reply: reply)
    }

    func testTheReplyRoundTripsThroughTheLog() throws {
        let blob = try JSONEncoder().encode(PersistedMessage(from: message(reply: reply)))
        let restored = try XCTUnwrap(
            JSONDecoder().decode(PersistedMessage.self, from: blob).toChatMessage())
        XCTAssertEqual(restored.reply, reply)
    }

    /// The target id alone is a valid reply on the wire (section 3.1's
    /// no-snapshot case) and must stay one across a relaunch.
    func testAReplyWithOnlyATargetRoundTrips() throws {
        let bare = ReplyRef(target: 7, author: nil, excerpt: nil)
        let blob = try JSONEncoder().encode(PersistedMessage(from: message(reply: bare)))
        let restored = try XCTUnwrap(
            JSONDecoder().decode(PersistedMessage.self, from: blob).toChatMessage())
        XCTAssertEqual(restored.reply, bare)
    }

    func testAnOrdinaryMessageStoresNoReplyAndReadsBackNil() throws {
        let stored = PersistedMessage(from: message(reply: nil))
        XCTAssertNil(stored.reply)
        let blob = try JSONEncoder().encode(stored)
        XCTAssertFalse(String(decoding: blob, as: UTF8.self).contains("\"reply\""))
        let restored = try XCTUnwrap(
            JSONDecoder().decode(PersistedMessage.self, from: blob).toChatMessage())
        XCTAssertNil(restored.reply)
    }

    /// A row written before the field existed carries no `reply` key at all.
    func testARowWithoutTheFieldDecodesAsNoReply() throws {
        let legacy = """
            {"messageId":3,"direction":"incoming","text":"hi","attachments":[],
             "createdAt":"2026-09-26T10:00:00Z","edited":false,"deleted":false,"reactions":[]}
            """
        let decoded = try JSONDecoder().decode(PersistedMessage.self, from: Data(legacy.utf8))
        XCTAssertNil(decoded.reply)
        XCTAssertNil(decoded.toChatMessage()?.reply)
    }

    /// The tombstone draws no quote, so the snapshot - somebody's words - is
    /// not kept, exactly as the deleted message's own text is not.
    func testADeletedReplyIsStoredWithoutItsQuote() {
        let stored = PersistedMessage(from: message(deleted: true, reply: reply))
        XCTAssertTrue(stored.deleted)
        XCTAssertNil(stored.reply)
    }
}
