// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import WebRTC

/// Multi-party SFU-backed call session on iOS — the native analog of
/// the web `apps/web/src/calls/sfu-call-session.ts`.
///
/// Where the 1×1 `CallSession` connects two devices peer-to-peer over
/// DTLS-SRTP, this connects the device to the Rust SFU and runs SFrame
/// end-to-end media encryption on top (the SFU only ever sees SFrame
/// ciphertext — security invariant #2).
///
/// Composes the pieces built earlier in the arc:
///   * `SfuSignalingClient` — the `lakko-sfu-v1` WebSocket (typed
///     frames through `lakko_sfu_proto`).
///   * The call-admission JWT (minted by `CallJwtClient`).
///   * `WebRTCPeerConnection` — the libwebrtc pod, with its
///     `armSFrameEncryptors` / `armSFrameDecryptor` hooks that install
///     the native SFrame bridge on each sender / receiver.
///
/// ## Signaling flow (one `RTCPeerConnection` to the SFU)
///   * **Publish** (client-initiated, once at connect): capture the
///     local mic, arm SFrame on the sender(s), `createOffer`, send it
///     as `ClientFrame::SdpOffer`; the SFU replies `SdpAnswer`.
///   * **Subscribe** (SFU-initiated, ongoing): when another participant
///     publishes, the SFU dispatches an `SdpOffer`; the session answers
///     (`createAnswer` -> `sendSdpAnswer`), and the new remote track
///     surfaces through the pod's `onRemoteReceiver` (SFrame decryptor
///     armed) + `onRemoteVideoTrack` (UI). Server offers are processed
///     through a serialized queue so two applies never overlap.
///
/// The SFU's own ICE candidates ride inside the SDP, so this session
/// only trickles its LOCAL candidates up via `sendIceCandidate` (the
/// wire protocol has no server -> client ICE frame).
///
/// Scope: audio publish on the first pass (video capture into the SFU
/// session is a follow-up; the receive path already renders remote
/// video). `connect()` refuses before any capture if the SFrame native
/// shim is unavailable (invariant #4).
@MainActor
final class SfuCallSession {

    enum State { case idle, connecting, connected, failed, closed }

    enum SfuCallError: Error { case alreadyConnected, sframeUnavailable }

    struct Config {
        let sfuBaseUrl: String
        let jwt: String
        /// 32-byte MLS exporter seed (`Group.exportSecret("lakko/call/v1", callId, 32)`).
        let seed: Data
        /// This device's MLS leaf index — the SFrame KID it stamps.
        let localLeaf: UInt32
        let audio: Bool
        let video: Bool
    }

    struct Callbacks {
        var onStateChange: ((State) -> Void)?
        var onWelcome: ((_ selfPeerId: String, _ roster: [SfuPeerSnapshot]) -> Void)?
        var onPeerJoined: ((_ peerId: String, _ accountPseudonym: String) -> Void)?
        var onPeerLeft: ((_ peerId: String) -> Void)?
        /// A remote video track (with its MSID stream id = source peer
        /// id) arrived or was removed (`nil`).
        var onRemoteVideoTrack: ((RTCVideoTrack?, _ streamId: String?) -> Void)?
        var onServerError: ((SfuErrorCode, String) -> Void)?
    }

    private let config: Config
    private let callbacks: Callbacks
    private var signaling: SfuSignalingClient?
    private var pc: WebRTCPeerConnection?
    private(set) var state: State = .idle

    /// Serializes SFU-initiated offers so two `createAnswer` cycles
    /// never overlap.
    private var offerQueue: [String] = []
    private var drainingOffers = false

    init(config: Config, callbacks: Callbacks) {
        self.config = config
        self.callbacks = callbacks
    }

    /// Open the SFU connection, publish the local audio (SFrame-sealed),
    /// and start subscribing. Throws before any media capture if the
    /// SFrame shim is unavailable (invariant #4).
    func connect() async throws {
        guard state == .idle else { throw SfuCallError.alreadyConnected }
        guard SFrameAvailability.shared.isCallingAvailable else {
            throw SfuCallError.sframeUnavailable
        }
        setState(.connecting)

        // 1. Open signaling first so the publish offer + trickled ICE
        //    have a live socket.
        let sig = SfuSignalingClient(
            baseUrl: config.sfuBaseUrl,
            jwt: config.jwt,
            callbacks: makeSignalingCallbacks()
        )
        signaling = sig
        do {
            try sig.connect()
        } catch {
            fail()
            throw error
        }

        // 2. Build the peer connection + publish.
        do {
            try await openPeerConnectionAndPublish()
        } catch {
            fail()
            throw error
        }
    }

