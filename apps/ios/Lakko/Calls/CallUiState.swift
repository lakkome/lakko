// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import SwiftUI
import WebRTC

/// In-call UI state (Phase 3c6-ios).
///
/// Mirror of `apps/web/src/state/call-state.ts` — same
/// Participant shape, same CallPhase variants, same actions.
/// SwiftUI's `@Published` + `ObservableObject` plays the same
/// role Solid's `createSignal` plays on web.
///
/// No WebRTC integration here — the live media path lands in
/// the platform's WebRTC pod (3a-pod-followup). Until then,
/// UI consumes mock data + the BandwidthController (#39) for
/// the connection-quality glyph.
@MainActor
public final class CallUiState: ObservableObject {

    // MARK: - Types

    /// Connection quality tier. Mirror of the Rust-side
    /// `lakko_calls::quality::QualityLevel`; collapses to three
    /// glyph colours.
    public enum QualityTier: String, Equatable {
        case excellent
        case good
        case fair
        case poor
        case bad

        /// Three-colour bucket. `green / yellow / red`.
        public var colour: Color {
            switch self {
            case .excellent: return Color(red: 0.06, green: 0.72, blue: 0.51) // #10B981
            case .good, .fair: return Color(red: 0.98, green: 0.75, blue: 0.14) // #FBBF24
            case .poor, .bad: return Color(red: 0.94, green: 0.27, blue: 0.27) // #EF4444
            }
        }
    }

    /// One participant in the call.
    public struct Participant: Equatable, Identifiable {
        public let id: String
        public let displayName: String
        public let isLocal: Bool
        public let micOn: Bool
        public let camOn: Bool
        public let handRaised: Bool
        public let quality: QualityTier
        public let speaking: Bool
        public let screenSharing: Bool
    }

    /// Lifecycle phase.
    public enum CallPhase: Equatable {
        case prejoin
        case connecting
        case connected
        case reconnecting(reason: String)
        case ended(endedAt: Date)
    }

    /// E2EE indicator state.
    public struct E2eeState: Equatable {
        public let active: Bool
        /// 12-hex-char short fingerprint (BLAKE3 over the
        /// CallKey seed). `nil` while `active == false`.
        public let shortFingerprint: String?

        public static let inactive = E2eeState(
            active: false, shortFingerprint: nil
        )
    }

    /// Connection-detail content for the quality-dot popover.
    public struct ConnectionDetail: Equatable {
        public let rttMs: Int?
        public let downBps: Int?
        public let upBps: Int?
        public enum ConnectionType: String, Equatable {
            case direct, relay, unknown
        }
        public let connectionType: ConnectionType

        public static let unknown = ConnectionDetail(
            rttMs: nil, downBps: nil, upBps: nil,
            connectionType: .unknown
        )
    }

    /// A capture / playback device the OS exposes (S-3-A-settings).
    /// Same shape as the web `CallDevice` and Android counterpart.
    public struct CallDevice: Equatable, Identifiable {
        public enum Kind: String, Equatable {
            case audioInput
            case videoInput
            case audioOutput
        }
        public let id: String
        public let label: String
        public let kind: Kind

        public init(id: String, label: String, kind: Kind) {
            self.id = id
            self.label = label
            self.kind = kind
        }
    }

    /// In-call audio / video settings (S-3-A-settings).
    /// Holds the device picks + processing toggles; the WebRTC
    /// pod sprint reads from here when configuring the local
    /// `RTCAudioSession` + capture chain.
    public struct CallSettings: Equatable {
        public var selectedAudioInput: String?
        public var selectedVideoInput: String?
        public var selectedAudioOutput: String?
        public var availableDevices: [CallDevice]
        public var noiseSuppression: Bool
        public var echoCancellation: Bool
        public var autoGain: Bool
        public var mirrorLocal: Bool
        public var lowDataMode: Bool

