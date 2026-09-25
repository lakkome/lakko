// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI
import UIKit
import XCTest

@testable import Lakko

/// ADR-0047 — the first tests in this target that measure real SwiftUI
/// LAYOUT rather than a pure function.
///
/// Why they exist: the arrangement of the voice bubble is a CROSS-PLATFORM
/// rule, and until now nothing on any client pinned it. The maintainer kept
/// reporting the same divergence — "the duration and the message time are on
/// one visual level on iOS and on different ones on Android" — which means iOS
/// happens to be the reference implementation, by luck rather than by
/// contract. A rule nobody measures is a rule that drifts the next time
/// somebody adds a row to the bubble.
///
/// These tests measure the PRODUCTION views. Two measurement techniques, both
/// chosen because they need no private view introspection and no simulator
/// gesture:
///
///   * ``UIHostingController/sizeThatFits(in:)`` for heights. A detached
///     hosting controller lays the real view out without ever appearing, so
///     the bubble's `.task` (which would try to download and decrypt the
///     audio) never runs.
///   * `ImageRenderer` for the waveform, whose bars are drawn shapes with no
///     addressable frame of their own. Rendering and counting dark pixels per
///     column measures what a person actually sees. It asserts a STRUCTURAL
///     property (the bars differ in height) rather than matching a golden
///     image, so it does not become a snapshot test that breaks on every OS
///     bump.
///
/// The three geometry numbers below are duplicated from the view because
/// `VoiceBubble` hardcodes them inline; there is no constant to import. That
/// duplication is deliberate and load-bearing: changing either side makes
/// these tests fail, which is exactly the notification a layout rule needs.
@MainActor
final class MediaBubbleLayoutTests: XCTestCase {

    /// `MediaBubbles.swift` — `waveform.frame(height: 26)`.
    private let waveformHeight: CGFloat = 26
    /// `MediaBubbles.swift` — the inner `VStack(alignment: .leading, spacing: 4)`
    /// between the waveform and the duration row.
    private let innerSpacing: CGFloat = 4
    /// `MediaBubbles.swift` — the play button's `.frame(width: 38, height: 38)`.
    private let playButtonHeight: CGFloat = 38

    /// Wide enough that nothing wraps (the bubble is 38 + 10 + 170 pt) and
    /// tall enough that nothing is squeezed. A finite height rather than
    /// `.greatestFiniteMagnitude`, so a view that turned out to be
    /// height-greedy would report an obviously wrong 10 000 instead of
    /// something unreadable.
    private let proposal = CGSize(width: 320, height: 10_000)

    private var controller: ChatMediaController!
    private var defaultsSuite: UserDefaults!

    override func setUp() {
        super.setUp()
        // An isolated defaults suite so the played-marker set and the record
        // mode of the real app are never touched by a test run.
        let suite = "me.lakko.tests.media-bubble-layout." + UUID().uuidString
        defaultsSuite = UserDefaults(suiteName: suite)
        controller = ChatMediaController(
            port: .init(sendAttachment: { _ in }, sessionToken: { nil }),
            prefs: MediaPrefs(defaults: defaultsSuite)
        )
    }

    override func tearDown() {
        controller.temp.cleanup()
        controller = nil
        defaultsSuite = nil
        super.tearDown()
    }

    // MARK: - The bubble must not resize when playback starts

    /// The rule the other two clients were breaking: pressing play must not
    /// change the bubble's height. A bubble that grows on play shoves every
    /// row below it down mid-scroll, and the growth is invisible in review
    /// because the playing state only exists while a finger is on the button.
    ///
    /// The playing state is not a cosmetic difference: the duration label
    /// swaps from the total to the running position, and a playback-rate
    /// button appears in the same row. Both are `caption2`, so the row's
    /// height is unchanged — and that is the thing being pinned, because a
    /// future rate control at a larger size would silently break it.
    func testVoiceBubbleHeightIsTheSameIdleAndPlaying() throws {
        let idle = measureVoiceBubble(playing: false)
        let playing = measureVoiceBubble(playing: true)

        XCTAssertEqual(
            playing.height,
            idle.height,
            accuracy: 0.5,
            "the voice bubble resized when playback started: idle \(idle.height), playing \(playing.height)"
        )
        // A zero-height measurement would make the assertion above pass while
        // measuring nothing at all.
        XCTAssertGreaterThan(idle.height, playButtonHeight, "the bubble measured smaller than its own play button")
    }

    // MARK: - The duration and the message time share a row

