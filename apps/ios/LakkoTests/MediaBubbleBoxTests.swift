// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI
import UIKit
import XCTest

@testable import Lakko

/// UX spec sections 3 and 10 — the boxes the photo, video, GIF and round
/// video-note bubbles occupy, measured on the production views.
///
/// Why this file exists next to ``MediaBubbleLayoutTests``. That one pinned the
/// VOICE bubble's arrangement and nothing else; the other four bubbles had their
/// sizing rule written down in the spec, computed by a pure function
/// (``InlineMedia/bubbleSize(width:height:thumbWidth:thumbHeight:)``, tested in
/// ``InlineMediaTests``) — and NOTHING anywhere checked that the views actually
/// apply it. A bubble that ignores the rule and sizes itself to whatever
/// downloaded is the defect the rule exists to prevent, and it is invisible to a
/// test of the rule alone.
///
/// The division of labour is deliberate: `InlineMediaTests` owns the spec's
/// NUMBERS (260 x 360, the 4:3 fallback, the aspect), and this file asserts that
/// each view MEASURES what that function returns. Restating the numbers here
/// would give two places to change and one of them would be forgotten. The one
/// exception is the two caps, asserted absolutely as well, because a change that
/// broke both the function and this file in step would otherwise pass.
///
/// Two measurement techniques, both already load-bearing in
/// ``MediaBubbleLayoutTests``:
///
///   * ``UIHostingController/sizeThatFits(in:)`` for boxes. A detached hosting
///     controller lays the real view out without ever appearing, so the bubbles'
///     `.task` — which would download and decrypt — never runs. Every bubble
///     here is additionally given a file under its own media key, so that even
///     if a task DID run it resolves locally and no assertion below depends on
///     the network.
///   * `ImageRenderer` plus pixel reads for the two properties that have no
///     frame to measure: that the round video note is drawn ROUND rather than
///     square, and that the time pill lands ON the photo instead of under it.
@MainActor
final class MediaBubbleBoxTests: XCTestCase {

    /// Wide and tall enough that no bubble is squeezed. Finite rather than
    /// `.greatestFiniteMagnitude`, so a view that turned out to be greedy
    /// reports an obviously wrong number instead of an unreadable one.
    private let proposal = CGSize(width: 400, height: 10_000)

    private var controller: ChatMediaController!
    private var defaultsSuite: UserDefaults!

    override func setUp() {
        super.setUp()
        // A mounted bubble starts its own download. These tests measure the
        // box BEFORE any byte arrives, so the download is one that never
        // answers within the test and never opens a connection.
        MediaSessionCache.shared.fetchThumb = { _ in throw URLError(.notConnectedToInternet) }
        MediaSessionCache.shared.fetchAttachment = { _ in throw URLError(.notConnectedToInternet) }
        let suite = "me.lakko.tests.media-bubble-box." + UUID().uuidString
        defaultsSuite = UserDefaults(suiteName: suite)
        controller = ChatMediaController(
            port: .init(sendAttachment: { _, _ in }, sessionToken: { nil }),
            prefs: MediaPrefs(defaults: defaultsSuite)
        )
    }

    override func tearDown() {
        controller.temp.cleanup()
        controller = nil
        defaultsSuite = nil
        super.tearDown()
    }

    // MARK: - Photo

