// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI
import WebRTC

/// The collapsed call (`docs/ux/calls.md` section 4.5): one small window that
/// floats over the app, above whatever screen is showing, surviving every
/// navigation because it is drawn by ``CallHostOverlay`` at the signed-in root
/// and not by any screen.
///
/// This view is the window's placement: where it sits, the drag that moves it
/// to the nearest corner, and on a phone the flick that tucks it past an edge
/// to a strip (section 4.5.2). The numbers are ``CallWindowGeometry``'s; the
/// window's picture is ``CollapsedCallWindow``.
struct FloatingCallWindow: View {
    @ObservedObject var l10n = Localization.shared
    @ObservedObject var state: CallUiState
    let groupCall: Bool
    /// Who the call is with: the call screen's label, the window's spoken name.
    let label: String
    let phase: CallBarPhase
    let connectedAt: Date?
    /// ADR-0084: the other person's signature key in a one-to-one, for their
    /// photo where no picture is drawn.
    let peerKey: Data?
    let onExpand: () -> Void
    let onToggleMic: () -> Void

    /// The corner the window sits in. Starts where the last collapsed call
    /// landed (``CallWindowPlacementMemory``).
    @State private var corner = CallWindowPlacementMemory.corner
    /// Tucked past an edge, on a phone.
    @State private var tuck: CallWindowTuck?
    /// How far the finger has moved the window or the strip.
    @State private var drag: CGSize = .zero
    /// The current touch sequence on the window has gone beyond the drag
    /// threshold, so it can only move the window (``CallWindowGeometry/touchOutcome``).
    @State private var touchMoved = false

    /// The coordinate space the window's frames and the touch are both in.
    private static let areaSpace = "call-window-area"

    var body: some View {
        GeometryReader { proxy in
            let area = CGRect(origin: .zero, size: proxy.size)
            let screen = CGSize(
                width: proxy.size.width + proxy.safeAreaInsets.leading + proxy.safeAreaInsets.trailing,
                height: proxy.size.height + proxy.safeAreaInsets.top + proxy.safeAreaInsets.bottom)
            let phone = CallWindowGeometry.isPhone(screen: screen)
            let shown = CallWindowContent.shown(state: state, groupCall: groupCall)
            let size = CallWindowGeometry.size(screen: screen, screenShare: shown.isScreenShare)
            if let tuck, phone {
                let strip = CallWindowGeometry.stripFrame(
                    tuck: tuck, corner: corner, size: size, in: area)
                tuckedStrip(tuck: tuck)
                    .frame(width: strip.width, height: strip.height)
                    .gesture(stripDrag(tuck: tuck))
                    .position(x: strip.midX + drag.width, y: strip.midY)
                    .transition(.opacity)
            } else {
                let frame = CallWindowGeometry.frame(corner: corner, size: size, in: area)
                CollapsedCallWindow(
                    state: state,
                    shown: shown,
                    size: size,
                    insetOnLeading: CallWindowGeometry.insetOnLeading(corner: corner),
                    label: label,
                    phase: phase,
                    connectedAt: connectedAt,
                    peerKey: peerKey,
                    onExpand: onExpand,
                    onToggleMic: onToggleMic)
                    .gesture(windowTouch(area: area, size: size, phone: phone))
                    .position(x: frame.midX + drag.width, y: frame.midY + drag.height)
                    .transition(.scale(scale: 0.85).combined(with: .opacity))
            }
        }
        .coordinateSpace(name: Self.areaSpace)
    }

    // MARK: - Moving it

    /// Every touch on the window, as ONE gesture: a tap on the mute button
    /// toggles the microphone, a tap anywhere else brings the call back, and a
    /// touch that moved beyond the drag threshold only moves the window, even
    /// when it started on the mute button. One gesture rather than a button
    /// inside a draggable view, because the window moves with the finger: a
    /// button under a dragging finger never sees the finger leave it, and
    /// would fire on release.
    private func windowTouch(area: CGRect, size: CGSize, phone: Bool) -> some Gesture {
        DragGesture(minimumDistance: 0, coordinateSpace: .named(Self.areaSpace))
            .onChanged { value in
                if touchMoved || CallWindowGeometry.isDrag(value.translation) {
                    touchMoved = true
                    drag = value.translation
                }
            }
            .onEnded { value in
                let moved = touchMoved || CallWindowGeometry.isDrag(value.translation)
                touchMoved = false
                let start = CallWindowGeometry.frame(corner: corner, size: size, in: area)
                switch CallWindowGeometry.touchOutcome(
                    start: value.startLocation, moved: moved, window: start) {
                case .toggleMute:
                    onToggleMic()
                case .expand:
                    onExpand()
                case .move:
                    land(value: value, start: start, area: area, size: size, phone: phone)
                }
            }
    }

