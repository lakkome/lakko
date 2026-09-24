// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0047 / UX spec §2-§3, §3.1, §6 — the tap / hold rules, driven through
/// the pure machine with no microphone, camera or gesture.
///
/// §3.1 is why the verdict is worth testing at all: the shipped iOS button
/// started a voice recording the instant it was touched, so the camera mode
/// could not be reached. The rule these pin is that the two gestures are told
/// apart BEFORE anything is captured — never by how long the take turned out
/// to be — so the numbers (`holdToRecordMs`, `tapSlopPoints`) are read from
/// the machine rather than written down here, and web's copy uses the same two.
final class RecordingMachineTests: XCTestCase {

    private func run(_ events: [RecEvent], from state: RecState = .idle) -> (RecState, [RecEffect]) {
        var state = state
        var effects: [RecEffect] = []
        for e in events {
            let step = RecordingMachine.reduce(state, e)
            state = step.state
            effects += step.effects
        }
        return (state, effects)
    }

    /// A press that has passed the hold threshold — the state every
    /// recording rule below starts from, now that a bare press is not yet a take.
    private func heldDown(
        kind: CaptureKind = .voice,
        at: Int64 = 0,
        permission: PermissionHint = .granted
    ) -> [RecEvent] {
        [.press(kind: kind, at: at, permission: permission), .holdElapsed(at: at)]
    }

    // MARK: - §3.1 — one button, two gestures, decided on the gesture

    func testAPressAloneRecordsNothingUntilTheHoldThresholdPasses() {
        // The defect this closes: the press used to start the microphone
        // outright, so the tap that switches to the camera could not be made.
        let (pressing, effects) = run([.press(kind: .voice, at: 1000, permission: .granted)])
        XCTAssertEqual(
            pressing, .pressing(kind: .voice, at: 1000, permission: .granted, moved: false))
        XCTAssertEqual(effects, [.armHold])
        XCTAssertFalse(pressing.isCapturing)

        let (recording, held) = run([.holdElapsed(at: 1000 + RecordingMachine.holdToRecordMs)],
                                    from: pressing)
        XCTAssertEqual(
            recording,
            .recording(kind: .voice, startedAt: 1000 + RecordingMachine.holdToRecordMs, dx: 0, dy: 0))
        XCTAssertEqual(held, [.startCapture(.voice)])
    }

    func testATapSwitchesTheModeAndRecordsNothing() {
        let (state, effects) = run([
            .press(kind: .voice, at: 0, permission: .granted),
            .release(at: RecordingMachine.holdToRecordMs - 1),
        ])
        XCTAssertEqual(state, .idle)
        // No capture was ever started, so there is nothing to finish and no
        // `Hold to record` hint to show: this was not a failed recording.
        XCTAssertEqual(effects, [.armHold, .toggleMode])
    }

    func testATapNeverOpensTheMicrophoneEvenWhenPermissionIsUnknownOrDenied() {
        // A tap must not raise a permission prompt and must not show the denied
        // card: it is asking for the other glyph, not for the microphone.
        for permission in [PermissionHint.prompt, .denied] {
            let (state, effects) = run([
                .press(kind: .videoNote, at: 0, permission: permission),
                .release(at: RecordingMachine.holdToRecordMs - 1),
            ])
            XCTAssertEqual(state, .idle, "\(permission)")
            XCTAssertEqual(effects, [.armHold, .toggleMode], "\(permission)")
        }
    }

    func testATouchThatTravelledIsNotATapAndSwitchesNothing() {
        let (moved, _) = run([
            .press(kind: .voice, at: 0, permission: .granted),
            .move(dx: -(RecordingMachine.tapSlopPoints + 1), dy: 0),
        ])
        XCTAssertEqual(moved, .pressing(kind: .voice, at: 0, permission: .granted, moved: true))
        let (state, effects) = run([.release(at: RecordingMachine.holdToRecordMs - 1)], from: moved)
        XCTAssertEqual(state, .idle)
        XCTAssertTrue(effects.isEmpty)

        // Drift inside the slop is still a tap — a thumb is not a stylus.
        let (steady, _) = run([
            .press(kind: .voice, at: 0, permission: .granted),
            .move(dx: -(RecordingMachine.tapSlopPoints - 1), dy: 1),
        ])
        XCTAssertEqual(steady, .pressing(kind: .voice, at: 0, permission: .granted, moved: false))
        XCTAssertEqual(run([.release(at: 1)], from: steady).1, [.toggleMode])
    }

    func testAPressCancelledBeforeTheHoldThresholdSwitchesNothing() {
        // A call arriving, or the chat leaving the screen, is not a tap.
        let (state, effects) = run([
            .press(kind: .voice, at: 0, permission: .granted),
            .cancel,
        ])
        XCTAssertEqual(state, .idle)
        XCTAssertEqual(effects, [.armHold])
    }

