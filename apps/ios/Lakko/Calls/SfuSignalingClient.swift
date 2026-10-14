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
    /// What the SFU gives this device of each publisher's video (ADR-0070
    /// step 6): the whole set, replacing the last one.
    var onVideoGiven: ((_ given: [SfuVideoGiven]) -> Void)?
    /// Which of this device's own camera senders somebody is being sent
    /// (ADR-0070, "senders nobody watches"): the whole state, the last
    /// frame wins. The light sender is always needed and is not named.
    var onVideoNeeded: ((_ main: Bool, _ middle: Bool) -> Void)?
    /// How much this device's main and middle camera sender may send, in
    /// kbit/s (ADR-0068, amendment of 2026-10-11): the whole state, the last
    /// frame wins. The light sender is not named.
    var onPublishBudget: ((_ mainKbps: UInt32, _ middleKbps: UInt32) -> Void)?
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

    // ADR-0026 — when anti-censorship is armed, the signaling WebSocket rides the
    // obfuscated tunnel via the Rust FFI instead of URLSession. The FFI `recv()`
    // is a blocking call, so the receive loop runs on a dedicated serial GCD
    // queue (not the Swift cooperative pool); sends go on a separate queue (the
    // FFI splits the sink + stream, so concurrent send / recv is safe).
    private let stateLock = NSLock()
    private var tunneledConn: TunneledWsConnection?
    /// `close()` was called. Read under `stateLock`: the tunneled dial runs
    /// on its own queue and may finish after the caller has gone, and a frame
    /// already read may be dispatched after it.
    private var closed = false

    private var isClosed: Bool {
        stateLock.lock()
        defer { stateLock.unlock() }
        return closed
    }
    private let tunnelRecvQueue = DispatchQueue(label: "me.lakko.sfu.tunnel.recv", qos: .userInitiated)

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

    /// True once the WebSocket (direct or tunneled) exists.
    var isOpen: Bool {
        if task != nil { return true }
        stateLock.lock()
        defer { stateLock.unlock() }
        return tunneledConn != nil
    }

    /// Open the WebSocket and start dispatching frames. The SFU's
    /// Welcome frame arrives on `onWelcome` shortly after.
    ///
    /// - Throws: `SfuSignalingError.alreadyConnected` if called twice,
    ///   `.badUrl` if the base URL / JWT cannot form a valid URL.
    func connect() throws {
        guard task == nil, tunneledConn == nil, !isClosed else {
            throw SfuSignalingError.alreadyConnected
        }
        // ADR-0026 — armed: route the signaling WebSocket through the tunnel.
        let antiCensorship = AntiCensorshipConfig.current
        if antiCensorship.armed {
            try connectTunneled(antiCensorship)
            return
        }
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
            switch message {
            case let .string(s):
                handleFrameText(s)
            case let .data(d):
                handleFrameText(String(decoding: d, as: UTF8.self))
            @unknown default:
                continue
            }
        }
        callbacks.onClosed?()
    }

    /// Parse + dispatch one signaling frame (shared by the direct + tunneled
    /// receive loops). A parse failure surfaces via `onParseError`, never closes.
    private func handleFrameText(_ text: String) {
        // A frame read before `close()` and dispatched after it belongs to a
        // call that is over.
        if isClosed { return }
        do {
            let frame = try parseSfuServerFrame(json: text)
            dispatch(frame)
        } catch {
            callbacks.onParseError?(text, error)
        }
    }

    /// Where the tunneled signaling socket dials and which request it sends.
    struct TunneledTarget: Equatable {
        let host: String
        let port: UInt16
        let innerUrl: String
    }

    /// The tunnel endpoint and the inner WebSocket URL for `baseUrl`.
    ///
    /// The inner URL is plain `ws://` (the obfuscated tunnel provides the
    /// encryption) and keeps the base URL's path: an SFU served under a prefix
    /// (`wss://test.lakko.me/sfu`) is reached at `/sfu/v1/connect`, exactly
    /// like the direct path. `nil` when the base URL has no host or port.
    static func tunneledTarget(baseUrl: String, jwt: String) -> TunneledTarget? {
        guard let comps = URLComponents(string: baseUrl), let host = comps.host, !host.isEmpty else {
            return nil
        }
        let isSecure = baseUrl.hasPrefix("https") || baseUrl.hasPrefix("wss")
        guard let port = UInt16(exactly: comps.port ?? (isSecure ? 443 : 80)) else { return nil }
        var path = comps.percentEncodedPath
        while path.hasSuffix("/") { path.removeLast() }
        let escaped = jwt.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? jwt
        return TunneledTarget(
            host: host,
            port: port,
            innerUrl: "ws://\(host)\(path)/v1/connect?token=\(escaped)"
        )
    }

    /// ADR-0026 — open the signaling WebSocket over the obfuscated tunnel and
    /// run a blocking receive loop on the dedicated serial queue. The dial +
    /// handshake block, so they run off the calling thread; a failure surfaces
    /// via `onClosed` (the same loud teardown the direct path uses).
    private func connectTunneled(_ config: AntiCensorshipConfig) throws {
        guard let target = Self.tunneledTarget(baseUrl: baseUrl, jwt: jwt) else {
            throw SfuSignalingError.badUrl
        }
        let host = target.host
        let port = target.port
        let innerUrl = target.innerUrl
        let now = UInt64(Date().timeIntervalSince1970)

        tunnelRecvQueue.async { [weak self] in
            guard let self else { return }
            let conn: TunneledWsConnection
            do {
                conn = try openTunneledWebsocket(
                    configJson: config.bootstrapConfigJson,
                    trustAnchor: config.trustAnchor,
                    nowSecs: now,
                    endpointHost: host,
                    endpointPort: port,
                    deadlineMs: config.deadlineMs,
                    url: innerUrl,
                    subprotocol: sfuSubprotocol
                )
            } catch {
                self.callbacks.onClosed?()
                return
            }
            // The caller may have closed while the tunnel was dialled (the
            // person ended the call during the connect). `close()` found no
            // connection to shut down then, so it is shut down here; kept, it
            // would hold a socket on the SFU and a place in the call for a
            // session nobody owns.
            self.stateLock.lock()
            let closedMeanwhile = self.closed
            if !closedMeanwhile { self.tunneledConn = conn }
            self.stateLock.unlock()
            if closedMeanwhile {
                try? conn.shutdown()
                return
            }

            // Blocking receive loop on this dedicated serial queue.
            while true {
                let message: FfiWsMessage
                do {
                    message = try conn.recv()
                } catch {
                    break
                }
                switch message {
                case let .text(text):
                    self.handleFrameText(text)
                case let .binary(data):
                    self.handleFrameText(String(decoding: data, as: UTF8.self))
                case .closed:
                    self.callbacks.onClosed?()
                    return
                }
            }
            self.callbacks.onClosed?()
        }
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
        case let .videoGiven(given):
            callbacks.onVideoGiven?(given)
        case let .videoNeeded(main, middle):
            callbacks.onVideoNeeded?(main, middle)
        case let .publishBudget(mainKbps, middleKbps):
            callbacks.onPublishBudget?(mainKbps, middleKbps)
        }
    }

    // MARK: - Send (client -> SFU)

    /// Send a client -> SFU SDP offer (publish path).
    ///
    /// `lightMids` and `middleMids` name the offer's m-lines that carry the
    /// light and the middle copy of the camera (ADR-0070 steps 1 and 5), so
    /// the SFU forwards them as the camera's `~light` companion and its
    /// `~mid` track. Empty while this device has no such sender.
    func sendSdpOffer(_ sdp: String, lightMids: [String] = [], middleMids: [String] = []) {
        enqueue(try? composeSfuSdpOfferWithMiddle(
            sdp: sdp, lightMids: lightMids, middleMids: middleMids))
    }

    /// Tell the SFU which picture of each publisher this device shows
    /// (ADR-0070 step 5): the whole set each time. `false` when the frame
    /// could not be composed or there is no open socket to put it on.
    @discardableResult
    func sendVideoWants(_ wants: [SfuVideoWant]) -> Bool {
        guard isOpen, let json = try? composeSfuVideoWants(wants: wants) else { return false }
        enqueue(json)
        return true
    }

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
        guard let json else { return }
        // Armed: send over the tunnel on a separate thread (the FFI send blocks
        // on its runtime; the split sink makes it independent of the recv loop).
        stateLock.lock()
        let conn = tunneledConn
        stateLock.unlock()
        if let conn {
            DispatchQueue.global(qos: .userInitiated).async {
                try? conn.sendText(text: json)
            }
            return
        }
        guard let t = task else { return }
        t.send(.string(json)) { _ in }
    }

    /// Close the socket + cancel the receive loop. Idempotent. Closing the
    /// tunneled connection unblocks the receive loop (its `recv()` returns
    /// `.closed`).
    func close() {
        receiveTask?.cancel()
        receiveTask = nil
        task?.cancel(with: .normalClosure, reason: nil)
        task = nil
        stateLock.lock()
        closed = true
        let conn = tunneledConn
        tunneledConn = nil
        stateLock.unlock()
        if let conn {
            DispatchQueue.global(qos: .userInitiated).async {
                try? conn.shutdown()
            }
        }
    }
}