    /// Section 10 — *"The bubble has a fixed size from `width` / `height`
    /// before anything is downloaded"*, and that size is the spec's box.
    ///
    /// The three cases are the three branches of the rule: the width cap binds
    /// on a landscape photo, the height cap binds on a portrait one, and an
    /// attachment carrying no dimensions at all falls back to 4:3. All three are
    /// measured on a bubble that has downloaded nothing, which is the part of
    /// the sentence that matters — a bubble sized only once its bytes arrive
    /// reflows the timeline under a reader's thumb.
    func testPhotoBubbleDrawsTheSpecSizedBoxBeforeAnyByteArrives() throws {
        try assertTheMeasurementTechniqueMeasures()

        // 2:1 and 1:2, rather than a real camera's 16:9 and 9:16, for one
        // reason: their boxes are whole points (130 and 180), so nothing below
        // rests on how `sizeThatFits` rounds a fraction of a point.
        let wide = measure(PhotoBubble(ref: photoRef(width: 1600, height: 800)))
        assertBox(wide, isTheRuleFor: (1600, 800), "a landscape photo")
        XCTAssertEqual(
            wide.width, 260, accuracy: 1,
            "section 10's width cap is 260 pt, and a landscape photo is what makes it bind")

        let tall = measure(PhotoBubble(ref: photoRef(width: 900, height: 1800)))
        assertBox(tall, isTheRuleFor: (900, 1800), "a portrait photo")
        XCTAssertEqual(
            tall.height, 360, accuracy: 1,
            "section 10's height cap is 360 pt, and a portrait photo is what makes it bind")
        XCTAssertLessThan(
            tall.width, 260,
            "a photo capped by its height must give width back, or the aspect is not preserved")

        // No `width`, no `height`, no thumb: the 4:3 placeholder.
        let bare = measure(PhotoBubble(ref: photoRef(width: nil, height: nil)))
        assertBox(bare, isTheRuleFor: (nil, nil), "a photo with no dimensions")
        XCTAssertGreaterThan(bare.height, 0, "the placeholder box measured as nothing")
    }

    /// Section 10 — *"Time, ticks and edited marker sit on the media in a small
    /// translucent pill, bottom right."*
    ///
    /// ON the media, which is the whole difference from the voice bubble's
    /// arrangement, where the time sits BESIDE the content and is part of the
    /// row. Two things follow, and both are asserted: the pill cannot change the
    /// bubble's box, and its ink has to land in the bottom-right of that box.
    ///
    /// The second half is a pixel read because an overlay has no frame of its
    /// own to interrogate. The marker is pure green, which nothing in the
    /// palette is in either appearance (``Color/lakkoSurfaceSunken`` is a near
    /// white or a dark slate), so a green pixel is the marker and nothing else.
    func testTheTimePillSitsOnThePhotoAndDoesNotGrowItsBox() throws {
        let ref = photoRef(width: 1600, height: 800)
        let bare = measure(PhotoBubble(ref: ref))
        let withPill = measure(PhotoBubble(ref: ref, overlay: AnyView(pillMarker)))

        XCTAssertGreaterThan(bare.height, 0, "the bubble measured as nothing, so equality below says nothing")
        XCTAssertEqual(
            withPill.width, bare.width, accuracy: 0.5,
            "the time pill widened the photo bubble, so it is beside the media and not on it")
        XCTAssertEqual(
            withPill.height, bare.height, accuracy: 0.5,
            "the time pill grew the photo bubble, so it is under the media and not on it")

        try assertTheRenderTechniqueDraws()
        let bitmap = try render(PhotoBubble(ref: ref, overlay: AnyView(pillMarker)))
        XCTAssertGreaterThan(bitmap.width, 2)
        XCTAssertGreaterThan(bitmap.height, 2)
        XCTAssertTrue(
            bitmap.containsMarker(inXRange: bitmap.width / 2..<bitmap.width, yRange: bitmap.height / 2..<bitmap.height),
            "nothing was drawn in the bottom-right of the photo, so the pill is not where section 10 puts it")
        XCTAssertFalse(
            bitmap.containsMarker(inXRange: 0..<bitmap.width / 2, yRange: 0..<bitmap.height / 2),
            "the pill was drawn in the top-left as well, so it is not one pill in one corner")
    }

    // MARK: - Video

    /// Section 10's *"same sizing box"*: a video and a photo of the same pixel
    /// size occupy the same bubble.
    ///
    /// Worth its own test rather than folded into the photo's, because the two
    /// views compute it separately (each calls `bubbleSize` from its own
    /// `size` property) and a change to one is exactly the drift that would put
    /// a differently-shaped hole in the timeline for the same picture.
    ///
    /// The file-size pill and the centre play button of a video are overlays and
    /// a fixed-size ZStack child, so neither may change the box either — which
    /// is what the equality also proves.
    func testVideoBubbleUsesTheSameBoxAsAPhoto() throws {
        let photo = measure(PhotoBubble(ref: photoRef(width: 1920, height: 960)))
        let video = measure(VideoBubble(ref: videoRef(width: 1920, height: 960)))

        assertBox(video, isTheRuleFor: (1920, 960), "a video")
        XCTAssertGreaterThan(photo.height, 0, "the photo measured as nothing, so equality below says nothing")
        XCTAssertEqual(video.width, photo.width, accuracy: 0.5, "a video is a different width from the same-sized photo")
        XCTAssertEqual(video.height, photo.height, accuracy: 0.5, "a video is a different height from the same-sized photo")
    }

