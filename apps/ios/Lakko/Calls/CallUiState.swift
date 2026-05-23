// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import SwiftUI

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
