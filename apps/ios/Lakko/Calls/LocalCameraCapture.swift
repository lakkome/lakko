// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Foundation
import WebRTC

/// G3-1-video — iOS local-camera capture wrapper.
///
/// Wraps `RTCCameraVideoCapturer` (libwebrtc's
/// `AVCaptureSession`-backed implementation of
/// `RTCVideoCapturer`) with a small lakko-side surface that:
///   - selects the **front** camera by default (`.front`
///     position; falls back to `.back` if no front camera is
///     present, e.g. a hypothetical Mac Catalyst host);
///   - picks a 720p-class format at 30 FPS — wide enough to
///     match the AV1 / VP9 / H.264 negotiation we already
///     ship via `lakko-calls::media` (3a7), narrow enough to
///     keep mobile encode budget sane;
///   - drives frames into a caller-supplied `RTCVideoSource`
///     via libwebrtc's standard
///     `RTCVideoCapturerDelegate` channel (the source is
///     itself the delegate).
///
/// Lifecycle:
///   1. Caller asks `WebRTCPeerConnection.newVideoSource()`
///      for a fresh source.
///   2. Caller constructs a `LocalCameraCapture(source:)`.
///   3. Caller `await capture.start()`.
///   4. Frames flow into the `RTCVideoSource` ; the
///      `RTCVideoTrack` added via
///      `peerConnection.addLocalVideoTrack(source)` then sees
///      them.
///   5. On call teardown, `await capture.stop()`.
///
/// `start` / `stop` are async because
/// `RTCCameraVideoCapturer.startCapture` / `.stopCapture` are
/// completion-handler-based; we wrap them in
/// `withCheckedContinuation` so the call site can keep its
/// linear `async` shape.
///
/// **Permission contract:** the host app MUST carry
/// `NSCameraUsageDescription` in its Info.plist (added to
/// `apps/ios/project.yml` in this sprint) AND
/// `AVCaptureDevice.requestAccess(for: .video)` MUST have
/// resolved to `true` before `start()` is called. The
/// capturer itself does not request permission — that's the
/// UI's responsibility.
public final class LocalCameraCapture {

    /// The libwebrtc capturer instance. Strong-ref'd here so
    /// it outlives the `start` continuation.
    private let capturer: RTCCameraVideoCapturer

    /// Selected capture device (front camera by default).
    /// `nil` only if the device has no cameras — the caller
    /// should have already gated on
    /// `Self.hasUsableCamera()` before constructing.
    /// A `var` because ``flip()`` swaps it for the one facing the other way.
    private var device: AVCaptureDevice?

    /// Selected format — typically 1280x720 or the closest
    /// resolution the device supports. Re-picked on a flip: the two cameras
    /// of one phone rarely offer the same format list.
    private var format: AVCaptureDevice.Format?

    /// Selected FPS — 30 by default, capped at the format's
    /// max supported framerate.
    private var targetFps: Int

    /// Whether the capturer is running and whether the owner has let this
    /// capture go (``CameraCaptureGate``): `stop()` calls libwebrtc's stop
    /// API only for a capturer that started (calling stop without a prior
    /// start crashes in debug builds of `WebRTC.framework`), and a start
    /// that completes after the owner's `stop()` is stopped again at once.
    ///
    /// `start`, `stop`, `flip` and `use` are not confined to one actor and
    /// the owner's `stop()` runs in a task of its own, so the gate is read
    /// and written under a lock.
    private var gate = CameraCaptureGate()
    private let gateLock = NSLock()

    private func withGate<T>(_ body: (inout CameraCaptureGate) -> T) -> T {
        gateLock.lock()
        defer { gateLock.unlock() }
        return body(&gate)
    }

    /// The camera the person chose for this call in the call's settings
    /// (`docs/ux/calls.md` section 4.1), by its `uniqueID`, or `nil` for the
    /// default (the front camera). Read when a capture is built, so a choice
    /// made while the camera is off holds for when it starts. Cleared when
    /// the call is over.
    public static var preferredDeviceId: String?

    /// The capture that is running now, if any, so a choice in the call's
    /// settings can move it to another camera at once. One call at a time
    /// across the app, so there is at most one.
    public private(set) static weak var active: LocalCameraCapture?

    /// The camera this capture is on, by its `uniqueID`.
    public var deviceId: String? { device?.uniqueID }

    /// Construct with the source frames should be delivered
    /// into. The source's lifetime must outlive this
    /// capturer — the standard pattern is to hold both on the
    /// `CallSession` and tear them down together.
    ///
    /// Returns `nil` if no usable camera is present on this
    /// device (e.g. iOS Simulator pre-iOS-15 without the
    /// virtual camera, or a hypothetical Mac Catalyst host
    /// running without a webcam).
    public init?(source: RTCVideoSource) {
        self.capturer = RTCCameraVideoCapturer(delegate: source)
        let devices = RTCCameraVideoCapturer.captureDevices()
        let chosen = Self.preferredDeviceId.flatMap { id in
            devices.first { $0.uniqueID == id }
        }
        let front = devices.first { $0.position == .front }
        let back = devices.first { $0.position == .back }
        guard let device = chosen ?? front ?? back ?? devices.first else {
            return nil
        }
        self.device = device
        let picked = Self.pickFormat(for: device)
        self.format = picked.format
        self.targetFps = picked.fps
    }

