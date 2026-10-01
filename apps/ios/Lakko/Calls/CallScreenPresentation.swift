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
        case .speaker: return Localization.text("call.control.speaker")
        case .microphone: return Localization.text("call.control.mute")
        case .camera: return Localization.text("call.control.camera")
        case .overflow: return Localization.text("call.control.more")
        case .end: return Localization.text("call.control.end")
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

/// The sizes one control row is drawn at — see
/// ``CallScreenPresentation/fitControlBar(availableWidth:count:)``.
public struct CallControlBarFit: Equatable, Sendable {
    /// Width of every column; the row is `count` of these plus the gaps and
    /// the bar's padding.
    public let column: CGFloat
    /// Diameter of an ordinary control's circle.
    public let diameter: CGFloat
    /// Diameter of the end button's circle.
    public let endDiameter: CGFloat

    public init(column: CGFloat, diameter: CGFloat, endDiameter: CGFloat) {
        self.column = column
        self.diameter = diameter
        self.endDiameter = endDiameter
    }

    /// The spec sizes, for a row with room to spare.
    public static let spec = CallControlBarFit(
        column: CallScreenPresentation.endDiameter,
        diameter: CallScreenPresentation.controlDiameter,
        endDiameter: CallScreenPresentation.endDiameter)
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
    /// The even gap between controls. The same 12 as Android's row, so the
    /// two shrink alike under `fitControlBar` (the spec fixes the circles and
    /// "twice the gap" for the end button, not the gap itself).
    public static let controlGap: CGFloat = 12
    /// The end button is set apart by twice that gap.
    public static var endGap: CGFloat { controlGap * 2 }
    /// A touch target never falls below this, however narrow the screen.
    public static let minimumTouchTarget: CGFloat = 44
    /// A circle never shrinks below this under `fitControlBar` (Android's
    /// floor, above the touch-target minimum so the glyph stays legible).
    public static let minimumControlDiameter: CGFloat = 48
    /// A control that cannot be used is dimmed to this, never hidden.
    public static let disabledOpacity: Double = 0.4
    /// How long a connected video call waits before fading the row out.
    public static let autoHideAfter: TimeInterval = 5

    /// The bar's own horizontal padding, inside the rounded container.
    public static let barHorizontalPadding: CGFloat = 16

    /// Size the control row so that every control, the end button included,
    /// is on the screen.
    ///
    /// At the spec sizes five controls do not fit a 402 pt iPhone: five
    /// columns of 72, five gaps and the bar's padding come to 452 pt, and on
    /// the four-way call the Speaker circle was clipped at the
    /// left edge while the bar ran past the right. When the room is short the
    /// circles shrink together, keeping their ratio and never going under the
    /// minimum touch target; with room to spare they stay at the spec sizes.
    /// The same rule as Android's `fitCallControlBar`, so the two rows shrink
    /// alike on the same width.
    ///
    /// - Parameters:
    ///   - availableWidth: the width the bar may take, margins already removed.
    ///   - count: how many controls the row holds.
    public static func fitControlBar(availableWidth: CGFloat, count: Int) -> CallControlBarFit {
        guard count > 0 else {
            return CallControlBarFit(
                column: endDiameter, diameter: controlDiameter, endDiameter: endDiameter)
        }
        // One gap per control: `count - 1` between them plus the extra one that
        // sets the end button apart.
        let room = availableWidth - 2 * barHorizontalPadding - CGFloat(count) * controlGap
        let column = min(room / CGFloat(count), endDiameter)
        let scale = min(max(column / endDiameter, 0), 1)
        let end = max(endDiameter * scale, minimumControlDiameter)
        let normal = max(controlDiameter * scale, minimumControlDiameter)
        return CallControlBarFit(column: max(column, end), diameter: normal, endDiameter: end)
    }

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
    ///   - groupCall: what KIND of call this is, not how many people are in it
    ///     right now. A group call is the gallery from its first second
    ///     (section 4.4): the person who started it is its first participant
    ///     and sees their own tile, and a second person joining does not turn
    ///     it into the 1x1 video fill, which shows no names and no own tile.
    ///     The 1x1 paths never set it.
    ///   - remoteCount: how many other people are on the call.
    ///   - remotePicture: a remote video frame has actually been rendered —
    ///     ``hasRemotePicture(trackPresent:framesArrived:)``, never a bare track
    ///     handle. With no picture the portrait is the honest thing to show.
    public static func layout(
        groupCall: Bool = false, remoteCount: Int, remotePicture: Bool
    ) -> CallScreenLayout {
        if groupCall || remoteCount > 1 { return .gallery }
        return remotePicture ? .video : .portrait
    }

    /// The tile a group call draws large, or `nil` for an equal grid (section
    /// 4.4): pinned, else speaking with video, else the first in roster order
    /// with video, else nobody. The user's own tile is never featured while
    /// anyone else is present; alone, their preview is the only thing there is
    /// to show. `speaking` is a hint the transport may never set, and the rule
    /// reads the same when it is always `false`.
    ///
    /// What this replaced picked the first remote whatever they were sending,
    /// so the first person to join held the big slot for the whole call with an
    /// avatar in it while other people's cameras ran in the small tiles.
    public static func featured(
        participants: [CallUiState.Participant], pinnedId: String?
    ) -> CallUiState.Participant? {
        if let pin = pinnedId, let pinned = participants.first(where: { $0.id == pin }) {
            return pinned
        }
        let remotes = participants.filter { !$0.isLocal }
        if remotes.isEmpty { return participants.first(where: { $0.isLocal }) }
        return remotes.first(where: { $0.speaking && $0.camOn })
            ?? remotes.first(where: { $0.camOn })
    }