    func testAHoldElapsedAfterTheFingerLiftedIsIgnored() {
        // A stale hold timer must not open the microphone behind the tap that
        // has already been answered. The host invalidates it; the machine
        // refuses it as well, so neither alone is load-bearing.
        let (state, effects) = run([
            .press(kind: .voice, at: 0, permission: .granted),
            .release(at: 10),
            .holdElapsed(at: RecordingMachine.holdToRecordMs),
        ])
        XCTAssertEqual(state, .idle)
        XCTAssertEqual(effects, [.armHold, .toggleMode])
    }

    // MARK: - §2-§3 — the hold itself

    func testHeldPressStartsAndReleaseSends() {
        let (s1, e1) = run(heldDown(at: 1000))
        XCTAssertEqual(s1, .recording(kind: .voice, startedAt: 1000, dx: 0, dy: 0))
        XCTAssertEqual(e1, [.armHold, .startCapture(.voice)])

        let (s2, e2) = run([.release(at: 1000 + RecordingMachine.minTakeMs)], from: s1)
        XCTAssertEqual(s2, .idle)
        XCTAssertEqual(e2, [.finish(.send)])
    }

    func testAnAccidentalPressDiscardsWithTheHintAndIsNotAModeSwitch() {
        // §3.1 — the hold threshold had already passed, so the microphone was
        // open. That is the accidental-press case, not the tap: it must not
        // move the glyph out from under the person's next attempt.
        let (state, effects) = run(heldDown() + [.release(at: RecordingMachine.minTakeMs - 1)])
        XCTAssertEqual(state, .idle)
        XCTAssertEqual(effects, [.armHold, .startCapture(.voice), .finish(.discard), .hint(.holdToRecord)])
        XCTAssertFalse(effects.contains(.toggleMode))
    }

    func testTheModeIsSwitchedByTheTapAndByNothingElse() {
        // One production path emits `.toggleMode`, and it is the tap. Anything
        // else reaching it would be the shipped defect coming back.
        let gestures: [[RecEvent]] = [
            heldDown() + [.release(at: RecordingMachine.minTakeMs - 1)],
            heldDown() + [.release(at: 5_000)],
            heldDown() + [.move(dx: -RecordingMachine.cancelSlidePoints, dy: 0)],
            heldDown() + [.move(dx: 0, dy: -RecordingMachine.lockSlidePoints), .stop(at: 5_000)],
            heldDown() + [.cancel],
            heldDown(permission: .prompt) + [.permissionResult(granted: true)],
            heldDown(permission: .denied),
            [.activate(kind: .voice, at: 0, permission: .granted)],
        ]
        for events in gestures {
            XCTAssertFalse(run(events).1.contains(.toggleMode), "\(events)")
        }
        XCTAssertTrue(
            run([.press(kind: .voice, at: 0, permission: .granted), .release(at: 1)])
                .1.contains(.toggleMode))
    }

    func testSlideLeftPastThresholdCancels() {
        let (s1, _) = run(heldDown() + [
            .move(dx: -(RecordingMachine.cancelSlidePoints - 1), dy: 0),
        ])
        XCTAssertEqual(s1, .recording(kind: .voice, startedAt: 0, dx: -(RecordingMachine.cancelSlidePoints - 1), dy: 0))
        let (s2, e2) = run([.move(dx: -RecordingMachine.cancelSlidePoints, dy: 0)], from: s1)
        XCTAssertEqual(s2, .idle)
        XCTAssertEqual(e2, [.finish(.discard), .hint(.cancelled)])
    }

    func testSlideUpLocksAndReleaseNoLongerSends() {
        let (locked, _) = run(heldDown(kind: .videoNote) + [
            .move(dx: 0, dy: -RecordingMachine.lockSlidePoints),
        ])
        XCTAssertEqual(locked, .locked(kind: .videoNote, startedAt: 0))
        let (afterRelease, effects) = run([.release(at: 5000)], from: locked)
        XCTAssertEqual(afterRelease, locked)
        XCTAssertTrue(effects.isEmpty)
    }

    func testLockedStopMovesToPreviewAndSendSendsIt() {
        let locked = RecState.locked(kind: .voice, startedAt: 0)
        let (finalizing, e1) = run([.stop(at: 2000)], from: locked)
        XCTAssertEqual(finalizing, .finalizing(kind: .voice))
        XCTAssertEqual(e1, [.finish(.preview)])
        let (preview, _) = run([.previewReady(durationMs: 2000)], from: finalizing)
        XCTAssertEqual(preview, .preview(kind: .voice, durationMs: 2000))
        let (idle, e2) = run([.send], from: preview)
        XCTAssertEqual(idle, .idle)
        XCTAssertEqual(e2, [.sendPreview])
    }

    func testLockedDeleteDiscards() {
        let (idle, effects) = run([.delete], from: .locked(kind: .voice, startedAt: 0))
        XCTAssertEqual(idle, .idle)
        XCTAssertEqual(effects, [.finish(.discard)])
    }

