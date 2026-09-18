// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import Foundation

/// The call screen's shape, in plain values: which controls are on the screen,
/// which of them can be pressed right now, what the one status line says, and
/// whether the whole row hides itself (`docs/ux/calls.md` sections 4, 4.1 and
/// 4.1.1).
///
/// ## Why this is not in the view
///
/// Every rule the redesign is about — no More menu in a 1x1 call, speaker always
/// present, the off state being the loud one, the status line never saying two
/// things at once — is a rule about *values*, and a SwiftUI body is the one
/// place in this app a test cannot reach. So the rules live here as pure
/// functions over an enum, `CallScreen` reads them, and a regression fails in
/// `LakkoTests` rather than on someone's phone.

// MARK: - The controls

/// One control in the call's bottom row.
public enum CallControl: String, CaseIterable, Identifiable, Sendable {
    /// Move the sound to the speaker and back.
    case speaker
    /// Mute and unmute the microphone.
    case microphone
    /// Start and stop sending the camera.
    case camera
    /// A group call's participants / chat / settings, behind one button.
    /// Never present in a 1x1 call (`docs/ux/calls.md` section 4.1).
    case overflow
    /// Hang up.
    case end

    public var id: String { rawValue }

    /// The one short word under the circle. It names the action, never the
    /// state, so it does not change size or meaning when the state flips
    /// (section 4.1.1).
    public var label: String {
        switch self {
        case .speaker: return "Speaker"
        case .microphone: return "Mute"
        case .camera: return "Camera"
        case .overflow: return "More"
        case .end: return "End"
        }
    }
}

/// How a control's circle is drawn. The off state is the loud one, because
/// being muted is the thing a person needs to see at a glance (section 4.1.1).
public enum CallControlFace: String, Equatable, Sendable {
    /// The thing is running: a translucent fill with a light icon.
    case outline
    /// The thing is off: solid white with a dark, slashed icon.
    case solid
}

// MARK: - The screen's shape

/// What the call screen is showing behind the controls.
public enum CallScreenLayout: String, Equatable, Sendable {
    /// A portrait: the other person's avatar, their name, one status line.
    /// The whole screen of a voice call, and of a video call that has not
    /// received a frame yet.
    case portrait
    /// The other person's video, full screen.
    case video
    /// More than one other person: the tile grid.
    case gallery
}

/// The layout, control set, enablement, status line and auto-hide rule.
public enum CallScreenPresentation {

    // MARK: Measurements (section 4.1.1)

    /// Every control is a circle of this size.
    public static let controlDiameter: CGFloat = 64
    /// The end button is bigger, and the only red thing on the screen.
    public static let endDiameter: CGFloat = 72
    /// The even gap between controls.
    public static let controlGap: CGFloat = 16
    /// The end button is set apart by twice that gap.
    public static var endGap: CGFloat { controlGap * 2 }
    /// A touch target never falls below this, however narrow the screen.
    public static let minimumTouchTarget: CGFloat = 44
    /// A control that cannot be used is dimmed to this, never hidden.
    public static let disabledOpacity: Double = 0.4
    /// How long a connected video call waits before fading the row out.
    public static let autoHideAfter: TimeInterval = 5

    // MARK: Layout

    /// Is there a remote picture to fill the screen with?
    ///
    /// **Both halves are required, and the second is the one that was missing.**
    /// A remote video track handle arrives with the SDP — the moment a peer's
    /// renegotiation adds an `m=video` line — so `trackPresent` alone answers
    /// "the other side says it will send video", not "there is something to
    /// look at". A live call found the difference: iOS switched to the
    /// full-screen video layout on the handle and showed a dark surface for over
    /// a minute, with the controls faded out over it.
    ///
    /// `framesArrived` comes from ``RemoteVideoFrameProbe``, which is the
    /// picture itself and nothing else.
    public static func hasRemotePicture(trackPresent: Bool, framesArrived: Bool) -> Bool {
        trackPresent && framesArrived
    }

    /// What the screen draws behind the controls.
    ///
    /// - Parameters:
    ///   - remoteCount: how many other people are on the call.
    ///   - remotePicture: a remote video frame has actually been rendered —
    ///     ``hasRemotePicture(trackPresent:framesArrived:)``, never a bare track
    ///     handle. With no picture the portrait is the honest thing to show.
    public static func layout(remoteCount: Int, remotePicture: Bool) -> CallScreenLayout {
        if remoteCount > 1 { return .gallery }
        return remotePicture ? .video : .portrait
    }

