// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import SwiftUI
import WebRTC

/// The call screen (`docs/ux/calls.md` section 4).
///
/// ## What the redesign changed
///
/// The old screen was a debugger's view of a call: a header bar with a hairline,
/// a scrolling tile grid (for a call with one other person in it), a floating
/// phase badge repeating the word the status line already carried, and a row of
/// nine loose buttons welded to the bottom edge — among them a `More` menu
/// holding the two controls people reach for mid-sentence, and no speaker button
/// at all. It also opened, for a *voice* call, on a settings card asking the
/// user to check a camera that was not on.
///
/// What it is now:
///
///   * **a voice call is a portrait** — the other person's initials large and
///     centred over a blurred, darkened ground built from their own colour,
///     their name in the largest type on the screen, and one quiet line under it
///     with the state or the running time;
///   * **a video call fills the screen** with the other person, with the
///     self-view a small draggable rectangle in a corner and the flip-camera
///     button on that rectangle, where the thing it changes is visible;
///   * **one row of equal round buttons** in a single rounded translucent bar
///     floating above the bottom safe area — Speaker, Mute, End for a voice
///     call, plus Camera for a video one, with no `More` menu in a 1x1 call at
///     all;
///   * **the off state is the loud one**, so a muted microphone is solid white
///     and unmissable rather than tinted in the accent colour, which read as
///     "this is on".
///
/// A group call keeps the tile grid and puts participants, chat and settings
/// behind one overflow button, which is the only place a menu is allowed.
///
/// What decides the shape — which controls exist, which can be pressed, what
/// the status line says, when the row hides itself — is
/// ``CallScreenPresentation``, not this file, so the rules are testable.

// MARK: - QualityDot

private struct QualityDot: View {
    let tier: CallUiState.QualityTier
    let interactive: Bool
    let onTap: () -> Void

    var body: some View {
        Circle()
            .fill(tier.colour)
            .frame(width: 8, height: 8)
            .contentShape(Rectangle().inset(by: -8))
            .onTapGesture {
                if interactive { onTap() }
            }
            .accessibilityLabel("Connection quality: \(tier.rawValue)")
    }
}

// MARK: - EncryptionChip

/// `docs/ux/calls.md` sections 4 and 5 — a lock and one word, next to the
/// status line. It replaces the strip of sixteen digits that used to run across
/// the top of every call: unreadable, meaningless to anyone who has not been
/// told what it is, and occupying the most valuable line on the screen. The
/// digits now live one tap away, in the encryption sheet, where there is room to
/// say what they are for.
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
            .foregroundStyle(.white)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(Capsule().fill(Color.white.opacity(0.16)))
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

/// The one line the whole screen agrees on (`docs/ux/calls.md` section 4): the
/// name, then the state or the running time — never both, and never repeated in
/// a badge floating somewhere else. The old screen printed `Connecting...` in a
/// coloured pill at the top AND had a status line underneath it; this is the one
/// line, and ``CallStatusText`` decides its words.
private struct CallStatusLine: View {
    let name: String
    let phase: CallUiState.CallPhase
    let connectedAt: Date?
    let e2eeActive: Bool
    let notice: String?
    /// Large, centred type for the portrait; compact for the overlay on video.
    let prominent: Bool
    let onEncryptionTap: () -> Void

    var body: some View {
        VStack(spacing: prominent ? 8 : 2) {
            Text(name)
                .font(prominent ? .system(size: 34, weight: .semibold) : .headline)
                .foregroundStyle(.white)
                .lineLimit(1)
                .minimumScaleFactor(0.6)

            HStack(spacing: 8) {
                statusText
                if e2eeActive {
                    EncryptionChip(onTap: onEncryptionTap)
                }
            }

            if let notice {
                Text(notice)
                    .font(.footnote)
                    .foregroundStyle(Color.white.opacity(0.6))
                    .multilineTextAlignment(.center)
            }
        }
        .multilineTextAlignment(.center)
    }

    @ViewBuilder
    private var statusText: some View {
        if case .connected = phase, let connectedAt {
            // One tick a second while the call runs; the view is the only thing
            // that needs a clock, so it owns it.
            TimelineView(.periodic(from: connectedAt, by: 1)) { context in
                let elapsed = Int64(max(0, context.date.timeIntervalSince(connectedAt)) * 1000)
                line(
                    CallStatusText.line(phase: phase, durationMs: elapsed),
                    spoken: CallStatusText.spokenLine(phase: phase, durationMs: elapsed))
            }
        } else {
            line(
                CallStatusText.line(phase: phase, durationMs: nil),
                spoken: CallStatusText.spokenLine(phase: phase, durationMs: nil))
        }
    }