    // MARK: - Round video note

    /// Section 3 (amended 2026-11-22) — *"A **circle** (about 240 pt), no
    /// bubble chrome"*, the duration and the time on pills INSIDE it, `Aa`
    /// beside it 6 pt away with its space reserved in every state, and nothing
    /// on a line under it.
    ///
    /// All the ways of breaking it fail here: a circle sized to something
    /// other than 240, a row put back under the circle, `Aa` moved off the
    /// side or its space no longer reserved, and padding added around the
    /// circle (which "no bubble chrome" forbids).
    func testVideoNoteIsItsCircleWithAaBesideItAndNothingBeneath() throws {
        try assertTheMeasurementTechniqueMeasures()

        for incoming in [false, true] {
            let size = measure(videoNoteBubble(incoming: incoming))
            let expectedWidth = VideoNoteBubble.diameter + VideoNoteLayout.asideGap + TranscriptButton.size.width
            XCTAssertEqual(
                size.width, expectedWidth, accuracy: 0.5,
                """
                a video note is no longer its \(VideoNoteBubble.diameter) pt circle with Aa beside it \
                (incoming \(incoming), measured \(size.width), expected \(expectedWidth))
                """)
            XCTAssertEqual(
                size.height, VideoNoteBubble.diameter, accuracy: 0.5,
                """
                a video note is no longer its circle alone (incoming \(incoming), measured \
                \(size.height)), so the circle is not \(VideoNoteBubble.diameter) pt or a line \
                was put back under it
                """)
        }
    }

    /// Section 3 — the bubble is *a circle*, and the state table's *"a neutral
    /// circle with a play glyph — never a broken-image icon"* for a note whose
    /// poster is missing, which is the state measured here (no thumb, nothing
    /// downloaded).
    ///
    /// Roundness has no frame to read: the shape is a filled `Circle` and a
    /// `clipShape(Circle())`, so the only way to see it is to look at the
    /// pixels. The production view is rendered over a pure-blue backdrop and
    /// eight points of its 240 x 240 media square are read — the four corners,
    /// which a circle leaves alone, and the centre plus four points just inside
    /// the edge midpoints, which it fills.
    ///
    /// The corners are what discriminate: 6 pt in from a corner is INSIDE a
    /// rounded rectangle of the bubble radius and OUTSIDE a circle, so a note
    /// that regressed to the photo bubble's rounded box fails here rather than
    /// passing on "something was drawn".
    func testTheVideoNoteMediaIsRoundAndNotASquare() throws {
        try assertTheRenderTechniqueDraws()

        // `incoming: true` for one reason only: its VStack is LEADING-aligned,
        // so the circle starts at the render's origin and the coordinates below
        // are the circle's own, whatever the row beneath it turns out to be.
        let bitmap = try render(videoNoteBubble(incoming: true).background(backdrop))
        let side = Int(VideoNoteBubble.diameter)

        XCTAssertGreaterThanOrEqual(
            bitmap.width, side, "the render is narrower than the circle, so the reads below are off the image")
        XCTAssertGreaterThanOrEqual(
            bitmap.height, side, "the render is shorter than the circle, so the reads below are off the image")

        for (x, y) in [(6, 6), (side - 7, 6), (6, side - 7), (side - 7, side - 7)] {
            XCTAssertTrue(
                bitmap.isBackdrop(x, y),
                "the corner (\(x), \(y)) of the media square is painted, so the video note is a box and not a circle")
        }
        // The centre, and four points 4 pt inside the edge midpoints — all well
        // within a circle inscribed in the square.
        let mid = side / 2
        for (x, y) in [(mid, mid), (mid, 4), (4, mid), (mid, side - 5), (side - 5, mid)] {
            XCTAssertFalse(
                bitmap.isBackdrop(x, y),
                "(\(x), \(y)) is inside the circle and nothing was drawn there, so no circle was drawn at all")
        }
    }