    /// The row, left to right (`docs/ux/calls.md` section 4.1): Speaker, Mute,
    /// Camera, End — the same four on **both** call kinds.
    ///
    /// **The camera button is on a voice call too**, and pressing it turns the
    /// call into a video one. People start a call with their voice and decide to
    /// show something a minute later; every messenger allows that, and dropping
    /// it to keep a voice call's row short would be a capability lost to
    /// tidiness. This function briefly did drop it, and the maintainer put it
    /// back — which is why there is no `videoCall` parameter here any more: the
    /// row does not depend on what the call is currently carrying, only on
    /// whether it is a group.
    ///
    /// A 1x1 call has no overflow button at all — the whole set is on the
    /// screen. A group call keeps participants and chat behind one, but mute,
    /// camera, speaker and end stay in the row.
    public static func controls(layout: CallScreenLayout) -> [CallControl] {
        if layout == .gallery {
            return [.speaker, .microphone, .camera, .overflow, .end]
        }
        return [.speaker, .microphone, .camera, .end]
    }

    /// Whether a control can be pressed in this phase. While the call is only
    /// ringing out there is nothing to mute yet, so the table in section 4 says
    /// "only End is active" — the rest stay on the screen at
    /// ``disabledOpacity``, never hidden.
    public static func isEnabled(_ control: CallControl, phase: CallUiState.CallPhase) -> Bool {
        if control == .end { return true }
        switch phase {
        case .calling, .ended:
            return false
        case .cameraCheck, .connecting, .connected, .reconnecting:
            return true
        }
    }

    /// The circle's face. `on` means the thing the button controls is running:
    /// the microphone is live, the camera is sending, the sound is on the
    /// loudspeaker.
    ///
    /// The rule is not "on is quiet" but **the loud face marks the state a
    /// glance must catch**. For the microphone and the camera that is being
    /// off: a muted call where the user thinks they are heard is the failure
    /// worth shouting about. For the speaker it is the opposite — the earpiece
    /// is the resting state and the room hearing the call is the thing to
    /// notice, which is also where every phone's own call screen puts its
    /// highlight.
    public static func face(_ control: CallControl, on: Bool) -> CallControlFace {
        switch control {
        case .microphone, .camera:
            return on ? .outline : .solid
        case .speaker:
            return on ? .solid : .outline
        case .overflow, .end:
            return .outline
        }
    }

    /// The controls fade out on a connected call that has something to look at
    /// underneath, and never on a voice call — hiding them over a portrait only
    /// makes the user tap (section 4.2).
    ///
    /// "Something to look at" is load-bearing, not a turn of phrase. Because the
    /// layout is now keyed on a rendered frame rather than a track handle
    /// (``hasRemotePicture(trackPresent:framesArrived:)``), a call whose peer
    /// announced video but whose picture never arrives stays a portrait and so
    /// keeps its controls — including, decisively, the End button. Fading a row
    /// out over a blank screen is how a user ends up unable to hang up.
    public static func autoHides(layout: CallScreenLayout, phase: CallUiState.CallPhase) -> Bool {
        guard case .connected = phase else { return false }
        return layout == .video || layout == .gallery
    }
}

// MARK: - The one status line

/// The single line under the name: the state, or the running time, never both
/// and never repeated in a floating badge (`docs/ux/calls.md` section 4).
public enum CallStatusText {

    /// What the line says, or `nil` when the screen is not a call screen yet
    /// (the camera check has its own words).
    ///
    /// - Parameter durationMs: how long media has been flowing. Read only in
    ///   ``CallUiState/CallPhase/connected``, where the line IS the clock.
    public static func line(phase: CallUiState.CallPhase, durationMs: Int64?) -> String? {
        switch phase {
        case .cameraCheck:
            return nil
        case .calling:
            return "Calling..."
        case .connecting:
            return "Connecting..."
        case .reconnecting:
            return "Reconnecting..."
        case .connected:
            return CallTranscriptRow.durationLabel(ms: durationMs ?? 0)
        case .ended:
            return "Call ended"
        }
    }

    /// What VoiceOver reads instead of the line. A duration is announced as a
    /// duration rather than digit by digit (`docs/ux/calls.md` section 8).
    public static func spokenLine(phase: CallUiState.CallPhase, durationMs: Int64?) -> String? {
        if case .connected = phase {
            return CallTranscriptRow.spokenDuration(ms: durationMs ?? 0)
        }
        return line(phase: phase, durationMs: durationMs)
    }

    /// The spoken label of a control, naming the action and the state it will
    /// produce (`docs/ux/calls.md` sections 4.1.1 and 8).
    public static func spokenControlLabel(_ control: CallControl, on: Bool) -> String {
        switch control {
        case .microphone:
            return on ? "Mute microphone" : "Unmute microphone"
        case .camera:
            return on ? "Stop camera" : "Start camera"
        case .speaker:
            return on ? "Turn off speaker" : "Turn on speaker"
        case .overflow:
            return "More controls"
        case .end:
            return "End call"
        }
    }
}
