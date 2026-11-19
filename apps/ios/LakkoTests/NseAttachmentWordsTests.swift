// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

/// `docs/ux/media-messages.md` section 11 — a notification says the same
/// words as the chat list row for a message of photos, videos or files. The
/// extension core (`NsePushDecrypt.swift`) is compiled into this target.
final class NseAttachmentWordsTests: XCTestCase {

    override func tearDown() {
        nseNotificationWordLookup = { _ in nil }
        super.tearDown()
    }

    private func words(_ pairs: [(kind: String?, mime: String?)]) -> String? {
        nseAttachmentWords(nseAttachmentCounts(pairs))
    }

    func testCountsFollowTheListRow() {
        let counts = nseAttachmentCounts([
            (kind: nil, mime: "image/jpeg"), (kind: nil, mime: "IMAGE/PNG; q=1"), (kind: nil, mime: "video/mp4"),
            (kind: nil, mime: "application/pdf"), (kind: "voice", mime: "audio/mp4"), (kind: nil, mime: "audio/mpeg"),
        ])
        XCTAssertEqual(counts, NseAttachmentCounts(photos: 2, videos: 1, files: 1, other: 2))
    }

    func testEnglishWords() {
        XCTAssertEqual(words([(kind: nil, mime: "image/jpeg")]), "Photo")
        XCTAssertEqual(words([(kind: nil, mime: "image/jpeg"), (kind: nil, mime: "image/webp")]), "2 photos")
        XCTAssertEqual(words([(kind: nil, mime: "video/mp4")]), "Video")
        XCTAssertEqual(
            words([(kind: nil, mime: "video/mp4"), (kind: nil, mime: "image/gif"), (kind: nil, mime: "video/mp4")]),
            "3 photos and videos")
        XCTAssertEqual(words([(kind: nil, mime: nil)]), "File")
        XCTAssertEqual(words([(kind: nil, mime: "text/plain"), (kind: nil, mime: "text/plain")]), "2 files")
        XCTAssertNil(words([(kind: "gif", mime: "image/gif")]))
    }

    func testTextBesideFilesPhotosOrNothing() {
        // Files only, with text: the words, the separator, the text.
        XCTAssertEqual(nseTextWithFileWords("Weekend plans", NseAttachmentCounts(files: 2)), "2 files · Weekend plans")
        // Photos or videos, with text: the text alone.
        XCTAssertEqual(nseTextWithFileWords("Weekend plans", NseAttachmentCounts(photos: 1, files: 1)), "Weekend plans")
        XCTAssertEqual(nseTextWithFileWords("Weekend plans", NseAttachmentCounts(videos: 2)), "Weekend plans")
        // No text: the words alone.
        XCTAssertEqual(nseTextWithFileWords("", NseAttachmentCounts(files: 2)), "")
        XCTAssertEqual(nseAttachmentWords(NseAttachmentCounts(files: 2)), "2 files")
    }

    func testStoredLinesWin() {
        nseNotificationWordLookup = { $0 == "chat.media.photos#3" ? "3 fotos" : nil }
        XCTAssertEqual(
            words([(kind: nil, mime: "image/jpeg"), (kind: nil, mime: "image/jpeg"), (kind: nil, mime: "image/jpeg")]),
            "3 fotos")
    }

    func testANotificationWithoutTextSaysTheWords() {
        let row = NseProcessedRow(
            kind: .chat, text: "", attachmentCount: 2,
            attachmentCounts: NseAttachmentCounts(photos: 1, videos: 1))
        XCTAssertEqual(row.attachmentCounts.photos, 1)
        XCTAssertEqual(row.attributed(senderKeyB64: nil).attachmentCounts, row.attachmentCounts)
    }
}
