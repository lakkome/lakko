// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import SwiftUI
import WebRTC

/// In-call UI (Phase 3c6-ios).
///
/// SwiftUI mirror of `apps/web/src/routes/Call.tsx`. Same layout
/// (active speaker on top, gallery row, controls bar, drawer),
/// same controls (mic / camera / participants / raise hand /
/// screen share / end call), same security UX (E2EE pill,
/// quality dot with popover, mute overlay, waiting message,
/// prejoin screen with cam/mic preview).
///
/// State lives in `CallUiState` (`apps/ios/Lakko/Calls/CallUiState.swift`),
/// which is the Swift port of `apps/web/src/state/call-state.ts`.
///
/// When a live `CallSession` is bound via `CallUiState`,
/// tiles render the real local / remote video tracks through
/// `LocalVideoPreviewView` (`RTCMTLVideoView`-backed). The
/// "(video)" gradient is the fallback shown only for an
/// audio-only call, before a track arrives, or on the
/// mock-design path (no session bound).
///
/// Three-platform parity: the same component tree exists on
/// web (Solid.js) and Android (Compose). A change to the
/// design here should land in the other two ports in lock-step.

// MARK: - QualityDot

private struct QualityDot: View {
    let tier: CallUiState.QualityTier
    let interactive: Bool
    let onTap: () -> Void

    var body: some View {
        Circle()
            .fill(tier.colour)
            .frame(width: 8, height: 8)
            .shadow(color: tier.colour.opacity(0.7), radius: 3)
            .contentShape(Rectangle().inset(by: -8))
            .onTapGesture {
                if interactive { onTap() }
            }
            .accessibilityLabel("Connection quality: \(tier.rawValue)")
    }
}

// MARK: - EncryptionChip

/// `docs/ux/calls.md` sections 4 and 5 — a lock and one word, next to the
/// status line, on a sunken surface. It replaces the strip of sixteen digits
/// that used to run across the top of every call: unreadable, meaningless to
/// anyone who has not been told what it is, and occupying the most valuable
/// line on the screen. The digits now live one tap away, in the encryption
/// sheet, where there is room to say what they are for.
///
/// It appears only when the call's own encryption is armed. There is no
/// "Securing..." state: an unarmed call has nothing true to claim, and a 1x1
/// native call refuses to connect at all rather than run unencrypted
/// (invariant #4).
private struct EncryptionChip: View {
    let onTap: () -> Void

    var body: some View {
        Button(action: onTap) {
            HStack(spacing: 5) {
                Image(systemName: "lock.fill")
                    .font(.caption2)
                Text("Encrypted")
                    .font(.caption2)
                    .fontWeight(.semibold)
            }
            .foregroundColor(Color.lakkoSuccess)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(Color.lakkoSurfaceSunken)
            .overlay(Capsule().stroke(Color.lakkoBorder, lineWidth: 1))
            .clipShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("End-to-end encrypted")
        .accessibilityHint("Shows the safety number to compare")
    }
}

// MARK: - EncryptionSheet

/// `docs/ux/calls.md` section 5 — what the chip opens: the title, one plain
/// sentence naming the other person, the safety number in four groups of four,
/// and the one line that says what comparing them proves.
private struct EncryptionSheet: View {
    let peerName: String?
    let safetyNumber: String?
    let onDone: () -> Void

    @State private var copied = false

    var body: some View {
        VStack(spacing: 20) {
            VStack(spacing: 8) {
                Image(systemName: "lock.fill")
                    .font(.title2)
                    .foregroundColor(Color.lakkoSuccess)
                Text("This call is encrypted")
                    .font(.title3)
                    .fontWeight(.semibold)
                    .foregroundColor(Color.lakkoText)
                Text(CallPeerName.encryptionSentence(peerName: peerName))
                    .font(.subheadline)
                    .foregroundColor(Color.lakkoTextMuted)
                    .multilineTextAlignment(.center)
            }

            if let safetyNumber {
                Text(safetyNumber)
                    .font(.system(.title2, design: .monospaced).weight(.medium))
                    .foregroundColor(Color.lakkoText)
                    .multilineTextAlignment(.center)
                    .padding(.vertical, 16)
                    .frame(maxWidth: .infinity)
                    .background(Color.lakkoSurfaceSunken)
                    .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))
                    // Section 8 — read in groups of four, not as one number.
                    .accessibilityLabel(
                        "Safety number: "
                            + safetyNumber.split(separator: " ").joined(separator: ", "))

                Text("Read these numbers aloud. If they match on both phones, no one is listening.")
                    .font(.footnote)
                    .foregroundColor(Color.lakkoTextMuted)
                    .multilineTextAlignment(.center)
            }

            Spacer(minLength: 0)

            HStack(spacing: 12) {
                if let safetyNumber {
                    Button {
                        UIPasteboard.general.string = safetyNumber
                        copied = true
                    } label: {
                        Text(copied ? "Copied" : "Copy")
                            .font(.headline)
                            .foregroundColor(Color.lakkoText)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 12)
                            .background(Color.lakkoSurfaceSunken)
                            .clipShape(
                                RoundedRectangle(cornerRadius: LakkoRadius.control, style: .continuous))
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Copy the safety number")
                }
                Button(action: onDone) {
                    Text("Done")
                        .font(.headline)
                        .foregroundColor(Color.lakkoAccentForeground)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 12)
                        .background(Color.lakkoAccent)
                        .clipShape(
                            RoundedRectangle(cornerRadius: LakkoRadius.control, style: .continuous))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Close")
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Color.lakkoBackground.ignoresSafeArea())
        .presentationDetents([.medium])
    }
}

// MARK: - CallStatusLine

/// `docs/ux/calls.md` section 4 item 1 — who is on the call, and under it the
/// running time once it is connected. The transient phase words
/// (`Connecting...`, `Reconnecting...`) stay with ``PhaseBanner`` so they are
/// never printed twice.
private struct CallStatusLine: View {
    let title: String?
    let phase: CallUiState.CallPhase
    let connectedAt: Date?

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            if let title {
                Text(title)
                    .font(.subheadline)
                    .fontWeight(.semibold)
                    .foregroundColor(Color.lakkoText)
                    .lineLimit(1)
            }
            if case .connected = phase, let connectedAt {
                // One tick a second while the call runs; the view is the only
                // thing that needs a clock, so it owns it.
                TimelineView(.periodic(from: connectedAt, by: 1)) { context in
                    let elapsed = Int64(
                        max(0, context.date.timeIntervalSince(connectedAt)) * 1000)
                    Text(CallTranscriptRow.durationLabel(ms: elapsed))
                        .font(.caption)
                        .foregroundColor(Color.lakkoTextMuted)
                        .monospacedDigit()
                        .accessibilityLabel(CallTranscriptRow.spokenDuration(ms: elapsed))
                }
            }
        }
    }
}

