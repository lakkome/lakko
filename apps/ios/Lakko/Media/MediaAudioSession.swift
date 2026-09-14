// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Foundation
import os

/// ADR-0047 — the one place voice / video-note capture and voice playback
/// touch `AVAudioSession`.
///
/// Three owners share the process's single audio session:
///
///   * a live call — WebRTC configures `.playAndRecord` / voice-chat itself and
///     must never be reconfigured underneath; recording is disabled during a
///     call (UX spec §6), and playback during one plays through the call's
///     session without touching it;
///   * `CallSoundController`, which leaves the session `.ambient` +
///     `.mixWithOthers` for its UI cues;
///   * media messages, here.
///
/// So a media owner takes the session only when no call is live, and hands it
/// back in the `.ambient` shape `CallSoundController` expects.
@MainActor
public enum MediaAudioSession {

    public enum Use {
        case record
        case playback
    }

    private static let log = Logger(subsystem: "me.lakko.media", category: "audio-session")
    private static var owner: Use?

    /// Configure and activate the session for `use`. Returns `false` (and
    /// leaves the session alone) while a call is live; throws when the system
    /// refuses the category, which the caller treats as a failed capture.
    @discardableResult
    public static func begin(_ use: Use, callActive: Bool) throws -> Bool {
        guard !callActive else { return false }
        let session = AVAudioSession.sharedInstance()
        switch use {
        case .record:
            try session.setCategory(
                .playAndRecord,
                mode: .default,
                options: [.defaultToSpeaker, .allowBluetoothHFP]
            )
        case .playback:
            // A voice message the user tapped plays even in silent mode, like
            // any other deliberate playback.
            try session.setCategory(.playback, mode: .spokenAudio, options: [.duckOthers])
        }
        try session.setActive(true)
        owner = use
        return true
    }

    /// Release the session if `use` still owns it, restoring the `.ambient`
    /// shape the call cue controller relies on.
    public static func end(_ use: Use) {
        guard owner == use else { return }
        owner = nil
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setActive(false, options: [.notifyOthersOnDeactivation])
            try session.setCategory(.ambient, mode: .default, options: [.mixWithOthers, .duckOthers])
        } catch {
            log.warning("media audio session release failed: \(error.localizedDescription, privacy: .public)")
        }
    }
}
