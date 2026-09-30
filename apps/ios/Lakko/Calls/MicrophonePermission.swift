// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFAudio
import Foundation

/// Record-permission gate every call entry point runs BEFORE it builds a
/// session, dials, accepts or joins.
///
/// Without it the first capture attempt either raises the system prompt at an
/// arbitrary moment inside WebRTC's audio-unit start, or — once the user has
/// denied it — silently produces an audio track that delivers zero samples, so
/// the call "works" and nobody can hear this user. Asking up front turns both
/// into a decision the UI can explain.
enum MicrophonePermission {

    /// What the system reports about record permission.
    enum Status: Equatable {
        case granted
        case denied
        case undetermined
    }

    /// The copy every call surface shows when the gate refuses.
    /// Rendered in the interface language when it is read.
    static var deniedMessage: String { Localization.text("call.permission.microphone.body") }

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

    /// `AVAudioApplication` is the iOS 17+ record-permission surface (the
    /// deployment floor in `project.yml`).
    static func systemStatus() -> Status {
        switch AVAudioApplication.shared.recordPermission {
        case .granted:
            return .granted
        case .denied:
            return .denied
        case .undetermined:
            return .undetermined
        @unknown default:
            return .undetermined
        }
    }

    static func systemRequest() async -> Bool {
        await AVAudioApplication.requestRecordPermission()
    }
}