    func testShortPreviewIsDiscardedWithHint() {
        let (idle, effects) = run([.previewReady(durationMs: 300)], from: .finalizing(kind: .voice))
        XCTAssertEqual(idle, .idle)
        XCTAssertEqual(effects, [.discardPreview, .hint(.holdToRecord)])
    }

    func testCapsStopIntoPreview() {
        let voice = RecState.recording(kind: .voice, startedAt: 0, dx: 0, dy: 0)
        XCTAssertEqual(run([.tick(at: RecordingMachine.voiceCapMs - 1)], from: voice).0, voice)
        let (capped, effects) = run([.tick(at: RecordingMachine.voiceCapMs)], from: voice)
        XCTAssertEqual(capped, .finalizing(kind: .voice))
        XCTAssertEqual(effects, [.finish(.preview)])

        let video = RecState.locked(kind: .videoNote, startedAt: 0)
        XCTAssertEqual(run([.tick(at: 60_000)], from: video).0, .finalizing(kind: .videoNote))
        XCTAssertEqual(RecordingMachine.videoNoteCapMs, 60_000)
        XCTAssertEqual(RecordingMachine.voiceCapMs, 15 * 60 * 1000)
    }

    func testHoldThatRaisesPromptNeverRecordsEvenWhenGranted() {
        let (requesting, e1) = run(heldDown(permission: .prompt))
        XCTAssertEqual(requesting, .requesting(kind: .voice))
        XCTAssertEqual(e1, [.armHold, .requestPermission(.voice)])
        let (afterGrant, e2) = run([.release(at: 3000), .permissionResult(granted: true)], from: requesting)
        XCTAssertEqual(afterGrant, .idle)
        XCTAssertFalse(e2.contains(.startCapture(.voice)))
    }

    func testDeniedShowsCardWithoutCapture() {
        let (s1, e1) = run(heldDown(kind: .videoNote, permission: .denied))
        XCTAssertEqual(s1, .idle)
        XCTAssertEqual(e1, [.armHold, .showDenied(.videoNote)])

        let (s2, e2) = run([.permissionResult(granted: false)], from: .requesting(kind: .voice))
        XCTAssertEqual(s2, .idle)
        XCTAssertEqual(e2, [.showDenied(.voice)])
    }

    func testRevokedMidRecordingDiscardsAndShowsCard() {
        let (s, e) = run([.revoked], from: .recording(kind: .voice, startedAt: 0, dx: 0, dy: 0))
        XCTAssertEqual(s, .idle)
        XCTAssertEqual(e, [.finish(.discard), .showDenied(.voice)])
    }

    func testAccessibilityActivationStartsLockedTake() {
        let (s, e) = run([.activate(kind: .voice, at: 7, permission: .granted)])
        XCTAssertEqual(s, .locked(kind: .voice, startedAt: 7))
        XCTAssertEqual(e, [.startCapture(.voice)])
    }

    func testCancelWhileRecordingDiscards() {
        let (s, e) = run([.cancel], from: .recording(kind: .videoNote, startedAt: 0, dx: 0, dy: 0))
        XCTAssertEqual(s, .idle)
        XCTAssertEqual(e, [.finish(.discard), .hint(.cancelled)])
    }

    func testTotalClockRoundsUpSubSecondDurations() {
        XCTAssertEqual(RecordingMachine.formatTotalClock(ms: 0), "0:00")
        XCTAssertEqual(RecordingMachine.formatTotalClock(ms: -5), "0:00")
        XCTAssertEqual(RecordingMachine.formatTotalClock(ms: 1), "0:01")
        XCTAssertEqual(RecordingMachine.formatTotalClock(ms: 480), "0:01")
        XCTAssertEqual(RecordingMachine.formatTotalClock(ms: 999), "0:01")
        XCTAssertEqual(RecordingMachine.formatTotalClock(ms: 1_000), "0:01")
        XCTAssertEqual(RecordingMachine.formatTotalClock(ms: 1_001), "0:02")
        XCTAssertEqual(RecordingMachine.formatTotalClock(ms: 59_500), "1:00")
        XCTAssertEqual(RecordingMachine.formatTotalClock(ms: 900_000), "15:00")
        // The elapsed / position clock stays rounded down.
        XCTAssertEqual(RecordingMachine.formatClock(ms: 480), "0:00")
    }

    func testClockAndSpokenDuration() {
        XCTAssertEqual(RecordingMachine.formatClock(ms: 7_400), "0:07")
        XCTAssertEqual(RecordingMachine.formatClock(ms: 62_000), "1:02")
        XCTAssertEqual(RecordingMachine.formatClock(ms: 900_000), "15:00")
        XCTAssertEqual(RecordingMachine.spokenDuration(ms: 12_000), "12 seconds")
        XCTAssertEqual(RecordingMachine.spokenDuration(ms: 65_000), "1 minute 5 seconds")
        XCTAssertEqual(RecordingMachine.spokenDuration(ms: 1_000), "1 second")
    }
}
