// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// 1×1 P2P call session on iOS (Phase 3a9 scaffolding).
///
/// Mirror of `apps/web/src/calls/call-session.ts` translated
/// into a `@MainActor` `ObservableObject`. Tracks the call
/// state machine + composes outbound signaling via the
/// UniFFI `composeCall*Signaling` free functions.
///
/// ## Phase 3a9 scope
///
/// The session ships **signaling-only** today — the real
/// `RTCPeerConnection` integration via the `WebRTC.framework`
/// pod / SwiftPM artefact lands in 3a9-followup. The UI
/// surface (Call button + state overlay) is fully wired
/// against this class today; once the WebRTC stack
/// integrates, only [`dial`] / [`acceptCall`] need to grow
/// the live media-stream branches (build SDP via
/// `RTCPeerConnection`, push tracks).
///
/// Two-device E2E demo today: an iOS device + a web client
/// see the signaling transit (`outgoing` → `connecting` →
/// `connected` state ticks); media doesn't flow yet on the
/// iOS side until the WebRTC pod lands.
@MainActor
public final class CallSession: ObservableObject {

    /// Lifecycle phases (mirror of `lakko_calls::CallState`).
    public enum WebCallState: String {
        case idle, outgoing, ringing, connecting, connected, ending, done
    }

    /// Reason carried in the terminal transition.
    public enum WebCallEndReason: String {
        case hungUp, busy, declined, timedOut, error
    }

    /// 16-byte call_id minted by the offerer.
    public let callId: Data

    @Published public private(set) var state: WebCallState = .idle
    @Published public private(set) var endReason: WebCallEndReason?

    private let onOutboundSignaling: (Data) -> Void
    private var isOfferer = false

    public init(callId: Data, onOutboundSignaling: @escaping (Data) -> Void) {
        self.callId = callId
        self.onOutboundSignaling = onOutboundSignaling
    }

    /// Generate a fresh 16-byte call_id.
    public static func mintCallId() -> Data {
        var bytes = Data(count: 16)
        _ = bytes.withUnsafeMutableBytes { ptr -> OSStatus in
            guard let base = ptr.baseAddress else { return -1 }
            return SecRandomCopyBytes(kSecRandomDefault, 16, base)
        }
        return bytes
    }

    // MARK: - Outbound

    /// Outbound side: dial the contact. In 3a9 scaffolding
    /// this immediately emits the Offer with a placeholder
    /// SDP body the web side accepts but produces no audio.
    /// The follow-up sprint replaces the placeholder with
    /// real `RTCPeerConnection.offer`.
    public func dial(audio: Bool, video: Bool) {
        guard state == .idle else { return }
        isOfferer = true
        state = .outgoing
        do {
            let bytes = try composeCallSignaling(
                body: .offer(
                    callId: callId,
                    sdp: Self.placeholderOfferSdp(),
                    createdAtMs: UInt64(Date().timeIntervalSince1970 * 1000),
                    media: FfiCallMediaFlags(audio: audio, video: video)
                )
            )
            onOutboundSignaling(bytes)
        } catch {
            NSLog("dial composeCallSignaling failed: %@", error.localizedDescription)
            endLocal(reason: .error)
        }
    }

    /// Outbound side: tear down a call we initiated or
    /// inbound side: tear down an active call.
    public func hangUp(reason: WebCallEndReason = .hungUp) {
        endLocal(reason: reason)
    }

    /// Inbound side: accept a ringing call. Emits the Answer
    /// signaling event.
    public func acceptCall() {
        guard state == .ringing, !isOfferer else { return }
        do {
            let bytes = try composeCallSignaling(
                body: .answer(
                    callId: callId,
                    sdp: Self.placeholderAnswerSdp()
                )
            )
            onOutboundSignaling(bytes)
            state = .connecting
            // Phase 3a9 scaffolding: no real ICE pair, so we
            // synthesise the `connected` transition
            // immediately after the answer leaves. The
            // follow-up replaces this with the
            // `oniceconnectionstatechange` hook.
            state = .connected
        } catch {
            NSLog("acceptCall composeCallSignaling failed: %@", error.localizedDescription)
            endLocal(reason: .error)
        }
    }