        public init(
            selectedAudioInput: String? = nil,
            selectedVideoInput: String? = nil,
            selectedAudioOutput: String? = nil,
            availableDevices: [CallDevice] = [],
            noiseSuppression: Bool = true,
            echoCancellation: Bool = true,
            autoGain: Bool = true,
            mirrorLocal: Bool = true,
            lowDataMode: Bool = false
        ) {
            self.selectedAudioInput = selectedAudioInput
            self.selectedVideoInput = selectedVideoInput
            self.selectedAudioOutput = selectedAudioOutput
            self.availableDevices = availableDevices
            self.noiseSuppression = noiseSuppression
            self.echoCancellation = echoCancellation
            self.autoGain = autoGain
            self.mirrorLocal = mirrorLocal
            self.lowDataMode = lowDataMode
        }

        public static let `default` = CallSettings()
    }

    /// A single chat message inside the call (S-3-A-chat).
    /// Same shape as the web `ChatMessage` and Android
    /// counterpart.
    public struct ChatMessage: Equatable, Identifiable {
        public enum Kind: String, Equatable {
            case text
            case system
        }
        public let id: String
        public let kind: Kind
        /// Author participant id; `nil` for system events.
        public let senderId: String?
        public let senderName: String
        public let text: String
        /// Wall-clock timestamp.
        public let ts: Date

        public init(
            id: String,
            kind: Kind,
            senderId: String?,
            senderName: String,
            text: String,
            ts: Date
        ) {
            self.id = id
            self.kind = kind
            self.senderId = senderId
            self.senderName = senderName
            self.text = text
            self.ts = ts
        }
    }

    // MARK: - Published state

    @Published public private(set) var phase: CallPhase = .prejoin
    @Published public private(set) var participants: [Participant] = []
    /// `nil` = follow active speaker.
    @Published public private(set) var pinnedId: String?
    @Published public private(set) var participantListOpen: Bool = false
    @Published public private(set) var e2ee: E2eeState = .inactive
    @Published public private(set) var connectionDetail: ConnectionDetail = .unknown
    @Published public private(set) var qualityPopoverOpen: Bool = false
    @Published public private(set) var hasMultipleCameras: Bool = false
    /// Gear-menu open / closed (S-3-A-settings).
    @Published public private(set) var settingsOpen: Bool = false
    /// In-call audio / video settings + enumerated devices
    /// (S-3-A-settings). Mock-populated until the WebRTC pod
    /// sprint wires real device enumeration.
    @Published public private(set) var settings: CallSettings = .default

    /// Chat panel open / closed (S-3-A-chat).
    @Published public private(set) var chatOpen: Bool = false
    /// Chat-message log — oldest first.
    @Published public private(set) var chatMessages: [ChatMessage] = []
    /// In-progress compose-box text (held in state so it
    /// survives panel close / reopen during a call).
    @Published public private(set) var composeText: String = ""
    /// Unread-message count since the user last opened the
    /// chat panel. System events do not count towards unread
    /// — they are background context.
    @Published public private(set) var chatUnreadCount: Int = 0

    /// G3-1-video-ui-screen — live local-camera track from the
    /// underlying `CallSession` (when one is attached via
    /// `bind(to:)`). `nil` while audio-only, while the prejoin
    /// flow is still up, or while the camera-pipeline init is
    /// in-flight. SwiftUI consumers (the self-tile in
    /// `ParticipantTile`, the prejoin preview in `PrejoinView`)
    /// render `LocalVideoPreviewView(track:)` when this is
    /// non-nil **and** the local participant's `camOn == true`.
    /// Untouched by `seedMockState()` — the mock-driven design
    /// path keeps the legacy "(video)" placeholder.
    @Published public private(set) var localVideoTrack: RTCVideoTrack?

    /// G3-1-remote-video — the remote participant's live video
    /// track (when one is attached via `bind(to:)`). `nil` for
    /// audio-only, before the remote adds a camera, or after
    /// teardown. The 1×1 direct-PeerConnection path has exactly
    /// one remote track; `CallScreen` renders it on the **sole
    /// remote participant's** tile (see `soleRemoteParticipantId`).
    /// Untouched by `seedMockState()` so the mock design path
    /// keeps the legacy placeholder; the N-way SFU path will
    /// replace this with a per-participant track map.
    @Published public private(set) var remoteVideoTrack: RTCVideoTrack?

