// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0047 / UX spec §2-§3, §6 — the hold-to-record rules, driven through the
/// pure machine with no microphone, camera or gesture.
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

    func testGrantedPressStartsImmediatelyAndReleaseSends() {
        let (s1, e1) = run([.press(kind: .voice, at: 1000, permission: .granted)])
        XCTAssertEqual(s1, .recording(kind: .voice, startedAt: 1000, dx: 0, dy: 0))
        XCTAssertEqual(e1, [.startCapture(.voice)])

        let (s2, e2) = run([.release(at: 1000 + RecordingMachine.minTakeMs)], from: s1)
        XCTAssertEqual(s2, .idle)
        XCTAssertEqual(e2, [.finish(.send)])
    }

    func testShortTapDiscardsHintsAndTogglesMode() {
        let (state, effects) = run([
            .press(kind: .voice, at: 0, permission: .granted),
            .release(at: RecordingMachine.minTakeMs - 1),
        ])
        XCTAssertEqual(state, .idle)
        XCTAssertEqual(effects, [.startCapture(.voice), .finish(.discard), .hint(.holdToRecord), .toggleMode])
    }

    func testSlideLeftPastThresholdCancels() {
        let (s1, _) = run([
            .press(kind: .voice, at: 0, permission: .granted),
            .move(dx: -(RecordingMachine.cancelSlidePoints - 1), dy: 0),
        ])
        XCTAssertEqual(s1, .recording(kind: .voice, startedAt: 0, dx: -(RecordingMachine.cancelSlidePoints - 1), dy: 0))
        let (s2, e2) = run([.move(dx: -RecordingMachine.cancelSlidePoints, dy: 0)], from: s1)
        XCTAssertEqual(s2, .idle)
        XCTAssertEqual(e2, [.finish(.discard), .hint(.cancelled)])
    }

    func testSlideUpLocksAndReleaseNoLongerSends() {
        let (locked, _) = run([
            .press(kind: .videoNote, at: 0, permission: .granted),
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

    func testPressThatRaisesPromptNeverRecordsEvenWhenGranted() {
        let (requesting, e1) = run([.press(kind: .voice, at: 0, permission: .prompt)])
        XCTAssertEqual(requesting, .requesting(kind: .voice))
        XCTAssertEqual(e1, [.requestPermission(.voice)])
        let (afterGrant, e2) = run([.release(at: 3000), .permissionResult(granted: true)], from: requesting)
        XCTAssertEqual(afterGrant, .idle)
        XCTAssertFalse(e2.contains(.startCapture(.voice)))
    }

    func testDeniedShowsCardWithoutCapture() {
        let (s1, e1) = run([.press(kind: .videoNote, at: 0, permission: .denied)])
        XCTAssertEqual(s1, .idle)
        XCTAssertEqual(e1, [.showDenied(.videoNote)])

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

    func testClockAndSpokenDuration() {
        XCTAssertEqual(RecordingMachine.formatClock(ms: 7_400), "0:07")
        XCTAssertEqual(RecordingMachine.formatClock(ms: 62_000), "1:02")
        XCTAssertEqual(RecordingMachine.formatClock(ms: 900_000), "15:00")
        XCTAssertEqual(RecordingMachine.spokenDuration(ms: 12_000), "12 seconds")
        XCTAssertEqual(RecordingMachine.spokenDuration(ms: 65_000), "1 minute 5 seconds")
        XCTAssertEqual(RecordingMachine.spokenDuration(ms: 1_000), "1 second")
    }
}
