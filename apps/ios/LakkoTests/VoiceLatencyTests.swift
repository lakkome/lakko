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

    // MARK: - The microphone that gives nothing

    /// A run of `MicrophoneSilence.warnAfterMs` of `level`, sampled at the
    /// recorder's own 50 ms cadence, to see whether it ends up accused.
    private func runOf(level: Double) -> MicrophoneSilence.Watch {
        var watch = MicrophoneSilence.Watch()
        var t: Int64 = 0
        while t <= MicrophoneSilence.warnAfterMs {
            watch = MicrophoneSilence.observe(watch, level: level, atMs: t)
            t += 50
        }
        return watch
    }

    /// Nothing is claimed before a take has begun.
    func testATakeWithNoSamplesYetIsNotAccused() {
        XCTAssertFalse(MicrophoneSilence.Watch().warns)
        XCTAssertNil(MicrophoneSilence.Watch().silentSinceMs)
    }

    /// The direction that matters most: a microphone that is merely slow to open
    /// reads zero for its first tick or two, and a bar that accused it on the
    /// first zero sample would accuse every microphone on the platform.
    func testAMicrophoneThatHasOnlyJustBeenOpenedIsNotAccused() {
        var watch = MicrophoneSilence.Watch()
        for t in stride(from: Int64(0), to: MicrophoneSilence.warnAfterMs, by: 50) {
            watch = MicrophoneSilence.observe(watch, level: 0, atMs: t)
            XCTAssertFalse(watch.warns, "accused at \(t) ms of silence")
        }
    }

    /// At the window, not past it: the spec's two seconds are two seconds.
    func testTheWarningArrivesExactlyAtTwoSeconds() {
        var watch = MicrophoneSilence.observe(.init(), level: 0, atMs: 0)
        watch = MicrophoneSilence.observe(watch, level: 0, atMs: MicrophoneSilence.warnAfterMs - 1)
        XCTAssertFalse(watch.warns)
        watch = MicrophoneSilence.observe(watch, level: 0, atMs: MicrophoneSilence.warnAfterMs)
        XCTAssertTrue(watch.warns)
    }

    /// §2 — "clears the moment any level is measured". The warning describes the
    /// present, so a microphone unmuted mid-take must not carry an accusation
    /// through the rest of the recording.
    func testOneAudibleSampleClearsEverything() {
        var watch = runOf(level: 0)
        XCTAssertTrue(watch.warns)
        watch = MicrophoneSilence.observe(watch, level: 0.01, atMs: 5_000)
        XCTAssertFalse(watch.warns)
        XCTAssertNil(watch.silentSinceMs)
    }

    /// And the window starts again from there, rather than from the take.
    func testTheWindowIsMeasuredFromWhenTheSilenceBegan() {
        var watch = MicrophoneSilence.observe(.init(), level: 0.5, atMs: 100)
        watch = MicrophoneSilence.observe(watch, level: 0, atMs: 200)
        XCTAssertEqual(watch.silentSinceMs, 200)
        watch = MicrophoneSilence.observe(watch, level: 0, atMs: 200 + MicrophoneSilence.warnAfterMs - 1)
        XCTAssertFalse(watch.warns, "the window was measured from the take, not from the silence")
        watch = MicrophoneSilence.observe(watch, level: 0, atMs: 200 + MicrophoneSilence.warnAfterMs)
        XCTAssertTrue(watch.warns)
    }

    /// The other direction it could be wrong in. The defect this rule exists to
    /// catch is a microphone that gives literally nothing, so the floor sits
    /// below anything a recorder can represent as sound: a person speaking
    /// quietly, or a room with nothing but its own hum, is never told the
    /// microphone is dead.
    func testAQuietRoomIsNeverAccused() {
        // One least-significant bit of 16-bit PCM, the quietest non-silence
        // either platform's recorder can report.
        let oneBit = 1.0 / 32_767
        XCTAssertTrue(MicrophoneSilence.hasSound(level: oneBit))
        XCTAssertFalse(runOf(level: oneBit).warns)
        // A whisper at -60 dBFS, and an ordinary speaking voice.
        XCTAssertFalse(runOf(level: Waveform.linearLevel(decibels: -60)).warns)
        XCTAssertFalse(runOf(level: 0.3).warns)
    }

    /// A dead input reads as silence on this platform's meter, which is the
    /// whole reason the floor is not an exact-zero test: `-160` dBFS converts to
    /// `1e-8`, not to `0`.
    func testADeadMicrophoneIsAccused() {
        let silentReading = Waveform.linearLevel(decibels: -160)
        XCTAssertGreaterThan(silentReading, 0, "this platform never reports exactly zero")
        XCTAssertFalse(MicrophoneSilence.hasSound(level: silentReading))
        XCTAssertTrue(runOf(level: silentReading).warns)
        // Android's integer meter does report exactly zero; the same floor takes
        // both, which is what makes it one rule rather than two.
        XCTAssertFalse(MicrophoneSilence.hasSound(level: 0))
        XCTAssertTrue(runOf(level: 0).warns)
    }

    /// The floor has room on both sides: far under one bit of real signal, far
    /// over what the meter reports when it hears nothing. Pinned so a later
    /// nudge toward "quiet" has to be deliberate.
    func testTheFloorSitsBetweenSilenceAndTheQuietestRealSignal() {
        XCTAssertLessThan(MicrophoneSilence.levelFloor, (1.0 / 32_767) / 10)
        XCTAssertGreaterThan(MicrophoneSilence.levelFloor, Waveform.linearLevel(decibels: -160) * 10)
    }

    /// A meter reporting nonsense has not heard anything either.
    func testANonFiniteOrNegativeReadingCountsAsSilence() {
        XCTAssertFalse(MicrophoneSilence.hasSound(level: .nan))
        XCTAssertFalse(MicrophoneSilence.hasSound(level: -1))
        XCTAssertTrue(runOf(level: .nan).warns)
    }

    /// The recorder's clock is monotonic, but a fold that read a backwards step
    /// as a huge gap would raise the warning on a take that had just started. It
    /// reads as no time passed instead, which only ever delays.
    func testABackwardsClockNeverRaisesTheWarning() {
        var watch = MicrophoneSilence.observe(.init(), level: 0, atMs: 5_000)
        watch = MicrophoneSilence.observe(watch, level: 0, atMs: 10)
        XCTAssertFalse(watch.warns)
    }

    /// Two seconds is the spec's number, shared by all three clients, so the
    /// same room produces the same warning on each.
    func testTheWindowIsTheSpecsTwoSeconds() {
        XCTAssertEqual(MicrophoneSilence.warnAfterMs, 2_000)
    }

    /// The spec's words, in one place, so the bar and the accessibility label
    /// cannot say two different things.
    func testTheWarningIsTheSpecsWording() {
        XCTAssertEqual(MicrophoneSilence.warningCopy, "No sound from the microphone")
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
