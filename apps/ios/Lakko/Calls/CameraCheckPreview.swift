// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import WebRTC

/// The live self-view of the camera check (`docs/ux/calls.md` section 3.2).
///
/// A camera that runs before any call exists. The check is the one screen where
/// the user is looking at themselves rather than at anybody else, and it happens
/// *before* the offer goes out — so it cannot borrow the call's capture, which
/// is built by `CallSession.dial`. This owns a capturer, a source and a
/// peer-connection-free track for exactly as long as the check is on screen, and
/// tears all three down when it leaves.
///
/// Turning the camera off on the check stops the capture rather than merely
/// hiding the view: a running camera with nothing showing it is a privacy
/// surprise, and the green indicator would say so.
@MainActor
final class CameraCheckPreview: ObservableObject {

    /// The track the check renders, or `nil` while the camera is off or
    /// starting. `LocalVideoPreviewView` takes it directly.
    @Published private(set) var track: RTCVideoTrack?

    private var capture: LocalCameraCapture?
    private var source: RTCVideoSource?
    /// A start that is past its tap. Two taps in a row must not leave two
    /// capturers running.
    private var starting = false

    /// Begin capturing. Idempotent; a no-op on a device with no camera, which
    /// never reaches the check anyway (``CallMediaChoice/startSurface(for:hasCamera:)``
    /// sends that device straight to a voice call).
    func start() {
        guard track == nil, !starting else { return }
        guard LocalCameraCapture.hasUsableCamera() else { return }
        let source = WebRTCPeerConnection.newVideoSource()
        guard let capture = LocalCameraCapture(source: source) else { return }
        starting = true
        self.source = source
        self.capture = capture
        Task { @MainActor [weak self] in
            do {
                try await capture.start()
            } catch {
                NSLog(
                    "CameraCheckPreview: camera start failed: %@",
                    String(describing: error))
                self?.stop()
                return
            }
            guard let self, self.capture === capture else { return }
            self.starting = false
            self.track = WebRTCPeerConnection.newPreviewVideoTrack(source: source)
        }
    }

    /// `docs/ux/calls.md` section 4.1 — is there a second camera to flip to?
    var canFlip: Bool { capture?.canFlip ?? false }

    /// Swap front for back on the live preview.
    func flip() async {
        guard let capture else { return }
        await capture.flip()
    }

    /// Stop capturing and drop the track. Safe to call repeatedly, and from
    /// `onDisappear`.
    func stop() {
        starting = false
        track = nil
        source = nil
        guard let capture else { return }
        self.capture = nil
        Task { await capture.stop() }
    }

    // No `deinit` teardown: `stop()` is main-actor isolated and `deinit` is
    // not. `LocalCameraCapture`'s own `deinit` already releases the camera when
    // the last reference goes, which is the case a forgotten `stop()` would
    // leave — so the safety net exists exactly once, where it can run.
}