// MARK: - QualityIndicator (header dot + popover)

private struct QualityIndicator: View {
    @ObservedObject var state: CallUiState

    var body: some View {
        ZStack(alignment: .topTrailing) {
            QualityDot(
                tier: state.localQualityTier,
                interactive: true,
                onTap: { state.setQualityPopoverOpen(!state.qualityPopoverOpen) }
            )
            if state.qualityPopoverOpen {
                popover
                    .offset(x: 0, y: 16)
                    .zIndex(25)
            }
        }
    }

    private var popover: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(state.localQualityTier.rawValue.capitalized)
                    .fontWeight(.semibold)
                Spacer()
                Text(connectionTypeLabel)
                    .foregroundColor(connectionTypeColour)
            }
            Divider().background(Color.lakkoBorder)
            HStack(spacing: 4) {
                Text("Latency").foregroundColor(Color.lakkoTextMuted)
                Text(state.connectionDetail.rttMs.map { "\($0) ms" } ?? "—")
            }
            HStack(spacing: 6) {
                Text("↓ \(formatBps(state.connectionDetail.downBps))")
                Text("↑ \(formatBps(state.connectionDetail.upBps))")
            }
        }
        .font(.caption)
        .padding(10)
        .foregroundColor(Color.lakkoText)
        .background(Color.lakkoSurface)
        .overlay(
            RoundedRectangle(cornerRadius: 8).stroke(Color.lakkoBorder, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .shadow(color: Color.lakkoBackground.opacity(0.5), radius: 8, y: 2)
        .frame(minWidth: 180, alignment: .leading)
    }

    private var connectionTypeLabel: String {
        switch state.connectionDetail.connectionType {
        case .direct: return "Direct"
        case .relay: return "Relay"
        case .unknown: return "—"
        }
    }

    private var connectionTypeColour: Color {
        switch state.connectionDetail.connectionType {
        case .direct: return Color.lakkoSuccess
        case .relay: return Color.lakkoWarning
        case .unknown: return Color.lakkoTextMuted
        }
    }

    private func formatBps(_ bps: Int?) -> String {
        guard let bps = bps else { return "—" }
        if bps >= 1_000_000 { return String(format: "%.1f Mbps", Double(bps) / 1_000_000) }
        if bps >= 1_000 { return "\(bps / 1_000) kbps" }
        return "\(bps) bps"
    }
}

// MARK: - ParticipantTile

private struct ParticipantTile: View {
    let participant: CallUiState.Participant
    let large: Bool
    @ObservedObject var state: CallUiState

    var isPinned: Bool { state.pinnedId == participant.id }

    var body: some View {
        ZStack {
            videoSurface
            // B2a — local self-tile hint while the mid-call camera
            // acquire + renegotiation is in flight (`.starting`). Local
            // tile only; the button spinner is the paired affordance in
            // the controls bar.
            if participant.isLocal && state.videoPublishState == .starting {
                HStack(spacing: 8) {
                    ProgressView()
                        .progressViewStyle(.circular)
                        .tint(Color.lakkoText)
                    Text("Starting camera...")
                        .font(.caption)
                        .foregroundColor(Color.lakkoText)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(Color.lakkoBackground.opacity(0.55))
                .clipShape(Capsule())
            }
            // Large mute overlay (centre) for muted remotes.
            if !participant.micOn && !participant.isLocal {
                Image(systemName: "mic.slash.fill")
                    .font(large ? .title : .body)
                    .foregroundColor(Color.lakkoDanger)
                    .padding(large ? 16 : 10)
                    .background(Color.lakkoBackground.opacity(0.55))
                    .clipShape(Circle())
            }
            // Top-left screen-share badge.
            if participant.screenSharing {
                VStack {
                    HStack {
                        Text("Sharing")
                            .font(.caption2)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(Color.lakkoBackground.opacity(0.6))
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                            .foregroundColor(Color.lakkoText)
                        Spacer()
                    }
                    Spacer()
                }
                .padding(8)
            }
            // Top-right raise-hand badge.
            if participant.handRaised {
                VStack {
                    HStack {
                        Spacer()
                        Image(systemName: "hand.raised.fill")
                            .font(.body)
                            .padding(4)
                            .background(Color.lakkoBackground.opacity(0.6))
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                    }
                    Spacer()
                }
                .padding(8)
            }
            // Bottom strip: name + mute + quality + pin.
            VStack {
                Spacer()
                HStack(spacing: 8) {
                    if !participant.micOn {
                        Image(systemName: "mic.slash.fill")
                            .font(.caption)
                            .foregroundColor(Color.lakkoDanger)
                    }
                    Text(displayLabel)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    QualityDot(
                        tier: participant.quality,
                        interactive: false,
                        onTap: {}
                    )
                    if isPinned {
                        Image(systemName: "pin.fill")
                            .font(.caption2)
                            .foregroundColor(Color.lakkoAccent)
                    }
                }
                .font(.caption)
                .foregroundColor(Color.lakkoText)
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
                .background(Color.lakkoBackground.opacity(0.45))
            }
        }
        .aspectRatio(large ? 16.0 / 9.0 : 4.0 / 3.0, contentMode: .fit)
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(
                    participant.speaking ? Color.lakkoAccent : Color.lakkoBorder,
                    lineWidth: participant.speaking ? 2 : 1
                )
        )
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .background(Color.lakkoSurface)
        .shadow(
            color: participant.speaking
                ? Color.lakkoAccent.opacity(0.6) : .clear,
            radius: 6
        )
        .onTapGesture {
            guard !participant.isLocal else { return }
            state.pin(isPinned ? nil : participant.id)
        }
    }