    @ViewBuilder
    private func line(_ text: String?, spoken: String?) -> some View {
        if let text {
            Text(text)
                .font(prominent ? .callout : .subheadline)
                .foregroundStyle(Color.white.opacity(0.7))
                .monospacedDigit()
                .accessibilityLabel(spoken ?? text)
        }
    }
}

// MARK: - QualityIndicator (dot + popover)

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

// MARK: - ParticipantTile (group calls)

/// A tile in a group call's grid. A 1x1 call has no tiles at all any more — it
/// is a portrait or a full-screen video.
private struct ParticipantTile: View {
    let participant: CallUiState.Participant
    let large: Bool
    @ObservedObject var state: CallUiState

    var isPinned: Bool { state.pinnedId == participant.id }

    var body: some View {
        ZStack {
            videoSurface
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
            if !participant.micOn && !participant.isLocal {
                Image(systemName: "mic.slash.fill")
                    .font(large ? .title : .body)
                    .foregroundColor(Color.lakkoDanger)
                    .padding(large ? 16 : 10)
                    .background(Color.lakkoBackground.opacity(0.55))
                    .clipShape(Circle())
            }
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
                    QualityDot(tier: participant.quality, interactive: false, onTap: {})
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
        .onTapGesture {
            guard !participant.isLocal else { return }
            state.pin(isPinned ? nil : participant.id)
        }
    }

    @ViewBuilder
    private var videoSurface: some View {
        if participant.camOn {
            if participant.isLocal, let track = state.localVideoTrack {
                LocalVideoPreviewView(
                    track: track,
                    mirrored: state.settings.mirrorLocal,
                    contentMode: .scaleAspectFill)
            } else if !participant.isLocal,
                let track = state.remoteVideoTracks[participant.id]
                    ?? (participant.id == state.soleRemoteParticipantId
                        ? state.remoteVideoTrack
                        : nil) {
                // Remote video is never mirrored — the viewer wants the other
                // party's true orientation. `.id(participant.id)` ties the
                // renderer's identity to the participant so a gallery reorder
                // never reuses one peer's view for another peer's stream.
                LocalVideoPreviewView(track: track, mirrored: false, contentMode: .scaleAspectFill)
                    .id(participant.id)
            } else {
                Color.lakkoSurfaceSunken
            }
        } else {
            ZStack {
                Color.lakkoSurfaceSunken
                InitialsAvatar(
                    name: displayLabel, identity: participant.id,
                    size: large ? 96 : 56, placeholderSymbol: "person.fill")
            }
        }
    }

    /// `docs/ux/calls.md` section 4 — who this tile is. "Unknown" is never an
    /// answer (see ``CallPeerName``).
    private var displayLabel: String {
        CallPeerName.tileLabel(
            displayName: participant.displayName, isLocal: participant.isLocal)
    }
}

// MARK: - ParticipantList drawer (group calls)

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
                            Text(CallPeerName.tileLabel(
                                displayName: p.displayName, isLocal: p.isLocal))
                                .foregroundColor(Color.lakkoText)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            if !p.micOn {
                                Image(systemName: "mic.slash.fill")
                                    .foregroundColor(Color.lakkoDanger)
                                    .accessibilityLabel("Muted")
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
    }
}

// MARK: - CallEndedOverlay

private struct CallEndedOverlay: View {
    /// How long media flowed, or `nil` for a call that never connected — which
    /// is the difference between a call and an attempt, and is why the duration
    /// is not simply printed as `0:00`.
    let durationMs: Int64?
    let onDismiss: () -> Void

    var body: some View {
        ZStack {
            Color.black.opacity(0.75).ignoresSafeArea()
            VStack(spacing: 6) {
                Text("Call ended")
                    .font(.title2)
                    .fontWeight(.semibold)
                    .foregroundStyle(.white)
                if let durationMs {
                    Text(CallTranscriptRow.durationLabel(ms: durationMs))
                        .font(.callout)
                        .foregroundStyle(Color.white.opacity(0.7))
                        .monospacedDigit()
                        .accessibilityLabel(CallTranscriptRow.spokenDuration(ms: durationMs))
                }
            }
        }
        .task {
            // Section 4's state table: the screen closes itself after 2 s.
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            onDismiss()
        }
    }
}

