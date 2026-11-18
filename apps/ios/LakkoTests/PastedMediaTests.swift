// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import UIKit
import XCTest
@testable import Lakko

/// `docs/ux/attachments.md` section 2.5 — what a paste in the composer holds.
final class PastedMediaTests: XCTestCase {

    func testACopiedPhotoAttaches() {
        XCTAssertTrue(PastedMedia.attachesAsMedia(itemTypes: [["public.jpeg"]]))
        XCTAssertTrue(PastedMedia.attachesAsMedia(itemTypes: [["public.heic", "public.jpeg"]]))
        XCTAssertTrue(PastedMedia.attachesAsMedia(itemTypes: [["public.png"]]))
    }

    func testACopiedVideoAttaches() {
        XCTAssertTrue(PastedMedia.attachesAsMedia(itemTypes: [["com.apple.quicktime-movie"]]))
        XCTAssertTrue(PastedMedia.attachesAsMedia(itemTypes: [["public.mpeg-4"]]))
    }

    func testAUIKitImageObjectAttaches() {
        XCTAssertTrue(PastedMedia.attachesAsMedia(itemTypes: [[PastedMedia.uiKitImageType]]))
    }

    func testSeveralPhotosAttach() {
        XCTAssertTrue(PastedMedia.attachesAsMedia(itemTypes: [["public.jpeg"], ["public.png"]]))
    }

    func testAnImageWithItsLinkAttaches() {
        // A browser copies an image with the address it came from.
        XCTAssertTrue(PastedMedia.attachesAsMedia(itemTypes: [["public.png", "public.url"]]))
    }

    func testPlainTextPastesAsText() {
        XCTAssertFalse(PastedMedia.attachesAsMedia(itemTypes: [["public.utf8-plain-text"]]))
    }

    func testADocumentWithAPicturePastesAsText() {
        XCTAssertFalse(PastedMedia.attachesAsMedia(itemTypes: [["public.html", "public.utf8-plain-text", "public.png"]]))
        XCTAssertFalse(PastedMedia.attachesAsMedia(itemTypes: [["public.rtf", "public.jpeg"]]))
        // Text in any item keeps the whole paste text.
        XCTAssertFalse(PastedMedia.attachesAsMedia(itemTypes: [["public.jpeg"], ["public.utf8-plain-text"]]))
    }

    func testNothingOrSomethingElseIsLeftToTheField() {
        XCTAssertFalse(PastedMedia.attachesAsMedia(itemTypes: []))
        XCTAssertFalse(PastedMedia.attachesAsMedia(itemTypes: [[]]))
        XCTAssertFalse(PastedMedia.attachesAsMedia(itemTypes: [["public.url"]]))
        XCTAssertFalse(PastedMedia.attachesAsMedia(itemTypes: [["com.adobe.pdf"]]))
        XCTAssertFalse(PastedMedia.attachesAsMedia(itemTypes: [["not a type at all"]]))
    }

    func testAnItemIsAPhotoUnderItsMostFaithfulImageType() {
        XCTAssertEqual(
            PastedMedia.itemKind(registered: ["public.heic", "public.jpeg"]),
            .photo(typeIdentifier: "public.heic"))
        XCTAssertEqual(
            PastedMedia.itemKind(registered: ["public.url", "public.png"]),
            .photo(typeIdentifier: "public.png"))
    }

    func testAPhotoWinsOverAVideoInOneItem() {
        // A Live Photo copies as both; the photo picker sends the still.
        XCTAssertEqual(
            PastedMedia.itemKind(registered: ["com.apple.quicktime-movie", "public.jpeg"]),
            .photo(typeIdentifier: "public.jpeg"))
    }

    func testAVideoItem() {
        XCTAssertEqual(
            PastedMedia.itemKind(registered: ["com.apple.quicktime-movie"]),
            .video(typeIdentifier: "com.apple.quicktime-movie"))
        XCTAssertEqual(
            PastedMedia.itemKind(registered: ["public.mpeg-4"]),
            .video(typeIdentifier: "public.mpeg-4"))
    }

    func testAUIKitImageObjectItem() {
        XCTAssertEqual(PastedMedia.itemKind(registered: [PastedMedia.uiKitImageType]), .uiImage)
    }

    func testAnItemOfAnotherKindIsSkipped() {
        XCTAssertNil(PastedMedia.itemKind(registered: ["public.utf8-plain-text"]))
        XCTAssertNil(PastedMedia.itemKind(registered: ["com.adobe.pdf"]))
        XCTAssertNil(PastedMedia.itemKind(registered: []))
    }

    func testLoadingReadsPhotosAndSkipsOtherItems() async {
        let png = UIGraphicsImageRenderer(size: CGSize(width: 2, height: 2)).pngData { context in
            UIColor.red.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 2, height: 2))
        }
        let photo = NSItemProvider(item: png as NSData, typeIdentifier: "public.png")
        let text = NSItemProvider(object: "hello" as NSString)
        let loaded = await PastedMedia.load([text, photo])
        XCTAssertEqual(loaded.count, 1)
        guard case .photo(let data)? = loaded.first else { return XCTFail("expected a photo") }
        XCTAssertEqual(data, png)
    }

    func testLoadingStopsAtTheLimit() async {
        let png = UIGraphicsImageRenderer(size: CGSize(width: 1, height: 1)).pngData { _ in }
        let providers = (0..<(PastedMedia.maxItems + 3)).map { _ in
            NSItemProvider(item: png as NSData, typeIdentifier: "public.png")
        }
        let loaded = await PastedMedia.load(providers)
        XCTAssertEqual(loaded.count, PastedMedia.maxItems)
    }
}