    /// UX spec section 10 — the message time sits beside the last line of the
    /// bubble's content, not under it. For a voice message that last line is
    /// the duration, which is why the maintainer sees the two "on one visual
    /// level".
    ///
    /// `MessageRow.bubble` composes the content and the time as
    /// `HStack(alignment: .bottom)`, so the time's bottom edge is pinned to
    /// the content's bottom edge. The two therefore share a visual row if and
    /// only if the bubble's own bottom row IS the duration row — flush with
    /// the bottom, with nothing below it and nothing taller beside it. That is
    /// what this measures, and it needs no access to the private `MessageRow`:
    ///
    ///     bubbleHeight == waveform(26) + spacing(4) + one caption2 line
    ///
    /// If somebody adds a row under the duration, pads the bubble's bottom, or
    /// lets the 38 pt play button become the tallest element, this arithmetic
    /// stops holding and the time drifts off the duration's level — the exact
    /// drift this test exists to catch.
    func testTheDurationIsTheVoiceBubblesBottomRow() throws {
        // Calibration first. If `sizeThatFits` itself added anything to what
        // it is asked to measure, it shows up here rather than masquerading as
        // a layout regression in the arithmetic below.
        let probe = UIHostingController(rootView: Color.clear.frame(width: 100, height: 47))
        XCTAssertEqual(
            probe.sizeThatFits(in: proposal).height, 47, accuracy: 0.5,
            "the measurement technique is off, so nothing below this line means what it says"
        )

        let bubble = measureVoiceBubble(playing: false)
        let caption = captionLineHeight()

        XCTAssertGreaterThan(caption, 0, "the caption line measured as nothing")
        // A point of tolerance, because the bubble's total and the bare line
        // are rounded to points independently. It is far below what this test
        // is looking for: an extra row costs a whole line (13 pt or more) and
        // bottom padding costs at least 4.
        XCTAssertEqual(
            bubble.height,
            waveformHeight + innerSpacing + caption,
            accuracy: 1,
            """
            the voice bubble's bottom row is no longer a single caption line \
            flush with its bottom edge (measured \(bubble.height), expected \
            \(waveformHeight + innerSpacing + caption) = 26 + 4 + \(caption)), \
            so the bottom-aligned message time no longer lands beside the duration
            """
        )
        // The other way the arrangement can break: if the play button became
        // the tallest thing in the row, the duration would float above the
        // bubble's bottom edge while the time stayed pinned to it.
        XCTAssertGreaterThan(
            waveformHeight + innerSpacing + caption,
            playButtonHeight,
            "the play button is now the tallest element, so the duration no longer sits on the bubble's bottom edge"
        )
    }

    // MARK: - The waveform draws a waveform

    /// ADR-0047 section 1 — a waveform built from real amplitude data must lay
    /// out bars of DIFFERING heights. A flat row of identical bars is the
    /// defect that was found on Android (an all-zero measurement drawn flatter
    /// than no measurement at all, see ``Waveform/downsample(_:buckets:)``);
    /// the rule belongs on all three clients, so it is pinned here too.
    ///
    /// Bars are drawn `Capsule`s with no view of their own to measure, so this
    /// renders the production view and counts, per pixel column, how many
    /// pixels are not background. Contiguous runs of non-background columns
    /// are the bars; a run's tallest column is that bar's height.
    func testWaveformFromRealAmplitudesDrawsBarsOfDifferingHeights() throws {
        // A ramp: the quietest bucket first, the loudest last. Any correct
        // renderer draws a visible slope; a flat renderer draws one height.
        let ramp = Data((0..<Waveform.maxBytes).map { UInt8($0 * 255 / (Waveform.maxBytes - 1)) })

        let heights = try barHeights(waveform: ramp)

        XCTAssertGreaterThanOrEqual(
            heights.count, 30,
            "expected close to 36 separable bars, measured \(heights.count): \(heights)"
        )
        let tallest = try XCTUnwrap(heights.max())
        let shortest = try XCTUnwrap(heights.min())
        XCTAssertGreaterThanOrEqual(
            tallest - shortest, 10,
            "the bars are (nearly) all one height, which is the flat-line defect: \(heights)"
        )
        XCTAssertGreaterThan(
            Set(heights).count, 5,
            "a ramp should produce many distinct bar heights, measured \(Set(heights).count): \(heights)"
        )
        // The slope runs the right way round, so a renderer that reversed or
        // shuffled the buckets would not pass on variance alone.
        let first = try XCTUnwrap(heights.first)
        let last = try XCTUnwrap(heights.last)
        XCTAssertGreaterThan(last, first, "the loudest bucket did not draw taller than the quietest: \(heights)")
    }

    /// The control for the test above, and the rule
    /// ``Waveform/bars(_:count:)`` documents: an ABSENT waveform draws a flat,
    /// low placeholder so the seek track stays visible. Without this arm the
    /// variance assertion above could be satisfied by a renderer that ignores
    /// its input entirely.
    func testAbsentWaveformDrawsAFlatPlaceholder() throws {
        let heights = try barHeights(waveform: nil)

        XCTAssertGreaterThanOrEqual(
            heights.count, 30,
            "the placeholder should still draw a full row of bars, measured \(heights.count)"
        )
        let tallest = try XCTUnwrap(heights.max())
        let shortest = try XCTUnwrap(heights.min())
        XCTAssertLessThanOrEqual(
            tallest - shortest, 2,
            "the no-waveform placeholder is supposed to be flat, measured \(heights)"
        )
    }

    // MARK: - Harness

