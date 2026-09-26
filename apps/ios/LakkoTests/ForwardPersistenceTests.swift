// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0052 decision 2 — a forward's attribution and origin survive the
/// message log, so a relaunch draws the "Forwarded from" line and keeps its
/// tap target; a row written without one reads back as an ordinary message.
final class ForwardPersistenceTests: XCTestCase {

    private let forward = ForwardRef(author: "Anna", originGroupIdHex: "0a0b0c0d", originMessageId: 12)

    private func message(deleted: Bool = false, forward: ForwardRef?) -> ChatMessage {
        ChatMessage(
            messageId: 40,
            direction: .incoming,
            text: "sure",
            createdAt: "2026-09-26T10:00:00Z",
            authorKeyB64: "Ym9i",
            deleted: deleted,
            forward: forward)
    }

    func testTheForwardRoundTripsThroughTheLog() throws {
        let blob = try JSONEncoder().encode(PersistedMessage(from: message(forward: forward)))
        let restored = try XCTUnwrap(
            JSONDecoder().decode(PersistedMessage.self, from: blob).toChatMessage())
        XCTAssertEqual(restored.forward, forward)
    }

    /// A name alone, with no origin, is a valid forward on the wire and must
    /// stay one across a relaunch — as must an origin with no name.
    func testAForwardWithOnlyANameOrOnlyAnOriginRoundTrips() throws {
        for bare in [
            ForwardRef(author: "Anna", originGroupIdHex: nil, originMessageId: nil),
            ForwardRef(author: nil, originGroupIdHex: "0a0b", originMessageId: 3),
        ] {
            let blob = try JSONEncoder().encode(PersistedMessage(from: message(forward: bare)))
            let restored = try XCTUnwrap(
                JSONDecoder().decode(PersistedMessage.self, from: blob).toChatMessage())
            XCTAssertEqual(restored.forward, bare)
        }
    }

    func testAnOrdinaryMessageStoresNoForwardAndReadsBackNil() throws {
        let stored = PersistedMessage(from: message(forward: nil))
        XCTAssertNil(stored.forward)
        let blob = try JSONEncoder().encode(stored)
        XCTAssertFalse(String(decoding: blob, as: UTF8.self).contains("\"forward\""))
        let restored = try XCTUnwrap(
            JSONDecoder().decode(PersistedMessage.self, from: blob).toChatMessage())
        XCTAssertNil(restored.forward)
    }

    /// A row written before the field existed carries no `forward` key at all.
    func testARowWithoutTheFieldDecodesAsNoForward() throws {
        let legacy = """
            {"messageId":3,"direction":"incoming","text":"hi","attachments":[],
             "createdAt":"2026-09-26T10:00:00Z","edited":false,"deleted":false,"reactions":[]}
            """
        let decoded = try JSONDecoder().decode(PersistedMessage.self, from: Data(legacy.utf8))
        XCTAssertNil(decoded.forward)
        XCTAssertNil(decoded.toChatMessage()?.forward)
    }

    /// The tombstone draws no attribution line, so the name - somebody's - is
    /// not kept, exactly as the deleted message's own text is not.
    func testADeletedForwardIsStoredWithoutItsAttribution() {
        let stored = PersistedMessage(from: message(deleted: true, forward: forward))
        XCTAssertTrue(stored.deleted)
        XCTAssertNil(stored.forward)
    }
}