    /// Monotone counter for client-side chat-message ids;
    /// the WebRTC pod replaces this with MLS application-
    /// message round-trip ids once 3a-pod-followup lands.
    private var chatIdCounter: Int = 0
    private func nextChatId() -> String {
        chatIdCounter += 1
        return "local-\(Int(Date().timeIntervalSince1970))-\(chatIdCounter)"
    }

    // MARK: - Derived state

    /// Active-speaker pick: pinned > speaking-remote > first
    /// remote > local. Same dispatch as the web `activeSpeaker`
    /// memo.
    public var activeSpeaker: Participant? {
        if let pin = pinnedId, let found = participants.first(where: { $0.id == pin }) {
            return found
        }
        if let speakingRemote = participants.first(where: { $0.speaking && !$0.isLocal }) {
            return speakingRemote
        }
        if let firstRemote = participants.first(where: { !$0.isLocal }) {
            return firstRemote
        }
        return participants.first
    }

    /// Gallery participants — everyone except the active
    /// speaker.
    public var galleryParticipants: [Participant] {
        guard let active = activeSpeaker else { return [] }
        return participants.filter { $0.id != active.id }
    }

    /// Quality tier of the local participant — drives the
    /// header-bar QualityIndicator.
    public var localQualityTier: QualityTier {
        participants.first(where: { $0.isLocal })?.quality ?? .good
    }

    /// `true` when the only participant in the call is the
    /// local one — surfaces the "Waiting for participant…"
    /// overlay.
    public var isAlone: Bool {
        !participants.contains(where: { !$0.isLocal })
    }

    // MARK: - Mutating actions

    public func setParticipants(_ list: [Participant]) {
        self.participants = list
    }

    public func participantJoined(_ p: Participant) {
        guard !participants.contains(where: { $0.id == p.id }) else { return }
        participants.append(p)
    }

    public func participantLeft(_ id: String) {
        participants.removeAll(where: { $0.id == id })
        if pinnedId == id { pinnedId = nil }
    }

    public func setQuality(_ id: String, _ tier: QualityTier) {
        patch(id: id) { p in
            Participant(
                id: p.id, displayName: p.displayName, isLocal: p.isLocal,
                micOn: p.micOn, camOn: p.camOn, handRaised: p.handRaised,
                quality: tier, speaking: p.speaking,
                screenSharing: p.screenSharing
            )
        }
    }

    public func setSpeaking(_ id: String?) {
        participants = participants.map { p in
            Participant(
                id: p.id, displayName: p.displayName, isLocal: p.isLocal,
                micOn: p.micOn, camOn: p.camOn, handRaised: p.handRaised,
                quality: p.quality, speaking: p.id == id,
                screenSharing: p.screenSharing
            )
        }
    }

    public func setMic(_ on: Bool) {
        guard let local = participants.first(where: { $0.isLocal }) else { return }
        patch(id: local.id) { p in
            Participant(
                id: p.id, displayName: p.displayName, isLocal: p.isLocal,
                micOn: on, camOn: p.camOn, handRaised: p.handRaised,
                quality: p.quality, speaking: p.speaking,
                screenSharing: p.screenSharing
            )
        }
    }

    public func setCamera(_ on: Bool) {
        guard let local = participants.first(where: { $0.isLocal }) else { return }
        patch(id: local.id) { p in
            Participant(
                id: p.id, displayName: p.displayName, isLocal: p.isLocal,
                micOn: p.micOn, camOn: on, handRaised: p.handRaised,
                quality: p.quality, speaking: p.speaking,
                screenSharing: p.screenSharing
            )
        }
    }

    public func setHandRaised(_ raised: Bool) {
        guard let local = participants.first(where: { $0.isLocal }) else { return }
        patch(id: local.id) { p in
            Participant(
                id: p.id, displayName: p.displayName, isLocal: p.isLocal,
                micOn: p.micOn, camOn: p.camOn, handRaised: raised,
                quality: p.quality, speaking: p.speaking,
                screenSharing: p.screenSharing
            )
        }
    }