    // MARK: - GIF

    /// Section 5 — a GIF is *"sized to its aspect ratio within the bubble
    /// width"*, with the clamps the view carries.
    ///
    /// Three cases, because the three branches fail differently: an ordinary
    /// landscape GIF keeps its aspect, a very tall one is cut off at the
    /// ceiling rather than running down the whole timeline, and a sliver is
    /// floored so it stays tappable. A renderer that ignored the aspect
    /// entirely would satisfy any one of the three on its own.
    func testGifBubbleFitsItsAspectIntoTheBubbleWidthAndClampsTheExtremes() throws {
        try assertTheMeasurementTechniqueMeasures()

        // 2:1 — the plain case: half the bubble width.
        let label = GifBubble.lineSpacing + measure(GifLabel()).height
        let landscape = measure(gifBubble(width: 400, height: 200))
        XCTAssertEqual(
            landscape.width, GifBubble.maxWidth, accuracy: 0.5,
            "a GIF is no longer \(GifBubble.maxWidth) pt wide (measured \(landscape.width))")
        XCTAssertEqual(
            landscape.height - label, GifBubble.maxWidth / 2, accuracy: 0.5,
            "a 2:1 GIF did not keep its aspect inside the bubble width (measured \(landscape.height))")

        // 1:4 — taller than the ceiling.
        let tall = measure(gifBubble(width: 400, height: 1600))
        XCTAssertEqual(
            tall.height - label, 320, accuracy: 0.5,
            "a very tall GIF was not clamped to the 320 pt ceiling (measured \(tall.height))")

        // 40:1 — a sliver, which must stay big enough to tap.
        let sliver = measure(gifBubble(width: 4000, height: 100))
        XCTAssertEqual(
            sliver.height - label, 80, accuracy: 0.5,
            "a sliver GIF was not floored at 80 pt (measured \(sliver.height))")
    }

    // MARK: - In a column (`docs/ux/feed.md` section 3)

    /// Nothing inside a feed item may be cut by its edge, and a phone with
    /// Display Zoom leaves the column narrower than a chat bubble. The chat's
    /// boxes, asserted above, are what a bubble takes with no column.
    func testABubbleTakesTheColumnItSitsIn() throws {
        try assertTheMeasurementTechniqueMeasures()
        let column: CGFloat = 212

        let photo = measure(
            PhotoBubble(ref: photoRef(width: 1600, height: 800)).environment(\.mediaColumnWidth, column))
        XCTAssertEqual(photo.width, column, accuracy: 1)
        XCTAssertEqual(photo.height, column / 2, accuracy: 1, "the photo lost its aspect")

        let video = measure(
            VideoBubble(ref: videoRef(width: 1600, height: 800)).environment(\.mediaColumnWidth, column))
        XCTAssertEqual(video.width, column, accuracy: 1)

        let gif = measure(gifBubble(width: 400, height: 200).environment(\.mediaColumnWidth, column))
        XCTAssertEqual(gif.width, column, accuracy: 1)
        let label = GifBubble.lineSpacing + measure(GifLabel()).height
        XCTAssertEqual(gif.height - label, column / 2, accuracy: 1, "the GIF lost its aspect")

        let note = measure(videoNoteBubble(incoming: true).environment(\.mediaColumnWidth, column))
        XCTAssertEqual(note.width, column, accuracy: 1)
    }