    /// Move this capture to the camera `deviceId` names, or back to the
    /// default (the front camera) for `nil`: on the same track and the same
    /// sender, so the SFrame encryptor attached to it keeps its counter and
    /// only the frames change. A camera that is not there, or the one already
    /// in use, changes nothing.
    public func use(deviceId: String?) async {
        let devices = RTCCameraVideoCapturer.captureDevices()
        let wanted = deviceId.flatMap { id in devices.first { $0.uniqueID == id } }
            ?? devices.first { $0.position == .front }
            ?? devices.first
        guard let next = wanted, next.uniqueID != device?.uniqueID else { return }
        let picked = Self.pickFormat(for: next)
        guard let format = picked.format else { return }
        let wasRunning = withGate { $0.running }
        await halt()
        self.device = next
        self.format = format
        self.targetFps = picked.fps
        // The call may have ended while the capturer stopped: `start()`
        // refuses a capture its owner has let go, so the camera stays off.
        guard wasRunning else { return }
        do {
            try await start()
        } catch {
            NSLog("LocalCameraCapture: camera change restart failed: %@", String(describing: error))
        }
    }

    /// The 720p-class format and frame rate to use for a device.
    ///
    /// Every supported format is scored by its distance from 1280x720 and the
    /// closest wins, which works equally well on devices that offer exact 720p
    /// (most iPhones) and those that only offer 1080p / 480p (older iPads). The
    /// rate is capped at 30 and never above the format's own maximum —
    /// libwebrtc's `startCapture` asserts on `fps > maxFrameRate`.
    private static func pickFormat(
        for device: AVCaptureDevice
    ) -> (format: AVCaptureDevice.Format?, fps: Int) {
        let target: (w: Int32, h: Int32) = (1280, 720)
        let formats = RTCCameraVideoCapturer.supportedFormats(for: device)
        let chosen = formats.min { a, b in
            formatScore(a, target: target) < formatScore(b, target: target)
        }
        var cappedFps = 30
        if let chosen = chosen {
            let maxFps = chosen.videoSupportedFrameRateRanges
                .map { Int($0.maxFrameRate) }
                .max() ?? 30
            cappedFps = min(30, maxFps)
        }
        return (chosen, cappedFps)
    }

    /// `true` when this host has a camera on the other side to flip to
    /// (`docs/ux/calls.md` section 4.1 — the flip button lives on the self-view
    /// and must not be drawn when there is nothing to flip to).
    public var canFlip: Bool {
        Self.otherFacingDevice(than: device) != nil
    }

    /// Swap front camera for back, or back for front, on the SAME video source —
    /// so the track, the sender and any SFrame encryptor attached to it keep
    /// running and only the frames change. A host with one camera is a no-op.
    ///
    /// The capture is stopped and restarted, which is the only way libwebrtc's
    /// camera capturer changes device; the gap is the few frames an
    /// `AVCaptureSession` reconfiguration takes.
    public func flip() async {
        guard let next = Self.otherFacingDevice(than: device) else { return }
        let picked = Self.pickFormat(for: next)
        guard let format = picked.format else { return }
        await halt()
        self.device = next
        self.format = format
        self.targetFps = picked.fps
        // As in `use(deviceId:)`: a call that ended meanwhile is not started
        // again.
        do {
            try await start()
        } catch {
            NSLog("LocalCameraCapture: flip restart failed: %@", String(describing: error))
        }
    }

    /// The first capture device facing the other way, or `nil` on a host with
    /// cameras on only one side.
    private static func otherFacingDevice(
        than current: AVCaptureDevice?
    ) -> AVCaptureDevice? {
        guard let current else { return nil }
        let wanted: AVCaptureDevice.Position = current.position == .front ? .back : .front
        return RTCCameraVideoCapturer.captureDevices().first { $0.position == wanted }
    }

    /// Score a format by Manhattan distance from the target
    /// resolution. Lower is better. Pure helper for `min(by:)`.
    private static func formatScore(
        _ fmt: AVCaptureDevice.Format,
        target: (w: Int32, h: Int32)
    ) -> Int32 {
        let dims = CMVideoFormatDescriptionGetDimensions(fmt.formatDescription)
        return abs(dims.width - target.w) + abs(dims.height - target.h)
    }

    /// `true` if at least one capture device exists on this
    /// host. The call UI should hide the "video" toggle when
    /// this returns `false`.
    public static func hasUsableCamera() -> Bool {
        !RTCCameraVideoCapturer.captureDevices().isEmpty
    }

