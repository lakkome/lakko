// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import ImageIO
import LakkoCore
import UniformTypeIdentifiers
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

    // MARK: - UX spec section 10: photos and videos

    func testImageOrMp4WithoutKindIsAPhotoOrVideoBubble() {
        let allow = fake(["image/jpeg": .image, "image/png": .image, "video/mp4": .video, "audio/mp4": .audio])
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: "image/jpeg", allowlist: allow).bubble, .photo)
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: "image/png", allowlist: allow).bubble, .photo)
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: "video/mp4", allowlist: allow).bubble, .video)
        // Audio without a kind stays a file; so does anything refused.
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: "audio/mp4", allowlist: allow).bubble, .file)
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: "image/heic", allowlist: allow).bubble, .file)
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: nil, allowlist: allow).bubble, .file)
        // An unknown kind is never guessed into a photo or video.
        XCTAssertEqual(InlineMedia.decision(kind: "sticker", mime: "image/png", allowlist: allow).bubble, .file)
        XCTAssertEqual(InlineMedia.decision(kind: "sticker", mime: "video/mp4", allowlist: allow).bubble, .file)
        // A known kind keeps its own shape.
        XCTAssertEqual(InlineMedia.decision(kind: "video_note", mime: "video/mp4", allowlist: allow).bubble, .videoNote)
    }

    func testCoreAllowlistPhotoAndVideo() {
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: "image/jpeg").bubble, .photo)
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: "image/webp").bubble, .photo)
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: "video/mp4").bubble, .video)
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: "image/svg+xml").bubble, .file)
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: "image/heic").bubble, .file)
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: "video/quicktime").bubble, .file)
    }

    func testBubbleSizeFitsTheBoxAndKeepsTheAspect() {
        // Landscape: full width, height by aspect.
        let landscape = InlineMedia.bubbleSize(width: 4000, height: 3000)
        XCTAssertEqual(landscape.width, 260, accuracy: 0.01)
        XCTAssertEqual(landscape.height, 195, accuracy: 0.01)
        // Tall portrait: height capped at 360, width follows.
        let portrait = InlineMedia.bubbleSize(width: 1000, height: 2000)
        XCTAssertEqual(portrait.height, 360, accuracy: 0.01)
        XCTAssertEqual(portrait.width, 180, accuracy: 0.01)
        // No dimensions: the thumb's aspect.
        let fromThumb = InlineMedia.bubbleSize(width: nil, height: nil, thumbWidth: 320, thumbHeight: 320)
        XCTAssertEqual(fromThumb.width, 260, accuracy: 0.01)
        XCTAssertEqual(fromThumb.height, 260, accuracy: 0.01)
        // Neither: 4:3.
        let placeholder = InlineMedia.bubbleSize(width: nil, height: nil)
        XCTAssertEqual(placeholder.width, 260, accuracy: 0.01)
        XCTAssertEqual(placeholder.height, 195, accuracy: 0.01)
        // A zero dimension is ignored, and a sliver stays tappable.
        XCTAssertEqual(InlineMedia.bubbleSize(width: 0, height: 10).height, 195, accuracy: 0.01)
        let sliver = InlineMedia.bubbleSize(width: 4096, height: 10)
        XCTAssertEqual(sliver.height, InlineMedia.mediaMinSide, accuracy: 0.01)
        XCTAssertLessThanOrEqual(sliver.width, 260)
    }

    func testWireDimensionsScaleDownToFit4096() {
        XCTAssertEqual(InlineMedia.wireDimensions(width: 1920, height: 1080)?.width, 1920)
        let big = InlineMedia.wireDimensions(width: 8192, height: 6144)
        XCTAssertEqual(big?.width, 4096)
        XCTAssertEqual(big?.height, 3072)
        let tall = InlineMedia.wireDimensions(width: 3024, height: 12096)
        XCTAssertEqual(tall?.height, 4096)
        XCTAssertEqual(tall?.width, 1024)
        XCTAssertNil(InlineMedia.wireDimensions(width: 0, height: 100))
    }

    // MARK: - Sender preparation

    private func encodedImage(type: UTType, width: Int, height: Int, orientation: Int = 1) -> Data? {
        guard let context = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
        ) else { return nil }
        context.setFillColor(red: 0.2, green: 0.4, blue: 0.8, alpha: 1)
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        guard let image = context.makeImage() else { return nil }
        let out = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(out, type.identifier as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(dest, image, [kCGImagePropertyOrientation: orientation] as CFDictionary)
        guard CGImageDestinationFinalize(dest) else { return nil }
        return out as Data
    }

    func testPreparePngKeepsBytesAndAddsSizeAndThumb() throws {
        let png = try XCTUnwrap(encodedImage(type: .png, width: 1200, height: 800))
        let prepared = try XCTUnwrap(PhotoVideoPrep.preparePhoto(png, stamp: "t"))
        XCTAssertEqual(prepared.mime, "image/png")
        XCTAssertEqual(prepared.bytes, png)
        XCTAssertEqual(prepared.width, 1200)
        XCTAssertEqual(prepared.height, 800)
        let thumb = try XCTUnwrap(prepared.thumb)
        XCTAssertLessThanOrEqual(thumb.count, PhotoVideoPrep.thumbTargetBytes)
        XCTAssertEqual(PhotoVideoPrep.sniffInlineImageMime(thumb), "image/jpeg")
        XCTAssertEqual(prepared.thumbWidth, 320)
        XCTAssertEqual(prepared.thumbHeight, 213)
        XCTAssertEqual(InlineMedia.decision(kind: nil, mime: prepared.mime).bubble, .photo)
    }

    func testPrepareHeicConvertsToJpegWithOrientationApplied() throws {
        guard let heic = encodedImage(type: .heic, width: 600, height: 400, orientation: 6) else {
            throw XCTSkip("no HEIC encoder on this runtime")
        }
        let prepared = try XCTUnwrap(PhotoVideoPrep.preparePhoto(heic, stamp: "t"))
        XCTAssertEqual(prepared.mime, "image/jpeg")
        XCTAssertTrue(prepared.name.hasSuffix(".jpg"))
        XCTAssertEqual(PhotoVideoPrep.sniffInlineImageMime(try XCTUnwrap(prepared.bytes)), "image/jpeg")
        // Orientation 6 rotates by 90 degrees: the sent size is portrait.
        XCTAssertEqual(prepared.width, 400)
        XCTAssertEqual(prepared.height, 600)
        XCTAssertNotNil(prepared.thumb)
    }

    func testPrepareRefusesBytesThatAreNotAnImage() {
        XCTAssertNil(PhotoVideoPrep.preparePhoto(Data("not an image at all".utf8), stamp: "t"))
    }
}