    /// Mute / unmute the local microphone (local-only track gate).
    func setMicrophoneEnabled(_ on: Bool) {
        pc?.setMicrophoneEnabled(on)
    }

    /// Tear down. Idempotent.
    func hangUp() {
        signaling?.close()
        signaling = nil
        pc?.close()
        pc = nil
        offerQueue.removeAll()
        if state != .failed, state != .closed {
            setState(.closed)
        }
    }

    // MARK: - Peer connection + publish

    private func openPeerConnectionAndPublish() async throws {
        let connection = WebRTCPeerConnection(
            iceServers: WebRTCPeerConnection.defaultIceServers()
        )
        connection.installDelegate(makePeerDelegate())
        pc = connection

        // Publish the local mic. Arm SFrame on the sender BEFORE the
        // offer so the SFU never sees a plaintext frame.
        connection.addLocalAudioTrack(enabled: config.audio)
        guard connection.armSFrameEncryptors(seed: config.seed, localLeaf: config.localLeaf) else {
            // A failed arm is fail-closed (invariant #4).
            throw SfuCallError.sframeUnavailable
        }
        let offer = try await connection.createOffer(audio: config.audio, video: config.video)
        signaling?.sendSdpOffer(offer)
    }

    // MARK: - Signaling

    private func makeSignalingCallbacks() -> SfuSignalingCallbacks {
        var cb = SfuSignalingCallbacks()
        cb.onWelcome = { [weak self] selfPeerId, _, roster in
            Task { @MainActor in self?.callbacks.onWelcome?(selfPeerId, roster) }
        }
        cb.onPeerJoined = { [weak self] peerId, pseudonym in
            Task { @MainActor in self?.callbacks.onPeerJoined?(peerId, pseudonym) }
        }
        cb.onPeerLeft = { [weak self] peerId in
            Task { @MainActor in self?.callbacks.onPeerLeft?(peerId) }
        }
        cb.onSdpAnswer = { [weak self] sdp in
            Task { @MainActor in await self?.applyRemoteAnswer(sdp) }
        }
        cb.onSdpOffer = { [weak self] sdp in
            Task { @MainActor in self?.enqueueServerOffer(sdp) }
        }
        cb.onServerError = { [weak self] code, detail in
            Task { @MainActor in self?.callbacks.onServerError?(code, detail) }
        }
        cb.onClosed = { [weak self] in
            Task { @MainActor in self?.onSignalingClosed() }
        }
        return cb
    }

    private func applyRemoteAnswer(_ sdp: String) async {
        guard let pc = pc else { return }
        do {
            try await pc.applyRemoteAnswer(sdp)
        } catch {
            fail()
        }
    }

    private func enqueueServerOffer(_ sdp: String) {
        offerQueue.append(sdp)
        Task { @MainActor in await self.drainServerOffers() }
    }

    private func drainServerOffers() async {
        if drainingOffers { return }
        drainingOffers = true
        defer { drainingOffers = false }
        while !offerQueue.isEmpty {
            let sdp = offerQueue.removeFirst()
            guard let pc = pc, let signaling = signaling else { break }
            do {
                let answer = try await pc.createAnswer(remoteOfferSdp: sdp)
                signaling.sendSdpAnswer(answer)
            } catch {
                // A single bad renegotiation drops that subscription;
                // the call stays up. Continue draining.
            }
        }
    }

    private func onSignalingClosed() {
        if state == .closed || state == .failed { return }
        fail()
    }

    // MARK: - Peer connection delegate

    private func makePeerDelegate() -> WebRTCPeerConnection.Delegate {
        WebRTCPeerConnection.Delegate(
            onLocalCandidate: { [weak self] _, _, candidate in
                self?.signaling?.sendIceCandidate(candidate)
            },
            onConnectionState: { [weak self] rtcState in
                self?.onConnectionState(rtcState)
            },
            onRemoteVideoTrack: { [weak self] track, streamId in
                self?.callbacks.onRemoteVideoTrack?(track, streamId)
            },
            onRemoteReceiver: { [weak self] receiver, _ in
                // Arm the SFrame decryptor on every incoming track
                // (audio + video) — invariant #4.
                guard let self = self else { return }
                _ = self.pc?.armSFrameDecryptor(for: receiver, seed: self.config.seed)
            }
        )
    }

    private func onConnectionState(_ rtcState: RTCPeerConnectionState) {
        switch rtcState {
        case .connected:
            if state == .connecting { setState(.connected) }
        case .failed, .disconnected, .closed:
            fail()
        default:
            break
        }
    }

    // MARK: - State

    private func setState(_ next: State) {
        state = next
        callbacks.onStateChange?(next)
    }

    private func fail() {
        if state == .failed || state == .closed { return }
        setState(.failed)
        hangUp()
    }
}
