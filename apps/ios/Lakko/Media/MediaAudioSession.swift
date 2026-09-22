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
    /// Every mutation goes through ``CallAudioSession`` rather than
    /// `AVAudioSession` directly. WebRTC's `RTCAudioSession` is a locking proxy
    /// whose header forbids direct setters, and a media owner shares the
    /// process with the call stack's one long-lived audio device module — so a
    /// category applied behind its back outlives this recording and breaks the
    /// next call's microphone.
    @discardableResult
    public static func begin(_ use: Use, callActive: Bool) throws -> Bool {
        guard !callActive, !CallAudioSession.callOwnsSession else { return false }
        switch use {
        case .record:
            guard CallAudioSession.apply(
                category: .playAndRecord,
                mode: .default,
                options: [.defaultToSpeaker, .allowBluetoothHFP]
            ) else {
                throw MediaAudioSessionError.categoryRefused
            }
        case .playback:
            // A voice message the user tapped plays even in silent mode, like
            // any other deliberate playback.
            guard CallAudioSession.apply(
                category: .playback,
                mode: .spokenAudio,
                options: [.duckOthers]
            ) else {
                throw MediaAudioSessionError.categoryRefused
            }
        }
        CallAudioSession.setActive(true)
        owner = use
        return true
    }

    /// Release the session if `use` still owns it, restoring the `.ambient`
    /// shape the call cue controller relies on.
    public static func end(_ use: Use) {
        guard owner == use else { return }
        owner = nil
        CallAudioSession.setActive(false)
        CallAudioSession.applyCuePosture()
    }
}

/// Why a media owner could not take the session.
public enum MediaAudioSessionError: Error {
    /// The system refused the category this use needs.
    case categoryRefused
}
