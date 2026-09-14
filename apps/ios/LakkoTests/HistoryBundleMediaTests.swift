// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
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
}