    @ViewBuilder
    private var videoSurface: some View {
        if participant.camOn {
            // G3-1-video-ui-screen — render the live LOCAL
            // track for the local participant.
            // Remote video — render the live REMOTE track. In an
            // N-way SFU call each remote tile has its own track in
            // `remoteVideoTracks`, keyed on the tile's id
            // (`"peer-<streamId>"`, the SFU pinning the MSID
            // stream-id to the source peer id). The 1×1 P2P
            // `CallSession` path has no MSID, so it falls back to
            // the single `remoteVideoTrack` handle — but only for
            // the sole remote participant, so that one handle is
            // never painted onto the wrong tile in a group call.
            // Both paths fall through to the gradient placeholder
            // when no live track is bound (audio-only, mock design
            // path, or pre-arming).
            if participant.isLocal, let track = state.localVideoTrack {
                LocalVideoPreviewView(
                    track: track,
                    mirrored: state.settings.mirrorLocal,
                    contentMode: .scaleAspectFill
                )
            } else if !participant.isLocal,
                let track = state.remoteVideoTracks[participant.id]
                    ?? (participant.id == state.soleRemoteParticipantId
                        ? state.remoteVideoTrack
                        : nil) {
                // Remote video is never mirrored — the viewer
                // wants the other party's true orientation.
                // `.id(participant.id)` ties the renderer's
                // identity to the participant so a gallery reorder
                // never reuses one peer's `RTCMTLVideoView` for
                // another peer's stream (a track swap for the SAME
                // peer is handled by `updateUIView`).
                LocalVideoPreviewView(
                    track: track,
                    mirrored: false,
                    contentMode: .scaleAspectFill
                )
                .id(participant.id)
            } else {
                Color.lakkoSurfaceSunken
                .overlay(
                    Image(systemName: "video.fill")
                        .font(.title3)
                        .foregroundColor(Color.lakkoTextMuted.opacity(0.6))
                )
            }
        } else {
            ZStack {
                Color.lakkoSurfaceSunken
                Text(initial)
                    .font(.system(size: large ? 80 : 40, weight: .semibold))
                    .foregroundColor(Color.lakkoText)
            }
        }
    }

    private var initial: String {
        let s = displayLabel.first.map { String($0).uppercased() } ?? "?"
        return s
    }

    /// `docs/ux/calls.md` section 4 — who this tile is. "Unknown" is never an
    /// answer (see ``CallPeerName``).
    private var displayLabel: String {
        CallPeerName.tileLabel(
            displayName: participant.displayName, isLocal: participant.isLocal)
    }
}

// MARK: - ControlsBar

/// The 56 pt round control face (UX spec section 5.7): elevated surface at
/// rest, accent when the control is active, danger for End call. The glyph
/// takes the accent foreground on a filled face so it reads on both.
private struct CircleFace: View {
    let systemImage: String
    let active: Bool
    var destructive: Bool = false
    var busy: Bool = false

    static let size: CGFloat = 56

    var body: some View {
        Group {
            if busy {
                ProgressView()
                    .progressViewStyle(.circular)
                    .tint(glyph)
            } else {
                Image(systemName: systemImage)
                    .font(.title3)
                    .foregroundColor(glyph)
            }
        }
        .frame(width: Self.size, height: Self.size)
        .background(background)
        .clipShape(Circle())
    }

    private var background: Color {
        if destructive { return Color.lakkoDanger }
        if active { return Color.lakkoAccent }
        return Color.lakkoSurface
    }

    private var glyph: Color {
        (destructive || active) ? Color.lakkoAccentForeground : Color.lakkoText
    }
}

private struct CircleButton: View {
    let label: String
    let systemImage: String
    let active: Bool
    let destructive: Bool
    /// B2a — busy state: swaps the glyph for a spinner and disables
    /// the button while an async action (e.g. the mid-call camera
    /// acquire) is in flight.
    let busy: Bool
    let onTap: () -> Void

    init(
        label: String,
        systemImage: String,
        active: Bool,
        destructive: Bool = false,
        busy: Bool = false,
        onTap: @escaping () -> Void
    ) {
        self.label = label
        self.systemImage = systemImage
        self.active = active
        self.destructive = destructive
        self.busy = busy
        self.onTap = onTap
    }

    var body: some View {
        Button(action: onTap) {
            CircleFace(
                systemImage: systemImage,
                active: active,
                destructive: destructive,
                busy: busy
            )
        }
        .buttonStyle(.plain)
        .disabled(busy)
        .accessibilityLabel(busy ? "\(label), starting" : label)
    }
}

/// Chat-toggle button — CircleButton with an overlaid unread
/// badge in the top-right corner. The badge surfaces only when
/// there is something unread AND the panel is closed.
private struct ChatToggleButton: View {
    @ObservedObject var state: CallUiState

    var body: some View {
        ZStack(alignment: .topTrailing) {
            CircleButton(
                label: state.chatOpen
                    ? "Close chat"
                    : (state.chatUnreadCount > 0
                        ? "Chat (\(state.chatUnreadCount) unread)"
                        : "Chat"),
                systemImage: "bubble.left.and.bubble.right.fill",
                active: state.chatOpen,
                onTap: { state.setChatOpen(!state.chatOpen) }
            )
            if state.chatUnreadCount > 0 && !state.chatOpen {
                Text(state.chatUnreadCount > 99 ? "99+" : "\(state.chatUnreadCount)")
                    .font(.system(size: 11, weight: .bold))
                    .foregroundColor(Color.lakkoAccentForeground)
                    .padding(.horizontal, 5)
                    .frame(minWidth: 18, minHeight: 18)
                    .background(Capsule().fill(Color.lakkoDanger))
                    .offset(x: 2, y: -2)
                    .allowsHitTesting(false)
            }
        }
    }
}

private struct ControlsBar: View {
    @ObservedObject var state: CallUiState
    /// G3-1-session-bind — end-call action. Routes to
    /// `session.hangUp()` when a real session drives the
    /// screen; falls back to `state.endCall()` (mock path).
    let onEndCall: () -> Void
    /// G3-1-call-controls — mic / camera toggles. Route to the
    /// real `CallSession` (driving `RTCAudio/VideoTrack.isEnabled`)
    /// when a session drives the screen; mock-state fallback
    /// otherwise. The argument is the *desired* enabled state.
    let onSetMic: (Bool) -> Void
    let onSetCamera: (Bool) -> Void