    /// Begin delivering frames into the source. Throws
    /// `CaptureError.noDevice` on a device without a camera,
    /// `CaptureError.noFormat` when the device reports zero
    /// formats (which would mean a broken iOS image), and
    /// `CaptureError.startFailed` if libwebrtc itself returns
    /// an error.
    public func start() async throws {
        guard let device = device else { throw CaptureError.noDevice }
        guard let format = format else { throw CaptureError.noFormat }
        // The owner already let this capture go (the call ended before the
        // camera was asked to start): the camera is not turned on at all.
        guard withGate({ $0.mayStart }) else { throw CaptureError.released }
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            capturer.startCapture(
                with: device,
                format: format,
                fps: targetFps
            ) { error in
                if let error = error {
                    cont.resume(throwing: CaptureError.startFailed(underlying: error))
                } else {
                    cont.resume()
                }
            }
        }
        // The owner's `stop()` may have run while the capturer was starting.
        // It found nothing to stop then, so the camera is stopped here: a
        // call that is over never keeps it.
        guard withGate({ $0.started() }) else {
            await stopCapturer()
            throw CaptureError.released
        }
        Self.active = self
    }

    /// Stop the underlying capture. Idempotent — repeated
    /// `stop` calls return immediately without throwing. The
    /// caller is allowed to drop the `LocalCameraCapture`
    /// without explicitly stopping (the deinit covers it),
    /// but the explicit stop is preferred so the caller can
    /// `await` the async tear-down for deterministic test
    /// ordering.
    ///
    /// Final: a capture that was stopped by its owner never starts again,
    /// including a `start()`, a `flip()` or a `use(deviceId:)` that was in
    /// flight when this was called. A camera turned on again later is a new
    /// `LocalCameraCapture`.
    public func stop() async {
        guard withGate({ $0.release() }) else { return }
        await stopCapturer()
    }

    /// Stop the capturer between two cameras (`flip`, `use`): the capture
    /// stays its owner's and may start again.
    private func halt() async {
        guard withGate({ $0.halt() }) else { return }
        await stopCapturer()
    }

    private func stopCapturer() async {
        if Self.active === self { Self.active = nil }
        await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
            capturer.stopCapture {
                cont.resume()
            }
        }
    }

    deinit {
        // Safety net: if the consumer forgot to call stop(),
        // tell libwebrtc to release the camera here.
        // stopCapture with no completion-handler is the
        // documented blocking shape; safe to call from deinit
        // when the capturer is running.
        if gate.running {
            capturer.stopCapture()
        }
    }

    /// Errors `start()` can throw. Mirrored on the Android
    /// side as `CameraVideoCapture.CaptureError`.
    public enum CaptureError: Error, Equatable {
        /// No `AVCaptureDevice` available on this host. Gate
        /// on `LocalCameraCapture.hasUsableCamera()` to
        /// avoid hitting this path in the UI.
        case noDevice
        /// The chosen device reports zero supported formats.
        case noFormat
        /// libwebrtc's underlying `startCapture` failed. The
        /// associated value carries the original
        /// `NSError` for diagnostics; tests compare on the
        /// enum case only (via `.startFailed`) since the
        /// underlying error is platform-version-dependent.
        case startFailed(underlying: Error)
        /// The owner stopped this capture (the call ended) before or while
        /// the camera started. Not a failure of the camera: nothing is shown
        /// for it.
        case released

        public static func == (lhs: CaptureError, rhs: CaptureError) -> Bool {
            switch (lhs, rhs) {
            case (.noDevice, .noDevice): return true
            case (.noFormat, .noFormat): return true
            case (.startFailed, .startFailed): return true
            case (.released, .released): return true
            default: return false
            }
        }
    }
}

/// Whether a camera capturer is running, and whether its owner has let the
/// capture go.
///
/// The capturer starts and stops through completion handlers, and the owner
/// (a call session) stops it from a task of its own when the call ends. So a
/// stop can arrive while a start is still in flight: there is nothing running
/// to stop yet, and without this the start would then complete and leave the
/// camera on for a call that is over. `release()` is remembered, and the
/// start that completes after it is told to stop again.
struct CameraCaptureGate: Equatable {
    /// The capturer is delivering frames.
    private(set) var running = false
    /// The owner called `stop()`: this capture never starts again.
    private(set) var released = false

    /// Whether a start may be asked of the capturer at all.
    var mayStart: Bool { !released }

    /// The capturer reported that it started. `false` when the owner let the
    /// capture go meanwhile: the caller stops the capturer again.
    mutating func started() -> Bool {
        if released { return false }
        running = true
        return true
    }

    /// The capturer is being stopped between two cameras. Returns whether
    /// there is a running capturer to stop.
    mutating func halt() -> Bool {
        let wasRunning = running
        running = false
        return wasRunning
    }

    /// The owner stops the capture for good. Returns whether there is a
    /// running capturer to stop now.
    mutating func release() -> Bool {
        released = true
        return halt()
    }
}
