// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import XCTest
@testable import Lakko

/// ADR-0047 / UX spec §3 — the decisions behind a video note tap, and the
/// bounded key-value wait that keeps a never-ready item from leaving the
/// bubble silently idle.
final class VideoNotePlaybackTests: XCTestCase {

    // MARK: - Tap

    func testTapOnPosterStarts() {
        XCTAssertEqual(
            VideoNotePlayback.tapAction(mode: .poster, paused: false, starting: false, hasPlayer: false),
            .start
        )
    }

    func testTapOnMutedAutoplayStartsWithSound() {
        XCTAssertEqual(
            VideoNotePlayback.tapAction(mode: .muted, paused: false, starting: false, hasPlayer: true),
            .start
        )
    }

    func testTapWhileStartingIsIgnored() {
        for mode in [VideoNotePlayback.Mode.poster, .muted, .sound] {
            XCTAssertEqual(
                VideoNotePlayback.tapAction(mode: mode, paused: false, starting: true, hasPlayer: true),
                .ignore
            )
        }
    }

    func testTapWhilePlayingWithSoundPausesAndThenResumes() {
        XCTAssertEqual(
            VideoNotePlayback.tapAction(mode: .sound, paused: false, starting: false, hasPlayer: true),
            .pause
        )
        XCTAssertEqual(
            VideoNotePlayback.tapAction(mode: .sound, paused: true, starting: false, hasPlayer: true),
            .resume
        )
    }

    func testSoundModeWithoutAPlayerStartsAgain() {
        XCTAssertEqual(
            VideoNotePlayback.tapAction(mode: .sound, paused: true, starting: false, hasPlayer: false),
            .start
        )
    }

    // MARK: - Readiness

    func testReadinessClassification() {
        XCTAssertEqual(VideoNotePlayback.readiness(.readyToPlay), .ready)
        XCTAssertEqual(VideoNotePlayback.readiness(.failed), .failed)
        XCTAssertEqual(VideoNotePlayback.readiness(nil), .timedOut)
        XCTAssertEqual(VideoNotePlayback.readiness(.unknown), .timedOut)
    }

    func testStartFailureOnlyWhenPlaybackIsStillWanted() {
        XCTAssertFalse(VideoNotePlayback.startFailed(observed: .playing, stillWanted: true))
        XCTAssertTrue(VideoNotePlayback.startFailed(observed: nil, stillWanted: true))
        XCTAssertFalse(VideoNotePlayback.startFailed(observed: nil, stillWanted: false))
    }

    // MARK: - Key-value wait

    private final class Observed: NSObject {
        @objc dynamic var value = 0
    }

    @MainActor
    func testWaitReturnsImmediatelyWhenAlreadySatisfied() async {
        let object = Observed()
        object.value = 3
        let result = await KeyValueWait.first(of: object, \.value, timeout: .seconds(5)) { $0 == 3 }
        XCTAssertEqual(result, 3)
    }

    @MainActor
    func testWaitResumesOnChange() async {
        let object = Observed()
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(50))
            object.value = 1
            object.value = 2
        }
        let result = await KeyValueWait.first(of: object, \.value, timeout: .seconds(5)) { $0 == 2 }
        XCTAssertEqual(result, 2)
    }

    @MainActor
    func testWaitTimesOutWithNil() async {
        let object = Observed()
        let result = await KeyValueWait.first(of: object, \.value, timeout: .milliseconds(100)) { $0 == 9 }
        XCTAssertNil(result)
    }

    @MainActor
    func testWaitForAnItemThatCannotLoadEndsFailedOrTimedOut() async {
        let missing = FileManager.default.temporaryDirectory
            .appendingPathComponent("video-note-missing-\(UUID().uuidString).mp4")
        let item = AVPlayerItem(url: missing)
        let player = AVPlayer(playerItem: item)
        let status = await KeyValueWait.first(of: item, \.status, timeout: .seconds(5)) { $0 != .unknown }
        XCTAssertNotEqual(VideoNotePlayback.readiness(status), .ready)
        _ = player
    }
}
