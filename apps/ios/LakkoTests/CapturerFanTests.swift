// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import WebRTC
import XCTest

@testable import Lakko

/// The capturer fan (`CapturerFan.swift`): every attached source gets every
/// frame, attaching twice hands a frame once, the main source is never
/// detached; the copies' sources are one per copy for the capture's life;
/// and on real `RTCVideoSource`s a source that narrows its picture narrows
/// only its own.
@MainActor
final class CapturerFanTests: XCTestCase {

    private final class Recorder: NSObject, RTCVideoCapturerDelegate {
        private let lock = NSLock()
        private var stamps: [Int64] = []
        var events: [Int64] {
            lock.lock()
            defer { lock.unlock() }
            return stamps
        }
        func capturer(_ capturer: RTCVideoCapturer, didCapture frame: RTCVideoFrame) {
            lock.lock()
            stamps.append(frame.timeStampNs)
            lock.unlock()
        }
    }

    /// Records the size of every frame a track renders.
    private final class SizeRecorder: NSObject, RTCVideoRenderer {
        private let lock = NSLock()
        private var seen: [CGSize] = []
        var sizes: [CGSize] {
            lock.lock()
            defer { lock.unlock() }
            return seen
        }
        func setSize(_ size: CGSize) {}
        func renderFrame(_ frame: RTCVideoFrame?) {
            guard let frame else { return }
            lock.lock()
            seen.append(CGSize(width: Int(frame.width), height: Int(frame.height)))
            lock.unlock()
        }
    }

    private func frame(_ ts: Int64, width: Int32 = 1280, height: Int32 = 720) -> RTCVideoFrame {
        RTCVideoFrame(
            buffer: RTCI420Buffer(width: width, height: height),
            rotation: ._0, timeStampNs: ts)
    }

    func testEveryAttachedSourceGetsEveryFrame() {
        let main = Recorder()
        let light = Recorder()
        let middle = Recorder()
        let fan = CapturerFan(first: main)
        fan.attach(light)
        fan.attach(middle)
        let capturer = RTCVideoCapturer(delegate: fan)
        fan.capturer(capturer, didCapture: frame(1))
        fan.capturer(capturer, didCapture: frame(2))
        XCTAssertEqual(main.events, [1, 2])
        XCTAssertEqual(light.events, [1, 2])
        XCTAssertEqual(middle.events, [1, 2])
    }

    func testAttachingTwiceHandsEachFrameOnce() {
        let main = Recorder()
        let copy = Recorder()
        let fan = CapturerFan(first: main)
        fan.attach(copy)
        fan.attach(copy)
        fan.capturer(RTCVideoCapturer(delegate: fan), didCapture: frame(7))
        XCTAssertEqual(fan.count, 2)
        XCTAssertEqual(copy.events, [7])
    }

    func testADetachedCopyGetsNoMoreFramesAndTheMainSourceCannotBeDetached() {
        let main = Recorder()
        let copy = Recorder()
        let fan = CapturerFan(first: main)
        fan.attach(copy)
        fan.detach(copy)
        fan.detach(main)
        fan.capturer(RTCVideoCapturer(delegate: fan), didCapture: frame(3))
        XCTAssertEqual(main.events, [3])
        XCTAssertEqual(copy.events, [])
        XCTAssertEqual(fan.count, 1)
    }

    func testEachCopyHasOneSourceOfItsOwnUntilReleased() {
        let main = WebRTCPeerConnection.newVideoSource()
        let fan = CapturerFan(first: main)
        var made = 0
        let copies = CameraCopySources(fan: fan) {
            made += 1
            return WebRTCPeerConnection.newVideoSource()
        }
        let light = copies.source(for: .light)
        let middle = copies.source(for: .middle)
        XCTAssertNotNil(light)
        XCTAssertNotNil(middle)
        XCTAssertFalse(light === middle)
        XCTAssertFalse(light === main)
        // Asked again (a camera re-enable, a renegotiation): the same source.
        XCTAssertTrue(copies.source(for: .light) === light)
        XCTAssertEqual(made, 2)
        XCTAssertEqual(fan.count, 3)

        copies.release()
        XCTAssertEqual(fan.count, 1)
    }