    private func land(
        value: DragGesture.Value, start: CGRect, area: CGRect, size: CGSize, phone: Bool
    ) {
        // Where the window would come to rest given how fast it was moving,
        // so a flick towards a corner reaches it.
        let projected = CGPoint(
            x: start.midX + value.predictedEndTranslation.width,
            y: start.midY + value.predictedEndTranslation.height)
        withAnimation(.spring(response: 0.35, dampingFraction: 0.8)) {
            if let tucked = CallWindowGeometry.tuck(projected: projected, in: area, phone: phone) {
                tuck = tucked
            } else {
                let landed = CallWindowGeometry.nearestCorner(to: projected, size: size, in: area)
                corner = landed
                CallWindowPlacementMemory.corner = landed
            }
            drag = .zero
        }
    }

    private func stripDrag(tuck: CallWindowTuck) -> some Gesture {
        DragGesture(minimumDistance: 4, coordinateSpace: .global)
            .onChanged { value in
                // The strip only moves away from its edge.
                let dx = value.translation.width
                drag = CGSize(width: tuck == .leading ? max(0, dx) : min(0, dx), height: 0)
            }
            .onEnded { value in
                withAnimation(.spring(response: 0.35, dampingFraction: 0.8)) {
                    if CallWindowGeometry.untucks(value.predictedEndTranslation, from: tuck) {
                        self.tuck = nil
                    }
                    drag = .zero
                }
            }
    }

    private func untuck() {
        withAnimation(.spring(response: 0.35, dampingFraction: 0.8)) { tuck = nil }
    }

    // MARK: - The strip

    /// What a tucked window leaves at the edge: a strip with a chevron
    /// pointing back in. The call goes on and the mute state is unchanged.
    private func tuckedStrip(tuck: CallWindowTuck) -> some View {
        Button(action: untuck) {
            ZStack {
                UnevenRoundedRectangle(
                    topLeadingRadius: tuck == .leading ? 0 : 12,
                    bottomLeadingRadius: tuck == .leading ? 0 : 12,
                    bottomTrailingRadius: tuck == .leading ? 12 : 0,
                    topTrailingRadius: tuck == .leading ? 12 : 0,
                    style: .continuous)
                    .fill(Color(red: 0.06, green: 0.07, blue: 0.13).opacity(0.92))
                Image(systemName: tuck == .leading ? "chevron.right" : "chevron.left")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(.white)
            }
            .shadow(color: .black.opacity(0.18), radius: 16, y: 6)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // `docs/ux/calls.md` section 4.5.6 names this button `Show call`.
        // That string is not in the catalog yet (`l10n/catalog`), so the
        // existing `Return to call` names it until it is added.
        .accessibilityLabel(l10n.t("guest.in_call.return"))
        .accessibilityIdentifier("collapsed-call-strip")
    }
}

/// The window itself (`docs/ux/calls.md` section 4.5.1): the call screen's
/// stage person on the picture, the own camera as a small inset while it
/// sends, the shown person's name and `+N` at the bottom, the state or the
/// running time with the lock at the top left, and one round mute button at
/// the top right.
///
/// **There is no End here, by design.** Ending is the one irreversible control
/// in a call; the call screen sets it apart and paints it the only red on the
/// screen, and a small window a thumb crosses on the way to a chat is the
/// wrong place for it. Coming back costs one tap.
///
/// What its line says is not decided here: ``callBarStatus(phase:connectedForMs:)``
/// in `lakko-ui-rules` answers with a sum type, and
/// ``CallBarPresentation/line(_:)`` prints it.
struct CollapsedCallWindow: View {
    @ObservedObject var l10n = Localization.shared
    @ObservedObject var state: CallUiState
    let shown: CallWindowShown
    let size: CGSize
    /// The own inset sits in the bottom-leading corner (the window is on the
    /// trailing side of the screen) rather than the bottom-trailing one.
    let insetOnLeading: Bool
    let label: String
    let phase: CallBarPhase
    let connectedAt: Date?
    let peerKey: Data?
    let onExpand: () -> Void
    let onToggleMic: () -> Void

    @Environment(\.displayScale) private var displayScale

    private static let corner: CGFloat = 12

    private var local: CallUiState.Participant? {
        state.participants.first(where: { $0.isLocal })
    }

    private var micOn: Bool { local?.micOn ?? true }

