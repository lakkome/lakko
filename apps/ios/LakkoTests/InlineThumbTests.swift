// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import ImageIO
import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0047 section 6 — the micro thumbnail a message's first photo or video
/// carries: the sender's size bound, which ref keeps it, and the chat list
/// row drawing it before the sent thumb.
@MainActor
final class InlineThumbTests: XCTestCase {

    private let jpeg = Data([0xFF, 0xD8, 0xFF, 0xE0]) + Data(repeating: 9, count: 100)

    private func ref(_ byte: UInt8, mime: String?, thumb: ThumbRef? = nil, inline: Data? = nil) -> AttachmentRef {
        AttachmentRef(
            blobId: Data(repeating: byte, count: 32), key: Data(repeating: 1, count: 32),
            nonce: Data(repeating: 2, count: 12), mime: mime, size: 1_000, name: nil, kind: nil,
            durationMs: nil, waveform: nil, width: 400, height: 300, thumb: thumb, inlineThumb: inline)
    }

    private func pictureImage(width: Int, height: Int) -> CGImage? {
        guard let context = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
        ) else { return nil }
        // A smooth picture with a few shapes, as a photo is.
        for x in 0..<width {
            let t = CGFloat(x) / CGFloat(width)
            context.setFillColor(red: t, green: 0.4, blue: 1 - t, alpha: 1)
            context.fill(CGRect(x: x, y: 0, width: 1, height: height))
        }
        context.setFillColor(red: 1, green: 1, blue: 0.2, alpha: 1)
        context.fillEllipse(in: CGRect(x: width / 4, y: height / 4, width: width / 3, height: height / 3))
        context.setFillColor(red: 0.1, green: 0.1, blue: 0.1, alpha: 1)
        context.fill(CGRect(x: width / 2, y: height / 2, width: width / 5, height: height / 6))
        return context.makeImage()
    }

    // MARK: - Sender size bound

    func testTheFirstQualityThatFitsIsTaken() {
        var asked: [Double] = []
        let out = InlineThumb.fit { quality in
            asked.append(quality)
            let size = quality > 0.5 ? 5_000 : 3_000
            return Data([0xFF, 0xD8, 0xFF]) + Data(repeating: 0, count: size - 3)
        }
        XCTAssertEqual(out?.count, 3_000)
        XCTAssertEqual(asked.count, 3, "stops at the first encoding within 4096 bytes")
    }

    func testNoPictureWhenNoQualityFits() {
        var asked = 0
        let out = InlineThumb.fit { _ in
            asked += 1
            return Data([0xFF, 0xD8, 0xFF]) + Data(repeating: 0, count: InlineThumb.maxBytes)
        }
        XCTAssertNil(out, "never fits: the message goes without one")
        XCTAssertEqual(asked, InlineThumb.qualities.count)
        XCTAssertNil(InlineThumb.fit { _ in Data([0x89, 0x50, 0x4E, 0x47]) }, "not a JPEG")
    }

    func testAMadeThumbIsAJpegWithinTheBoundAndAtMost96Pixels() throws {
        let image = try XCTUnwrap(pictureImage(width: 400, height: 300))
        let bytes = try XCTUnwrap(InlineThumb.make(from: image))
        XCTAssertLessThanOrEqual(bytes.count, InlineThumb.maxBytes)
        XCTAssertEqual([UInt8](bytes.prefix(3)), [0xFF, 0xD8, 0xFF])
        let source = try XCTUnwrap(CGImageSourceCreateWithData(bytes as CFData, nil))
        let decoded = try XCTUnwrap(CGImageSourceCreateImageAtIndex(source, 0, nil))
        XCTAssertEqual(max(decoded.width, decoded.height), InlineThumb.maxPixel)
        XCTAssertEqual(decoded.width, 96)
        XCTAssertEqual(decoded.height, 72)
    }

    func testBoundedRefusesOverLimitAndNonJpeg() {
        XCTAssertEqual(InlineThumb.bounded(jpeg), jpeg)
        let atLimit = Data([0xFF, 0xD8, 0xFF]) + Data(repeating: 1, count: InlineThumb.maxBytes - 3)
        XCTAssertEqual(InlineThumb.bounded(atLimit), atLimit)
        XCTAssertNil(InlineThumb.bounded(atLimit + Data([0])))
        XCTAssertNil(InlineThumb.bounded(Data([0x47, 0x49, 0x46, 0x38])))
        XCTAssertNil(InlineThumb.bounded(nil))
    }

    // MARK: - Which ref keeps it

    func testOnlyTheFirstPhotoOrVideoKeepsTheInlineThumb() {
        let refs = [
            ref(1, mime: "application/pdf", inline: jpeg),
            ref(2, mime: "image/jpeg", inline: jpeg),
            ref(3, mime: "video/mp4", inline: jpeg),
        ]
        let kept = ComposerTray.keepingInlineThumbOnFirstMedia(refs)
        XCTAssertEqual(kept.map(\.inlineThumb), [nil, jpeg, nil])
        XCTAssertEqual(kept.map(\.blobId), refs.map(\.blobId))
    }

    func testRemovingTheFirstPhotoHandsTheThumbToTheNext() {
        let first = ComposerTrayItem(kind: .photo, status: .ready, ref: ref(1, mime: "image/jpeg", inline: jpeg))
        let second = ComposerTrayItem(kind: .video, status: .ready, ref: ref(2, mime: "video/mp4", inline: jpeg))
        var tray = ComposerTray()
        tray.add([first, second])
        XCTAssertEqual(tray.refs.map(\.inlineThumb), [jpeg, nil])
        tray.remove(first.id)
        XCTAssertEqual(tray.refs.map(\.inlineThumb), [jpeg])
    }

    /// A JPEG picked through File and placed first is the carrier the core
    /// keeps: it gets a micro thumbnail of its own, and the photo after it
    /// carries none.
    func testAFilePickedJpegFirstCarriesTheThumbAndThePhotoAfterItNone() async throws {
        let image = try XCTUnwrap(pictureImage(width: 640, height: 480))
        let out = NSMutableData()
        let dest = try XCTUnwrap(CGImageDestinationCreateWithData(out, "public.jpeg" as CFString, 1, nil))
        CGImageDestinationAddImage(dest, image, nil)
        XCTAssertTrue(CGImageDestinationFinalize(dest))
        let fileBytes = out as Data

        let made = await ComposerTrayUpload.inlineThumb(forFile: fileBytes, mime: "image/jpeg")
        let fileThumb = try XCTUnwrap(made, "a File-picked JPEG gets a micro thumbnail")
        XCTAssertNotNil(InlineThumb.bounded(fileThumb))
        let none = await ComposerTrayUpload.inlineThumb(forFile: fileBytes, mime: "application/pdf")
        XCTAssertNil(none, "a file that is not an inline image or video gets none")

        let file = ComposerTrayItem(
            kind: .file, name: "photo.jpg", status: .ready, ref: ref(1, mime: "image/jpeg", inline: fileThumb))
        let photo = ComposerTrayItem(kind: .photo, status: .ready, ref: ref(2, mime: "image/jpeg", inline: jpeg))
        var tray = ComposerTray()
        tray.add([file, photo])
        XCTAssertEqual(tray.refs.map(\.inlineThumb), [fileThumb, nil])
    }

    // MARK: - Forwarding

    /// A forward re-encrypts the blob and keeps what rides in the message:
    /// title, artist and the micro thumbnail.
    func testAForwardKeepsTitleArtistAndTheInlineThumb() {
        var original = ref(1, mime: "audio/mpeg", inline: jpeg)
        original.title = "Song"
        original.artist = "Band"
        let uploaded = ref(9, mime: "audio/mpeg")
        let out = ForwardAttachments.carryingInlineFields(from: original, onto: uploaded)
        XCTAssertEqual(out.blobId, uploaded.blobId, "the new blob")
        XCTAssertEqual(out.key, uploaded.key)
        XCTAssertEqual(out.title, "Song")
        XCTAssertEqual(out.artist, "Band")
        XCTAssertEqual(out.inlineThumb, jpeg)

        let bad = ForwardAttachments.carryingInlineFields(
            from: ref(1, mime: "image/jpeg", inline: Data([1, 2, 3])), onto: uploaded)
        XCTAssertNil(bad.inlineThumb, "out-of-bound bytes are not carried")
    }

    // MARK: - Chat list row

    func testTheListRowPrefersTheInlineThumbAndFetchesOnlyWithoutIt() {
        let thumb = ThumbRef(
            blobId: Data(repeating: 5, count: 32), key: Data(repeating: 1, count: 32),
            nonce: Data(repeating: 2, count: 12), mime: "image/jpeg", size: 100, width: 32, height: 24)
        let inline = ChatListFormatting.previewMedia([ref(1, mime: "image/jpeg", thumb: thumb, inline: jpeg)])
        XCTAssertEqual(inline?.source, .inline(jpeg))

        let fetched = ChatListFormatting.previewMedia([ref(1, mime: "image/jpeg", thumb: thumb)])
        XCTAssertEqual(fetched?.source, .fetch(thumb))

        let bad = ChatListFormatting.previewMedia([ref(1, mime: "image/jpeg", thumb: thumb, inline: Data([1, 2, 3, 4]))])
        XCTAssertEqual(bad?.source, .fetch(thumb), "bytes that are not a JPEG are not drawn")

        XCTAssertEqual(ConversationPreviewMedia(thumb: nil, isVideo: false, count: 1).source, .none)
    }

    /// The stored message keeps the micro thumbnail, so the row draws it after
    /// a restart.
    func testThePersistedAttachmentKeepsTheInlineThumb() throws {
        let stored = PersistedAttachment(from: ref(1, mime: "image/jpeg", inline: jpeg))
        let json = try JSONEncoder().encode(stored)
        let back = try JSONDecoder().decode(PersistedAttachment.self, from: json)
        XCTAssertEqual(back.toAttachmentRef()?.inlineThumb, jpeg)
    }
}