    public func setScreenSharing(_ on: Bool) {
        guard let local = participants.first(where: { $0.isLocal }) else { return }
        patch(id: local.id) { p in
            Participant(
                id: p.id, displayName: p.displayName, isLocal: p.isLocal,
                micOn: p.micOn, camOn: p.camOn, handRaised: p.handRaised,
                quality: p.quality, speaking: p.speaking,
                screenSharing: on
            )
        }
    }

    public func pin(_ id: String?) {
        self.pinnedId = id
    }

    public func setParticipantListOpen(_ open: Bool) {
        self.participantListOpen = open
    }

    public func setPhase(_ p: CallPhase) {
        self.phase = p
    }

    public func join() {
        self.phase = .connecting
    }

    public func setE2ee(_ state: E2eeState) {
        self.e2ee = state
    }

    public func setConnectionDetail(_ d: ConnectionDetail) {
        self.connectionDetail = d
    }

    public func setQualityPopoverOpen(_ open: Bool) {
        self.qualityPopoverOpen = open
    }

    public func setHasMultipleCameras(_ many: Bool) {
        self.hasMultipleCameras = many
    }

    // MARK: - Settings (S-3-A-settings)

    /// Open / close the gear menu.
    public func setSettingsOpen(_ open: Bool) {
        self.settingsOpen = open
    }

    /// Replace the enumerated devices snapshot. Called by the
    /// WebRTC pod after `AVCaptureDevice.DiscoverySession`
    /// (cameras) and `AVAudioSession.availableInputs` (mics)
    /// have produced their lists.
    public func setAvailableDevices(_ devices: [CallDevice]) {
        self.settings.availableDevices = devices
    }

    /// Pick a microphone (`nil` → OS default).
    public func setSelectedAudioInput(_ id: String?) {
        self.settings.selectedAudioInput = id
    }

    /// Pick a camera (`nil` → OS default).
    public func setSelectedVideoInput(_ id: String?) {
        self.settings.selectedVideoInput = id
    }

    /// Pick a speaker / headset (`nil` → OS default). On iOS
    /// this is a hint — `AVAudioSession` overrides for BT and
    /// wired connections.
    public func setSelectedAudioOutput(_ id: String?) {
        self.settings.selectedAudioOutput = id
    }

    public func setNoiseSuppression(_ on: Bool) {
        self.settings.noiseSuppression = on
    }

    public func setEchoCancellation(_ on: Bool) {
        self.settings.echoCancellation = on
    }

    public func setAutoGain(_ on: Bool) {
        self.settings.autoGain = on
    }

    public func setMirrorLocal(_ on: Bool) {
        self.settings.mirrorLocal = on
    }

    public func setLowDataMode(_ on: Bool) {
        self.settings.lowDataMode = on
    }

    // MARK: - Chat (S-3-A-chat)

    /// Open / close the chat panel. Opening clears the unread
    /// badge.
    public func setChatOpen(_ open: Bool) {
        self.chatOpen = open
        if open { self.chatUnreadCount = 0 }
    }

    /// Update the in-progress compose-box content.
    public func setComposeText(_ text: String) {
        self.composeText = text
    }

