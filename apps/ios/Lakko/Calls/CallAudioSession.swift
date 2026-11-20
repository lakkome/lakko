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

    // MARK: - Where a call's sound comes out by default

    /// The category options a call's audio session runs with.
    ///
    /// `loudspeakerByDefault` adds `.defaultToSpeaker`, and that option is
    /// chosen for what it does NOT do: it sends the sound to the loudspeaker
    /// only when nothing else is connected, so a wired headset or a Bluetooth
    /// device takes over by itself. Forcing the output port instead
    /// (`overrideOutputAudioPort(.speaker)`) would play a call out loud over
    /// a headset that is plugged in. Everything else in `base` is kept.
    static func callCategoryOptions(
        base: AVAudioSession.CategoryOptions, loudspeakerByDefault: Bool
    ) -> AVAudioSession.CategoryOptions {
        loudspeakerByDefault
            ? base.union(.defaultToSpeaker)
            : base.subtracting(.defaultToSpeaker)
    }

    /// The whole audio session configuration WebRTC brings a call up with.
    ///
    /// Stated in full rather than amended: the webrtc-sdk build (125.6422.07)
    /// starts its WebRTC configuration as a copy of whatever the shared
    /// session was when the class was first touched, not the record-and-play
    /// voice-call configuration of the upstream build. In Lakko that is the
    /// cue posture (`.ambient`, mixing with others), and `.defaultToSpeaker`
    /// on it is refused (OSStatus -50): on an iPhone on iOS 27.2 no audio unit
    /// came up, so a call had neither microphone nor sound and iOS suspended
    /// the app as soon as it left the screen (2026-11-10). The values are the
    /// upstream build's (play and record, voice chat, Bluetooth headsets)
    /// plus the loudspeaker rule.
    static func callConfiguration(
        loudspeakerByDefault: Bool
    ) -> (category: AVAudioSession.Category, mode: AVAudioSession.Mode,
          options: AVAudioSession.CategoryOptions) {
        (
            .playAndRecord,
            .voiceChat,
            callCategoryOptions(
                base: [.allowBluetoothHFP], loudspeakerByDefault: loudspeakerByDefault)
        )
    }

    /// Decide where the NEXT call's sound comes out when nothing else is
    /// connected (`docs/ux/calls.md` section 4.1).
    ///
    /// A group call plays on the loudspeaker: people join one to talk to a
    /// room of others, with the phone in front of them, and it has no Speaker
    /// control to move the sound with. A one-to-one call keeps the earpiece,
    /// and its Speaker control.
    ///
    /// This changes the configuration WebRTC applies when it brings the audio
    /// up for a call, and nothing about a session that is already running:
    /// the call's owner sets it before opening its peer connection.
    static func configureCallOutput(loudspeakerByDefault: Bool) {
        #if os(iOS)
        let configuration = RTCAudioSessionConfiguration.webRTC()
        let wanted = callConfiguration(loudspeakerByDefault: loudspeakerByDefault)
        guard configuration.category != wanted.category.rawValue
            || configuration.mode != wanted.mode.rawValue
            || configuration.categoryOptions != wanted.options
        else { return }
        configuration.category = wanted.category.rawValue
        configuration.mode = wanted.mode.rawValue
        configuration.categoryOptions = wanted.options
        RTCAudioSessionConfiguration.setWebRTC(configuration)
        NSLog(
            "CallAudioSession: call output default=%@",
            loudspeakerByDefault ? "loudspeaker" : "earpiece")
        #endif
    }

    /// A call that is already running now has video.
    ///
    /// Any call with video plays on the loudspeaker unless a headset or a
    /// Bluetooth device is connected: a person watching a picture is not
    /// holding the phone to their ear, and there is no Speaker control to
    /// move the sound with. Applied to the live session through
    /// `RTCAudioSession`'s lock, and recorded for the next activation.
    static func callGainedVideo() {
        #if os(iOS)
        configureCallOutput(loudspeakerByDefault: true)
        guard callOwnsSession else { return }
        let session = AVAudioSession.sharedInstance()
        guard session.category == .playAndRecord,
              !session.categoryOptions.contains(.defaultToSpeaker)
        else { return }
        apply(
            category: session.category,
            mode: session.mode,
            options: callCategoryOptions(base: session.categoryOptions, loudspeakerByDefault: true))
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