// MARK: - CallScreen top-level

public struct CallScreen: View {
    @StateObject private var state = CallUiState()
    /// One sound controller per CallScreen lifetime; deallocated with the view.
    @StateObject private var sounds = CallSoundsObservable()
    /// Last observed remote-participant ids, used to fire join / leave cues +
    /// chat system events on deltas while the call is connected.
    @State private var lastRemoteIds: [String] = []
    private let session: CallSession?
    @State private var sessionBindings: [AnyCancellable] = []
    /// `true` when the screen renders an externally-owned `CallUiState` (the
    /// SFU group-call path, where `SfuCallUi` drives the state).
    private let externallyDriven: Bool
    let onExit: () -> Void
    /// A camera start this screen refused because camera access is denied.
    @State private var permissionMessage: String?
    /// When media started flowing, for the status line's running time.
    @State private var connectedAt: Date?
    /// How long media flowed, captured as the call ends so the "Call ended"
    /// screen can say it (section 4's state table). `nil` for a call that never
    /// connected.
    @State private var endedDurationMs: Int64?
    /// Which button opened this call, and what that button opened
    /// (`docs/ux/calls.md` sections 3.1 and 3.2).
    private let button: CallButtonKind
    private let surface: CallStartSurface?
    /// The other person's name, resolved by the call's owner.
    private let peerName: String?
    /// One plain line about this call's media (today: the no-camera downgrade).
    private let notice: String?
    /// Place the call from the camera check.
    private let onPlaceCall: ((_ micOn: Bool, _ cameraOn: Bool) -> Void)?

    /// Section 4 — the controls fade out on a connected video call and come
    /// back on a tap; on a voice call they never hide.
    @State private var controlsVisible = true
    @State private var hideTask: Task<Void, Never>?
    /// Where the user dragged the self-view to.
    @State private var selfViewOffset: CGSize = .zero
    @GestureState private var selfViewDrag: CGSize = .zero

    /// `true` when the camera controls drive a real call, so turning the camera
    /// on must settle the camera permission first.
    private var gatesCamera: Bool { session != nil || externallyDriven }

    public init(
        session: CallSession? = nil,
        button: CallButtonKind = .voice,
        surface: CallStartSurface? = nil,
        peerName: String? = nil,
        notice: String? = nil,
        onPlaceCall: ((_ micOn: Bool, _ cameraOn: Bool) -> Void)? = nil,
        onExit: @escaping () -> Void
    ) {
        self.session = session
        self.externallyDriven = false
        self.button = button
        self.surface = surface
        self.peerName = peerName
        self.notice = notice
        self.onPlaceCall = onPlaceCall
        self.onExit = onExit
    }

    /// SFU group-call init: render a `CallUiState` that an external owner
    /// (`SfuCallUi`) already drives. No session, and never a camera check —
    /// the call is already connecting when presented.
    public init(externalState: CallUiState, onExit: @escaping () -> Void) {
        _state = StateObject(wrappedValue: externalState)
        self.session = nil
        self.externallyDriven = true
        self.button = .voice
        self.surface = nil
        self.peerName = nil
        self.notice = nil
        self.onPlaceCall = nil
        self.onExit = onExit
    }

    // MARK: - What the screen is showing

    /// The camera check is shown only for the one case that asked for it: an
    /// outgoing video call, before it is placed. Everything else — a voice call
    /// (which is already on the wire), an accepted incoming call, a group call —
    /// goes straight to the call.
    private var showsCameraCheck: Bool {
        guard !externallyDriven, session != nil, surface == .cameraCheck else { return false }
        if case .cameraCheck = state.phase { return true }
        return false
    }

    /// A voice call is dialed before this screen is ever presented, so the
    /// session's brief `.idle` must not read as "camera check" — it reads as
    /// `Calling...`, which is what is happening.
    private var displayPhase: CallUiState.CallPhase {
        if case .cameraCheck = state.phase, !showsCameraCheck { return .calling }
        return state.phase
    }

    private var remoteParticipants: [CallUiState.Participant] {
        state.participants.filter { !$0.isLocal }
    }

    private var soleRemoteVideoTrack: RTCVideoTrack? {
        guard let sole = state.soleRemoteParticipantId else { return nil }
        return state.remoteVideoTracks[sole] ?? state.remoteVideoTrack
    }

