// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore
import WebRTC

/// 1×1 P2P call session on iOS.
///
/// Mirror of `apps/web/src/calls/call-session.ts` translated
/// into a `@MainActor` `ObservableObject`. Tracks the call
/// state machine + composes outbound signaling via the
/// UniFFI `composeCall*Signaling` free functions.
///
/// ## G3-1 status
///
/// Real WebRTC integration: `dial()` / `acceptCall()` now
/// build real SDP via [`WebRTCPeerConnection`], local ICE
/// candidates are emitted through the `iceCandidate`
/// signaling body, and the underlying RTCPeerConnection's
/// connection-state transitions drive `WebCallState`.
/// Audio-only on the first pass — the video plumbing
/// extension point (`WebRTCPeerConnection.addLocalVideoTrack`)
/// lands in a follow-up (`G3-1-video`) once the SwiftUI
/// preview surface is wired.
///
/// ## SFrame deferred
///
/// Today's transport rides plain SRTP (per-hop encryption
/// only). The end-to-end SFrame layer keyed off the call's
/// MLS exporter arrives in a follow-up sub-sprint
/// (`G3-1-sframe`); the iOS call surface is **not yet
/// E2EE-complete** despite the wire saying so. This caveat
/// is repeated verbatim in
/// `docs/security/calls.md` so the audit pack doesn't
/// double-count.
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

    /// Stored remote SDP offer between `.ringing` → `.connecting`.
    /// `acceptCall` reads it to build the answer.
    private var pendingRemoteOfferSdp: String?

    /// The WebRTC pod. Lazily constructed inside `dial` /
    /// `acceptCall` so an `idle` session pays nothing.
    private var rtc: WebRTCPeerConnection?

    /// Cached audio toggle for `dial`-side construction.
    private var dialAudio: Bool = true
    private var dialVideo: Bool = false

    /// G3-sframe-native N-6 — per-call MLS exporter seed +
    /// local participant leaf index. Both must be set before
    /// `dial` / `acceptCall` so the SFrame attach can fire
    /// against every fresh RTCRtpSender immediately after the
    /// track is added. A nil seed at attach time aborts the
    /// call (security invariant #4: no fallback to non-E2EE).
    private var callKeySeed: Data?
    private var localLeaf: UInt32?

    /// G3-1-turn — TURN-credential client. Injected by the
    /// call-UI layer via [`setTurnClient`] so tests can swap
    /// a stub. When `nil` (or when its `fetchOrNil()`
    /// returns `nil`) the call proceeds with STUN-only ICE —
    /// fail-soft path per `TurnCredentialClient` docs.
    private var turnClient: TurnCredentialClient?

    /// Bind the TURN client used for ICE-server resolution at
    /// `dial` / `acceptCall` time. Idempotent; passing `nil`
    /// disables TURN fetching for subsequent calls (STUN-only
    /// fallback).
    public func setTurnClient(_ client: TurnCredentialClient?) {
        self.turnClient = client
    }

    public init(callId: Data, onOutboundSignaling: @escaping (Data) -> Void) {
        self.callId = callId
        self.onOutboundSignaling = onOutboundSignaling
    }

    /// Inject the per-call SFrame keying material. Caller MUST
    /// run this AFTER the welcome-ferry / KP-forward dispatcher
    /// has resolved (for joiners) or AFTER `Group.create`
    /// on the 1×1-from-chat path (for the call originator),
    /// and BEFORE `dial` / `acceptCall`.
    ///
    /// `seed` is exactly 32 bytes
    /// (`Group.export_secret("lakko/call/v1", call_id, 32)`).
    /// `localLeaf` is this device's leaf index in the call's
    /// MLS group.
    public func setCallKey(seed: Data, localLeaf: UInt32) {
        self.callKeySeed = seed
        self.localLeaf = localLeaf
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

    /// Outbound side: dial the contact. Spins up
    /// `WebRTCPeerConnection`, attaches a local audio track,
    /// generates a real SDP offer and ships it via the
    /// existing signaling pipe.
    public func dial(audio: Bool, video: Bool) {
        guard state == .idle else { return }
        isOfferer = true
        dialAudio = audio
        dialVideo = video
        state = .outgoing

        // G3-1-turn — fetch TURN credential before constructing
        // the RTCPeerConnection so its initial RTCConfiguration
        // already lists the relay candidate. Doing it after
        // construction would require an `setConfiguration:` call
        // mid-flight, which the stasel/WebRTC pod accepts but
        // some libwebrtc forks have known footguns around (the
        // ICE agent may have already started gathering, in which
        // case the new TURN server is ignored).
        Task { [weak self] in
            guard let self = self else { return }
            let iceServers = await self.resolveIceServers()
            await MainActor.run {
                self.completeDial(iceServers: iceServers, audio: audio, video: video)
            }
        }
    }

    private func completeDial(iceServers: [RTCIceServer], audio: Bool, video: Bool) {
        let rtc = WebRTCPeerConnection(iceServers: iceServers)
        guard rtc.isReady else {
            NSLog("CallSession.dial: RTCPeerConnection allocation failed")
            endLocal(reason: .error)
            return
        }
        installDelegate(on: rtc)
        rtc.addLocalAudioTrack(enabled: audio)
        // Video track extension point: when the SwiftUI
        // preview surface lands (G3-1-video), pass the
        // capture-session source here. Today audio-only.
        // if video { rtc.addLocalVideoTrack(<source>) }

        // G3-sframe-native N-6 — arm SFrame on every freshly-
        // added sender. Must happen AFTER addLocalAudioTrack
        // (it captures the RtpSender we need to attach to) and
        // BEFORE composeAndShipOffer (the SDP-build path can
        // run on a codec thread the moment the sender exists,
        // so we want the encryptor already in place to avoid
        // a window where libwebrtc would tee bytes unencrypted).
        if !attachSFrame(on: rtc) {
            NSLog("dial: SFrame attach failed — refusing per security invariant #4")
            endLocal(reason: .error)
            return
        }
        self.rtc = rtc

        Task { [weak self] in
            await self?.composeAndShipOffer(rtc: rtc, audio: audio, video: video)
        }
    }

    /// G3-1-turn — build the ICE-server list for this call:
    /// `defaultIceServers()` (STUN-only) plus optionally one
    /// TURN row freshly minted via the auth service. Fails
    /// soft to STUN-only when `turnClient` is unset or
    /// `fetchOrNil()` returns nil — the call still attempts
    /// to connect (the SFU is reachable; only symmetric-NAT
    /// peers will fail), and the failure surfaces as a normal
    /// ICE-disconnected state rather than a refused call.
    private func resolveIceServers() async -> [RTCIceServer] {
        var servers = WebRTCPeerConnection.defaultIceServers()
        if let turn = turnClient, let row = await turn.fetchOrNil() {
            servers.append(row)
        }
        return servers
    }

    /// Resolve the seed + leaf and invoke
    /// `armSFrameEncryptors`. Returns `false` when:
    ///   - `setCallKey` was never called (caller forgot — bug)
    ///   - `SFrameAvailability.isCallingAvailable` is false
    ///   - any sender failed to attach
    /// All three cases collapse to a refused call.
    private func attachSFrame(on rtc: WebRTCPeerConnection) -> Bool {
        guard let seed = callKeySeed, let leaf = localLeaf else {
            NSLog("attachSFrame: setCallKey was never called — security invariant #4")
            return false
        }
        guard SFrameAvailability.shared.isCallingAvailable else {
            NSLog("attachSFrame: SFrameAvailability reports calling unavailable")
            return false
        }
        return rtc.armSFrameEncryptors(seed: seed, localLeaf: leaf)
    }

    private func composeAndShipOffer(
        rtc: WebRTCPeerConnection,
        audio: Bool,
        video: Bool
    ) async {
        do {
            let offerSdp = try await rtc.createOffer(audio: audio, video: video)
            let bytes = try composeCallSignaling(
                body: .offer(
                    callId: callId,
                    sdp: offerSdp,
                    createdAtMs: UInt64(Date().timeIntervalSince1970 * 1000),
                    media: FfiCallMediaFlags(audio: audio, video: video)
                )
            )
            onOutboundSignaling(bytes)
        } catch {
            NSLog("dial offer-build failed: %@", String(describing: error))
            endLocal(reason: .error)
        }
    }

    /// Outbound side: tear down a call we initiated or
    /// inbound side: tear down an active call.
    public func hangUp(reason: WebCallEndReason = .hungUp) {
        endLocal(reason: reason)
    }

    /// Inbound side: accept a ringing call. Builds the SDP
    /// answer against the stored remote offer + ships it.
    public func acceptCall() {
        guard state == .ringing, !isOfferer else { return }
        guard let offerSdp = pendingRemoteOfferSdp else {
            NSLog("acceptCall: pendingRemoteOfferSdp missing — race?")
            endLocal(reason: .error)
            return
        }

        // G3-1-turn — same async TURN-fetch shape as `dial()`.
        Task { [weak self] in
            guard let self = self else { return }
            let iceServers = await self.resolveIceServers()
            await MainActor.run {
                self.completeAccept(iceServers: iceServers, offerSdp: offerSdp)
            }
        }
    }

    private func completeAccept(iceServers: [RTCIceServer], offerSdp: String) {
        let rtc = WebRTCPeerConnection(iceServers: iceServers)
        guard rtc.isReady else {
            NSLog("acceptCall: RTCPeerConnection allocation failed")
            endLocal(reason: .error)
            return
        }
        installDelegate(on: rtc)
        // Mic on by default for the answerer; if the call
        // UI later supports "accept muted" we will pass the
        // user pick through this method.
        rtc.addLocalAudioTrack(enabled: true)

        // G3-sframe-native N-6 — same attach point as dial().
        if !attachSFrame(on: rtc) {
            NSLog("acceptCall: SFrame attach failed — refusing per security invariant #4")
            endLocal(reason: .error)
            return
        }
        self.rtc = rtc

        state = .connecting
        Task { [weak self] in
            await self?.composeAndShipAnswer(rtc: rtc, offerSdp: offerSdp)
        }
    }

    private func composeAndShipAnswer(
        rtc: WebRTCPeerConnection,
        offerSdp: String
    ) async {
        do {
            let answerSdp = try await rtc.createAnswer(remoteOfferSdp: offerSdp)
            let bytes = try composeCallSignaling(
                body: .answer(
                    callId: callId,
                    sdp: answerSdp
                )
            )
            onOutboundSignaling(bytes)
            // Don't synthesise `.connected` here — wait for
            // the RTCPeerConnection's real connectionState
            // transition. The delegate hook below drives it.
        } catch {
            NSLog("acceptCall answer-build failed: %@", String(describing: error))
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
        case .offer(let cid, let sdp, _, _):
            guard cid == callId, state == .idle else { return }
            isOfferer = false
            pendingRemoteOfferSdp = sdp
            state = .ringing
        case .answer(let cid, let sdp):
            guard cid == callId, state == .outgoing else { return }
            state = .connecting
            // Apply the remote answer asynchronously; the
            // real `.connected` transition fires from the
            // RTCPeerConnection state delegate.
            guard let rtc = rtc else { return }
            Task { [weak self] in
                do {
                    try await rtc.applyRemoteAnswer(sdp)
                } catch {
                    NSLog(
                        "applyRemoteAnswer failed: %@",
                        String(describing: error)
                    )
                    self?.endLocal(reason: .error)
                }
            }
        case .iceCandidate(let cid, let candidate, let sdpMid, let sdpMLineIndex):
            guard cid == callId else { return }
            guard let rtc = rtc else { return }
            // sdpMLineIndex on the wire is UInt32; WebRTC's
            // RTCIceCandidate takes Int32. The valid range
            // for an SDP m-line index is 0..255 in practice,
            // so the cast is loss-free.
            let mLineIndex = Int32(bitPattern: sdpMLineIndex)
            Task { await rtc.addRemoteCandidate(
                sdpMid: sdpMid,
                sdpMLineIndex: mLineIndex,
                candidate: candidate
            ) }
        case .groupStart, .groupJoin, .groupLeave:
            // Group-call signaling (Phase 3c2) is handled by
            // the in-chat joinable-call card dispatcher, not
            // by the 1×1 `CallSession`. Ignore here — the
            // chat-route owner filters these away from the
            // 1×1 sink before they reach this method, but
            // the exhaustive-switch contract requires us to
            // name the cases.
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
        rtc?.close()
        rtc = nil
        pendingRemoteOfferSdp = nil
        state = .done
    }

    private func endRemote(reason: WebCallEndReason) {
        guard state != .done else { return }
        endReason = reason
        rtc?.close()
        rtc = nil
        pendingRemoteOfferSdp = nil
        state = .done
    }

    // MARK: - WebRTC delegate plumbing

    /// Wire the wrapper's typed callbacks back into the
    /// session. `self` is captured weakly to defend against
    /// the wrapper outliving the session in error-path
    /// teardown.
    private func installDelegate(on rtc: WebRTCPeerConnection) {
        rtc.installDelegate(.init(
            onLocalCandidate: { [weak self] sdpMid, mLineIndex, sdp in
                self?.shipLocalCandidate(
                    sdpMid: sdpMid,
                    sdpMLineIndex: mLineIndex,
                    candidate: sdp
                )
            },
            onConnectionState: { [weak self] newState in
                self?.handleConnectionState(newState)
            }
        ))
    }

    private func shipLocalCandidate(
        sdpMid: String,
        sdpMLineIndex: Int32,
        candidate: String
    ) {
        // WebRTC `Int32` → wire `UInt32`. Symmetric to the
        // applyIncomingSignaling cast above.
        let wireIndex = UInt32(bitPattern: sdpMLineIndex)
        do {
            let bytes = try composeCallSignaling(
                body: .iceCandidate(
                    callId: callId,
                    candidate: candidate,
                    sdpMid: sdpMid,
                    sdpMLineIndex: wireIndex
                )
            )
            onOutboundSignaling(bytes)
        } catch {
            NSLog(
                "shipLocalCandidate composeCallSignaling failed: %@",
                error.localizedDescription
            )
        }
    }

    /// Map RTCPeerConnectionState transitions to the public
    /// WebCallState. Only `.connected`, `.disconnected`,
    /// `.failed`, `.closed` are actionable — `.new` /
    /// `.connecting` are transient.
    private func handleConnectionState(_ newState: RTCPeerConnectionState) {
        switch newState {
        case .connected:
            // First `.connected` after connecting → move to
            // the public connected state. Reconnects after a
            // brief `.disconnected` interlude also pass
            // through here.
            if state == .connecting || state == .outgoing {
                state = .connected
            }
        case .failed:
            endLocal(reason: .error)
        case .disconnected:
            // WebRTC sometimes flaps disconnected → connected
            // on a transient network glitch (e.g. wifi → LTE
            // handover). Don't tear down the session — let
            // the wrapper recover. If it sticks for too long
            // the upper-layer watchdog will fire `.timedOut`.
            break
        case .closed:
            if state != .done {
                endRemote(reason: .hungUp)
            }
        case .new, .connecting:
            break
        @unknown default:
            break
        }
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
        case .groupStart(let cid, _, _, _),
             .groupJoin(let cid, _, _),
             .groupLeave(let cid, _, _):
            return cid
        }
    }
}