    /// The feed brings the chat's fill along with a voice message, and the
    /// bubble with its fill still has to fit the column.
    func testAVoiceMessageWithItsFillFitsTheColumn() throws {
        try assertTheMeasurementTechniqueMeasures()
        let ref = AttachmentRef(
            blobId: Data(repeating: 0xA1, count: 32),
            key: Data(repeating: 0xA2, count: 32),
            nonce: Data(repeating: 0xA3, count: 12),
            mime: "audio/mp4",
            size: 40_000,
            name: "voice.m4a",
            kind: "voice",
            durationMs: 12_000
        )
        for column: CGFloat in [212, 282] {
            let voice = measure(FeedMediaBubble(ref: ref, incoming: false).environment(\.mediaColumnWidth, column))
            XCTAssertGreaterThan(voice.width, 0)
            XCTAssertLessThanOrEqual(voice.width, column, "the voice bubble overflows a \(column) pt column")
        }
    }

    // MARK: - Harness

    /// The spec's box for a pixel size, from the one function that owns it.
    private func assertBox(_ measured: CGSize, isTheRuleFor pixels: (UInt32?, UInt32?), _ what: String) {
        let box = InlineMedia.bubbleSize(width: pixels.0, height: pixels.1)
        // A point of tolerance, well under what this is looking for: getting the
        // box wrong costs tens of points, and every case fed to this helper has
        // a whole-point box anyway.
        XCTAssertEqual(
            measured.width, box.width, accuracy: 1,
            "\(what) is \(measured.width) pt wide where the sizing rule says \(box.width)")
        XCTAssertEqual(
            measured.height, box.height, accuracy: 1,
            "\(what) is \(measured.height) pt tall where the sizing rule says \(box.height)")
    }

    /// A received photo, with nothing downloaded and no thumb.
    private func photoRef(width: UInt32?, height: UInt32?) -> AttachmentRef {
        AttachmentRef(
            blobId: Data(repeating: 0xC1, count: 32),
            key: Data(repeating: 0xC2, count: 32),
            nonce: Data(repeating: 0xC3, count: 12),
            mime: "image/jpeg",
            size: 120_000,
            name: "photo.jpg",
            width: width,
            height: height
        )
    }

    private func videoRef(width: UInt32?, height: UInt32?) -> AttachmentRef {
        AttachmentRef(
            blobId: Data(repeating: 0xD1, count: 32),
            key: Data(repeating: 0xD2, count: 32),
            nonce: Data(repeating: 0xD3, count: 12),
            mime: "video/mp4",
            size: 3_400_000,
            name: "clip.mp4",
            width: width,
            height: height
        )
    }

    /// A received video note. No `thumb`, so the poster branch has nothing to
    /// fetch, and a file is filed under its key so any load resolves locally.
    private func videoNoteBubble(incoming: Bool) -> VideoNoteBubble {
        let ref = AttachmentRef(
            blobId: Data(repeating: 0xE1, count: 32),
            key: Data(repeating: 0xE2, count: 32),
            nonce: Data(repeating: 0xE3, count: 12),
            mime: "video/mp4",
            size: 900_000,
            name: "note.mp4",
            kind: "video_note",
            durationMs: 14_000
        )
        let key = ChatMediaController.key(for: ref)
        _ = try? controller.temp.write(Data([0]), key: key, name: "note.mp4", fallback: "note.mp4")
        return VideoNoteBubble(ref: ref, mediaKey: key, incoming: incoming, upload: nil, onRetry: {})
    }

    private func gifBubble(width: UInt32, height: UInt32) -> GifBubble {
        let ref = AttachmentRef(
            blobId: Data(repeating: UInt8(truncatingIfNeeded: width), count: 32),
            key: Data(repeating: 0xF2, count: 32),
            nonce: Data(repeating: 0xF3, count: 12),
            mime: "image/gif",
            size: 400_000,
            name: "reaction.gif",
            kind: "gif",
            width: width,
            height: height
        )
        let key = ChatMediaController.key(for: ref)
        _ = try? controller.temp.write(Data([0]), key: key, name: "reaction.gif", fallback: "reaction.gif")
        return GifBubble(ref: ref, mediaKey: key, upload: nil, localReady: true, onRetry: {})
    }

    /// Stand-in for the time-and-ticks pill: pure green, which nothing in the
    /// palette is, so its ink is unmistakable in a render.
    private var pillMarker: some View {
        Color(red: 0, green: 1, blue: 0).frame(width: 30, height: 12)
    }