    var body: some View {
        ZStack(alignment: .topTrailing) {
            // Touches are the placement's (``FloatingCallWindow``): one
            // gesture decides between a tap here, a tap on mute and a drag.
            // A screen reader reaches both through their actions.
            content
                .contentShape(RoundedRectangle(cornerRadius: Self.corner, style: .continuous))
                .accessibilityElement(children: .ignore)
                .accessibilityAddTraits(.isButton)
                .accessibilityLabel(spokenLabel)
                .accessibilityAction { onExpand() }
                // The spoken sentence is in the interface language; the UI
                // tests find the window by this handle instead.
                .accessibilityIdentifier("collapsed-call-window")
            muteButton
                .padding(CallWindowGeometry.muteInset)
        }
        .frame(width: size.width, height: size.height)
        .shadow(color: .black.opacity(0.18), radius: 16, y: 6)
        // The call surface: dark whatever the app's theme, as the call screen.
        .environment(\.colorScheme, .dark)
        // Section 4.5.6: state changes are heard and not only seen, so
        // `Reconnecting...` reaches a screen reader without the window being
        // focused. On the phase and never on the clock — the running time
        // would otherwise interrupt whatever is being read once a second,
        // which is why a verdict that IS the clock is skipped.
        .onChange(of: phase) { _, newPhase in
            let status = callBarStatus(phase: newPhase, connectedForMs: 0)
            if case .elapsed = status { return }
            AccessibilityNotification.Announcement(CallBarPresentation.line(status)).post()
        }
        .modifier(
            TileVideoWantReport(
                state: state,
                participantId: shown.stage?.id ?? "",
                quality: reportedQuality))
    }

    // MARK: - The picture

    private var content: some View {
        ZStack {
            Color.lakkoBackground
            picture
            // Two soft shades so the words stay readable over any picture.
            VStack(spacing: 0) {
                LinearGradient(
                    colors: [.black.opacity(0.45), .clear], startPoint: .top, endPoint: .bottom)
                    .frame(height: 44)
                Spacer(minLength: 0)
                LinearGradient(
                    colors: [.clear, .black.opacity(0.5)], startPoint: .top, endPoint: .bottom)
                    .frame(height: 40)
            }
            .allowsHitTesting(false)
            VStack(alignment: .leading, spacing: 0) {
                statusLine
                    .padding(.leading, 8)
                    .padding(.trailing, 44)
                    .padding(.top, 8)
                Spacer(minLength: 0)
                if shown.showsOwnInset, let track = state.localVideoTrack {
                    ownInset(track: track)
                        .frame(maxWidth: .infinity, alignment: insetOnLeading ? .leading : .trailing)
                        .padding(.horizontal, 6)
                        .padding(.bottom, 4)
                }
                nameRow
                    .padding(.horizontal, 8)
                    .padding(.bottom, 6)
            }
        }
        .frame(width: size.width, height: size.height)
        .clipShape(RoundedRectangle(cornerRadius: Self.corner, style: .continuous))
    }

    @ViewBuilder
    private var picture: some View {
        if let stage = shown.stage {
            if stage.isScreen {
                ScreenSharePicture(state: state, large: false)
            } else if stage.camOn, stage.isLocal, let track = state.localVideoTrack {
                LocalVideoPreviewView(
                    track: track, mirrored: state.settings.mirrorLocal, contentMode: .scaleAspectFill)
            } else if stage.camOn, !stage.isLocal, let track = state.remoteVideoTracks[stage.id] {
                let light = state.remoteLightVideoTracks[stage.id]
                LayeredRemoteVideoView(
                    heavy: VideoGiven.heavyTrack(
                        wanted: wantedQuality(hasLight: light != nil),
                        given: state.remoteVideoGiven[stage.id],
                        main: track,
                        middle: state.remoteMiddleVideoTracks[stage.id],
                        hasLight: light != nil),
                    light: light,
                    peerId: CallScreenPresentation.sfuPeerId(ofParticipant: stage.id))
            } else if stage.camOn, !stage.isLocal,
                stage.id == state.soleRemoteParticipantId,
                let track = state.remoteVideoTrack {
                // Remote video is never mirrored.
                LocalVideoPreviewView(track: track, mirrored: false, contentMode: .scaleAspectFill)
            } else {
                avatar(for: stage)
            }
        } else {
            InitialsAvatar(
                name: label, identity: label,
                size: CallScreenPresentation.tileInitialsDiameter(tileSize: size),
                placeholderSymbol: "person.fill",
                photo: peerKey.map(AvatarPhoto.person))
        }
    }

    /// The avatar on the call surface, drawn as the person's tile draws it.
    private func avatar(for person: CallUiState.Participant) -> some View {
        InitialsAvatar(
            name: shown.name, identity: person.id,
            size: CallScreenPresentation.tileInitialsDiameter(tileSize: size),
            placeholderSymbol: "person.fill",
            photo: person.isLocal
                ? .own
                : (state.personKeys[person.id] ?? peerKey).map(AvatarPhoto.person))
    }

