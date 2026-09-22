// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AVFoundation
import Foundation
import WebRTC
import os

/// The one place this app changes `AVAudioSession` while WebRTC is loaded.
///
/// `RTCAudioSession`'s own header states the contract:
///
/// > Proxy class for AVAudioSession that adds a locking mechanism similar to
/// > AVCaptureDevice. This is used to that interleaving configurations between
/// > WebRTC and the application layer are avoided.
/// > Callers should not call setters on AVAudioSession directly.
///
/// Every direct `AVAudioSession.sharedInstance().setCategory(...)` in the app
/// was a breach of that contract, and the consequence was not theoretical.
/// `WebRTCPeerConnection.sharedFactory` is a process-wide singleton, so one
/// audio device module serves every call and outlives each of them. A category
/// applied behind `RTCAudioSession`'s back leaves its bookkeeping describing a
/// session that no longer exists, and the next call's activation therefore does
/// not re-apply the `.playAndRecord` posture recording needs. Observed on the
/// simulator as `input voice: client stopping after failed start` and zero
/// outbound RTP on every call after the first in a process, with the microphone
/// recovering only on relaunch.
///
/// So: route every mutation through here, and leave the session alone entirely
/// while a call owns it.
enum CallAudioSession {

    private static let log = Logger(subsystem: "me.lakko.calls", category: "audio-session")

    /// Whether WebRTC currently holds the session for a live call.
    ///
    /// `RTCAudioSession.isActive` is documented as "our best guess", which is
    /// the honest reading: it reflects the activations WebRTC knows about. That
    /// is exactly the question a caller wants answered — *may I reshape the
    /// session without stepping on a call* — so the guess is the right oracle
    /// even though it is not an OS-level truth.
    static var callOwnsSession: Bool {
        RTCAudioSession.sharedInstance().isActive
    }

    /// Apply a category through `RTCAudioSession`'s lock.
    ///
    /// Returns `false` when the system refused it; the caller decides whether
    /// that is fatal. Never throws: a cue posture that cannot be applied is a
    /// cosmetic loss, and a call must not fail because a ringtone could not get
    /// the session it preferred.
    @discardableResult
    static func apply(
        category: AVAudioSession.Category,
        mode: AVAudioSession.Mode,
        options: AVAudioSession.CategoryOptions
    ) -> Bool {
        let session = RTCAudioSession.sharedInstance()
        session.lockForConfiguration()
        defer { session.unlockForConfiguration() }
        do {
            try session.setCategory(category, mode: mode, options: options)
            return true
        } catch {
            log.warning(
                "setCategory(\(category.rawValue, privacy: .public)) failed: \(error.localizedDescription, privacy: .public)"
            )
            return false
        }
    }

    /// Activate or deactivate through `RTCAudioSession`, so its activation
    /// count — the thing that decides whether the next activation re-applies
    /// the WebRTC configuration — stays balanced.
    @discardableResult
    static func setActive(_ active: Bool) -> Bool {
        let session = RTCAudioSession.sharedInstance()
        session.lockForConfiguration()
        defer { session.unlockForConfiguration() }
        do {
            try session.setActive(active)
            return true
        } catch {
            log.warning(
                "setActive(\(active, privacy: .public)) failed: \(error.localizedDescription, privacy: .public)"
            )
            return false
        }
    }

    /// The cue posture: cues honour the silent switch and duck other audio
    /// rather than stopping it.
    ///
    /// Skipped outright while a call owns the session. `.ambient` cannot
    /// record, so stamping it onto a live call's session is the defect this
    /// type exists to prevent; and a cue played during a call should come out
    /// of the call's own session anyway.
    static func applyCuePosture() {
        #if os(iOS) || os(tvOS) || os(watchOS)
        guard !callOwnsSession else { return }
        apply(category: .ambient, mode: .default, options: [.mixWithOthers, .duckOthers])
        #endif
    }

    /// One line describing what the session actually is at `moment`.
    ///
    /// Kept because the defect above is invisible from every other vantage
    /// point: the call negotiates, connects, and reports healthy ICE and DTLS
    /// while sending nothing, and only the category tells you why.
    static func describe(_ moment: String) {
        #if os(iOS) || os(tvOS) || os(watchOS)
        let session = AVAudioSession.sharedInstance()
        NSLog(
            "CallAudioSession: %@ category=%@ mode=%@ rtcActive=%@ inputs=%d",
            moment,
            session.category.rawValue,
            session.mode.rawValue,
            callOwnsSession ? "yes" : "no",
            session.availableInputs?.count ?? -1
        )
        #endif
    }
}