    private var local: CallUiState.Participant? {
        state.participants.first(where: { $0.isLocal })
    }

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 12) {
                micButton
                cameraButton
                switchCameraButton
                participantsButton
                raiseHandButton
                screenShareButton
                ChatToggleButton(state: state)
                settingsButton
                endCallButton
            }
            HStack(spacing: 12) {
                micButton
                cameraButton
                participantsButton
                ChatToggleButton(state: state)
                moreMenu
                endCallButton
            }
            HStack(spacing: 10) {
                micButton
                cameraButton
                moreMenu
                endCallButton
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 14)
        .frame(maxWidth: .infinity)
        .background(Color.lakkoSurfaceSunken)
        .overlay(
            Rectangle()
                .frame(height: 1)
                .foregroundColor(Color.lakkoBorder),
            alignment: .top
        )
    }

    private var micButton: some View {
        CircleButton(
            label: local?.micOn == true ? "Mute microphone" : "Unmute microphone",
            systemImage: local?.micOn == true ? "mic.fill" : "mic.slash.fill",
            active: local?.micOn == false,
            onTap: { onSetMic(!(local?.micOn ?? true)) }
        )
    }

    private var cameraButton: some View {
        CircleButton(
            label: local?.camOn == true ? "Stop camera" : "Start camera",
            systemImage: local?.camOn == true ? "video.fill" : "video.slash.fill",
            active: local?.camOn == false,
            // B2a — while the mid-call camera acquire is in flight the
            // button shows a spinner + is disabled (the session already
            // in-flight-guards the acquire; this is the visual half).
            busy: state.videoPublishState == .starting,
            onTap: { onSetCamera(!(local?.camOn ?? true)) }
        )
    }

    private var canSwitchCamera: Bool {
        state.hasMultipleCameras && local?.camOn == true
    }

    @ViewBuilder
    private var switchCameraButton: some View {
        if canSwitchCamera {
            CircleButton(
                label: "Switch camera",
                systemImage: "arrow.triangle.2.circlepath.camera",
                active: false,
                onTap: { /* WebRTC pod owns facingMode toggle (3a-pod-followup) */ }
            )
        }
    }

    private var participantsButton: some View {
        CircleButton(
            label: "Participants",
            systemImage: "person.3.fill",
            active: state.participantListOpen,
            onTap: { state.setParticipantListOpen(!state.participantListOpen) }
        )
    }

    private var raiseHandButton: some View {
        CircleButton(
            label: local?.handRaised == true ? "Lower hand" : "Raise hand",
            systemImage: "hand.raised.fill",
            active: local?.handRaised == true,
            onTap: { state.setHandRaised(!(local?.handRaised ?? false)) }
        )
    }

    private var screenShareButton: some View {
        CircleButton(
            label: local?.screenSharing == true
                ? "Stop sharing screen" : "Share screen",
            systemImage: "rectangle.on.rectangle",
            active: local?.screenSharing == true,
            onTap: { state.setScreenSharing(!(local?.screenSharing ?? false)) }
        )
    }

    private var settingsButton: some View {
        CircleButton(
            label: "Settings",
            systemImage: "gearshape.fill",
            active: state.settingsOpen,
            onTap: { state.setSettingsOpen(!state.settingsOpen) }
        )
    }

    private var endCallButton: some View {
        CircleButton(
            label: "End call",
            systemImage: "phone.down.fill",
            active: false,
            destructive: true,
            onTap: onEndCall
        )
    }

    /// The controls that do not fit a phone-width row. Same actions, same
    /// state; only the placement differs from the wide layout.
    private var moreMenu: some View {
        Menu {
            Button {
                state.setParticipantListOpen(!state.participantListOpen)
            } label: {
                Label("Participants", systemImage: "person.3")
            }
            Button {
                state.setChatOpen(!state.chatOpen)
            } label: {
                Label(
                    state.chatUnreadCount > 0
                        ? "Chat (\(state.chatUnreadCount) unread)" : "Chat",
                    systemImage: "bubble.left.and.bubble.right")
            }
            Button {
                state.setHandRaised(!(local?.handRaised ?? false))
            } label: {
                Label(
                    local?.handRaised == true ? "Lower hand" : "Raise hand",
                    systemImage: "hand.raised")
            }
            Button {
                state.setScreenSharing(!(local?.screenSharing ?? false))
            } label: {
                Label(
                    local?.screenSharing == true ? "Stop sharing screen" : "Share screen",
                    systemImage: "rectangle.on.rectangle")
            }
            if canSwitchCamera {
                Button {
                    /* WebRTC pod owns facingMode toggle (3a-pod-followup) */
                } label: {
                    Label("Switch camera", systemImage: "arrow.triangle.2.circlepath.camera")
                }
            }
            Button {
                state.setSettingsOpen(!state.settingsOpen)
            } label: {
                Label("Settings", systemImage: "gearshape")
            }
        } label: {
            CircleFace(
                systemImage: "ellipsis",
                active: local?.handRaised == true || local?.screenSharing == true
            )
        }
        .accessibilityLabel("More controls")
    }
}

// MARK: - ParticipantList drawer

private struct ParticipantListDrawer: View {
    @ObservedObject var state: CallUiState

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Participants (\(state.participants.count))")
                    .font(.headline)
                    .foregroundColor(Color.lakkoText)
                Spacer()
                Button(action: { state.setParticipantListOpen(false) }) {
                    Image(systemName: "xmark")
                        .foregroundColor(Color.lakkoTextMuted)
                }
                .accessibilityLabel("Close participant list")
            }
            .padding(16)

            Divider().background(Color.lakkoBorder)

            ScrollView {
                VStack(spacing: 0) {
                    ForEach(state.participants) { p in
                        HStack(spacing: 10) {
                            QualityDot(tier: p.quality, interactive: false, onTap: {})
                            Text(p.displayName.isEmpty
                                 ? (p.isLocal ? "You" : "Unknown")
                                 : p.displayName)
                                .foregroundColor(Color.lakkoText)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            if p.handRaised {
                                Image(systemName: "hand.raised.fill").accessibilityLabel("Hand raised")
                            }
                            if !p.micOn {
                                Image(systemName: "mic.slash.fill")
                                    .foregroundColor(Color.lakkoDanger)
                                    .accessibilityLabel("Muted")
                            }
                            if p.screenSharing {
                                Image(systemName: "rectangle.on.rectangle")
                                    .accessibilityLabel("Sharing screen")
                            }
                        }
                        .padding(.vertical, 10)
                        .padding(.horizontal, 16)
                        Divider().background(Color.lakkoBorder)
                    }
                }
            }
        }
        .frame(width: 320)
        .frame(maxHeight: .infinity)
        .background(Color.lakkoSurface)
        .overlay(
            Rectangle()
                .frame(width: 1)
                .foregroundColor(Color.lakkoBorder),
            alignment: .leading
        )
    }
}

// MARK: - PrejoinView

private struct PrejoinView: View {
    @ObservedObject var state: CallUiState
    /// `docs/ux/calls.md` section 3 — what the camera toggle starts at: off
    /// for a voice call, on for a video one. It used to be hard-coded on, so
    /// the phone button opened a video call.
    let cameraOn: Bool
    let onJoin: (_ displayName: String, _ micOn: Bool, _ camOn: Bool) -> Void
    /// G3-1-prejoin-join — when the screen is driven by a real
    /// session (presented over the chat before dialing), a Cancel
    /// button tears the prepped-but-undialed session down. `nil`
    /// on the standalone design route (no session to abandon).
    var onCancel: (() -> Void)?