    /// The grid under (or instead of) the featured tile: everyone but the
    /// featured one, in roster order; everyone, when there is none.
    public static func gallery(
        participants: [CallUiState.Participant], featured: CallUiState.Participant?
    ) -> [CallUiState.Participant] {
        guard let featured else { return participants }
        return participants.filter { $0.id != featured.id }
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
        case .preparing, .connecting, .connected, .reconnecting:
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
    ///
    /// `alone` carries the same argument one step further, and it is a case the
    /// original rule could not reach. A group call used to be entered from a
    /// conversation, where somebody else was already there; a call started from
    /// the Calls screen begins with one person in it, and the tiles above are
    /// this device's own. Found live on 2026-09-23: five seconds after starting
    /// a call the row faded, and the person who had just created it was left
    /// looking at their own tile with no way to invite anyone and no way to
    /// hang up. A tap brings the row back, but a screen whose only remaining
    /// affordance is an undiscoverable one is a screen with no exit.
    public static func autoHides(
        layout: CallScreenLayout,
        phase: CallUiState.CallPhase,
        alone: Bool = false
    ) -> Bool {
        guard case .connected = phase else { return false }
        if alone { return false }
        return layout == .video || layout == .gallery
    }
}

// MARK: - Media that cannot be decrypted

/// What the screen says about a stream whose SFrame decryptor failed to arm.
///
/// The security half of this is already settled before any of these words are
/// read: `RTCCryptoOptions.sframeRequireFrameEncryption` is on, so a receiver
/// without a decryptor **discards** every frame it is handed. No plaintext can
/// reach the app, whatever goes wrong here — invariant #4 holds structurally
/// rather than by anything this type does.
///
/// What is left is honesty. An undecryptable stream and a stalled one look
/// identical from the outside: a tile that never paints, a call with no sound.
/// The whole point of these strings is that the screen says which of the two it
/// is, instead of letting a fail-closed refusal pass for a weak network.
///
/// Attribution is per **receiver**, not per call — a peer's audio can arm while
/// its video does not — so the state is held per participant and never
/// described as "the call is broken".
public enum UndecryptableMedia {

    /// The badge on that participant's tile in a group call.
    public static var tileLabel: String { Localization.text("call.undecryptable.tile") }

    /// What VoiceOver reads for the badge, naming who it is about — the badge
    /// itself sits next to a name the screen reader has already moved past
    /// (`docs/ux/calls.md` section 8).
    public static func spokenTileLabel(name: String) -> String {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty
            ? Localization.text("call.undecryptable.tile_a11y")
            : Localization.text("call.undecryptable.named", texts: ["name": trimmed])
    }

    /// The one plain line for a screen with no tiles to badge — the 1x1
    /// portrait and the video overlay header. `nil` when everything that
    /// arrived is opening, which is the ordinary case.
    ///
    /// - Parameter names: display names of the participants whose media cannot
    ///   be decrypted. An empty or blank name is counted but not quoted, since
    ///   a 1x1 call often has nothing better than the peer's fingerprint.
    public static func notice(names: [String]) -> String? {
        if names.isEmpty { return nil }
        if names.count == 1 {
            let trimmed = names[0].trimmingCharacters(in: .whitespaces)
            return trimmed.isEmpty
                ? Localization.text("call.undecryptable.notice")
                : Localization.text("call.undecryptable.named", texts: ["name": trimmed])
        }
        return Localization.text("call.undecryptable.notice_many", counts: ["count": UInt64(names.count)])
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
        case .preparing:
            return nil
        case .calling:
            return Localization.text("call.status.calling")
        case .connecting:
            return Localization.text("common.connecting")
        case .reconnecting:
            return Localization.text("call.status.reconnecting")
        case .connected:
            return CallTranscriptRow.durationLabel(ms: durationMs ?? 0)
        case .ended:
            return Localization.text("call.status.ended")
        }
    }

    /// What VoiceOver reads instead of the line. A duration is announced as a
    /// duration rather than digit by digit (`docs/ux/calls.md` section 8).
    public static func spokenLine(phase: CallUiState.CallPhase, durationMs: Int64?) -> String? {
        switch phase {
        case .connected:
            return CallTranscriptRow.spokenDuration(ms: durationMs ?? 0)
        case .calling:
            return Localization.text("call.status.calling_spoken")
        case .connecting:
            return Localization.text("call.status.connecting_spoken")
        case .reconnecting:
            return Localization.text("call.status.reconnecting_spoken")
        case .preparing, .ended:
            return line(phase: phase, durationMs: durationMs)
        }
    }

    /// The spoken label of a control, naming the action and the state it will
    /// produce (`docs/ux/calls.md` sections 4.1.1 and 8).
    public static func spokenControlLabel(_ control: CallControl, on: Bool) -> String {
        switch control {
        case .microphone:
            return Localization.text(on ? "call.control.mute_a11y" : "call.control.unmute_a11y")
        case .camera:
            return Localization.text(on ? "call.control.camera_stop_a11y" : "call.control.camera_start_a11y")
        case .speaker:
            return Localization.text(on ? "call.control.speaker_off_a11y" : "call.control.speaker_on_a11y")
        case .overflow:
            return Localization.text("call.control.more_a11y")
        case .end:
            return Localization.text("call.control.end_a11y")
        }
    }
}
