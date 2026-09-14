// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0047 §1 — waveform downsampling into the <=128-byte envelope field, and
/// UX spec §2 — auto-continue to the next consecutive unplayed voice message.
final class WaveformPlaybackTests: XCTestCase {

    // MARK: - Waveform

    func testEmptySeriesHasNoWaveform() {
        XCTAssertNil(Waveform.downsample([]))
    }

    func testLongSeriesFoldsToAtMostTheEnvelopeBound() {
        let levels = (0..<18_000).map { Double($0 % 50) / 50 }
        let bytes = Waveform.downsample(levels)
        XCTAssertEqual(bytes?.count, Waveform.maxBytes)
        XCTAssertEqual(Waveform.maxBytes, 128)
    }

    func testShortSeriesKeepsOneBytePerSample() {
        XCTAssertEqual(Waveform.downsample([0.1, 0.2, 0.4])?.count, 3)
    }

    func testPeakSurvivesTheFoldAndLoudestIsNormalisedTo255() {
        // One loud sample in a long quiet run must not be averaged away.
        var levels = [Double](repeating: 0.01, count: 1280)
        levels[645] = 0.9
        let bytes = [UInt8](Waveform.downsample(levels) ?? Data())
        XCTAssertEqual(bytes.count, 128)
        XCTAssertEqual(bytes[64], 255)
        XCTAssertEqual(bytes.max(), 255)
        XCTAssertLessThan(bytes[0], 10)
    }

    func testSilenceAndGarbageAreHonestZeros() {
        XCTAssertEqual(Waveform.downsample([0, 0, 0]), Data([0, 0, 0]))
        XCTAssertEqual(Waveform.downsample([.nan, -1, .infinity]), Data([0, 0, 0]))
    }

    func testBarsResampleAndPlaceholder() {
        XCTAssertEqual(Waveform.bars(nil, count: 4), [0.15, 0.15, 0.15, 0.15])
        let bars = Waveform.bars(Data([255, 0]), count: 4)
        XCTAssertEqual(bars.count, 4)
        XCTAssertEqual(bars[0], 1)
        XCTAssertEqual(bars[3], 0.08)
    }

    func testDecibelsToLinearLevel() {
        XCTAssertEqual(Waveform.linearLevel(decibels: 0), 1)
        XCTAssertEqual(Waveform.linearLevel(decibels: -20), 0.1, accuracy: 1e-9)
        XCTAssertEqual(Waveform.linearLevel(decibels: -160), 1e-8, accuracy: 1e-9)
        XCTAssertEqual(Waveform.linearLevel(decibels: .nan), 0)
        XCTAssertEqual(Waveform.linearLevel(decibels: 12), 1)
    }

    // MARK: - Playback queue

    private func item(_ key: String, voice: Bool = true, sender: String? = "alice", played: Bool = false) -> PlaybackQueue.Item {
        PlaybackQueue.Item(key: key, isVoice: voice, senderKey: sender, played: played)
    }

    func testContinuesToNextUnplayedFromSameSenderSkippingPlayed() {
        let items = [item("a"), item("b", played: true), item("c"), item("d")]
        XCTAssertEqual(PlaybackQueue.nextVoiceToPlay(items, finishedKey: "a"), "c")
    }

    func testAnotherSenderOrNonVoiceEndsTheRun() {
        XCTAssertNil(PlaybackQueue.nextVoiceToPlay([item("a"), item("b", sender: "bob"), item("c")], finishedKey: "a"))
        XCTAssertNil(PlaybackQueue.nextVoiceToPlay([item("a"), item("t", voice: false), item("c")], finishedKey: "a"))
    }

    func testUnknownSenderNeverMatches() {
        XCTAssertNil(PlaybackQueue.nextVoiceToPlay([item("a", sender: nil), item("b", sender: nil)], finishedKey: "a"))
        XCTAssertNil(PlaybackQueue.nextVoiceToPlay([item("a")], finishedKey: "missing"))
    }

    func testRateCycle() {
        XCTAssertEqual(PlaybackQueue.nextRate(after: 1), 1.5)
        XCTAssertEqual(PlaybackQueue.nextRate(after: 1.5), 2)
        XCTAssertEqual(PlaybackQueue.nextRate(after: 2), 1)
        XCTAssertEqual(PlaybackQueue.nextRate(after: 3), 1)
        XCTAssertEqual(PlaybackQueue.rateLabel(1), "1x")
        XCTAssertEqual(PlaybackQueue.rateLabel(1.5), "1.5x")
    }
}
