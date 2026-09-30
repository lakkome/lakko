// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Foundation

/// Camera-permission gate every video entry point runs BEFORE it starts a
/// camera: a 1x1 dial with the camera on, a group video call start or join, a
/// call-link join for a video call, and the in-call camera toggle turning the
/// camera on. The camera sibling of ``MicrophonePermission``.
///
/// Without it `RTCCameraVideoCapturer` raises the system prompt at an
/// arbitrary moment inside the capture start, and — once the user has denied
/// it — starts a capture session that delivers no frames: peers see a black
/// tile and this user sees a "camera on" button with nothing behind it.
/// Asking up front turns both into a decision the UI can explain.
enum CameraPermission {

    /// What the system reports about camera authorization.
    enum Status: Equatable {
        case granted
        /// Denied by the user, or restricted (Screen Time / MDM). Both mean
        /// the system will not prompt and capture will not deliver frames.
        case denied
        case undetermined
    }

    /// The copy every call surface shows when the gate refuses.
    /// Rendered in the interface language when it is read.
    static var deniedMessage: String { Localization.text("call.permission.camera.body") }

    /// Grant state now, then — only when undetermined — the system prompt.
    ///
    /// The two ports are injected so the decision is testable without the
    /// real permission database; production callers use the defaults.
    static func ensureGranted(
        status: () -> Status = systemStatus,
        request: () async -> Bool = systemRequest
    ) async -> Bool {
        switch status() {
        case .granted:
            return true
        case .denied:
            // The system never re-prompts after a denial; asking again would
            // resolve `false` immediately and hide why.
            return false
        case .undetermined:
            return await request()
        }
    }

    /// The gate for an entry point that may or may not start the camera. An
    /// audio-only start never reads the permission, so it never prompts for
    /// a camera it will not use.
    static func ensureGranted(
        forVideo video: Bool,
        status: () -> Status = systemStatus,
        request: () async -> Bool = systemRequest
    ) async -> Bool {
        guard video else { return true }
        return await ensureGranted(status: status, request: request)
    }

    static func systemStatus() -> Status {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            return .granted
        case .denied, .restricted:
            return .denied
        case .notDetermined:
            return .undetermined
        @unknown default:
            return .undetermined
        }
    }

    static func systemRequest() async -> Bool {
        await AVCaptureDevice.requestAccess(for: .video)
    }
}
