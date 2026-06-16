// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The WebSocket subprotocol the SFU requires on the upgrade.
let sfuSubprotocol = "lakko-sfu-v1"

/// Caller misuse / setup failures before any I/O.
enum SfuSignalingError: Error {
    case alreadyConnected
    case badUrl
}

/// Handlers the caller wires in. Every field is optional; an unhandled
/// frame is silently ignored. Invoked from the receive-loop `Task` (the
/// caller hops to `@MainActor` itself if it touches UI state).
struct SfuSignalingCallbacks {
    var onWelcome: ((_ selfPeerId: String, _ callId: String, _ roster: [SfuPeerSnapshot]) -> Void)?
    var onPeerJoined: ((_ peerId: String, _ accountPseudonym: String, _ peerTag: String) -> Void)?
    var onPeerLeft: ((_ peerId: String) -> Void)?
    var onSignalRelay: ((_ from: String, _ payloadB64: String) -> Void)?
    var onSdpAnswer: ((_ sdp: String) -> Void)?
    var onSdpOffer: ((_ sdp: String) -> Void)?
    var onPong: ((_ nonce: UInt64) -> Void)?
    var onServerError: ((_ code: SfuErrorCode, _ detail: String) -> Void)?
    /// A frame the shared parser rejected. The connection is NOT closed
    /// automatically — the caller decides.
    var onParseError: ((_ raw: String, _ error: Error) -> Void)?
    /// The receive loop ended (socket closed, clean or not).
    var onClosed: (() -> Void)?
}

/// SFU WebSocket signaling client (SFU multi-party arc, native half) —
/// mirror of the Kotlin `apps/android/.../calls/SfuSignalingClient.kt`
/// and the web `apps/web/src/calls/sfu-signaling-client.ts`.
///
/// Connects to the Rust SFU's `lakko-sfu-v1` WebSocket
/// (`{baseUrl}/v1/connect?token=<jwt>`), parses the typed server frames
/// through the shared `lakko_sfu_proto` definition (the UniFFI
/// `parseSfuServerFrame`), and sends the typed client frames via the
/// `composeSfu*` helpers — so the client and `services/sfu` agree on the
/// wire shape from one definition.
///
/// Transport + framing only — it does not touch `RTCPeerConnection`. The
/// CallSession SFU-mode wiring (route the local SDP offer through
/// `sendSdpOffer`, apply the answer, trickle ICE via `sendIceCandidate`,
/// drive SFU-initiated renegotiation from `onSdpOffer`) is the next step
/// on top of this client.
///
/// One instance per call. Construct with the SFU base URL (an http(s) /
/// ws(s) origin, no path) + the call-admission JWT (from
/// `CallJwtClient`), then `connect()`. Tear down with `close()`
/// (idempotent).
final class SfuSignalingClient {
    private let baseUrl: String
    private let jwt: String
    private let callbacks: SfuSignalingCallbacks
    private let session: URLSession
    private var task: URLSessionWebSocketTask?
    private var receiveTask: Task<Void, Never>?

    init(
        baseUrl: String,
        jwt: String,
        callbacks: SfuSignalingCallbacks,
        session: URLSession = .shared
    ) {
        self.baseUrl = baseUrl
        self.jwt = jwt
        self.callbacks = callbacks
        self.session = session
    }

    /// True once the WebSocket task exists.
    var isOpen: Bool { task != nil }