    private var layout: CallScreenLayout {
        CallScreenPresentation.layout(
            remoteCount: remoteParticipants.count,
            remoteVideo: soleRemoteVideoTrack != nil)
    }

    private var local: CallUiState.Participant? {
        state.participants.first(where: { $0.isLocal })
    }

    /// Does this call carry video, so the camera button has something to do?
    /// The header button that opened it, the local camera, and a remote video
    /// frame each answer yes; a plain voice call answers no and its row is
    /// Speaker, Mute, End (`docs/ux/calls.md` section 4.1).
    private var videoCall: Bool {
        if button == .video { return true }
        if local?.camOn == true { return true }
        return soleRemoteVideoTrack != nil || !state.remoteVideoTracks.isEmpty
    }

    private var controls: [CallControl] {
        CallScreenPresentation.controls(layout: layout, videoCall: videoCall)
    }

    private var displayName: String {
        CallPeerName.resolve(profileName: state.peerName, chatTitle: nil)
            ?? (externallyDriven ? "Group call" : CallPeerName.placeholder)
    }

    /// The identity the ground and the avatar are tinted from. The peer's name
    /// when there is one, so a call and that person's chat row agree.
    private var tintIdentity: String {
        state.peerName ?? "lakko.call"
    }

    public var body: some View {
        ZStack {
            if showsCameraCheck {
                CameraCheckScreen(
                    peerName: state.peerName ?? peerName,
                    identity: tintIdentity,
                    onCall: { micOn, cameraOn in
                        onPlaceCall?(micOn, cameraOn)
                    },
                    onCancel: {
                        // Abandon a prepared-but-unplaced call: hanging up an
                        // idle session drives it to done, and `onExit` clears it.
                        session?.hangUp(reason: .hungUp)
                        onExit()
                    })
            } else {
                callView
            }
        }
        .onChange(of: state.phase) { _, newPhase in
            // Section 4 — the running time starts when media does, and only
            // then ("Connected only when media flows").
            if case .connected = newPhase {
                if connectedAt == nil { connectedAt = Date() }
            } else {
                if let connectedAt {
                    endedDurationMs = Int64(max(0, Date().timeIntervalSince(connectedAt)) * 1000)
                }
                connectedAt = nil
            }
            switch newPhase {
            case .cameraCheck:
                sounds.controller.stopAll()
            case .calling, .connecting:
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
            restartAutoHide()
        }
        // A call becomes a video call when the first remote frame lands, not
        // when its phase changes — so the fade-out has to be armed from the
        // layout too, or the row of a connected video call would never hide.
        .onChange(of: layout) { _, _ in
            restartAutoHide()
        }
        // Peer arrival / departure cues + chat system events. The id-based diff
        // catches "Alice left, Bob joined" in the same tick.
        .onChange(of: state.participants.map(\.id)) { _, _ in
            let remote = remoteParticipants
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
                }
            }
            lastRemoteIds = remoteIds
        }
        .onAppear {
            if let session, sessionBindings.isEmpty {
                sessionBindings = state.driveFrom(session: session, peerName: peerName)
            }
            if let notice { state.setMediaNotice(notice) }
            // The route the platform actually settled on, not one we assumed.
            state.setSpeakerOn(CallAudioRoute.isSpeakerOn())
            restartAutoHide()
        }
        // A name that resolves mid-call reaches the status line straight away.
        .onChange(of: peerName) { _, newValue in
            guard !externallyDriven else { return }
            state.setPeerName(newValue)
        }
        .onDisappear {
            hideTask?.cancel()
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
        // Calls are dark by design on every platform (UX spec section 4).
        .preferredColorScheme(.dark)
    }

    // MARK: - The call