    @State private var camOn: Bool
    @State private var micOn: Bool = true

    init(
        state: CallUiState,
        cameraOn: Bool,
        onJoin: @escaping (_ displayName: String, _ micOn: Bool, _ camOn: Bool) -> Void,
        onCancel: (() -> Void)? = nil
    ) {
        self.state = state
        self.cameraOn = cameraOn
        self.onJoin = onJoin
        self.onCancel = onCancel
        _camOn = State(initialValue: cameraOn)
    }

    var body: some View {
        ZStack {
            Color.lakkoBackground.ignoresSafeArea()
            ScrollView {
                // One card (design system section 3): the preview, two
                // toggles, Join. The name field is gone — the roster name
                // comes from the contact, not from a prompt here.
                VStack(spacing: 20) {
                    VStack(spacing: 4) {
                        Text("Join call")
                            .font(.title2)
                            .fontWeight(.semibold)
                            .foregroundColor(Color.lakkoText)
                        // Says what there is to check: a voice call has no
                        // camera to look at until the toggle turns one on.
                        Text(camOn ? "Check your camera and microphone" : "Check your microphone")
                            .font(.subheadline)
                            .foregroundColor(Color.lakkoTextMuted)
                    }

                    preview
                        .aspectRatio(16.0 / 9.0, contentMode: .fit)
                        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))

                    VStack(spacing: 0) {
                        Toggle(isOn: $camOn) {
                            Label("Camera", systemImage: camOn ? "video.fill" : "video.slash.fill")
                                .foregroundColor(Color.lakkoText)
                        }
                        .padding(.vertical, 10)
                        Divider().overlay(Color.lakkoBorder)
                        Toggle(isOn: $micOn) {
                            Label("Microphone", systemImage: micOn ? "mic.fill" : "mic.slash.fill")
                                .foregroundColor(Color.lakkoText)
                        }
                        .padding(.vertical, 10)
                    }
                    .tint(Color.lakkoAccent)

                    Button(action: { onJoin("", micOn, camOn) }) {
                        Text("Join")
                            .font(.headline)
                            .foregroundColor(Color.lakkoAccentForeground)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 12)
                            .background(Color.lakkoAccent)
                            .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.control, style: .continuous))
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Join the call")

                    if let onCancel = onCancel {
                        Button(action: onCancel) {
                            Text("Cancel")
                                .font(.subheadline)
                                .foregroundColor(Color.lakkoTextMuted)
                                .padding(.vertical, 6)
                        }
                        .accessibilityLabel("Cancel the call")
                    }
                }
                .padding(20)
                .background(Color.lakkoSurface)
                .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: LakkoRadius.card, style: .continuous)
                        .stroke(Color.lakkoBorder, lineWidth: 1)
                )
                .frame(maxWidth: 480)
                .padding(24)
            }
        }
    }

    /// G3-1-video-ui-screen — same posture as the in-call self-tile: when
    /// the host has already armed the camera via
    /// `CallSession.startLocalCameraCapture` (rare on the prejoin path —
    /// production typically waits until the user taps Join — but the
    /// testbed + the accept-with-video flow both arm it here), render the
    /// live preview. Otherwise a placeholder on the sunken surface.
    @ViewBuilder
    private var preview: some View {
        if camOn, let track = state.localVideoTrack {
            LocalVideoPreviewView(
                track: track,
                mirrored: state.settings.mirrorLocal,
                contentMode: .scaleAspectFill
            )
        } else {
            ZStack {
                Color.lakkoSurfaceSunken
                VStack(spacing: 8) {
                    Image(systemName: camOn ? "video.fill" : "video.slash.fill")
                        .font(.largeTitle)
                        .foregroundColor(Color.lakkoTextMuted)
                    Text(camOn ? "Your camera starts when you join" : "Camera off")
                        .font(.footnote)
                        .foregroundColor(Color.lakkoTextMuted)
                }
            }
        }
    }
}

// MARK: - PhaseBanner

private struct PhaseBanner: View {
    let phase: CallUiState.CallPhase

    var body: some View {
        Group {
            if let info = bannerInfo {
                Text(info.text)
                    .font(.caption)
                    .fontWeight(.semibold)
                    .foregroundColor(Color.lakkoAccentForeground)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 6)
                    .background(info.colour)
                    .clipShape(Capsule())
            }
        }
    }

    private var bannerInfo: (text: String, colour: Color)? {
        switch phase {
        case .prejoin, .connected:
            return nil
        case .connecting:
            return ("Connecting...", Color.lakkoAccent)
        case .reconnecting(let reason):
            return ("Reconnecting... \(reason)", Color.lakkoWarning)
        case .ended:
            return ("Call ended", Color.lakkoDanger)
        }
    }
}

// MARK: - WaitingMessage

private struct WaitingMessageOverlay: View {
    var body: some View {
        Text("Waiting for others to join...")
            .font(.subheadline)
            .foregroundColor(Color.lakkoText)
            .padding(.horizontal, 24)
            .padding(.vertical, 16)
            .background(Color.lakkoSurface.opacity(0.9))
            .overlay(
                RoundedRectangle(cornerRadius: 12)
                    .stroke(Color.lakkoBorder, lineWidth: 1)
            )
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .accessibilityLabel("Waiting for another participant to join the call")
    }
}

// MARK: - CallEndedOverlay

private struct CallEndedOverlay: View {
    let onDismiss: () -> Void

    var body: some View {
        ZStack {
            Color.lakkoBackground.opacity(0.85).ignoresSafeArea()
            VStack(spacing: 12) {
                Text("Call ended")
                    .font(.largeTitle)
                    .fontWeight(.semibold)
                    .foregroundColor(Color.lakkoText)
                Text("Returning to your chats...")
                    .foregroundColor(Color.lakkoTextMuted)
            }
        }
        .task {
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            onDismiss()
        }
    }
}

// MARK: - CallScreen top-level