    /// Open the WebSocket and start dispatching frames. The SFU's
    /// Welcome frame arrives on `onWelcome` shortly after.
    ///
    /// - Throws: `SfuSignalingError.alreadyConnected` if called twice,
    ///   `.badUrl` if the base URL / JWT cannot form a valid URL.
    func connect() throws {
        guard task == nil else { throw SfuSignalingError.alreadyConnected }
        // http(s) -> ws(s); the SFU upgrade endpoint speaks WebSocket.
        var wsBase = baseUrl
            .replacingOccurrences(of: "http://", with: "ws://")
            .replacingOccurrences(of: "https://", with: "wss://")
        if wsBase.hasSuffix("/") { wsBase.removeLast() }
        // The JWT is base64url + dots, so already URL-safe; encode
        // defensively anyway.
        let escaped =
            jwt.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? jwt
        guard let url = URL(string: "\(wsBase)/v1/connect?token=\(escaped)") else {
            throw SfuSignalingError.badUrl
        }
        // The subprotocol array sets the `Sec-WebSocket-Protocol` header
        // the SFU's accept-check requires.
        let t = session.webSocketTask(with: url, protocols: [sfuSubprotocol])
        task = t
        t.resume()
        receiveTask = Task { [weak self] in
            await self?.receiveLoop(t)
        }
    }

    private func receiveLoop(_ t: URLSessionWebSocketTask) async {
        while !Task.isCancelled {
            let message: URLSessionWebSocketTask.Message
            do {
                message = try await t.receive()
            } catch {
                // Socket closed or errored — end the loop.
                break
            }
            let text: String
            switch message {
            case let .string(s):
                text = s
            case let .data(d):
                text = String(decoding: d, as: UTF8.self)
            @unknown default:
                continue
            }
            do {
                let frame = try parseSfuServerFrame(json: text)
                dispatch(frame)
            } catch {
                callbacks.onParseError?(text, error)
            }
        }
        callbacks.onClosed?()
    }

    private func dispatch(_ frame: SfuServerFrame) {
        switch frame {
        case let .welcome(peerId, callId, roster, _):
            callbacks.onWelcome?(peerId, callId, roster)
        case let .peerJoined(peerId, accountPseudonym, peerTag):
            callbacks.onPeerJoined?(peerId, accountPseudonym, peerTag)
        case let .peerLeft(peerId):
            callbacks.onPeerLeft?(peerId)
        case let .signalRelay(from, payloadB64):
            callbacks.onSignalRelay?(from, payloadB64)
        case let .sdpAnswer(sdp):
            callbacks.onSdpAnswer?(sdp)
        case let .sdpOffer(sdp):
            callbacks.onSdpOffer?(sdp)
        case let .pong(nonce):
            callbacks.onPong?(nonce)
        case let .error(code, detail):
            callbacks.onServerError?(code, detail)
        }
    }

    // MARK: - Send (client -> SFU)

    /// Send a client -> SFU SDP offer (publish path).
    func sendSdpOffer(_ sdp: String) { enqueue(try? composeSfuSdpOffer(sdp: sdp)) }

    /// Send a client -> SFU SDP answer (responds to a SFU-initiated
    /// offer).
    func sendSdpAnswer(_ sdp: String) { enqueue(try? composeSfuSdpAnswer(sdp: sdp)) }

    /// Send a trickle-ICE candidate (the `candidate` line value).
    func sendIceCandidate(_ candidateSdp: String) {
        enqueue(try? composeSfuIceCandidate(candidateSdp: candidateSdp))
    }

    /// Relay an opaque payload to a single peer.
    func sendSignalRelay(toPeer peerId: String, payload: Data) {
        enqueue(try? composeSfuSignalRelayToPeer(peerId: peerId, payload: payload))
    }

    /// Relay an opaque payload to every other peer in the call.
    func sendSignalRelayToCall(payload: Data) {
        enqueue(try? composeSfuSignalRelayToCall(payload: payload))
    }

    /// Application-layer keepalive; the SFU echoes a Pong.
    func sendPing(nonce: UInt64) { enqueue(try? composeSfuPing(nonce: nonce)) }

    /// Send a composed wire string. Fire-and-forget — the completion
    /// only matters for transport errors, which the receive loop already
    /// surfaces via `onClosed`. A failed compose (nil) is a no-op.
    private func enqueue(_ json: String?) {
        guard let json, let t = task else { return }
        t.send(.string(json)) { _ in }
    }

    /// Close the socket + cancel the receive loop. Idempotent.
    func close() {
        receiveTask?.cancel()
        receiveTask = nil
        task?.cancel(with: .normalClosure, reason: nil)
        task = nil
    }
}
