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
    private let device: AVCaptureDevice?

    /// Selected format — typically 1280x720 or the closest
    /// resolution the device supports.
    private let format: AVCaptureDevice.Format?

    /// Selected FPS — 30 by default, capped at the format's
    /// max supported framerate.
    private let targetFps: Int

    /// Tracks whether `start()` has actually started capture
    /// so `stop()` knows whether to call libwebrtc's stop API
    /// (calling stop without a prior start crashes in debug
    /// builds of `WebRTC.framework`).
    private var didStart: Bool = false

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
        let front = devices.first { $0.position == .front }
        let back = devices.first { $0.position == .back }
        guard let device = front ?? back ?? devices.first else {
            return nil
        }
        self.device = device

        // Pick a 720p-ish format. We score every supported
        // format by `(width × height)` distance to 1280×720
        // and take the closest match; this works equally well
        // on devices that offer exact 720p (most iPhones) and
        // those that only offer 1080p / 480p (older iPads).
        let target: (w: Int32, h: Int32) = (1280, 720)
        let formats = RTCCameraVideoCapturer.supportedFormats(for: device)
        let chosen = formats.min { a, b in
            Self.formatScore(a, target: target) <
                Self.formatScore(b, target: target)
        }
        self.format = chosen

        // Cap at 30 FPS, but never above the format's max
        // — libwebrtc's startCapture asserts on
        // `fps > maxFrameRate`.
        var cappedFps = 30
        if let chosen = chosen {
            let maxFps = chosen.videoSupportedFrameRateRanges
                .map { Int($0.maxFrameRate) }
                .max() ?? 30
            cappedFps = min(30, maxFps)
        }
        self.targetFps = cappedFps
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
        didStart = true
    }

    /// Stop the underlying capture. Idempotent — repeated
    /// `stop` calls return immediately without throwing. The
    /// caller is allowed to drop the `LocalCameraCapture`
    /// without explicitly stopping (the deinit covers it),
    /// but the explicit stop is preferred so the caller can
    /// `await` the async tear-down for deterministic test
    /// ordering.
    public func stop() async {
        guard didStart else { return }
        didStart = false
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
        // when didStart is true.
        if didStart {
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

        public static func == (lhs: CaptureError, rhs: CaptureError) -> Bool {
            switch (lhs, rhs) {
            case (.noDevice, .noDevice): return true
            case (.noFormat, .noFormat): return true
            case (.startFailed, .startFailed): return true
            default: return false
            }
        }
    }
}