    private var callView: some View {
        ZStack {
            CallBackdrop(identity: tintIdentity)

            switch layout {
            case .portrait:
                portrait
            case .video:
                remoteVideo
            case .gallery:
                gallery
            }

            if layout != .gallery, local?.camOn == true, let track = state.localVideoTrack {
                selfView(track: track)
            }

            VStack(spacing: 0) {
                if layout == .portrait {
                    // A portrait keeps the quality dot and nothing else up
                    // there: the name and the state belong in the middle of the
                    // screen, with the avatar, not in a header bar.
                    HStack {
                        Spacer()
                        QualityIndicator(state: state)
                    }
                    .padding(.horizontal, 20)
                    .padding(.top, 12)
                } else {
                    videoOverlayHeader
                }
                Spacer(minLength: 0)
                if controlsVisible {
                    controlsBar
                        .padding(.bottom, layout == .portrait ? 56 : 24)
                        .transition(.opacity)
                }
            }

            if case .ended = displayPhase {
                CallEndedOverlay(durationMs: endedDurationMs, onDismiss: onExit).zIndex(30)
            }

            if state.participantListOpen {
                HStack {
                    Spacer()
                    ParticipantListDrawer(state: state)
                        .transition(.move(edge: .trailing))
                }
                .zIndex(10)
            }
        }
        .contentShape(Rectangle())
        .onTapGesture {
            if state.qualityPopoverOpen { state.setQualityPopoverOpen(false) }
            showControls()
        }
        .animation(.easeInOut(duration: 0.2), value: controlsVisible)
        .animation(.easeInOut(duration: 0.2), value: state.participantListOpen)
        .sheet(
            isPresented: Binding(
                get: { state.settingsOpen }, set: { state.setSettingsOpen($0) })
        ) {
            SettingsSheet(state: state)
        }
        .sheet(
            isPresented: Binding(get: { state.chatOpen }, set: { state.setChatOpen($0) })
        ) {
            ChatSheet(state: state)
        }
        // Section 5 — the safety number, only when asked for.
        .sheet(
            isPresented: Binding(
                get: { state.encryptionSheetOpen },
                set: { state.setEncryptionSheetOpen($0) })
        ) {
            EncryptionSheet(
                peerName: state.peerName,
                safetyNumber: state.e2ee.shortFingerprint,
                onDone: { state.setEncryptionSheetOpen(false) })
        }
    }

    /// A voice call: the avatar large and centred in the upper half, the name
    /// under it in the largest type on the screen, and one quiet line under that.
    private var portrait: some View {
        VStack(spacing: 24) {
            Spacer(minLength: 0)
            InitialsAvatar(
                name: state.peerName ?? "", identity: tintIdentity, size: 132,
                placeholderSymbol: "person.fill")
            CallStatusLine(
                name: displayName,
                phase: displayPhase,
                connectedAt: connectedAt,
                e2eeActive: state.e2ee.active,
                notice: state.mediaNotice,
                prominent: true,
                onEncryptionTap: { state.setEncryptionSheetOpen(true) })
            Spacer(minLength: 0)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 32)
    }

    /// A video call: the other person, full screen.
    @ViewBuilder
    private var remoteVideo: some View {
        if let track = soleRemoteVideoTrack {
            LocalVideoPreviewView(track: track, mirrored: false, contentMode: .scaleAspectFill)
                .ignoresSafeArea()
                .accessibilityHidden(true)
        }
    }

    private var gallery: some View {
        ScrollView {
            VStack(spacing: 12) {
                if let active = state.activeSpeaker {
                    ParticipantTile(participant: active, large: true, state: state)
                }
                LazyVGrid(
                    columns: [GridItem(.adaptive(minimum: 160), spacing: 10)], spacing: 10
                ) {
                    ForEach(state.galleryParticipants) { p in
                        ParticipantTile(participant: p, large: false, state: state)
                    }
                }
            }
            .padding(16)
            .padding(.bottom, 140)
        }
    }