    /// Send a chat message from the local participant.
    /// Returns the constructed message so the WebRTC pod can
    /// hand it to MLS application-message encryption. Local-
    /// echo: the message appears in `chatMessages`
    /// immediately. Returns `nil` for whitespace-only input.
    @discardableResult
    public func sendChatMessage(_ text: String) -> ChatMessage? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        let local = participants.first(where: { $0.isLocal })
        let senderId = local?.id ?? "local"
        let senderName = local?.displayName ?? "You"
        let msg = ChatMessage(
            id: nextChatId(),
            kind: .text,
            senderId: senderId,
            senderName: senderName,
            text: trimmed,
            ts: Date()
        )
        self.chatMessages.append(msg)
        self.composeText = ""
        return msg
    }

    /// Receive a chat message from a remote participant
    /// (decrypted MLS application message). If the chat panel
    /// is closed, the unread badge increments.
    public func receiveChatMessage(_ msg: ChatMessage) {
        self.chatMessages.append(msg)
        if !chatOpen, msg.kind == .text {
            self.chatUnreadCount += 1
        }
    }

    /// Append a system event ("Alice joined the call"). Does
    /// NOT bump the unread counter — system events are
    /// background context.
    public func addSystemEvent(_ text: String) {
        let msg = ChatMessage(
            id: nextChatId(),
            kind: .system,
            senderId: nil,
            senderName: "",
            text: text,
            ts: Date()
        )
        self.chatMessages.append(msg)
    }

    public func clearChatUnread() {
        self.chatUnreadCount = 0
    }

    // MARK: - Live video track (G3-1-video-ui-screen)

    /// Push a fresh local video track into the state. Callers
    /// are typically the `bind(to:)` Combine subscription; an
    /// explicit setter is exposed so testbeds + previews can
    /// drive the UI without standing up a real `CallSession`.
    public func setLocalVideoTrack(_ track: RTCVideoTrack?) {
        self.localVideoTrack = track
    }

    /// G3-1-remote-video — push the remote participant's video
    /// track. Same `bind(to:)`-driven / testbed-explicit dual
    /// use as [`setLocalVideoTrack`].
    public func setRemoteVideoTrack(_ track: RTCVideoTrack?) {
        self.remoteVideoTrack = track
    }

    /// The id of the only remote participant, when exactly one
    /// exists (the 1×1 case). `nil` when alone or in an N-way
    /// call. `CallScreen` gates remote-video rendering on
    /// `participant.id == soleRemoteParticipantId` so the
    /// single `remoteVideoTrack` handle is never painted onto
    /// the wrong tile in a group call (where per-participant
    /// track mapping is the deferred follow-up).
    public var soleRemoteParticipantId: String? {
        let remotes = participants.filter { !$0.isLocal }
        return remotes.count == 1 ? remotes.first?.id : nil
    }

    public func endCall() {
        self.phase = .ended(endedAt: Date())
    }

    public func reset() {
        self.phase = .prejoin
        self.participants = []
        self.pinnedId = nil
        self.participantListOpen = false
        self.e2ee = .inactive
        self.connectionDetail = .unknown
        self.qualityPopoverOpen = false
        self.settingsOpen = false
        // Preferences (toggles + selected ids) survive a
        // reset — only the enumerated-devices snapshot is
        // cleared.
        self.settings.availableDevices = []
        // Chat is per-call — wipe.
        self.chatOpen = false
        self.chatMessages = []
        self.composeText = ""
        self.chatUnreadCount = 0
        // Live media — drop both tracks so the next call
        // re-binds against a fresh CallSession.
        self.localVideoTrack = nil
        self.remoteVideoTrack = nil
    }

    /// Seed mock state — same shape as the web `seedMockParticipants`.
    /// Replaced by the WebRTC pod once 3a-pod-followup lands.
    public func seedMockState() {
        setE2ee(E2eeState(active: true, shortFingerprint: "3F7A B2C1 9E0D"))
        setConnectionDetail(ConnectionDetail(
            rttMs: 45, downBps: 720_000, upBps: 380_000,
            connectionType: .direct
        ))
        setHasMultipleCameras(false)
        setParticipants([
            Participant(
                id: "local", displayName: "You", isLocal: true,
                micOn: true, camOn: true, handRaised: false,
                quality: .excellent, speaking: false, screenSharing: false
            ),
            Participant(
                id: "leaf-1", displayName: "Alice", isLocal: false,
                micOn: true, camOn: true, handRaised: false,
                quality: .good, speaking: true, screenSharing: false
            ),
            Participant(
                id: "leaf-2", displayName: "Bob", isLocal: false,
                micOn: false, camOn: true, handRaised: false,
                quality: .fair, speaking: false, screenSharing: false
            ),
            Participant(
                id: "leaf-3", displayName: "Carl", isLocal: false,
                micOn: true, camOn: false, handRaised: true,
                quality: .poor, speaking: false, screenSharing: false
            ),
        ])
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 400_000_000)
            self.setPhase(.connected)
        }
    }

    // MARK: - Private

    private func patch(id: String, _ patch: (Participant) -> Participant) {
        participants = participants.map { p in
            p.id == id ? patch(p) : p
        }
    }
}

