// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0047 §1 — `kind` picks the bubble shape, the MIME allowlist decides
/// whether the bytes are played at all. Both must agree.
final class InlineMediaTests: XCTestCase {

    /// A fake that allowlists exactly what the test names.
    private func fake(_ table: [String: InlineMediaType]) -> InlineMedia.Allowlist {
        { table[$0] }
    }

    func testKindAndMatchingClassProduceMediaBubbles() {
        let allow = fake(["audio/mp4": .audio, "video/mp4": .video, "image/gif": .image])
        XCTAssertEqual(InlineMedia.decision(kind: "voice", mime: "audio/mp4", allowlist: allow).bubble, .voice)
        XCTAssertEqual(InlineMedia.decision(kind: "video_note", mime: "video/mp4", allowlist: allow).bubble, .videoNote)
        XCTAssertEqual(InlineMedia.decision(kind: "gif", mime: "image/gif", allowlist: allow).bubble, .gif)
        XCTAssertEqual(InlineMedia.decision(kind: "gif", mime: "video/mp4", allowlist: allow).bubble, .gif)
    }

    func testKindAloneNeverMakesMedia() {
        let allow = fake(["audio/mp4": .audio])
        // A sender-chosen kind with a refused or mismatched MIME is a file.
        XCTAssertEqual(InlineMedia.decision(kind: "voice", mime: "text/html", allowlist: allow).bubble, .file)
        XCTAssertEqual(InlineMedia.decision(kind: "voice", mime: nil, allowlist: allow).bubble, .file)
        XCTAssertEqual(InlineMedia.decision(kind: "voice", mime: "", allowlist: allow).bubble, .file)
        XCTAssertEqual(InlineMedia.decision(kind: "video_note", mime: "audio/mp4", allowlist: allow).bubble, .file)
        XCTAssertEqual(InlineMedia.decision(kind: "sticker", mime: "audio/mp4", allowlist: allow).bubble, .file)
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: "audio/mp4", allowlist: allow).bubble, .file)
    }

    func testEmptyMimeNeverReachesTheAllowlist() {
        var asked = false
        let spy: InlineMedia.Allowlist = { _ in
            asked = true
            return .image
        }
        XCTAssertNil(InlineMedia.inlineType(mime: "", allowlist: spy))
        XCTAssertNil(InlineMedia.inlineType(mime: nil, allowlist: spy))
        XCTAssertFalse(asked)
    }

    /// The real core allowlist over the FFI: parameters ignored, SVG never.
    func testCoreAllowlist() {
        XCTAssertEqual(InlineMedia.inlineType(mime: "audio/mp4"), .audio)
        XCTAssertEqual(InlineMedia.inlineType(mime: "VIDEO/MP4; codecs=avc1"), .video)
        XCTAssertEqual(InlineMedia.inlineType(mime: "image/webp"), .image)
        XCTAssertNil(InlineMedia.inlineType(mime: "image/svg+xml"))
        XCTAssertNil(InlineMedia.inlineType(mime: "text/html"))
        XCTAssertEqual(InlineMedia.decision(kind: "voice", mime: "text/html").bubble, .file)
        XCTAssertEqual(InlineMedia.decision(kind: "voice", mime: "audio/mp4").bubble, .voice)
    }

    func testImagePreviewGateUsesTheAllowlist() {
        func ref(_ mime: String?) -> AttachmentRef {
            AttachmentRef(blobId: Data(count: 32), key: Data(count: 32), nonce: Data(count: 12), mime: mime, size: 1, name: nil)
        }
        XCTAssertTrue(AttachmentBridge.isInlinePreviewable(ref("image/png")))
        XCTAssertFalse(AttachmentBridge.isInlinePreviewable(ref("image/svg+xml")))
        XCTAssertFalse(AttachmentBridge.isInlinePreviewable(ref("audio/mp4")))
        XCTAssertFalse(AttachmentBridge.isInlinePreviewable(ref(nil)))
    }
}