    /// One `AttachmentRef` for a received voice message, with the waveform and
    /// duration a real sender would carry.
    private func voiceRef() -> AttachmentRef {
        AttachmentRef(
            blobId: Data(repeating: 0xA1, count: 32),
            key: Data(repeating: 0xA2, count: 32),
            nonce: Data(repeating: 0xA3, count: 12),
            mime: "audio/mp4",
            size: 8_192,
            name: "voice.m4a",
            kind: "voice",
            durationMs: 9_400,
            waveform: Data((0..<64).map { UInt8(($0 * 4) % 256) })
        )
    }

    /// Lay the production ``VoiceBubble`` out and report the size it asks for.
    ///
    /// A fresh hosting controller per state rather than one mutated in place:
    /// a detached controller is not guaranteed to re-evaluate its body when an
    /// observed object changes, and measuring a stale layout would make this
    /// suite quietly meaningless.
    ///
    /// `incoming: false` on purpose — an incoming message that has not been
    /// played carries an extra unread dot in the duration row, and the point
    /// here is to measure the row, not the dot.
    private func measureVoiceBubble(playing: Bool) -> CGSize {
        let ref = voiceRef()
        let key = ChatMediaController.key(for: ref)
        // Belt and braces: the bubble's `.task` prefetches the audio, and a
        // failed prefetch adds a "Couldn't play this message" row that would
        // change the height. A file already filed under the key short-circuits
        // `prefetchVoice`. (A detached hosting controller never appears, so
        // the task should not run at all — this makes that not matter.)
        _ = try? controller.temp.write(Data([0]), key: key, name: "voice.m4a", fallback: "voice.m4a")
        if playing {
            controller.live.voice = ChatMediaController.VoicePlayback(
                key: key,
                playing: true,
                positionMs: 3_200,
                durationMs: 9_400
            )
        } else {
            controller.live.voice = nil
        }
        let host = UIHostingController(
            rootView: VoiceBubble(ref: ref, mediaKey: key, incoming: false, upload: nil, onRetry: {})
                .environmentObject(controller)
                .environmentObject(controller.live)
        )
        return host.sizeThatFits(in: proposal)
    }

    /// The height of one line in the font the duration label and the message
    /// time both use. Measured rather than hardcoded, so it follows the
    /// device's text-size setting exactly as the views do.
    private func captionLineHeight() -> CGFloat {
        let host = UIHostingController(rootView: Text("0:09").font(.caption2.monospacedDigit()))
        return host.sizeThatFits(in: proposal).height
    }

    /// Render the production ``WaveformBars`` and measure each drawn bar.
    ///
    /// Geometry chosen for unambiguous pixel arithmetic rather than to match
    /// the bubble: 36 bars (the view's own default) at 6 pt each with the
    /// view's fixed 2 pt gaps, rendered at scale 2, so every gap is several
    /// clean background pixels wide and adjacent bars cannot merge into one
    /// run. The height IS the production 26 pt.
    private func barHeights(waveform: Data?) throws -> [Int] {
        let barCount = 36
        let width = CGFloat(barCount) * 6 + CGFloat(barCount - 1) * 2
        let renderer = ImageRenderer(
            content: WaveformBars(waveform: waveform, progress: 0, barCount: barCount, tint: .black)
                .frame(width: width, height: waveformHeight)
                .background(Color.white)
        )
        renderer.scale = 2
        renderer.isOpaque = true
        let image = try XCTUnwrap(renderer.uiImage, "ImageRenderer produced no image")
        let cgImage = try XCTUnwrap(image.cgImage, "the rendered image has no bitmap")

        let pixelWidth = cgImage.width
        let pixelHeight = cgImage.height
        XCTAssertGreaterThan(pixelWidth, 0)
        XCTAssertGreaterThan(pixelHeight, 0)
        var pixels = [UInt8](repeating: 0, count: pixelWidth * pixelHeight * 4)
        let context = try XCTUnwrap(
            CGContext(
                data: &pixels,
                width: pixelWidth,
                height: pixelHeight,
                bitsPerComponent: 8,
                bytesPerRow: pixelWidth * 4,
                space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
            ),
            "could not open a bitmap context over the render"
        )
        context.draw(cgImage, in: CGRect(x: 0, y: 0, width: pixelWidth, height: pixelHeight))

        // An unplayed bar is the tint at 35 % over white, which composites to
        // roughly 166 on each channel; the background stays 255. The cut sits
        // well clear of both so antialiased edges land on the bar's side.
        let backgroundCut: UInt8 = 230
        let inkPerColumn: [Int] = (0..<pixelWidth).map { x in
            (0..<pixelHeight).reduce(into: 0) { count, y in
                if pixels[(y * pixelWidth + x) * 4] < backgroundCut { count += 1 }
            }
        }

        // Contiguous non-background columns are one bar; its tallest column is
        // the bar's height. Taking the tallest column rather than the average
        // is what makes a 6 pt-wide capsule measure its full height instead of
        // its rounded shoulders.
        var heights: [Int] = []
        var run = 0
        for ink in inkPerColumn {
            if ink > 0 {
                run = max(run, ink)
            } else if run > 0 {
                heights.append(run)
                run = 0
            }
        }
        if run > 0 { heights.append(run) }
        return heights
    }
}