    func testACopyNoSourceCanBeMadeForIsNil() {
        let fan = CapturerFan(first: Recorder())
        let copies = CameraCopySources(fan: fan) { nil }
        XCTAssertNil(copies.source(for: .middle))
        XCTAssertEqual(fan.count, 1)
    }

    /// On real sources: the light copy's source is asked for a quarter of
    /// the picture, as an encoder adapting down asks; the main source keeps
    /// serving the full capture. On one shared source both tracks would get
    /// the narrowed picture (the control below).
    func testASourceThatNarrowsNarrowsOnlyItsOwnPicture() {
        let mainSource = WebRTCPeerConnection.newVideoSource()
        let fan = CapturerFan(first: mainSource)
        let copies = CameraCopySources(fan: fan) { WebRTCPeerConnection.newVideoSource() }
        guard let lightSource = copies.source(for: .light) else {
            return XCTFail("no copy source")
        }
        lightSource.adaptOutputFormat(toWidth: 320, height: 180, fps: 30)

        let mainTrack = WebRTCPeerConnection.newPreviewVideoTrack(source: mainSource)
        let lightTrack = WebRTCPeerConnection.newPreviewVideoTrack(source: lightSource)
        let mainSeen = SizeRecorder()
        let lightSeen = SizeRecorder()
        mainTrack.add(mainSeen)
        lightTrack.add(lightSeen)
        defer {
            mainTrack.remove(mainSeen)
            lightTrack.remove(lightSeen)
        }

        let capturer = RTCVideoCapturer(delegate: fan)
        for i in 0..<5 {
            fan.capturer(capturer, didCapture: frame(1_000_000_000 + Int64(i) * 33_333_333))
        }

        XCTAssertFalse(mainSeen.sizes.isEmpty)
        XCTAssertFalse(lightSeen.sizes.isEmpty)
        XCTAssertTrue(
            mainSeen.sizes.allSatisfy { $0 == CGSize(width: 1280, height: 720) },
            "main: \(mainSeen.sizes)")
        XCTAssertTrue(
            lightSeen.sizes.allSatisfy { $0.width <= 320 && $0.height <= 180 },
            "light: \(lightSeen.sizes)")
    }

    /// The defect this type closes, on real objects: two tracks on ONE
    /// source both get the picture that source was narrowed to.
    func testControlTwoTracksOnOneSourceBothGetTheNarrowedPicture() {
        let shared = WebRTCPeerConnection.newVideoSource()
        shared.adaptOutputFormat(toWidth: 320, height: 180, fps: 30)
        let mainTrack = WebRTCPeerConnection.newPreviewVideoTrack(source: shared)
        let copyTrack = WebRTCPeerConnection.newPreviewVideoTrack(source: shared)
        let mainSeen = SizeRecorder()
        let copySeen = SizeRecorder()
        mainTrack.add(mainSeen)
        copyTrack.add(copySeen)
        defer {
            mainTrack.remove(mainSeen)
            copyTrack.remove(copySeen)
        }
        let capturer = RTCVideoCapturer(delegate: shared)
        for i in 0..<5 {
            shared.capturer(capturer, didCapture: frame(1_000_000_000 + Int64(i) * 33_333_333))
        }
        XCTAssertFalse(mainSeen.sizes.isEmpty)
        XCTAssertTrue(mainSeen.sizes.allSatisfy { $0.width <= 320 }, "main: \(mainSeen.sizes)")
        XCTAssertTrue(copySeen.sizes.allSatisfy { $0.width <= 320 }, "copy: \(copySeen.sizes)")
    }
}