    /// This device's camera while it sends: the reminder that a collapsed
    /// video call is still a camera. Mirrored, as on the call screen.
    private func ownInset(track: RTCVideoTrack) -> some View {
        let width = max(36, (size.width * 0.3).rounded())
        return LocalVideoPreviewView(
            track: track, mirrored: state.settings.mirrorLocal, contentMode: .scaleAspectFill)
            .frame(width: width, height: (width * 4 / 3).rounded())
            .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .strokeBorder(Color.white.opacity(0.35), lineWidth: 1))
            .allowsHitTesting(false)
    }

    // MARK: - The words

    @ViewBuilder
    private var statusLine: some View {
        if let connectedAt {
            TimelineView(.periodic(from: connectedAt, by: 1)) { context in
                statusText(callBarStatus(
                    phase: phase,
                    connectedForMs: Self.elapsedMs(since: connectedAt, at: context.date)))
            }
        } else {
            // No clock to read. The rule ignores `connectedForMs` for every
            // phase but `Connected`, which is the phase that always has one.
            statusText(callBarStatus(phase: phase, connectedForMs: 0))
        }
    }

    private func statusText(_ status: CallBarStatus) -> some View {
        HStack(spacing: 4) {
            Text(CallBarPresentation.line(status))
                .font(.caption.weight(.semibold))
                .monospacedDigit()
                .lineLimit(1)
            // A lock and never the word: a claim about encryption belongs
            // where the safety number can be read, which is one tap away.
            if state.e2ee.active {
                Image(systemName: "lock.fill")
                    .font(.system(size: 9, weight: .semibold))
            }
        }
        .foregroundStyle(.white)
        .shadow(color: .black.opacity(0.5), radius: 2)
    }

    private var nameRow: some View {
        HStack(spacing: 4) {
            Text(shown.name)
                .font(.caption.weight(.semibold))
                .lineLimit(1)
                .truncationMode(.tail)
            if let others = CallWindowContent.othersLabel(shown.othersBeyond) {
                Text(others)
                    .font(.caption)
                    .monospacedDigit()
                    .lineLimit(1)
                    .layoutPriority(1)
            }
            Spacer(minLength: 0)
        }
        .foregroundStyle(.white)
        .shadow(color: .black.opacity(0.5), radius: 2)
    }

    private var spokenLabel: String {
        let status: CallBarStatus
        if let connectedAt {
            status = callBarStatus(
                phase: phase, connectedForMs: Self.elapsedMs(since: connectedAt, at: Date()))
        } else {
            status = callBarStatus(phase: phase, connectedForMs: 0)
        }
        return CallBarPresentation.spokenLabel(
            name: label, status: status, encrypted: state.e2ee.active,
            cameraSending: shown.showsOwnInset || (shown.stage?.isLocal == true && local?.camOn == true))
    }

    private static func elapsedMs(since start: Date, at now: Date) -> UInt64 {
        UInt64(max(0, now.timeIntervalSince(start)) * 1000)
    }

    // MARK: - The picture asked for

    /// The picture the window asks the SFU for: the shared rule for the
    /// window's width, never counted as the stage.
    private func wantedQuality(hasLight: Bool) -> TileVideoQuality {
        CallWindowContent.wantedQuality(
            people: state.participants.count,
            windowWidthPx: VideoWants.physicalTileWidth(points: size.width, scale: displayScale),
            hasLight: hasLight)
    }

    /// What the call is told the window shows: the shown person only, while a
    /// picture of them is drawn. Everyone else falls back to the light picture
    /// alone, which is what no tile asking means.
    private var reportedQuality: TileVideoQuality? {
        guard let stage = shown.stage, !stage.isLocal, !stage.isScreen, stage.camOn,
            state.remoteVideoTracks[stage.id] != nil
        else { return nil }
        return wantedQuality(hasLight: state.remoteLightVideoTracks[stage.id] != nil)
    }

    // MARK: - Mute

    /// The one control on the window: the control a person needs in the
    /// second they need it, and pressing it the wrong way round is undone by
    /// pressing it again. Drawn on and off exactly as the call screen's row
    /// draws it (``CallScreenPresentation/face(_:on:)``).
    ///
    /// Not a `Button`: the touch is decided by the window's one gesture
    /// (``CallWindowGeometry/touchOutcome(start:moved:window:)``), so a drag
    /// that starts here moves the window and never toggles the microphone.
    private var muteButton: some View {
        CallControlFaceView(
            systemImage: micOn ? "mic.fill" : "mic.slash.fill",
            face: CallScreenPresentation.face(.microphone, on: micOn),
            diameter: 32)
            .frame(width: CallWindowGeometry.muteTarget, height: CallWindowGeometry.muteTarget)
            .contentShape(Circle())
            .accessibilityElement(children: .ignore)
            .accessibilityAddTraits(.isButton)
            .accessibilityLabel(CallStatusText.spokenControlLabel(.microphone, on: micOn))
            .accessibilityAction { onToggleMic() }
    }
}