    /// Pure blue, for the same reason.
    private var backdrop: Color { Color(red: 0, green: 0, blue: 1) }

    /// Lay a production bubble out and report the size it asks for.
    private func measure<V: View>(_ view: V) -> CGSize {
        let host = UIHostingController(
            rootView: view
                .environmentObject(controller!)
                .environmentObject(controller!.live)
        )
        return host.sizeThatFits(in: proposal)
    }

    /// Guard against the whole file measuring nothing: if `sizeThatFits` itself
    /// were adding to or ignoring what it is asked to measure, every assertion
    /// above would be arithmetic about a number that means nothing. This fails
    /// first and says so.
    private func assertTheMeasurementTechniqueMeasures() throws {
        let probe = UIHostingController(rootView: Color.clear.frame(width: 123, height: 47))
        let size = probe.sizeThatFits(in: proposal)
        XCTAssertEqual(size.width, 123, accuracy: 0.5, "the measurement technique is off; nothing below means what it says")
        XCTAssertEqual(size.height, 47, accuracy: 0.5, "the measurement technique is off; nothing below means what it says")
    }

    /// The same guard for the pixel technique: a render that produced nothing,
    /// or produced it in a colour space these reads cannot understand, fails
    /// here and says so rather than looking like a view that drew no circle.
    private func assertTheRenderTechniqueDraws() throws {
        let bitmap = try render(backdrop.frame(width: 10, height: 10))
        XCTAssertGreaterThanOrEqual(bitmap.width, 10)
        XCTAssertTrue(
            bitmap.isBackdrop(5, 5),
            "ImageRenderer did not draw a plain colour where one was asked for; nothing rendered below means what it says")
    }

    /// Render a production bubble at scale 1, so a pixel is a point and the
    /// geometry in the assertions is the geometry in the view.
    private func render<V: View>(_ view: V) throws -> Bitmap {
        let renderer = ImageRenderer(
            content: view
                .environmentObject(controller!)
                .environmentObject(controller!.live)
        )
        renderer.scale = 1
        renderer.isOpaque = true
        let image = try XCTUnwrap(renderer.uiImage, "ImageRenderer produced no image")
        let cgImage = try XCTUnwrap(image.cgImage, "the rendered image has no bitmap")
        return try Bitmap(cgImage)
    }

    /// The pixels of a render, and the two questions asked of them.
    private struct Bitmap {
        let width: Int
        let height: Int
        private let bytes: [UInt8]

        init(_ image: CGImage) throws {
            width = image.width
            height = image.height
            var buffer = [UInt8](repeating: 0, count: max(1, width * height * 4))
            let context = try XCTUnwrap(
                CGContext(
                    data: &buffer,
                    width: width,
                    height: height,
                    bitsPerComponent: 8,
                    bytesPerRow: width * 4,
                    space: CGColorSpaceCreateDeviceRGB(),
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
                ),
                "could not open a bitmap context over the render"
            )
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            bytes = buffer
        }

        private func rgb(_ x: Int, _ y: Int) -> (r: Int, g: Int, b: Int) {
            let offset = (y * width + x) * 4
            return (Int(bytes[offset]), Int(bytes[offset + 1]), Int(bytes[offset + 2]))
        }

        /// The pure-blue backdrop, within a tolerance that leaves every colour
        /// in the palette — a near-white in light appearance, a dark slate in
        /// dark — comfortably outside it.
        func isBackdrop(_ x: Int, _ y: Int) -> Bool {
            guard x >= 0, y >= 0, x < width, y < height else { return false }
            let pixel = rgb(x, y)
            return pixel.r < 40 && pixel.g < 40 && pixel.b > 215
        }

        /// Pure green, which is the pill marker and nothing else.
        func containsMarker(inXRange xs: Range<Int>, yRange ys: Range<Int>) -> Bool {
            for y in ys where y >= 0 && y < height {
                for x in xs where x >= 0 && x < width {
                    let pixel = rgb(x, y)
                    if pixel.g > 180, pixel.r < 90, pixel.b < 90 { return true }
                }
            }
            return false
        }
    }
}