public struct CallScreen: View {
    @StateObject private var state = CallUiState()
    /// One sound controller per CallScreen lifetime; deallocated
    /// with the view (which deactivates the underlying
    /// AVAudioPlayer instances via its `deinit`).
    @StateObject private var sounds = CallSoundsObservable()
    /// Last observed remote-participant ids, used to fire
    /// `join` / `leave` cues + chat system events on deltas
    /// while the call is `connected`. Tracked in the view
    /// rather than the state object because it is bookkeeping
    /// for side effects, not model-of-the-call state.
    @State private var lastRemoteIds: [String] = []
    /// G3-1-session-bind — the live call session driving this
    /// screen. When `nil` (SwiftUI previews / design work) the
    /// screen runs on `seedMockState()` exactly as before. When
    /// injected, `onAppear` wires `state.driveFrom(session:)` so
    /// phase + roster + video reflect the real call, and the
    /// end-call button routes to `session.hangUp()`.
    private let session: CallSession?
    /// Combine subscriptions from `driveFrom(session:)`, held
    /// for the screen's lifetime.
    @State private var sessionBindings: [AnyCancellable] = []
    /// `true` when the screen renders an externally-owned `CallUiState`
    /// (the SFU group-call path, where `SfuCallUi` drives the state).
    /// In that mode there is no `session` to `driveFrom` and no prejoin
    /// screen — the call is already connecting when presented.
    private let externallyDriven: Bool
    let onExit: () -> Void
    /// A camera start this screen refused because camera access is denied
    /// (``CameraPermission``). Shown as an alert; `nil` when none.
    @State private var permissionMessage: String?
    /// When media started flowing, for the status line's running time. `nil`
    /// until the call is connected.
    @State private var connectedAt: Date?
    /// `docs/ux/calls.md` section 3 — which button opened this call, which is
    /// what the pre-join's camera toggle starts at. The toggles then decide
    /// what is actually offered.
    private let button: CallButtonKind
    /// The other person's name, resolved by the call's owner. Drives the
    /// status line, the remote tile and the encryption sheet's sentence.
    private let peerName: String?

    /// `true` when the camera controls drive a real call (a 1x1 session, or
    /// an externally driven SFU call), so turning the camera on must settle
    /// the camera permission first. The mock design route has no camera.
    private var gatesCamera: Bool { session != nil || externallyDriven }

    public init(
        session: CallSession? = nil,
        button: CallButtonKind = .voice,
        peerName: String? = nil,
        onExit: @escaping () -> Void
    ) {
        self.session = session
        self.externallyDriven = false
        self.button = button
        self.peerName = peerName
        self.onExit = onExit
    }

    /// SFU group-call init: render a `CallUiState` that an external
    /// owner (`SfuCallUi`) already drives. No `session`, no prejoin.
    public init(externalState: CallUiState, onExit: @escaping () -> Void) {
        _state = StateObject(wrappedValue: externalState)
        self.session = nil
        self.externallyDriven = true
        self.button = .voice
        self.peerName = nil
        self.onExit = onExit
    }

    public var body: some View {
        ZStack {
            switch state.phase {
            case .prejoin where !externallyDriven:
                PrejoinView(
                    state: state,
                    cameraOn: CallMediaChoice.prejoinCameraOn(for: button),
                    onJoin: { displayName, micOn, camOn in
                        // G3-1-prejoin-join — with a real session the
                        // user picks mic / camera here and Join dials.
                        // `startCall` builds the session in `.idle`
                        // (mapped to `.prejoin`) without dialing, so
                        // this is the first SDP-producing call. Web is
                        // audio-only; iOS carries the camera choice.
                        if let session = session {
                            _ = displayName
                            // Settle the camera permission before the dial
                            // starts a capture: a denied camera refuses with
                            // an explanation, and the user can turn the
                            // camera off and join with audio only.
                            Task { @MainActor in
                                guard await CameraPermission.ensureGranted(forVideo: camOn) else {
                                    permissionMessage = CameraPermission.deniedMessage
                                    return
                                }
                                // `docs/ux/calls.md` section 3 — the toggles
                                // decide what is offered, so what the callee
                                // is told is what this call actually carries.
                                let media = CallMediaChoice.offeredMedia(
                                    micOn: micOn, cameraOn: camOn)
                                session.dial(audio: media.audio, video: media.video)
                            }
                            return
                        }
                        // Mock design route — no session injected.
                        state.seedMockState()
                        state.setMic(micOn)
                        state.setCamera(camOn)
                        // displayName intentionally not wired into state —
                        // production code maps it onto the local
                        // participant's displayName via the contact-id
                        // → name resolution path the chat layer owns.
                        _ = displayName
                        state.join()
                    },
                    onCancel: session == nil
                        ? nil
                        : {
                            // Abandon a prepped-but-undialed call:
                            // hanging up an `.idle` session drives it
                            // to `.done`, and `onExit` clears it.
                            session?.hangUp(reason: .hungUp)
                            onExit()
                        }
                )
            default:
                callView
            }
        }
        // Phase → sound mapping. Mirrors the same trigger map
        // on web (`Call.tsx`) and Android (`CallScreen.kt`) so
        // the audible UX is identical across platforms. iOS-17
        // two-parameter `onChange` form (the single-parameter
        // form is deprecated in iOS 17).
        .onChange(of: state.phase) { _, newPhase in
            // `docs/ux/calls.md` section 4 — the running time starts when
            // media does, and only then ("Connected only when media flows").
            if case .connected = newPhase {
                if connectedAt == nil { connectedAt = Date() }
            } else {
                connectedAt = nil
            }
            switch newPhase {
            case .prejoin:
                sounds.controller.stopAll()
            case .connecting:
                sounds.controller.startLoop(.ringbackOutgoing)
            case .connected:
                sounds.controller.stopLoop(.ringbackOutgoing)
                sounds.controller.playOnce(.join)
            case .reconnecting:
                sounds.controller.stopLoop(.ringbackOutgoing)
            case .ended:
                sounds.controller.stopAll()
                sounds.controller.playOnce(.leave)
            }
        }
        // Peer arrival / departure cues + chat system events.
        // The id-based diff catches "Alice left, Bob joined"
        // in the same StateFlow tick (which a pure count would
        // miss).
        .onChange(of: state.participants.map(\.id)) { _, _ in
            let remote = state.participants.filter { !$0.isLocal }
            let remoteIds = remote.map(\.id)
            if case .connected = state.phase, !lastRemoteIds.isEmpty {
                for p in remote where !lastRemoteIds.contains(p.id) {
                    sounds.controller.playOnce(.join)
                    state.addSystemEvent("\(p.displayName) joined the call")
                }
                let currentIds = Set(remoteIds)
                for prevId in lastRemoteIds where !currentIds.contains(prevId) {
                    sounds.controller.playOnce(.leave)
                    state.addSystemEvent("A participant left the call")
                    // Display name has been dropped from state
                    // by the time we observe the deletion — the
                    // WebRTC pod sprint will preserve the last-
                    // known name (out of scope for S-3-A-chat).
                }
            }
            lastRemoteIds = remoteIds
        }
        .onAppear {
            // G3-1-session-bind — drive the UI from the real
            // session when one is injected; otherwise the
            // prejoin → seedMockState() path runs as before.
            if let session, sessionBindings.isEmpty {
                sessionBindings = state.driveFrom(session: session, peerName: peerName)
            }
        }
        // A name that resolves mid-call (their profile arrives while the call
        // runs) reaches the status line straight away, and the tile at the
        // next roster rebuild.
        .onChange(of: peerName) { _, newValue in
            guard !externallyDriven else { return }
            state.setPeerName(newValue)
        }
        .onDisappear {
            sounds.controller.stopAll()
            sessionBindings.removeAll()
            state.reset()
        }
        .alert(
            "Camera unavailable",
            isPresented: Binding(
                get: { permissionMessage != nil },
                set: { if !$0 { permissionMessage = nil } }
            ),
            presenting: permissionMessage
        ) { _ in
            Button("OK", role: .cancel) { permissionMessage = nil }
        } message: { message in
            Text(message)
        }
        // Calls are dark by design on every platform (UX spec section 4): the
        // shared tokens resolve to their dark variants inside this screen and
        // every sheet it presents; no second palette.
        .preferredColorScheme(.dark)
    }