    /// Over video, the name and the state float at the top on a soft shade so
    /// they stay readable — the same one line, smaller.
    private var videoOverlayHeader: some View {
        HStack(alignment: .top) {
            CallStatusLine(
                name: displayName,
                phase: displayPhase,
                connectedAt: connectedAt,
                e2eeActive: state.e2ee.active,
                notice: nil,
                prominent: false,
                onEncryptionTap: { state.setEncryptionSheetOpen(true) })
            Spacer(minLength: 12)
            QualityIndicator(state: state)
                .padding(.top, 6)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(
            LinearGradient(
                colors: [Color.black.opacity(0.55), .clear],
                startPoint: .top, endPoint: .bottom)
                .ignoresSafeArea(edges: .top))
    }

    /// The self-view: a small rounded rectangle about a quarter of the width,
    /// draggable, with the flip-camera button in its own corner — where the
    /// thing it changes is visible (`docs/ux/calls.md` section 4.1).
    private func selfView(track: RTCVideoTrack) -> some View {
        GeometryReader { geo in
            let width = geo.size.width * 0.26
            ZStack(alignment: .bottomTrailing) {
                LocalVideoPreviewView(
                    track: track,
                    mirrored: state.settings.mirrorLocal,
                    contentMode: .scaleAspectFill)
                if session?.canFlipCamera == true {
                    Button {
                        Task { await session?.flipCamera() }
                    } label: {
                        Image(systemName: "arrow.triangle.2.circlepath.camera")
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(.white)
                            .frame(width: 30, height: 30)
                            .background(Circle().fill(Color.black.opacity(0.45)))
                    }
                    .buttonStyle(.plain)
                    .padding(6)
                    .accessibilityLabel("Flip camera")
                }
            }
            .frame(width: width, height: width * 4 / 3)
            .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(Color.white.opacity(0.16), lineWidth: 1))
            .shadow(color: .black.opacity(0.35), radius: 10, y: 4)
            .offset(
                x: geo.size.width - width - 16 + selfViewOffset.width + selfViewDrag.width,
                y: 90 + selfViewOffset.height + selfViewDrag.height)
            .gesture(
                DragGesture()
                    .updating($selfViewDrag) { value, drag, _ in drag = value.translation }
                    .onEnded { value in
                        selfViewOffset.width += value.translation.width
                        selfViewOffset.height += value.translation.height
                    })
        }
        .accessibilityLabel("Your camera")
    }

    // MARK: - Controls

    private var controlsBar: some View {
        CallControlsBar {
            ForEach(controls) { control in
                if control == .end { CallControlsEndSpacer() }
                controlButton(control)
            }
        }
    }

    @ViewBuilder
    private func controlButton(_ control: CallControl) -> some View {
        let enabled = CallScreenPresentation.isEnabled(control, phase: displayPhase)
        switch control {
        case .speaker:
            CallControlButton(
                control: .speaker,
                systemImage: "speaker.wave.2.fill",
                on: state.speakerOn,
                enabled: enabled,
                action: {
                    state.setSpeakerOn(CallAudioRoute.setSpeakerOn(!state.speakerOn))
                    showControls()
                })
        case .microphone:
            let on = local?.micOn ?? true
            CallControlButton(
                control: .microphone,
                systemImage: on ? "mic.fill" : "mic.slash.fill",
                on: on,
                enabled: enabled,
                action: {
                    setMic(!on)
                    showControls()
                })
        case .camera:
            let on = local?.camOn ?? false
            CallControlButton(
                control: .camera,
                systemImage: on ? "video.fill" : "video.slash.fill",
                on: on,
                enabled: enabled,
                busy: state.videoPublishState == .starting,
                action: {
                    setCamera(!on)
                    showControls()
                })
        case .overflow:
            overflowButton(enabled: enabled)
        case .end:
            CallControlButton(
                control: .end,
                systemImage: "phone.down.fill",
                on: false,
                enabled: true,
                action: endCall)
        }
    }