// MARK: - CallSession bridge (G3-1-video-ui-screen)

extension CallUiState {

    /// Attach a live `CallSession` so the UI mirrors its
    /// `@Published` outputs. Bridges the local **and** remote
    /// video tracks (G3-1-video-ui-screen + G3-1-remote-video);
    /// the audio / participant / e2ee / connection-detail
    /// fields stay on the mock path until the
    /// full-session-integration sub-sprint lands (the screen
    /// today reads from `seedMockState()`).
    ///
    /// The returned array of `AnyCancellable` owns the Combine
    /// subscriptions — store it in the SwiftUI host (typically
    /// a `@State` on the screen that constructs the session)
    /// and drop it when the call ends. Dropping the
    /// cancellables does NOT clear the existing tracks from
    /// state — call `reset()` for that, or wait for the
    /// session itself to publish `nil` on teardown
    /// (`CallSession.teardownMediaPipeline()` does this).
    ///
    /// Safe to call from any actor — the Combine subscriptions
    /// receive on the main queue and re-enter the
    /// `@MainActor`-isolated setters.
    public func bind(to session: CallSession) -> [AnyCancellable] {
        let local = session.$localVideoTrack
            .receive(on: DispatchQueue.main)
            .sink { [weak self] track in
                MainActor.assumeIsolated {
                    self?.setLocalVideoTrack(track)
                }
            }
        let remote = session.$remoteVideoTrack
            .receive(on: DispatchQueue.main)
            .sink { [weak self] track in
                MainActor.assumeIsolated {
                    self?.setRemoteVideoTrack(track)
                }
            }
        return [local, remote]
    }

