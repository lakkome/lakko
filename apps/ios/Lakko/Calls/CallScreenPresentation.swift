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
/// Every rule the redesign is about — no More menu on any call, one row on
/// every screen, the off state being the loud one, the status line never
/// saying two things at once — is a rule about *values*, and a SwiftUI body is the one
/// place in this app a test cannot reach. So the rules live here as pure
/// functions over an enum, `CallScreen` reads them, and a regression fails in
/// `LakkoTests` rather than on someone's phone.

// MARK: - The controls

/// One control in the call's bottom row.
public enum CallControl: String, CaseIterable, Identifiable, Sendable {
    /// Mute and unmute the microphone.
    case microphone
    /// Start and stop sending the camera.
    case camera
    /// A group call's one control about its people: who is in it, and, on a
    /// call that can hand out a link to itself, inviting more
    /// (`docs/ux/calls.md` section 4.1, `docs/ux/call-links.md` section 3).
    /// Never present in a 1x1 call.
    case participants
    /// Which microphone, camera and speaker the call uses: opens the call's
    /// own settings sheet.
    case settings
    /// Hang up.
    case end

    public var id: String { rawValue }

    /// The one short word under the circle. It names the action, never the
    /// state, so it does not change size or meaning when the state flips
    /// (section 4.1.1).
    public var label: String {
        switch self {
        case .microphone: return Localization.text("call.control.mute")
        case .camera: return Localization.text("call.control.camera")
        case .participants: return Localization.text("call.control.participants")
        case .settings: return Localization.text("call.control.settings")
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
    /// The even gap between controls.
    public let gap: CGFloat
    /// Whether each circle carries its word under it. A group call's row on a
    /// phone does not (`docs/ux/calls.md` section 4.1): the words are what
    /// made the row wrap, and each circle keeps its spoken name.
    public let showsLabels: Bool
    /// The bar's own padding, inside the rounded container.
    public let horizontalPadding: CGFloat
    public let verticalPadding: CGFloat

    public init(
        column: CGFloat,
        diameter: CGFloat,
        endDiameter: CGFloat,
        gap: CGFloat = CallScreenPresentation.controlGap,
        showsLabels: Bool = true,
        horizontalPadding: CGFloat = CallScreenPresentation.barHorizontalPadding,
        verticalPadding: CGFloat = CallScreenPresentation.barVerticalPadding
    ) {
        self.column = column
        self.diameter = diameter
        self.endDiameter = endDiameter
        self.gap = gap
        self.showsLabels = showsLabels
        self.horizontalPadding = horizontalPadding
        self.verticalPadding = verticalPadding
    }

    /// The width of the whole row for `count` controls, the bar's padding
    /// included: `count - 1` columns, the end button's own column, one gap
    /// between each pair and the extra gap that sets the end button apart.
    public func rowWidth(count: Int) -> CGFloat {
        guard count > 0 else { return 2 * horizontalPadding }
        return CGFloat(count - 1) * column + max(column, endDiameter)
            + CGFloat(count) * gap + 2 * horizontalPadding
    }

    /// The height the bar takes on the screen: the tallest circle, the word
    /// under it when there is one, and the bar's padding. The tile area of a
    /// group call ends above this.
    public var barHeight: CGFloat {
        max(diameter, endDiameter) + 2 * verticalPadding
            + (showsLabels ? CallScreenPresentation.labelHeight : 0)
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
    /// The bar's own vertical padding.
    public static let barVerticalPadding: CGFloat = 14
    /// The space the word under a circle takes, its gap to the circle
    /// included.
    public static let labelHeight: CGFloat = 20

    // MARK: A group call's row (section 4.1)

    /// A group call's circles are a step smaller, so the whole set stays on
    /// one row.
    public static let groupControlDiameter: CGFloat = 56
    public static let groupEndDiameter: CGFloat = 64
    /// Under this width, or under ``compactGroupHeight``, the group row drops
    /// its words and is sized from the width.
    public static let compactGroupWidth: CGFloat = 600
    public static let compactGroupHeight: CGFloat = 520
    /// The compact group row: its gap, its padding, and the range its circles
    /// are sized within. The same numbers as the web row.
    public static let compactGroupGap: CGFloat = 8
    public static let compactGroupHorizontalPadding: CGFloat = 10
    public static let compactGroupVerticalPadding: CGFloat = 8
    public static let compactGroupMinDiameter: CGFloat = 38
    public static let compactGroupMaxDiameter: CGFloat = 52
    /// How much larger the end button is than the rest in the compact row.
    public static let compactGroupEndExtra: CGFloat = 6

    /// Size a GROUP call's control row (`docs/ux/calls.md` section 4.1).
    ///
    /// The group row is always one line. On a narrow screen (under 600 pt)
    /// or a short one (under 520 pt) the words under the circles go and the
    /// circles are sized from the width and the number of controls, never
    /// below 38 pt: a row of labelled circles took half a phone screen
    /// (found live 2026-10-02). With room it keeps its words at 56 / 64 pt.
    ///
    /// - Parameters:
    ///   - availableWidth: the width the bar may take, margins already removed.
    ///   - screenSize: the call screen's own size, which decides compact.
    ///   - count: how many controls the row holds.
    public static func fitGroupControlBar(
        availableWidth: CGFloat, screenSize: CGSize, count: Int
    ) -> CallControlBarFit {
        let compact = screenSize.width < compactGroupWidth || screenSize.height < compactGroupHeight
        guard compact else {
            return fitControlBar(
                availableWidth: availableWidth, count: count,
                diameter: groupControlDiameter, endDiameter: groupEndDiameter)
        }
        let n = CGFloat(max(count, 1))
        // Everything in the row that is not a circle: the padding, one gap
        // per control (the end button's extra gap included) and the end
        // button's extra width.
        let fixed = 2 * compactGroupHorizontalPadding + n * compactGroupGap + compactGroupEndExtra
        let diameter = min(
            max((availableWidth - fixed) / n, compactGroupMinDiameter), compactGroupMaxDiameter)
        return CallControlBarFit(
            column: diameter,
            diameter: diameter,
            endDiameter: diameter + compactGroupEndExtra,
            gap: compactGroupGap,
            showsLabels: false,
            horizontalPadding: compactGroupHorizontalPadding,
            verticalPadding: compactGroupVerticalPadding)
    }

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
    public static func fitControlBar(
        availableWidth: CGFloat,
        count: Int,
        diameter controlDiameter: CGFloat = CallScreenPresentation.controlDiameter,
        endDiameter: CGFloat = CallScreenPresentation.endDiameter
    ) -> CallControlBarFit {
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

    // MARK: A group call's tiles (`docs/ux/group-call-grid.md` section 7)

    /// Whether a tile shows its connection: fair or worse only. A dot on
    /// every healthy tile says nothing, and for other people it was never a
    /// measurement at all (section 7.2).
    public static func connectionIsWorthShowing(_ tier: CallUiState.QualityTier) -> Bool {
        switch tier {
        case .fair, .poor, .bad: return true
        case .excellent, .good: return false
        }
    }

    /// The initials circle of a tile without a picture, scaled to the tile so
    /// a strip tile and a full-screen tile both read.
    public static func tileInitialsDiameter(tileSize: CGSize) -> CGFloat {
        min(max(min(tileSize.width, tileSize.height) * 0.45, 24), 96)
    }

    /// Under this width a tile carries the first name only (section 7.2).
    public static let narrowTileWidth: CGFloat = 150

    /// The name a tile carries: the whole label, or its first word on a tile
    /// too narrow for more.
    public static func tileName(_ label: String, tileWidth: CGFloat) -> String {
        guard tileWidth < narrowTileWidth else { return label }
        return label.split(whereSeparator: { $0.isWhitespace }).first.map(String.init) ?? label
    }

    /// The row, left to right (`docs/ux/calls.md` section 4.1), on every
    /// screen size:
    ///
    /// - a one-to-one call: Microphone, Camera, Settings, End;
    /// - a group call: Microphone, Camera, Participants, Settings, End.
    ///
    /// Nothing else, and no overflow menu behind it.
    ///
    /// **The camera button is on a voice call too**, and pressing it turns the
    /// call into a video one. People start a call with their voice and decide
    /// to show something a minute later; every messenger allows that.
    ///
    /// **There is no Speaker control**, on any call. A group call, and any
    /// call with video, plays on the loudspeaker unless a headset or a
    /// Bluetooth device is connected, which then takes over by itself
    /// (``CallAudioSession/configureCallOutput(loudspeakerByDefault:)``); a
    /// one-to-one voice call starts on the earpiece as before. Moving the
    /// sound by hand is one of the three choices behind `Settings`.
    ///
    /// **The chat, the old settings panel and the overflow button that held
    /// them are gone**: the chat sent nothing anywhere, and a control that
    /// does nothing is worse than none (removed 2026-10-02).
    ///
    /// `Participants` is there only where it leads somewhere
    /// (``participantsPress(canInvite:hasOthers:)``): on a call that can hand
    /// out a link to itself it is always there; on a call placed inside a
    /// conversation it is there once somebody else is in the call, and absent
    /// while the person is alone, when it would open a list of one.
    ///
    /// - Parameters:
    ///   - canInvite: this call has a link to offer.
    ///   - hasOthers: at least one other person is in the call.
    public static func controls(
        layout: CallScreenLayout, canInvite: Bool = false, hasOthers: Bool = false
    ) -> [CallControl] {
        var row: [CallControl] = [.microphone, .camera]
        if layout == .gallery,
           participantsPress(canInvite: canInvite, hasOthers: hasOthers) != .nothing {
            row.append(.participants)
        }
        row.append(.settings)
        row.append(.end)
        return row
    }

    /// What pressing `Participants` does.
    public enum ParticipantsPress: Equatable, Sendable {
        /// Open the app's own menu above the bar
        /// (``participantsMenu(hasOthers:guest:)``).
        case menu
        /// Open the list of who is in the call, directly: a menu with one
        /// item would be a second step for nothing.
        case participants
        /// There is nothing to open, so the control is absent.
        case nothing
    }

    /// What pressing `Participants` does on this call.
    ///
    /// A call that can hand out a link opens the menu, which always holds the
    /// invite items. A call placed inside a conversation has no link: with
    /// somebody else in it the press opens the list of people, and alone
    /// there is nothing to show.
    public static func participantsPress(canInvite: Bool, hasOthers: Bool) -> ParticipantsPress {
        if canInvite { return .menu }
        return hasOthers ? .participants : .nothing
    }

    /// One item of the `Participants` menu.
    public enum ParticipantsMenuItem: String, Equatable, Sendable {
        /// The list of who is in the call.
        case participants
        /// Copy the link to this call.
        case copyLink
        /// Send the invitation into conversations the person picks.
        case inviteContacts
    }

    /// The `Participants` menu of a call that can hand out a link, top to
    /// bottom.
    ///
    /// `Participants` only when somebody else is in the call: alone, the menu
    /// is the invite items.
    ///
    /// `Invite contacts` is always there for an account, with or without
    /// conversations: an item that comes and goes with the chat list reads as
    /// a missing feature (found live on web 2026-10-02), so the list it opens
    /// says when it is empty. Only a guest account (``GuestAccountMarker``),
    /// which never has conversations, gets the link alone.
    public static func participantsMenu(
        hasOthers: Bool, guest: Bool
    ) -> [ParticipantsMenuItem] {
        var items: [ParticipantsMenuItem] = []
        if hasOthers { items.append(.participants) }
        items.append(.copyLink)
        if !guest { items.append(.inviteContacts) }
        return items
    }

    /// Whether a control can be pressed in this phase. While the call is only
    /// ringing out there is nothing to mute yet, so the table in section 4 says
    /// "only End is active" — the rest stay on the screen at
    /// ``disabledOpacity``, never hidden.
    ///
    /// `Participants` is not about the media, so it stays usable until the
    /// call is over. `Settings` is live from the first second: a person who
    /// cannot be heard wants it before anyone answers.
    public static func isEnabled(_ control: CallControl, phase: CallUiState.CallPhase) -> Bool {
        if control == .end { return true }
        if control == .participants || control == .settings {
            if case .ended = phase { return false }
            return true
        }
        switch phase {
        case .calling, .ended:
            return false
        case .preparing, .connecting, .connected, .reconnecting:
            return true
        }
    }

    /// The circle's face. `on` means the thing the button controls is running:
    /// the microphone is live, the camera is sending.
    ///
    /// **The loud face marks the state a glance must catch.** For the
    /// microphone and the camera that is being off: a muted call where the
    /// user thinks they are heard is the failure worth shouting about. The
    /// controls that open something (the list of people, the settings) are
    /// not states, so they are always the quiet face.
    public static func face(_ control: CallControl, on: Bool) -> CallControlFace {
        switch control {
        case .microphone, .camera:
            return on ? .outline : .solid
        case .participants, .settings, .end:
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
        // Only the video fill hides them. A group call's tiles are not one
        // picture the bar stands in front of: the tile area ends above the
        // bar, and a tap on a tile pins it, so the tap is not there to bring
        // a faded row back (`docs/ux/group-call-grid.md` section 6.2).
        return layout == .video
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
    /// - Parameter groupCall: a group call, or a call joined from a link.
    ///   Nobody is being called in one, so its first seconds read
    ///   `Connecting...` and never `Calling...` (section 3.4).
    public static func line(
        phase: CallUiState.CallPhase, durationMs: Int64?, groupCall: Bool = false
    ) -> String? {
        switch phase {
        case .preparing:
            return groupCall ? Localization.text("common.connecting") : nil
        case .calling:
            return Localization.text(groupCall ? "common.connecting" : "call.status.calling")
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
    public static func spokenLine(
        phase: CallUiState.CallPhase, durationMs: Int64?, groupCall: Bool = false
    ) -> String? {
        switch phase {
        case .connected:
            return CallTranscriptRow.spokenDuration(ms: durationMs ?? 0)
        case .calling:
            return Localization.text(
                groupCall ? "call.status.connecting_spoken" : "call.status.calling_spoken")
        case .connecting:
            return Localization.text("call.status.connecting_spoken")
        case .reconnecting:
            return Localization.text("call.status.reconnecting_spoken")
        case .preparing:
            return groupCall ? Localization.text("call.status.connecting_spoken") : nil
        case .ended:
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
        case .participants:
            // `on` is "the list is open". The control that opens the MENU is
            // named by ``spokenParticipantsLabel(press:listOpen:)``.
            return Localization.text(
                on ? "call.control.participants_hide_a11y" : "call.control.participants_show_a11y")
        case .settings:
            return Localization.text("call.settings.title")
        case .end:
            return Localization.text("call.control.end_a11y")
        }
    }

    /// The spoken label of `Participants`, which says what the press opens:
    /// the menu of people and invitations on a call that can hand out a link,
    /// the list of people (to show, or to hide) otherwise.
    public static func spokenParticipantsLabel(
        press: CallScreenPresentation.ParticipantsPress, listOpen: Bool
    ) -> String {
        switch press {
        case .menu:
            return Localization.text("call.control.participants_menu_a11y")
        case .participants, .nothing:
            return spokenControlLabel(.participants, on: listOpen)
        }
    }
}
