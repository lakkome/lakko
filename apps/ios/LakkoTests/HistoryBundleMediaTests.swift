// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0047 §5: a media message moved to a new device through the history
/// bundle restores as itself, through the real core codec.
final class HistoryBundleMediaTests: XCTestCase {
    private func b64(_ count: Int, _ fill: UInt8) -> String {
        Data(repeating: fill, count: count).base64EncodedString()
    }

    private func roundTrip(_ attachment: PersistedAttachment) throws -> PersistedAttachment {
        let message = PersistedMessage(
            messageId: 42,
            direction: "incoming",
            text: "",
            attachments: [attachment],
            createdAt: "2026-09-14T12:00:00Z"
        )
        let bytes = try HistoryBundleCodec.encodePlaceholder(message)
        let back = try XCTUnwrap(HistoryBundleCodec.decodePlaceholder(bytes))
        return try XCTUnwrap(back.attachments.first)
    }

    func testVoiceMessageRestoresAsVoiceMessage() throws {
        let waveform = Data([3, 9, 27, 81]).base64EncodedString()
        let back = try roundTrip(PersistedAttachment(
            blobIdB64: b64(32, 1), keyB64: b64(32, 2), nonceB64: b64(12, 3),
            mime: "audio/mp4", size: 1234, name: "voice.m4a",
            kind: "voice", durationMs: 7250, waveformB64: waveform
        ))
        XCTAssertEqual(back.kind, "voice")
        XCTAssertEqual(back.durationMs, 7250)
        XCTAssertEqual(back.waveformB64, waveform)
    }

    func testVideoNoteKeepsItsPosterFrame() throws {
        let thumb = PersistedThumb(
            blobIdB64: b64(32, 7), keyB64: b64(32, 8), nonceB64: b64(12, 9),
            mime: "image/jpeg", size: 20_000, width: 240, height: 240
        )
        let back = try roundTrip(PersistedAttachment(
            blobIdB64: b64(32, 1), keyB64: b64(32, 2), nonceB64: b64(12, 3),
            mime: "video/mp4", size: 800_000, name: "note.mp4",
            kind: "video_note", durationMs: 42_000, width: 480, height: 480, thumb: thumb
        ))
        XCTAssertEqual(back.kind, "video_note")
        XCTAssertEqual(back.width, 480)
        XCTAssertEqual(back.thumb, thumb)
    }

    func testOutOfBoundValuesAreDroppedLikeTheEnvelope() throws {
        let back = try roundTrip(PersistedAttachment(
            blobIdB64: b64(32, 1), keyB64: b64(32, 2), nonceB64: b64(12, 3),
            mime: "image/gif", size: 10, name: "a.gif",
            kind: "gif", durationMs: 1000, width: 0, height: 5000
        ))
        XCTAssertEqual(back.kind, "gif")
        XCTAssertNil(back.durationMs)
        XCTAssertNil(back.width)
        XCTAssertNil(back.height)
    }

    // MARK: - MLS bookkeeping rows never travel

    private func row(_ id: Int64, _ direction: String, _ text: String) -> PersistedMessage {
        PersistedMessage(
            messageId: id, direction: direction, text: text, attachments: [],
            createdAt: "2026-10-28T12:00:0\(id)Z")
    }

    private let secret = Data(repeating: 0x42, count: 32)
    private let groupHex = String(repeating: "cd", count: 32)

    func testTheBuilderLeavesOutCommitAndExternalJoinRows() throws {
        let account = "history-bookkeeping-build-\(UUID().uuidString)"
        let store = MessageLogStore()
        defer { store.deleteLog(accountIdB64: account) }
        store.putLog(accountIdB64: account, payload: MessageLogPayload(groups: [
            groupHex: PersistedGroup(cursor: 4, messages: [
                row(1, "incoming", "hello"),
                row(2, "system", "[mls commit]"),
                row(3, "system", "[mls external join]"),
                row(4, "outgoing", "[mls commit]"),
            ]),
        ]))
        XCTAssertNotNil(store.getLog(accountIdB64: account), "the test log was written")

        let sealed = try XCTUnwrap(HistoryBundleCodec.buildFromMessageLog(
            accountIdB64: account, secret: secret, store: store))
        let opened = try openHistoryBundleStructured(secret: secret, sealedBytes: sealed)
        let rows = opened.groups.flatMap(\.messages).compactMap {
            HistoryBundleCodec.decodePlaceholder($0.payloadBytes)
        }
        XCTAssertEqual(rows.map(\.messageId), [1, 4], "a message reading like the marker is still carried")
    }

    func testTheReceiverDropsCommitRowsItIsSent() throws {
        let account = "history-bookkeeping-apply-\(UUID().uuidString)"
        let store = MessageLogStore()
        defer { store.deleteLog(accountIdB64: account) }
        let messages = try [
            row(1, "incoming", "hello"),
            row(2, "system", "[mls commit]"),
        ].map { m in
            StructuredHistoryMessage(
                payloadBytes: try HistoryBundleCodec.encodePlaceholder(m), senderLeaf: 0,
                messageId: withUnsafeBytes(of: m.messageId.bigEndian) { Data($0) }, receivedAtMs: 0)
        }
        let sealed = try sealHistoryBundleStructured(
            secret: secret,
            groups: [StructuredHistoryGroup(
                groupId: try XCTUnwrap(StateBundle.hexDecode(groupHex)), messages: messages, countCapped: false)],
            maxHlc: 2)

        let applied = try HistoryBundleCodec.applyToMessageLog(
            accountIdB64: account, secret: secret, sealedBytes: sealed, store: store)

        XCTAssertEqual(applied, 1)
        XCTAssertEqual(store.getLog(accountIdB64: account)?.groups[groupHex]?.messages.map(\.text), ["hello"])
    }
}