    /// Drive the camera choice onto whatever owns the call. Turning the
    /// camera ON reaches here only after ``CameraPermission`` granted it
    /// (see the `onSetCamera` control).
    private func applyCamera(_ on: Bool) {
        guard let session else {
            // Mock route, or an externally driven SFU call whose owner
            // observes `camOn` on the state and drives its session.
            state.setCamera(on)
            return
        }
        guard on else {
            session.setCameraEnabled(false)
            return
        }
        // 1×1 mid-call camera-on (piece B2b): addLocalVideo acquires + adds a
        // video track + renegotiates the first time (a call started
        // audio-only), and re-enables an existing sender after. Idempotent +
        // in-flight-guarded. On a capture failure revert the toggle + log the
        // reason — fail-safe, the call stays up audio-only (invariant #4).
        // Mirror of the web Chat.tsx camera effect.
        Task {
            do {
                try await session.addLocalVideo()
            } catch {
                state.setCamera(false)
                NSLog(
                    "CallScreen: addLocalVideo failed: %@",
                    String(describing: error)
                )
            }
        }
    }

    private var callView: some View {
        ZStack(alignment: .top) {
            Color.lakkoBackground.ignoresSafeArea()

            VStack(spacing: 0) {
                // Header bar (`docs/ux/calls.md` section 4): the status line
                // with the encryption chip beside it, quality dot on the
                // right.
                HStack(spacing: 10) {
                    CallStatusLine(
                        title: state.peerName,
                        phase: state.phase,
                        connectedAt: connectedAt)
                    if state.e2ee.active {
                        EncryptionChip { state.setEncryptionSheetOpen(true) }
                    }
                    Spacer(minLength: 0)
                    QualityIndicator(state: state)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(Color.lakkoSurface)
                .overlay(
                    Rectangle()
                        .frame(height: 1)
                        .foregroundColor(Color.lakkoBorder),
                    alignment: .bottom
                )

                // Main area: active speaker + gallery.
                ScrollView {
                    VStack(spacing: 12) {
                        if let active = state.activeSpeaker {
                            ParticipantTile(
                                participant: active, large: true, state: state
                            )
                            .frame(maxWidth: 1080)
                        }
                        LazyVGrid(
                            columns: [GridItem(.adaptive(minimum: 180), spacing: 10)],
                            spacing: 10
                        ) {
                            ForEach(state.galleryParticipants) { p in
                                ParticipantTile(
                                    participant: p, large: false, state: state
                                )
                            }
                        }
                        .frame(maxWidth: 1080)
                    }
                    .padding(16)
                }
                .contentShape(Rectangle())
                .onTapGesture {
                    // Tap-outside-to-close for the quality popover.
                    if state.qualityPopoverOpen {
                        state.setQualityPopoverOpen(false)
                    }
                }

                ControlsBar(
                    state: state,
                    onEndCall: {
                        if let session {
                            session.hangUp()
                        } else {
                            state.endCall()
                        }
                    },
                    onSetMic: { on in
                        if let session {
                            session.setMicrophoneEnabled(on)
                        } else {
                            state.setMic(on)
                        }
                    },
                    onSetCamera: { on in
                        if on && gatesCamera {
                            // Camera on: settle the permission first. A denial
                            // leaves the camera off and explains why.
                            Task { @MainActor in
                                guard await CameraPermission.ensureGranted() else {
                                    permissionMessage = CameraPermission.deniedMessage
                                    return
                                }
                                applyCamera(true)
                            }
                        } else {
                            applyCamera(on)
                        }
                    }
                )
            }

            // Centre overlay when alone. The ZStack aligns its children to the
            // top, so the overlay needs its own full-size frame to sit in the
            // centre; without it the message covered the safety number.
            if state.isAlone && state.phase == .connected {
                WaitingMessageOverlay()
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .center)
                    .allowsHitTesting(false)
            }

            // Top-centre phase banner.
            VStack {
                PhaseBanner(phase: state.phase)
                    .padding(.top, 12)
                Spacer()
            }

            // Drawer slides in from the right.
            if state.participantListOpen {
                HStack {
                    Spacer()
                    ParticipantListDrawer(state: state)
                        .transition(.move(edge: .trailing))
                }
                .zIndex(10)
            }

            // Call-ended overlay (auto-navigates after 3s).
            if case .ended = state.phase {
                CallEndedOverlay(onDismiss: onExit)
                    .zIndex(30)
            }
        }
        .animation(.easeInOut(duration: 0.2), value: state.participantListOpen)
        // Gear-menu sheet (S-3-A-settings). `.sheet` gives us
        // the native iOS bottom-detent presentation; iPad gets
        // the centred form-sheet style automatically.
        .sheet(
            isPresented: Binding(
                get: { state.settingsOpen },
                set: { state.setSettingsOpen($0) }
            )
        ) {
            SettingsSheet(state: state)
        }
        // Chat sheet (S-3-A-chat). Same `.sheet` mechanism;
        // the gear and chat are mutually exclusive on iOS by
        // construction (presenting one dismisses the other).
        .sheet(
            isPresented: Binding(
                get: { state.chatOpen },
                set: { state.setChatOpen($0) }
            )
        ) {
            ChatSheet(state: state)
        }
        // `docs/ux/calls.md` section 5 — the safety number, only when asked for.
        .sheet(
            isPresented: Binding(
                get: { state.encryptionSheetOpen },
                set: { state.setEncryptionSheetOpen($0) }
            )
        ) {
            EncryptionSheet(
                peerName: state.peerName,
                safetyNumber: state.e2ee.shortFingerprint,
                onDone: { state.setEncryptionSheetOpen(false) })
        }
    }
}