    /// Inbound side: decline a ringing call.
    public func declineCall() {
        endLocal(reason: .declined)
    }

    // MARK: - Inbound

    /// Route a parsed signaling payload into the state
    /// machine. Called from `ChatRepository.callSignalingSink`
    /// once the chat-route owner has filtered to this
    /// session's `call_id`.
    public func applyIncomingSignaling(_ body: FfiCallSignalingBody) {
        switch body {
        case .offer(let cid, _, _, _):
            guard cid == callId, state == .idle else { return }
            isOfferer = false
            state = .ringing
        case .answer(let cid, _):
            guard cid == callId, state == .outgoing else { return }
            state = .connecting
            // Phase 3a9 scaffolding: synthesise the
            // `connected` transition. Real ICE pair lands in
            // the follow-up.
            state = .connected
        case .iceCandidate:
            // Phase 3a9 scaffolding: candidates are no-ops
            // until the WebRTC pod lands.
            break
        case .end(let cid, let reason):
            guard cid == callId, state != .done else { return }
            endRemote(reason: Self.endReason(from: reason))
        }
    }

    // MARK: - Internal

    private func endLocal(reason: WebCallEndReason) {
        guard state != .done else { return }
        state = .ending
        endReason = reason
        do {
            let bytes = try composeCallSignaling(
                body: .end(
                    callId: callId,
                    reason: Self.toFfiEndReason(reason)
                )
            )
            onOutboundSignaling(bytes)
        } catch {
            NSLog("endLocal composeCallSignaling failed: %@", error.localizedDescription)
        }
        state = .done
    }

    private func endRemote(reason: WebCallEndReason) {
        guard state != .done else { return }
        endReason = reason
        state = .done
    }

    // MARK: - SDP placeholders (3a9 scaffolding)

    private static func placeholderOfferSdp() -> String {
        // Minimal SDP-offer envelope. Phase 3a9 scaffolding
        // — the web client + future iOS WebRTC pod ignore
        // bodies they don't understand, but the wire still
        // carries something parseable so the receive path
        // doesn't crash on an empty string.
        return [
            "v=0",
            "o=- 1 1 IN IP4 0.0.0.0",
            "s=lakko-3a9-scaffold",
            "t=0 0",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111",
            "a=rtpmap:111 opus/48000/2",
            "a=mid:audio",
            ""
        ].joined(separator: "\n")
    }

    private static func placeholderAnswerSdp() -> String {
        return [
            "v=0",
            "o=- 2 2 IN IP4 0.0.0.0",
            "s=lakko-3a9-scaffold",
            "t=0 0",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111",
            "a=rtpmap:111 opus/48000/2",
            "a=mid:audio",
            ""
        ].joined(separator: "\n")
    }

    private static func endReason(from ffi: FfiCallEndReason) -> WebCallEndReason {
        switch ffi {
        case .hungUp: return .hungUp
        case .busy: return .busy
        case .declined: return .declined
        case .timedOut: return .timedOut
        case .error: return .error
        }
    }

    private static func toFfiEndReason(_ r: WebCallEndReason) -> FfiCallEndReason {
        switch r {
        case .hungUp: return .hungUp
        case .busy: return .busy
        case .declined: return .declined
        case .timedOut: return .timedOut
        case .error: return .error
        }
    }
}

// MARK: - call_id extraction helper

extension CallSession {
    /// Pull the embedded `call_id` from any
    /// [`FfiCallSignalingBody`] variant. The chat-route
    /// dispatcher uses this to route stale signaling away
    /// from the active session.
    public static func callId(of body: FfiCallSignalingBody) -> Data {
        switch body {
        case .offer(let cid, _, _, _),
             .answer(let cid, _),
             .iceCandidate(let cid, _, _, _),
             .end(let cid, _):
            return cid
        }
    }
}
