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
            case .excellent: return .lakkoSuccess
            case .good, .fair: return .lakkoWarning
            case .poor, .bad: return .lakkoDanger
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

    /// Lifecycle phase (`docs/ux/calls.md` section 4).
    public enum CallPhase: Equatable {
        /// The instant between a session existing and its offer being on the
        /// wire. Nothing is drawn for it: section 3 gives neither kind of call
        /// a preparation screen, and an outgoing call is dialled in the same
        /// breath as it is built, so this maps to `Calling...` on screen. It
        /// was `cameraCheck` until 2026-09-21, when the check it named was
        /// removed.
        case preparing
        /// Ringing out: the offer is on the wire, nobody has answered.
        case calling
        /// Answered; the transport is coming up.
        case connecting
        case connected
        case reconnecting(reason: String)
        case ended(endedAt: Date)
    }

    /// B2a — mid-call camera-on lifecycle, mirrored from whichever
    /// session drives the screen (the 1×1 `CallSession` via
    /// `driveFrom`, or the SFU `SfuCallSession` via `SfuCallUi`). One
    /// surface the `CallScreen` reads for the camera-button spinner +
    /// the self-tile "Starting camera…" hint. `.starting` is the only
    /// value the UI reacts to; the rest are terminal.
    public enum VideoPublishState: String, Equatable {
        case off, starting, live, failed
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

    // MARK: - Published state

    @Published public private(set) var phase: CallPhase = .preparing
    @Published public private(set) var participants: [Participant] = []
    /// The pinned person, or `nil`. A pin always means the Focus layout.
    @Published public private(set) var pinnedId: String?
    /// The group layout the person chose for this call, or `nil` for the
    /// default (`docs/ux/group-call-grid.md` section 3). Lasts for the call.
    @Published private(set) var layoutChoice: GroupLayoutKind?
    /// Who holds the floor and when each person last did, worked out on this
    /// device from the audio it plays (section 7.3).
    @Published private(set) var speech: StageSpeech = .none
    @Published public private(set) var participantListOpen: Bool = false
    @Published public private(set) var e2ee: E2eeState = .inactive
    /// `docs/ux/calls.md` section 4 — the other person's name for the status
    /// line, and for the encryption sheet's one sentence. `nil` in a group
    /// call (there is no single other person) and while nobody is known.
    @Published public private(set) var peerName: String?
    /// `docs/ux/calls.md` sections 4 and 5 — the encryption sheet is open.
    @Published public private(set) var encryptionSheetOpen: Bool = false
    /// `docs/ux/calls.md` section 4.1 — the sound is on the loudspeaker. Held
    /// here so the button's face is decided by one value the tests can read;
    /// ``CallAudioRoute`` is what actually moves the audio.
    @Published public private(set) var speakerOn: Bool = false
    /// One plain line about this call's media, or `nil`: section 3.2's "no
    /// camera on this device" downgrade, and what the call did on the
    /// person's behalf (a camera that could not start). It sits under the
    /// status line; ``showNotice(_:)`` takes it away again after a few
    /// seconds.
    @Published public private(set) var mediaNotice: UiText?
    private var noticeTask: Task<Void, Never>?
    /// Participants (by roster id) at least one of whose incoming streams could
    /// not be SFrame-decrypted, because arming that receiver's decryptor
    /// failed. Their media is already discarded fail-closed by libwebrtc
    /// (`sframeRequireFrameEncryption`), so this changes nothing about
    /// security — it exists so the screen can say "can't decrypt" instead of
    /// showing a blank tile that reads as a bad network (invariant #4 is about
    /// never *looking* like a degraded call when the media is refused).
    ///
    /// Membership is sticky for the life of the call: a receiver that failed to
    /// arm is never re-armed, so the condition does not clear on its own and
    /// silently dropping the badge would be the same lie in slow motion.
    @Published public private(set) var undecryptable: Set<String> = []
    @Published public private(set) var connectionDetail: ConnectionDetail = .unknown
    @Published public private(set) var qualityPopoverOpen: Bool = false
    @Published public private(set) var hasMultipleCameras: Bool = false
    /// In-call audio / video settings + enumerated devices
    /// (S-3-A-settings). Mock-populated until the WebRTC pod
    /// sprint wires real device enumeration.
    @Published public private(set) var settings: CallSettings = .default

    /// G3-1-video-ui-screen — live local-camera track from the
    /// underlying `CallSession` (when one is attached via
    /// `bind(to:)`). `nil` while audio-only, or while the
    /// camera-pipeline init is in-flight. SwiftUI consumers (the
    /// self-view corner of a video call, the tile of a group one)
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
    /// keeps the legacy placeholder; the N-way SFU path uses the
    /// per-participant `remoteVideoTracks` map below instead.
    @Published public private(set) var remoteVideoTrack: RTCVideoTrack?

    /// `true` once the remote video track has actually rendered a frame. The
    /// call screen keys its layout on this rather than on the track handle,
    /// which arrives with the SDP and says nothing about whether a picture
    /// follows (``RemoteVideoFrameProbe``).
    @Published public private(set) var remoteVideoFlowing: Bool = false

    /// N-way SFU remote video — per-peer video tracks keyed on the
    /// roster participant id (`"peer-<streamId>"`, where the SFU pins
    /// the downstream MSID stream-id to the source peer id). Populated
    /// by `SfuCallUi` from the session's per-stream
    /// `onRemoteVideoTrack(track, streamId)` callback so every remote
    /// tile in a group call renders its own participant's stream, not
    /// just the sole-remote 1×1 case. `CallScreen` prefers this per-tile
    /// entry and falls back to the single `remoteVideoTrack` handle only
    /// for the 1×1 P2P `CallSession` path (which has no MSID). Cleared
    /// wholesale on `reset()`, and per-entry on peer-left.
    @Published public private(set) var remoteVideoTracks: [String: RTCVideoTrack] = [:]

    /// B2a — mid-call camera-on lifecycle, driven by the bound session
    /// (1×1 `CallSession.videoPublishState` via `driveFrom`; SFU via
    /// `SfuCallUi`). `CallScreen` reads this for the camera-button
    /// spinner (+ disable) and the local self-tile "Starting camera…"
    /// hint while `.starting`. Untouched by `seedMockState()`.
    @Published public private(set) var videoPublishState: VideoPublishState = .off

    // MARK: - Derived state

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
        // If the pinned person leaves, the pin is cleared and the layout
        // returns to what it was (`docs/ux/group-call-grid.md` section 6.2).
        if pinnedId == id { pinnedId = nil }
        // A peer that left takes its undecryptable streams with it. Keeping the
        // id would make a later rejoin under the same id start out accused.
        undecryptable.remove(id)
    }

    /// Record that a participant's incoming media cannot be decrypted (an
    /// SFrame decryptor that failed to arm). Idempotent — one participant can
    /// have several receivers fail.
    public func markUndecryptable(_ id: String) {
        undecryptable.insert(id)
    }

    /// The one plain line for a screen with no tiles to badge, built from the
    /// display names of the participants currently marked. `nil` when nothing
    /// is marked.
    public var undecryptableNotice: String? {
        let names =
            participants
            .filter { undecryptable.contains($0.id) }
            .map(\.displayName)
        return UndecryptableMedia.notice(names: names)
    }

    /// ADR-0021 / ADR-0048 — a participant's name resolved AFTER the roster
    /// event that placed them: a profile that arrived mid-call (a newcomer's
    /// own is answered within about thirty seconds of joining). An unknown id
    /// or an unchanged name is a no-op, so re-applying every name on a
    /// profile change publishes nothing for a roster that already agrees.
    public func setParticipantName(_ id: String, _ name: String) {
        guard participants.contains(where: { $0.id == id && $0.displayName != name }) else { return }
        patch(id: id) { p in
            Participant(
                id: p.id, displayName: name, isLocal: p.isLocal,
                micOn: p.micOn, camOn: p.camOn, handRaised: p.handRaised,
                quality: p.quality, speaking: p.speaking,
                screenSharing: p.screenSharing
            )
        }
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

    /// What the sound levels say (``GroupSpeakingMonitor``): whose ring is on,
    /// who holds the floor, and when each person last did. Several people can
    /// be speaking at once; only one holds the floor.
    ///
    /// Writes the roster only when a ring actually changed, and the speech
    /// only when the stage could read it differently: this runs several times
    /// a second, and a new array on every reading would redraw every tile.
    func setSpeech(speakingIds: Set<String>, speech next: StageSpeech) {
        if participants.contains(where: { $0.speaking != speakingIds.contains($0.id) }) {
            participants = participants.map { p in
                let speaking = speakingIds.contains(p.id)
                guard p.speaking != speaking else { return p }
                return Participant(
                    id: p.id, displayName: p.displayName, isLocal: p.isLocal,
                    micOn: p.micOn, camOn: p.camOn, handRaised: p.handRaised,
                    quality: p.quality, speaking: speaking,
                    screenSharing: p.screenSharing
                )
            }
        }
        // The floor's own timestamp moves on every reading; nothing reads it
        // while the same person holds the floor, so only a change of holder
        // (or of who has ever spoken) is worth a write.
        if speech.activeSpeakerId != next.activeSpeakerId
            || Set(speech.lastSpokeAtMs.keys) != Set(next.lastSpokeAtMs.keys) {
            speech = next
        }
    }

    /// The person's own choice of group layout for this call.
    func setLayoutChoice(_ choice: GroupLayoutKind?) {
        if layoutChoice != choice { layoutChoice = choice }
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

    /// N-way SFU remote video — flip a specific participant's `camOn`
    /// by roster id (unlike `setCamera`, which only touches the LOCAL
    /// participant). `SfuCallUi` calls this when a remote video track
    /// arrives / departs so the tile's video surface — gated on
    /// `participant.camOn` in `CallScreen` — actually paints (a remote
    /// participant is created `camOn: false`, so without this the
    /// per-peer track map populates but the tile never renders).
    /// A no-op if `id` is not in the roster (e.g. the track raced the
    /// join). Mirrors the web `setParticipantCamera`.
    public func setParticipantCamera(_ id: String, _ on: Bool) {
        patch(id: id) { p in
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

    /// `docs/ux/calls.md` section 4 — name the other person on the call.
    public func setPeerName(_ name: String?) {
        guard self.peerName != name else { return }
        self.peerName = name
    }

    /// `docs/ux/calls.md` section 5 — open / close the encryption sheet. It
    /// can only open while encryption is armed: the chip that opens it is not
    /// there otherwise, and a sheet with no code to show would say nothing.
    public func setEncryptionSheetOpen(_ open: Bool) {
        self.encryptionSheetOpen = open && e2ee.active
    }

    /// `docs/ux/calls.md` section 4.1 — record where the sound is going. The
    /// caller is ``CallAudioRoute``, which only records what the platform
    /// accepted, so the button never claims a route the device refused.
    public func setSpeakerOn(_ on: Bool) {
        self.speakerOn = on
    }

    /// Set (or clear) the one line under the status line.
    public func setMediaNotice(_ notice: UiText?) {
        noticeTask?.cancel()
        noticeTask = nil
        self.mediaNotice = notice
    }

    /// How long ``showNotice(_:)`` keeps its line on the screen.
    public static let noticeSeconds: UInt64 = 8

    /// Show one line under the status line for a few seconds, replacing any
    /// line already up: what the call did on the person's behalf (the camera
    /// could not start, camera access is off). These used to be written into
    /// the call's chat, which nobody had open at the moment they mattered.
    public func showNotice(_ notice: UiText) {
        setMediaNotice(notice)
        noticeTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: Self.noticeSeconds * 1_000_000_000)
            guard !Task.isCancelled else { return }
            self?.mediaNotice = nil
        }
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
    /// use as [`setLocalVideoTrack`]. This single-handle setter is
    /// for the 1×1 P2P `CallSession` path only; the N-way SFU path
    /// uses the keyed setter below.
    public func setRemoteVideoTrack(_ track: RTCVideoTrack?) {
        self.remoteVideoTrack = track
        if track == nil { self.remoteVideoFlowing = false }
    }

    /// Record whether the remote track is producing a picture. Driven by the
    /// bound session's own frame probe; a dropped track resets it through
    /// ``setRemoteVideoTrack(_:)``.
    public func setRemoteVideoFlowing(_ flowing: Bool) {
        self.remoteVideoFlowing = flowing
    }

    /// N-way SFU remote video — set (or, with `nil`, remove) a remote
    /// peer's video track keyed on its roster participant id. Called by
    /// `SfuCallUi` from the session's per-stream `onRemoteVideoTrack`
    /// callback (id = `"peer-<streamId>"`) and on peer-left cleanup.
    /// A `nil` track removes the entry so a departed / camera-off tile
    /// stops rendering a stale last frame.
    public func setRemoteVideoTrack(_ track: RTCVideoTrack?, forParticipant id: String) {
        if let track {
            self.remoteVideoTracks[id] = track
        } else {
            self.remoteVideoTracks.removeValue(forKey: id)
        }
    }

    /// B2a — push the mid-call camera-on lifecycle. Driven by the
    /// bound session (`driveFrom` for 1×1, `SfuCallUi` for the SFU
    /// path); exposed as a setter so both callers converge on the one
    /// UI surface `CallScreen` reads.
    public func setVideoPublishState(_ s: VideoPublishState) {
        self.videoPublishState = s
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
        self.phase = .preparing
        self.participants = []
        self.pinnedId = nil
        self.layoutChoice = nil
        self.speech = .none
        self.participantListOpen = false
        self.e2ee = .inactive
        self.peerName = nil
        self.encryptionSheetOpen = false
        self.speakerOn = false
        setMediaNotice(nil)
        self.undecryptable = []
        self.connectionDetail = .unknown
        self.qualityPopoverOpen = false
        // Preferences (toggles + selected ids) survive a
        // reset — only the enumerated-devices snapshot is
        // cleared.
        self.settings.availableDevices = []
        // Live media — drop both tracks so the next call
        // re-binds against a fresh CallSession.
        self.localVideoTrack = nil
        self.remoteVideoTrack = nil
        self.remoteVideoFlowing = false
        // N-way SFU per-peer video tracks — drop them all.
        self.remoteVideoTracks = [:]
        // B2a — no camera-on in flight for a fresh call.
        self.videoPublishState = .off
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
    /// Placing the call is ``CallHost``'s job, not this one's —
    /// `CallScreen` calls this from `onAppear` when a session is
    /// injected and falls back to `seedMockState()` otherwise.
    ///
    /// Returns the owning `[AnyCancellable]`; store it on the
    /// SwiftUI host for the call's lifetime.
    /// - Parameter peerName: the other person's name, resolved by the caller
    ///   through ``CallPeerName`` (the call host owns the directory + the
    ///   conversation title; this state object owns neither). It names the
    ///   remote tile and the status line. `nil` leaves the tile on the neutral
    ///   placeholder — never the word "Unknown", which is what the roster
    ///   showed while nothing passed a name in at all.
    public func driveFrom(session: CallSession, peerName: String? = nil) -> [AnyCancellable] {
        setPeerName(peerName)
        let rebuild: () -> Void = { [weak self, weak session] in
            guard let self, let session else { return }
            self.setPhase(Self.mapSessionPhase(session.state))
            let connected = session.state == .connected
            let remoteTrack = session.remoteVideoTrack
            // A track handle is not a picture — `camOn` must mean the tile has
            // something to draw, or it paints a black rectangle over an avatar.
            let remotePicture = CallScreenPresentation.hasRemotePicture(
                trackPresent: remoteTrack != nil, framesArrived: session.remoteVideoFlowing)
            // G3-1-conn-quality — local participant's quality dot
            // is the path quality bucketed from the live RTT.
            let tier = Self.qualityTier(forRttMs: session.rttMs)
            var roster: [Participant] = [
                Participant(
                    id: "local", displayName: "", isLocal: true,
                    micOn: session.micEnabled, camOn: session.cameraEnabled,
                    handRaised: false, quality: tier, speaking: false,
                    screenSharing: false
                ),
            ]
            if connected || remoteTrack != nil {
                roster.append(
                    Participant(
                        id: "remote", displayName: self.peerName ?? "", isLocal: false,
                        micOn: true, camOn: remotePicture,
                        handRaised: false, quality: .good, speaking: false,
                        screenSharing: false
                    )
                )
            }
            self.setParticipants(roster)
            // A receiver whose decryptor never armed decodes nothing. Mark the
            // remote tile so the screen can say that, instead of leaving a
            // blank portrait that reads as "they have no signal". Marking is
            // one-way: the flag on the session is sticky, and so is this.
            if session.remoteMediaUndecryptable { self.markUndecryptable("remote") }
            self.setLocalVideoTrack(session.localVideoTrack)
            self.setRemoteVideoTrack(remoteTrack)
            self.setRemoteVideoFlowing(remotePicture)
            // B2a — mirror the 1×1 mid-call camera-on lifecycle so the
            // camera button spins (+ disables) and the self-tile shows
            // the "Starting camera…" hint while it is `.starting`.
            self.setVideoPublishState(
                Self.mapVideoPublishState(session.videoPublishState)
            )
            // G3-1-e2ee-active — the pill reflects real SFrame
            // keying; the safety number (canonical hash of the same
            // seed, set alongside `e2eeActive` in `setCallKey`) is the
            // out-of-band MITM-verification code.
            self.setE2ee(
                E2eeState(active: session.e2eeActive, shortFingerprint: session.safetyNumber)
            )
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
        let videoPublish = session.$videoPublishState
            .receive(on: DispatchQueue.main)
            .sink { _ in onMain(rebuild) }
        // The first remote frame is what moves the screen to the video layout,
        // so it has to trigger a rebuild like every other input.
        let remoteFlowing = session.$remoteVideoFlowing
            .receive(on: DispatchQueue.main)
            .sink { _ in onMain(rebuild) }
        // A failed decryptor arm changes what the screen must say, so it is an
        // input to the rebuild like any other.
        let undecryptable = session.$remoteMediaUndecryptable
            .receive(on: DispatchQueue.main)
            .sink { _ in onMain(rebuild) }
        // Seed once synchronously so the first frame is correct
        // before any publisher fires.
        rebuild()
        return [
            state, localTrack, remoteTrack, mic, camera, e2ee,
            rtt, relay, down, up, videoPublish, remoteFlowing, undecryptable,
        ]
    }

    /// B2a — map the 1×1 `CallSession` camera-on lifecycle onto the UI
    /// enum. Identity mapping (the two enums share the same cases); kept
    /// as an explicit switch so a future divergence is caught by the
    /// compiler rather than silently mis-mapped.
    private static func mapVideoPublishState(
        _ s: CallSession.VideoPublishState
    ) -> VideoPublishState {
        switch s {
        case .off: return .off
        case .starting: return .starting
        case .live: return .live
        case .failed: return .failed
        }
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

    /// Map the `CallSession` lifecycle onto the UI phase
    /// (`docs/ux/calls.md` section 4's state table).
    ///
    /// `.outgoing` is its own line on that table — the call is on the wire and
    /// nobody has answered, which the screen says as `Calling...` — and it used
    /// to collapse into `.connecting`, so a ringing call claimed to be
    /// connecting to someone who had not picked up. `.idle` is the camera check
    /// a video call opens with; a voice call dials before the screen is even
    /// presented, so it is never seen there. `ending` holds at `.connected`
    /// until the terminal `done` flips to `.ended`.
    private static func mapSessionPhase(_ s: CallSession.WebCallState) -> CallPhase {
        switch s {
        case .idle: return .preparing
        case .outgoing, .ringing: return .calling
        case .connecting: return .connecting
        case .connected, .ending: return .connected
        case .done: return .ended(endedAt: Date())
        }
    }
}
