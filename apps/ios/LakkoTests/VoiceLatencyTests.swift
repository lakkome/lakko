// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// The rules behind the second of silence at the start of every voice message,
/// and behind a long press on the waveform moving the playhead.
final class VoiceLatencyTests: XCTestCase {

    // MARK: - The recorded silence

    /// The settle window must be outside the file, which is only true if the
    /// recorder is given a window to schedule past.
    func testInputSettleWindowIsPositive() {
        XCTAssertGreaterThan(VoiceCapture.inputSettleSeconds, 0)
    }

    /// The window is subtracted from what the person can say, so it has to
    /// stay well under the floor a take must clear to be sent at all —
    /// otherwise a take that clears the floor by a hair would be discarded as
    /// too short and the recorder would look broken.
    func testInputSettleWindowStaysFarBelowTheMinimumTake() {
        let minTake = Double(RecordingMachine.minTakeMs) / 1000
        XCTAssertLessThan(VoiceCapture.inputSettleSeconds, minTake / 2)
    }

    /// The level timer is armed before the scheduled start, so a tick taken
    /// while the recorder's clock reads zero belongs to no sample in the file.
    /// Letting it through prepends bars to the waveform that the audio does
    /// not have.
    func testNoLevelIsSampledBeforeTheRecorderClockMoves() {
        XCTAssertFalse(VoiceCapture.recordsLevel(recordedSeconds: 0))
        XCTAssertTrue(VoiceCapture.recordsLevel(recordedSeconds: 0.001))
        XCTAssertTrue(VoiceCapture.recordsLevel(recordedSeconds: 3))
    }

    /// A recorder that reports a non-positive clock for any reason is treated
    /// as not yet capturing rather than as capturing at time zero.
    func testANonPositiveRecorderClockNeverSamples() {
        XCTAssertFalse(VoiceCapture.recordsLevel(recordedSeconds: -0.05))
    }

    // MARK: - The unfetched bytes

    /// The defect: the voice bubble was the only media bubble that fetched
    /// nothing until it was pressed. A received message warms on appear.
    func testAReceivedVoiceMessageWarmsOnAppear() {
        XCTAssertTrue(VoicePrefetch.shouldWarm(pending: false))
    }

    /// An outgoing take still in the send queue has its bytes on disk already
    /// and no blob to fetch, so warming it would be a round trip for nothing.
    func testAPendingSendIsNotWarmed() {
        XCTAssertFalse(VoicePrefetch.shouldWarm(pending: true))
    }

    // MARK: - The waveform and the context menu

    /// A tap on the waveform is a seek.
    func testAShortTouchOnTheWaveformSeeks() {
        XCTAssertTrue(VoiceSeekGate.acceptsSeek(pressSeconds: 0))
        XCTAssertTrue(VoiceSeekGate.acceptsSeek(pressSeconds: 0.2))
    }

    /// A press held long enough to open the message's context menu is not a
    /// seek: the person was reaching for React or Delete. Its lift must not
    /// also jump the playhead, whether or not the system cancelled the drag.
    func testALongPressOnTheWaveformDoesNotSeek() {
        XCTAssertFalse(VoiceSeekGate.acceptsSeek(pressSeconds: VoiceSeekGate.longPressSeconds))
        XCTAssertFalse(VoiceSeekGate.acceptsSeek(pressSeconds: 1.5))
    }

    /// The gate's threshold is the system's own long-press threshold. A gate
    /// shorter than it would refuse seeks the menu never took; a longer one
    /// would let the menu's own lift through.
    func testTheGateThresholdIsTheSystemLongPressThreshold() {
        XCTAssertEqual(VoiceSeekGate.longPressSeconds, 0.5, accuracy: 0.0001)
    }
}
