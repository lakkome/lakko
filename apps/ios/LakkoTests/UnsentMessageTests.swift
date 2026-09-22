// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// A message appears in the conversation when the person writes it, and
/// settles afterwards.
///
/// Before this the send was awaited first and the row appended only on
/// success, so a message written on a bad connection left the person looking
/// at an error over an unchanged composer, with nothing in the conversation
/// at all. The words were safe; the message was not there, and there was
/// nothing to retry.
final class UnsentMessageTests: XCTestCase {

    private func message(
        ack: ChatMessage.Ack,
        messageId: Int64 = 0,
        localId: Int64 = -1
    ) -> ChatMessage {
        ChatMessage(
            messageId: messageId,
            direction: .outgoing,
            text: "the words",
            createdAt: "1970-01-01T00:00:00Z",
            ack: ack,
            localId: localId)
    }

    // MARK: - what the bubble says

    func testAPendingRowSaysItIsSending() {
        XCTAssertEqual(ChatTimeline.statusLabel(ack: .pending, time: "17:21"), "Sending...")
    }

    func testAFailedRowSaysItIsNotSent() {
        // The same word the media bubbles and the web client use. Two
        // spellings of "this did not send" would be one of them being wrong.
        XCTAssertEqual(ChatTimeline.statusLabel(ack: .failed, time: "17:21"), "Not sent")
    }

    func testASentRowShowsItsTime() {
        XCTAssertEqual(ChatTimeline.statusLabel(ack: .sent, time: "17:21"), "17:21")
    }

    func testASentRowWithNoTimeShowsNothing() {
        XCTAssertNil(ChatTimeline.statusLabel(ack: .sent, time: nil))
    }

    // MARK: - identity

    func testALocalRowIsIdentifiedByItsLocalId() {
        // Two unsent rows both have `messageId` 0, so without this they share
        // an identity and SwiftUI draws one row for two messages.
        XCTAssertEqual(message(ack: .pending, localId: -1).id, -1)
        XCTAssertNotEqual(
            message(ack: .pending, localId: -1).id,
            message(ack: .pending, localId: -2).id)
    }

    func testTheIdentityDoesNotMoveWhenTheSendLands() {
        // An identity that changed at that moment would tear the bubble down
        // and build a new one, mid-send.
        let pending = message(ack: .pending, localId: -1)
        let landed = message(ack: .sent, messageId: 4321, localId: -1)
        XCTAssertEqual(pending.id, landed.id)
        XCTAssertEqual(landed.messageId, 4321)
    }

    func testARowFromTheServerIsIdentifiedByItsMessageId() {
        XCTAssertEqual(message(ack: .sent, messageId: 9, localId: 0).id, 9)
    }

    // MARK: - what reaches the disk

    func testAnUnsentRowIsNotWritten() {
        let rows = PersistedMessage.persistable([
            message(ack: .sent, messageId: 1, localId: 0),
            message(ack: .pending),
            message(ack: .failed, localId: -2),
        ])
        XCTAssertEqual(rows.map(\.messageId), [1])
    }

    func testAnOrdinaryTranscriptIsWrittenWhole() {
        // The control: filtering on the wrong condition would silently write
        // nothing at all.
        let rows = PersistedMessage.persistable([
            message(ack: .sent, messageId: 1, localId: 0),
            message(ack: .sent, messageId: 2, localId: 0),
        ])
        XCTAssertEqual(rows.count, 2)
    }

    func testARowFromTheJournalIsSentWithoutSayingSo() {
        // Everything off the journal, the message log or a call transcript
        // exists BECAUSE it was delivered, so the default carries them.
        let incoming = ChatMessage(
            messageId: 5,
            direction: .incoming,
            text: "hi",
            createdAt: "1970-01-01T00:00:00Z")
        XCTAssertEqual(incoming.ack, .sent)
        XCTAssertEqual(PersistedMessage.persistable([incoming]).count, 1)
    }
}