// MARK: - SettingsSheet (S-3-A-settings)

/// In-call settings sheet — device pickers + processing
/// toggles. Mirrors the same control surface as the web
/// `SettingsPanel` and the Android `SettingsSheet`.
private struct SettingsSheet: View {
    @ObservedObject var state: CallUiState

    private var audioInputs: [CallUiState.CallDevice] {
        state.settings.availableDevices.filter { $0.kind == .audioInput }
    }
    private var videoInputs: [CallUiState.CallDevice] {
        state.settings.availableDevices.filter { $0.kind == .videoInput }
    }
    private var audioOutputs: [CallUiState.CallDevice] {
        state.settings.availableDevices.filter { $0.kind == .audioOutput }
    }

    var body: some View {
        NavigationStack {
            Form {
                Section(header: Text("Devices")) {
                    devicePicker(
                        title: "Microphone",
                        devices: audioInputs,
                        selection: Binding(
                            get: { state.settings.selectedAudioInput },
                            set: { state.setSelectedAudioInput($0) }
                        )
                    )
                    devicePicker(
                        title: "Camera",
                        devices: videoInputs,
                        selection: Binding(
                            get: { state.settings.selectedVideoInput },
                            set: { state.setSelectedVideoInput($0) }
                        )
                    )
                    devicePicker(
                        title: "Speaker",
                        devices: audioOutputs,
                        selection: Binding(
                            get: { state.settings.selectedAudioOutput },
                            set: { state.setSelectedAudioOutput($0) }
                        )
                    )
                }
                Section(header: Text("Audio processing")) {
                    Toggle(
                        "Noise suppression",
                        isOn: Binding(
                            get: { state.settings.noiseSuppression },
                            set: { state.setNoiseSuppression($0) }
                        )
                    )
                    Toggle(
                        "Echo cancellation",
                        isOn: Binding(
                            get: { state.settings.echoCancellation },
                            set: { state.setEchoCancellation($0) }
                        )
                    )
                    Toggle(
                        "Automatic gain control",
                        isOn: Binding(
                            get: { state.settings.autoGain },
                            set: { state.setAutoGain($0) }
                        )
                    )
                }
                Section(header: Text("Video")) {
                    Toggle(
                        "Mirror local preview",
                        isOn: Binding(
                            get: { state.settings.mirrorLocal },
                            set: { state.setMirrorLocal($0) }
                        )
                    )
                }
                Section(
                    header: Text("Network"),
                    footer: Text("Caps video at 360p — useful on metered mobile connections.")
                ) {
                    Toggle(
                        "Low-data mode",
                        isOn: Binding(
                            get: { state.settings.lowDataMode },
                            set: { state.setLowDataMode($0) }
                        )
                    )
                }
            }
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { state.setSettingsOpen(false) }
                }
            }
        }
    }

    @ViewBuilder
    private func devicePicker(
        title: String,
        devices: [CallUiState.CallDevice],
        selection: Binding<String?>
    ) -> some View {
        Picker(title, selection: selection) {
            Text("System default").tag(String?.none)
            ForEach(devices) { device in
                Text(device.label.isEmpty ? device.kind.rawValue : device.label)
                    .tag(String?.some(device.id))
            }
        }
    }
}

// MARK: - ChatSheet (S-3-A-chat)

/// In-call chat sheet. Text-only; system events ("Alice
/// joined") rendered inline as grey italic centred lines.
/// Mock transport — messages live in CallUiState only until
/// the WebRTC pod wires real MLS application-message
/// round-trips.
private struct ChatSheet: View {
    @ObservedObject var state: CallUiState
    @FocusState private var composerFocused: Bool

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 10) {
                            if state.chatMessages.isEmpty {
                                Text("No messages yet. Only people in this call can read what you send.")
                                .font(.system(size: 13))
                                .foregroundColor(Color.lakkoTextMuted)
                                .multilineTextAlignment(.center)
                                .frame(maxWidth: .infinity)
                                .padding(.top, 40)
                                .padding(.horizontal, 16)
                            } else {
                                ForEach(state.chatMessages) { msg in
                                    ChatMessageRow(msg: msg).id(msg.id)
                                }
                            }
                        }
                        .padding(.horizontal, 14)
                        .padding(.vertical, 12)
                    }
                    .onChange(of: state.chatMessages.count) { _, _ in
                        // Auto-scroll to the newest message.
                        if let last = state.chatMessages.last {
                            withAnimation(.easeOut(duration: 0.15)) {
                                proxy.scrollTo(last.id, anchor: .bottom)
                            }
                        }
                    }
                }

                Divider().background(Color.lakkoBorder)

                HStack(spacing: 8) {
                    TextField(
                        "Send a message",
                        text: Binding(
                            get: { state.composeText },
                            set: { state.setComposeText($0) }
                        )
                    )
                    .focused($composerFocused)
                    .textFieldStyle(.roundedBorder)
                    .submitLabel(.send)
                    .onSubmit(send)

                    Button(action: send) {
                        Text("Send")
                            .font(.system(size: 14, weight: .semibold))
                            .foregroundColor(canSend ? Color.lakkoAccentForeground : Color.lakkoTextMuted)
                            .padding(.horizontal, 14)
                            .padding(.vertical, 8)
                            .background(
                                RoundedRectangle(cornerRadius: LakkoRadius.control)
                                    .fill(canSend ? Color.lakkoAccent : Color.lakkoSurfaceSunken)
                            )
                    }
                    .disabled(!canSend)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
            }
            .background(Color.lakkoBackground)
            .navigationTitle("Chat")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { state.setChatOpen(false) }
                }
            }
            .onAppear { composerFocused = true }
        }
    }

    private var canSend: Bool {
        !state.composeText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func send() {
        state.sendChatMessage(state.composeText)
    }
}

private struct ChatMessageRow: View {
    let msg: CallUiState.ChatMessage

    private static let timeFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm"
        return f
    }()

    var body: some View {
        if msg.kind == .system {
            HStack {
                Spacer()
                Text(msg.text)
                    .font(.system(size: 12).italic())
                    .foregroundColor(Color.lakkoTextMuted)
                    .multilineTextAlignment(.center)
                Spacer()
            }
        } else {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 8) {
                    Text(msg.senderName)
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundColor(Color.lakkoAccent)
                    Text(Self.timeFormatter.string(from: msg.ts))
                        .font(.system(size: 12))
                        .foregroundColor(Color.lakkoTextMuted)
                }
                Text(msg.text)
                    .font(.system(size: 14))
                    .foregroundColor(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}
