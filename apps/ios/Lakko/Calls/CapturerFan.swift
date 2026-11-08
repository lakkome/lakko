// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import WebRTC

/// One camera capturer feeding several video sources: what gives each of the
/// group call's camera senders (``CameraSender``) a source of its own.
///
/// ## Why not one source for every sender
///
/// libwebrtc lets an encoder ask its source for fewer pixels (bandwidth, the
/// quality scaler, the frames dropped at a low start bitrate). A source fed to
/// several tracks serves the smallest picture any of its encoders asked for:
/// its broadcaster takes the minimum of every sink's wanted pixel count, and
/// the source's adapter scales the frames before any sender sees them. With
/// the main, middle and light senders on one source, the adaptation at the
/// low start bitrate held all three down for the whole call (seen on the
/// Android client in the call of 2026-11-07: the main sender at 480x270 while
/// it sent about 2.5 Mbit/s, the middle one at 240x135). The iOS client had
/// the same shape until this type.
///
/// Here each copy gets an `RTCVideoSource` of its own and the capturer's
/// frames are handed to every source in turn, so an encoder narrows only its
/// own source. Each sender still scales its source down by its own
/// encoding's `scaleResolutionDownBy`.
///
/// ## Threading
///
/// Frames arrive on the capturer's queue; delegates are attached from the
/// call's thread. The list is read and written under a lock and copied out
/// before the frame is handed on, so a delegate is never called under it. A
/// frame is handed to each delegate synchronously: an `RTCVideoSource` makes
/// its adapted copy before returning.
///
/// `RTCVideoCapturer.delegate` is weak, so whoever installs a fan as the
/// capturer's delegate must hold it (``LocalCameraCapture`` does). The fan
/// holds its delegates strongly.
///
/// ## The camera background
///
/// Each capturer frame is handed to the ``CameraFrameStage`` once, before
/// any delegate sees it (ADR-0089 section 2): every copy and the self view
/// get the same processed frame, and a frame the stage drops reaches no
/// source at all. While a background is selected the stage never answers
/// with the raw camera frame.
final class CapturerFan: NSObject, RTCVideoCapturerDelegate {
    private let lock = NSLock()
    private var delegates: [RTCVideoCapturerDelegate]
    private let stage: CameraFrameStage?

    init(first: RTCVideoCapturerDelegate, stage: CameraFrameStage? = nil) {
        delegates = [first]
        self.stage = stage
    }

    /// The number of delegates frames are handed to.
    var count: Int {
        lock.lock()
        defer { lock.unlock() }
        return delegates.count
    }

    /// Hand frames to `delegate` too. Attaching one twice does nothing.
    func attach(_ delegate: RTCVideoCapturerDelegate) {
        lock.lock()
        defer { lock.unlock() }
        guard !delegates.contains(where: { $0 === delegate }) else { return }
        delegates.append(delegate)
    }

    /// Stop handing frames to `delegate`. The first one is never detached.
    func detach(_ delegate: RTCVideoCapturerDelegate) {
        lock.lock()
        defer { lock.unlock() }
        guard let first = delegates.first, first !== delegate else { return }
        delegates.removeAll { $0 === delegate }
    }

    func capturer(_ capturer: RTCVideoCapturer, didCapture frame: RTCVideoFrame) {
        let out: RTCVideoFrame
        if let stage {
            guard let processed = stage.process(frame) else { return }
            out = processed
        } else {
            out = frame
        }
        lock.lock()
        let now = delegates
        lock.unlock()
        for delegate in now {
            delegate.capturer(capturer, didCapture: out)
        }
    }
}

/// The sources the camera's copies are sent from, one per copy, each fed the
/// capturer's frames through the ``CapturerFan``. Made on first ask and kept
/// for the capture's life, so the copies' sources live with the capture: a
/// mid-call camera re-enable (a track switched back on) and a camera flip
/// keep the same capturer, the same fan and the same sources.
final class CameraCopySources {
    private let fan: CapturerFan
    private let makeSource: @MainActor () -> RTCVideoSource?
    private let lock = NSLock()
    private var sources: [CameraCopyKind: RTCVideoSource] = [:]

    init(fan: CapturerFan, makeSource: @escaping @MainActor () -> RTCVideoSource?) {
        self.fan = fan
        self.makeSource = makeSource
    }

    /// The source the `kind` copy of the camera is sent from: a source of its
    /// own, fed the same frames as the main one, so the copy's encoder
    /// narrows only its own picture. `nil` when no source can be made. Asked
    /// on the call's thread, where the copies' senders are added.
    @MainActor
    func source(for kind: CameraCopyKind) -> RTCVideoSource? {
        lock.lock()
        defer { lock.unlock() }
        if let existing = sources[kind] { return existing }
        guard let made = makeSource() else { return nil }
        sources[kind] = made
        fan.attach(made)
        return made
    }

    /// Stop feeding the copies' sources and let them go. A track still on
    /// one holds a reference of its own.
    func release() {
        lock.lock()
        let all = Array(sources.values)
        sources.removeAll()
        lock.unlock()
        for source in all { fan.detach(source) }
    }
}