    /// The one menu a call is allowed: a group call's participants, chat and
    /// settings. A 1x1 call never reaches this — its whole set is in the row.
    private func overflowButton(enabled: Bool) -> some View {
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
                    state.chatUnreadCount > 0 ? "Chat (\(state.chatUnreadCount) unread)" : "Chat",
                    systemImage: "bubble.left.and.bubble.right")
            }
            Button {
                state.setSettingsOpen(!state.settingsOpen)
            } label: {
                Label("Settings", systemImage: "gearshape")
            }
        } label: {
            VStack(spacing: 6) {
                CallControlFaceView(
                    systemImage: "ellipsis",
                    face: CallScreenPresentation.face(.overflow, on: false))
                Text(CallControl.overflow.label)
                    .font(.caption2)
                    .foregroundStyle(Color.white.opacity(0.75))
            }
            .opacity(enabled ? 1 : CallScreenPresentation.disabledOpacity)
        }
        .disabled(!enabled)
        .accessibilityLabel(CallStatusText.spokenControlLabel(.overflow, on: false))
    }

    private func endCall() {
        if let session {
            session.hangUp()
        } else {
            state.endCall()
        }
    }

    private func setMic(_ on: Bool) {
        if let session {
            session.setMicrophoneEnabled(on)
        } else {
            state.setMic(on)
        }
    }

    private func setCamera(_ on: Bool) {
        guard on, gatesCamera else {
            applyCamera(on)
            return
        }
        // Camera on: settle the permission first. A denial leaves the camera
        // off and explains why.
        Task { @MainActor in
            guard await CameraPermission.ensureGranted() else {
                permissionMessage = CameraPermission.deniedMessage
                return
            }
            applyCamera(true)
        }
    }

    /// Drive the camera choice onto whatever owns the call.
    private func applyCamera(_ on: Bool) {
        guard let session else {
            // Mock route, or an externally driven SFU call whose owner observes
            // `camOn` on the state and drives its session.
            state.setCamera(on)
            return
        }
        guard on else {
            session.setCameraEnabled(false)
            return
        }
        // Mid-call camera-on: `addLocalVideo` acquires + adds a video track +
        // renegotiates the first time, and re-enables an existing sender after.
        // On a capture failure revert the toggle and log the reason — the call
        // stays up audio-only (invariant #4).
        Task {
            do {
                try await session.addLocalVideo()
            } catch {
                state.setCamera(false)
                NSLog("CallScreen: addLocalVideo failed: %@", String(describing: error))
            }
        }
    }

    // MARK: - Auto-hide

    private func showControls() {
        controlsVisible = true
        restartAutoHide()
    }

    /// Section 4 — after five seconds of a connected video call the row fades
    /// out; a voice call's row never hides, because there is nothing underneath
    /// it to look at and hiding it only makes the user tap.
    private func restartAutoHide() {
        hideTask?.cancel()
        guard CallScreenPresentation.autoHides(layout: layout, phase: displayPhase) else {
            controlsVisible = true
            return
        }
        hideTask = Task { @MainActor in
            try? await Task.sleep(
                nanoseconds: UInt64(CallScreenPresentation.autoHideAfter * 1_000_000_000))
            guard !Task.isCancelled else { return }
            controlsVisible = false
        }
    }
}

// MARK: - SettingsSheet (group calls)

/// In-call settings — device pickers + processing toggles. Reachable from a
/// group call's overflow button; a 1x1 call has no menu at all.
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
                            set: { state.setSelectedAudioInput($0) }))
                    devicePicker(
                        title: "Camera",
                        devices: videoInputs,
                        selection: Binding(
                            get: { state.settings.selectedVideoInput },
                            set: { state.setSelectedVideoInput($0) }))
                    devicePicker(
                        title: "Speaker",
                        devices: audioOutputs,
                        selection: Binding(
                            get: { state.settings.selectedAudioOutput },
                            set: { state.setSelectedAudioOutput($0) }))
                }
                Section(header: Text("Audio processing")) {
                    Toggle(
                        "Noise suppression",
                        isOn: Binding(
                            get: { state.settings.noiseSuppression },
                            set: { state.setNoiseSuppression($0) }))
                    Toggle(
                        "Echo cancellation",
                        isOn: Binding(
                            get: { state.settings.echoCancellation },
                            set: { state.setEchoCancellation($0) }))
                    Toggle(
                        "Automatic gain control",
                        isOn: Binding(
                            get: { state.settings.autoGain },
                            set: { state.setAutoGain($0) }))
                }
                Section(header: Text("Video")) {
                    Toggle(
                        "Mirror local preview",
                        isOn: Binding(
                            get: { state.settings.mirrorLocal },
                            set: { state.setMirrorLocal($0) }))
                }
                Section(
                    header: Text("Network"),
                    footer: Text("Caps video at 360p — useful on metered mobile connections.")
                ) {
                    Toggle(
                        "Low-data mode",
                        isOn: Binding(
                            get: { state.settings.lowDataMode },
                            set: { state.setLowDataMode($0) }))
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

// MARK: - ChatSheet (group calls)

/// In-call chat. Text-only; system events ("Alice joined") render inline as
/// centred muted lines. Reachable from a group call's overflow button.
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
                            get: { state.composeText }, set: { state.setComposeText($0) })
                    )
                    .focused($composerFocused)
                    .textFieldStyle(.roundedBorder)
                    .submitLabel(.send)
                    .onSubmit(send)

                    Button(action: send) {
                        Text("Send")
                            .font(.system(size: 14, weight: .semibold))
                            .foregroundColor(
                                canSend ? Color.lakkoAccentForeground : Color.lakkoTextMuted)
                            .padding(.horizontal, 14)
                            .padding(.vertical, 8)
                            .background(
                                RoundedRectangle(cornerRadius: LakkoRadius.control)
                                    .fill(canSend ? Color.lakkoAccent : Color.lakkoSurfaceSunken))
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