    /// G3-1-session-bind — drive the **full** mock-free UI from
    /// a live 1×1 `CallSession`. Maps `session.state` →
    /// [`CallPhase`], synthesises the 2-person roster (local
    /// always; the single remote appears once the call is
    /// `connected` or a remote track has arrived), and bridges
    /// both video tracks — every relevant publisher triggers a
    /// full rebuild from the session's current truth, so there
    /// are no partial-update races.
    ///
    /// Scope (1×1 direct-PeerConnection): the roster is exactly
    /// two participants; `camOn` is driven by track presence so
    /// a tile flips from avatar to live video the moment a track
    /// lands. **Not** mapped here (each needs a dedicated
    /// `CallSession` signal, deferred): the e2ee fingerprint
    /// pill, connection-quality detail, and the mic / hand /
    /// screen-share controls writing *back* into the session.
    /// The prejoin Join → `dial` / `acceptCall` action and the
    /// navigation that presents this screen with a session are
    /// separate follow-ups — `CallScreen` calls this from
    /// `onAppear` when a session is injected and falls back to
    /// `seedMockState()` otherwise.
    ///
    /// Returns the owning `[AnyCancellable]`; store it on the
    /// SwiftUI host for the call's lifetime.
    public func driveFrom(session: CallSession) -> [AnyCancellable] {
        let rebuild: () -> Void = { [weak self, weak session] in
            guard let self, let session else { return }
            self.setPhase(Self.mapSessionPhase(session.state))
            let connected = session.state == .connected
            let remoteTrack = session.remoteVideoTrack
            // G3-1-conn-quality — local participant's quality dot
            // is the path quality bucketed from the live RTT.
            let tier = Self.qualityTier(forRttMs: session.rttMs)
            var roster: [Participant] = [
                Participant(
                    id: "local", displayName: "You", isLocal: true,
                    micOn: session.micEnabled, camOn: session.cameraEnabled,
                    handRaised: false, quality: tier, speaking: false,
                    screenSharing: false
                ),
            ]
            if connected || remoteTrack != nil {
                roster.append(
                    Participant(
                        id: "remote", displayName: "", isLocal: false,
                        micOn: true, camOn: remoteTrack != nil,
                        handRaised: false, quality: .good, speaking: false,
                        screenSharing: false
                    )
                )
            }
            self.setParticipants(roster)
            self.setLocalVideoTrack(session.localVideoTrack)
            self.setRemoteVideoTrack(remoteTrack)
            // G3-1-e2ee-active — the pill reflects real SFrame
            // keying. No comparable fingerprint yet (canonical
            // cross-platform hash is a deferred follow-up), so
            // `shortFingerprint` stays nil.
            self.setE2ee(E2eeState(active: session.e2eeActive, shortFingerprint: nil))
            // G3-1-conn-quality — RTT + connection type from the
            // live getStats sample; iter2 adds the down/up bitrate
            // derived from the candidate-pair byte delta.
            self.setConnectionDetail(ConnectionDetail(
                rttMs: session.rttMs,
                downBps: session.downBps,
                upBps: session.upBps,
                connectionType: session.connectionRelayed
                    ? .relay
                    : (session.rttMs != nil ? .direct : .unknown)
            ))
        }
        let onMain: (@escaping () -> Void) -> Void = { work in
            MainActor.assumeIsolated { work() }
        }
        let state = session.$state
            .receive(on: DispatchQueue.main)
            .sink { _ in onMain(rebuild) }
        let localTrack = session.$localVideoTrack
            .receive(on: DispatchQueue.main)
            .sink { _ in onMain(rebuild) }
        let remoteTrack = session.$remoteVideoTrack
            .receive(on: DispatchQueue.main)
            .sink { _ in onMain(rebuild) }
        let mic = session.$micEnabled
            .receive(on: DispatchQueue.main)
            .sink { _ in onMain(rebuild) }
        let camera = session.$cameraEnabled
            .receive(on: DispatchQueue.main)
            .sink { _ in onMain(rebuild) }
        let e2ee = session.$e2eeActive
            .receive(on: DispatchQueue.main)
            .sink { _ in onMain(rebuild) }
        let rtt = session.$rttMs
            .receive(on: DispatchQueue.main)
            .sink { _ in onMain(rebuild) }
        let relay = session.$connectionRelayed
            .receive(on: DispatchQueue.main)
            .sink { _ in onMain(rebuild) }
        let down = session.$downBps
            .receive(on: DispatchQueue.main)
            .sink { _ in onMain(rebuild) }
        let up = session.$upBps
            .receive(on: DispatchQueue.main)
            .sink { _ in onMain(rebuild) }
        // Seed once synchronously so the first frame is correct
        // before any publisher fires.
        rebuild()
        return [state, localTrack, remoteTrack, mic, camera, e2ee, rtt, relay, down, up]
    }

    /// G3-1-conn-quality — bucket a measured round-trip time
    /// into the three-colour quality glyph. These are **UX
    /// display thresholds** (not measured SLOs): green for a
    /// snappy path, yellow as it degrades, red when calls start
    /// to feel laggy. `nil` RTT (pre-measurement) shows the
    /// neutral `good` rather than alarming the user.
    private static func qualityTier(forRttMs rtt: Int?) -> QualityTier {
        guard let rtt else { return .good }
        switch rtt {
        case ..<100: return .excellent
        case ..<200: return .good
        case ..<350: return .fair
        case ..<600: return .poor
        default: return .bad
        }
    }

    /// Map the `CallSession` lifecycle onto the UI phase. The
    /// pre-connect states collapse to `.connecting` (the UI has
    /// no separate "ringing"); `ending` holds at `.connected`
    /// until the terminal `done` flips to `.ended`.
    private static func mapSessionPhase(_ s: CallSession.WebCallState) -> CallPhase {
        switch s {
        case .idle: return .prejoin
        case .outgoing, .ringing, .connecting: return .connecting
        case .connected, .ending: return .connected
        case .done: return .ended(endedAt: Date())
        }
    }
}
