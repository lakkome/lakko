// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import io.ktor.client.HttpClient
import io.ktor.client.plugins.websocket.DefaultClientWebSocketSession
import io.ktor.client.plugins.websocket.WebSockets
import io.ktor.client.plugins.websocket.webSocketSession
import io.ktor.client.request.header
import io.ktor.websocket.Frame
import io.ktor.websocket.close
import io.ktor.websocket.readText
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.launch
import me.lakko.app.transport.AntiCensorshipConfig
import uniffi.lakko_ffi_uniffi.FfiWsMessage
import uniffi.lakko_ffi_uniffi.SfuErrorCode
import uniffi.lakko_ffi_uniffi.SfuPeerSnapshot
import uniffi.lakko_ffi_uniffi.SfuServerFrame
import uniffi.lakko_ffi_uniffi.SfuVideoGiven
import uniffi.lakko_ffi_uniffi.SfuVideoWant
import uniffi.lakko_ffi_uniffi.composeSfuIceCandidate
import uniffi.lakko_ffi_uniffi.composeSfuPing
import uniffi.lakko_ffi_uniffi.composeSfuSdpAnswer
import uniffi.lakko_ffi_uniffi.composeSfuSdpOfferWithMiddle
import uniffi.lakko_ffi_uniffi.composeSfuSignalRelayToCall
import uniffi.lakko_ffi_uniffi.composeSfuSignalRelayToPeer
import uniffi.lakko_ffi_uniffi.composeSfuVideoWants
import uniffi.lakko_ffi_uniffi.openTunneledWebsocket
import uniffi.lakko_ffi_uniffi.parseSfuServerFrame
import uniffi.lakko_ffi_uniffi.TunneledWsConnection

/** The WebSocket subprotocol the SFU requires on the upgrade. */
const val SFU_SUBPROTOCOL: String = "lakko-sfu-v1"

/**
 * Handlers the caller wires in. Every method has a no-op default, so a
 * caller overrides only the frames it cares about. Dispatched from the
 * receive-loop coroutine (the caller hops to the main thread itself if
 * it touches UI state).
 */
interface SfuSignalingCallbacks {
    /** First frame after admission — seeds the roster. */
    fun onWelcome(selfPeerId: String, callId: String, roster: List<SfuPeerSnapshot>) {}

    /** A participant joined the same call. `peerTag` is the ADR-0021
     *  per-call tag (empty from a pre-ADR-0021 SFU). */
    fun onPeerJoined(peerId: String, accountPseudonym: String, peerTag: String) {}

    /** A participant left. */
    fun onPeerLeft(peerId: String) {}

    /** Opaque payload relayed from another peer (base64url). */
    fun onSignalRelay(from: String, payloadB64: String) {}

    /** SFU's answer to our publish offer. */
    fun onSdpAnswer(sdp: String) {}

    /** SFU-initiated offer (a new downstream subscription). */
    fun onSdpOffer(sdp: String) {}

    /** Application-layer pong reply. */
    fun onPong(nonce: ULong) {}

    /** What the SFU gives this device of each publisher's video (ADR-0070
     *  step 6): the whole set, one entry per publisher it sends video of. */
    fun onVideoGiven(given: List<SfuVideoGiven>) {}

    /** Which of this device's own camera senders somebody is being sent
     *  (ADR-0070, "senders nobody watches"): the whole state each time. */
    fun onVideoNeeded(main: Boolean, middle: Boolean) {}

    /** The ceiling of this device's main and middle camera sender, kbit/s
     *  (ADR-0068, the publish budget): the whole state each time, the last
     *  frame wins. As the shared parser read them; `PublishBudget.kt` checks
     *  the bounds again. */
    fun onPublishBudget(mainKbps: Long, middleKbps: Long) {}

    /** Server reported a protocol-level error. */
    fun onServerError(code: SfuErrorCode, detail: String) {}

    /** A frame the shared validator / parser rejected. The connection
     *  is NOT closed automatically. */
    fun onParseError(raw: String, error: Throwable) {}

    /** The receive loop ended (socket closed, clean or not). */
    fun onClosed() {}
}

/**
 * SFU WebSocket signaling client (SFU multi-party arc, native half) —
 * the Swift mirror lives at
 * `apps/ios/Lakko/Calls/SfuSignalingClient.swift`, and both mirror the
 * web `apps/web/src/calls/sfu-signaling-client.ts`.
 *
 * Connects to the Rust SFU's `lakko-sfu-v1` WebSocket
 * (`{baseUrl}/v1/connect?token=<jwt>`), parses the typed server frames
 * through the shared `lakko_sfu_proto` definition (via the UniFFI
 * `parseSfuServerFrame`), and sends the typed client frames via the
 * `composeSfu*` helpers — so the client and `services/sfu` agree on the
 * wire shape from one definition.
 *
 * Transport + framing only — it does not touch the `PeerConnection`.
 * The CallSession SFU-mode wiring (route the local SDP offer through
 * [sendSdpOffer], apply the answer, trickle ICE via [sendIceCandidate],
 * drive SFU-initiated renegotiation from [SfuSignalingCallbacks.onSdpOffer])
 * is the next step on top of this client.
 *
 * One instance per call. Construct with the SFU base URL (an http(s) or
 * ws(s) origin, no path) + the call-admission JWT (from
 * [CallJwtClient]) + a [CoroutineScope] the receive loop rides, then
 * [connect]. Frames arrive on the wired callbacks; send with the
 * `send*` methods. Tear down with [close] (idempotent).
 */
class SfuSignalingClient(
    private val baseUrl: String,
    private val jwt: String,
    private val scope: CoroutineScope,
    private val callbacks: SfuSignalingCallbacks,
) {
    private val engine = me.lakko.app.transport.BaseHttpEngine.create()
    private val client = HttpClient(engine) { install(WebSockets) }
    private var session: DefaultClientWebSocketSession? = null
    private var receiveJob: Job? = null

    // ADR-0026 — when anti-censorship is armed, the signaling WebSocket rides the
    // obfuscated tunnel via the Rust FFI instead of Ktor/OkHttp. The FFI `recv()`
    // blocks, so the receive loop runs on `Dispatchers.IO`; outbound frames go
    // through a single channel + sender coroutine so order is preserved (mirror
    // of the direct path's `session.outgoing`).
    @Volatile
    private var tunneledConn: TunneledWsConnection? = null
    private var tunnelSendJob: Job? = null
    private val tunnelOutbound = Channel<String>(Channel.UNLIMITED)

    /**
     * Set by [close]. A socket that finishes opening after it is shut again
     * at once and never becomes this client's session: [connect] suspends on
     * the upgrade (and the tunneled dial blocks), and the call may be left
     * meanwhile. Without it the late socket stayed open with nobody to close
     * it, and the SFU went on counting this device as a peer of the call.
     */
    @Volatile
    private var closed = false

    /** True once the WebSocket session (direct or tunneled) is open. */
    val isOpen: Boolean
        get() = session != null || tunneledConn != null

    /**
     * Open the WebSocket and start dispatching frames. Suspends until
     * the session is established (the SFU's Welcome frame then follows
     * on [SfuSignalingCallbacks.onWelcome]); throws if the upgrade
     * fails, and if [close] ran before the upgrade finished (the socket is
     * then closed again and nothing is dispatched). Idempotent-guarded:
     * throws if already connected.
     */
    suspend fun connect() {
        check(session == null && tunneledConn == null) { "connect() called twice" }
        check(!closed) { CLOSED_WHILE_CONNECTING }
        // ADR-0026 — armed: route the signaling WebSocket through the tunnel.
        val antiCensorship = AntiCensorshipConfig.current
        if (antiCensorship.armed) {
            connectTunneled(antiCensorship)
            return
        }
        // http(s) -> ws(s); the SFU upgrade endpoint speaks WebSocket.
        val wsBase = baseUrl
            .replaceFirst("http://", "ws://")
            .replaceFirst("https://", "wss://")
            .trimEnd('/')
        val wsUrl = "$wsBase/v1/connect?token=$jwt"
        val s = client.webSocketSession(urlString = wsUrl) {
            // The SFU refuses upgrades without this subprotocol header.
            header("Sec-WebSocket-Protocol", SFU_SUBPROTOCOL)
        }
        if (closed) {
            // Closed while the upgrade was in flight: this socket belongs to
            // no call. Shut it and say so, rather than keep it as the session.
            discard(s)
            throw IllegalStateException(CLOSED_WHILE_CONNECTING)
        }
        session = s
        receiveJob = scope.launch { receiveLoop(s) }
    }

    /** Close a socket that opened after [close]. Best effort, off the caller. */
    private fun discard(s: DefaultClientWebSocketSession) {
        scope.launch {
            try {
                s.close()
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }

    private suspend fun receiveLoop(s: DefaultClientWebSocketSession) {
        try {
            for (frame in s.incoming) {
                val text = when (frame) {
                    is Frame.Text -> frame.readText()
                    // Binary frames are not part of lakko-sfu-v1 — ignore.
                    else -> continue
                }
                handleFrameText(text)
            }
        } catch (e: CancellationException) {
            throw e
        } catch (_: Throwable) {
            // Transport-level failure — fall through to onClosed.
        } finally {
            callbacks.onClosed()
        }
    }

    /**
     * Parse + dispatch one signaling frame (shared by the direct + tunneled
     * receive loops). A parse failure surfaces via [SfuSignalingCallbacks.onParseError]
     * and never closes the connection.
     */
    private fun handleFrameText(text: String) {
        val parsed = try {
            parseSfuServerFrame(text)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            callbacks.onParseError(text, e)
            return
        }
        dispatch(parsed)
    }

    /**
     * ADR-0026 — open the signaling WebSocket over the obfuscated tunnel and run
     * a blocking receive loop on [Dispatchers.IO] (the FFI `recv()` blocks). The
     * dial + handshake also block, so they run on the IO dispatcher; a failure
     * surfaces via [SfuSignalingCallbacks.onClosed] (the same teardown the direct
     * path uses). Outbound frames ride [tunnelOutbound] -> a single sender
     * coroutine so order is preserved.
     */
    private fun connectTunneled(config: AntiCensorshipConfig) {
        receiveJob = scope.launch(Dispatchers.IO) {
            val target = tunneledSignalingTarget(baseUrl, jwt)
            if (target == null) {
                callbacks.onClosed()
                return@launch
            }
            val host = target.host
            val port = target.port
            val innerUrl = target.innerUrl
            val nowSecs = (System.currentTimeMillis() / 1000L).toULong()

            val conn = try {
                openTunneledWebsocket(
                    config.bootstrapConfigJson,
                    config.trustAnchor,
                    nowSecs,
                    host,
                    port,
                    config.deadlineMs,
                    innerUrl,
                    SFU_SUBPROTOCOL,
                )
            } catch (e: Throwable) {
                callbacks.onClosed()
                return@launch
            }
            if (closed) {
                // The dial blocks and cannot be cancelled; the client was
                // closed while it ran. Shut the tunnel rather than keep it.
                runCatching { conn.shutdown() }
                return@launch
            }
            tunneledConn = conn
            if (closed) {
                // [close] ran between the check and the assignment (it is on
                // another thread): it saw no connection to shut.
                tunneledConn = null
                runCatching { conn.shutdown() }
                return@launch
            }

            // Ordered outbound sender on its own IO coroutine.
            tunnelSendJob = scope.launch(Dispatchers.IO) {
                try {
                    for (json in tunnelOutbound) {
                        runCatching { conn.sendText(json) }
                    }
                } catch (_: Throwable) {
                    // sender torn down — receive loop owns onClosed
                }
            }

            // Blocking receive loop.
            try {
                while (true) {
                    val message = try {
                        conn.recv()
                    } catch (_: Throwable) {
                        break
                    }
                    when (message) {
                        is FfiWsMessage.Text -> handleFrameText(message.text)
                        is FfiWsMessage.Binary ->
                            handleFrameText(String(message.data, Charsets.UTF_8))
                        is FfiWsMessage.Closed -> break
                    }
                }
            } finally {
                callbacks.onClosed()
            }
        }
    }

    private fun dispatch(frame: SfuServerFrame) {
        when (frame) {
            is SfuServerFrame.Welcome ->
                callbacks.onWelcome(frame.peerId, frame.callId, frame.roster)
            is SfuServerFrame.PeerJoined ->
                callbacks.onPeerJoined(frame.peerId, frame.accountPseudonym, frame.peerTag)
            is SfuServerFrame.PeerLeft -> callbacks.onPeerLeft(frame.peerId)
            is SfuServerFrame.SignalRelay ->
                callbacks.onSignalRelay(frame.from, frame.payloadB64)
            is SfuServerFrame.SdpAnswer -> callbacks.onSdpAnswer(frame.sdp)
            is SfuServerFrame.SdpOffer -> callbacks.onSdpOffer(frame.sdp)
            is SfuServerFrame.Pong -> callbacks.onPong(frame.nonce)
            is SfuServerFrame.Error -> callbacks.onServerError(frame.code, frame.detail)
            is SfuServerFrame.VideoGiven -> callbacks.onVideoGiven(frame.given)
            is SfuServerFrame.VideoNeeded -> callbacks.onVideoNeeded(frame.main, frame.middle)
            is SfuServerFrame.PublishBudget ->
                callbacks.onPublishBudget(frame.mainKbps.toLong(), frame.middleKbps.toLong())
        }
    }

    // ---- Send (client -> SFU) ---------------------------------------

    /**
     * Send a client -> SFU SDP offer (publish path). [lightMids] and
     * [middleMids] name the offer's media lines that carry the light and the
     * middle copy of the camera (ADR-0070 steps 1 and 5), so the SFU forwards
     * them as the camera's light companion and its middle track.
     */
    fun sendSdpOffer(sdp: String, lightMids: List<String>, middleMids: List<String>) =
        enqueue(composeSfuSdpOfferWithMiddle(sdp, lightMids, middleMids))

    /**
     * Tell the SFU which picture of each publisher this device shows
     * (ADR-0070 step 5): the whole set each time. `false` when the frame
     * could not be composed or the socket is not open.
     */
    fun sendVideoWants(wants: List<SfuVideoWant>): Boolean {
        if (!isOpen) return false
        val json = try {
            composeSfuVideoWants(wants)
        } catch (e: CancellationException) {
            throw e
        } catch (_: Throwable) {
            return false
        }
        enqueue(json)
        return true
    }

    /** Send a client -> SFU SDP answer (responds to a SFU-initiated
     *  offer). */
    fun sendSdpAnswer(sdp: String) = enqueue(composeSfuSdpAnswer(sdp))

    /** Send a trickle-ICE candidate (the `candidate` line value). */
    fun sendIceCandidate(candidateSdp: String) =
        enqueue(composeSfuIceCandidate(candidateSdp))

    /** Relay an opaque payload to a single peer. */
    fun sendSignalRelayToPeer(peerId: String, payload: ByteArray) =
        enqueue(composeSfuSignalRelayToPeer(peerId, payload))

    /** Relay an opaque payload to every other peer in the call. */
    fun sendSignalRelayToCall(payload: ByteArray) =
        enqueue(composeSfuSignalRelayToCall(payload))

    /** Application-layer keepalive; the SFU echoes a Pong. */
    fun sendPing(nonce: ULong) = enqueue(composeSfuPing(nonce))

    /** Enqueue a composed wire string onto the session's outbound
     *  channel. Non-suspending (`trySend`) so call sites — ICE
     *  callbacks, UI handlers — stay synchronous; ordering is preserved
     *  because a single channel serialises the writes. A full channel
     *  drops the frame (back-pressure), mirroring the web's behaviour. */
    private fun enqueue(json: String) {
        // Armed: ordered send over the tunnel (the sender coroutine drains this).
        if (tunneledConn != null) {
            tunnelOutbound.trySend(json)
            return
        }
        session?.outgoing?.trySend(Frame.Text(json))
    }

    /** Close the socket + cancel the receive loop. Idempotent. Closing the
     *  tunneled connection unblocks its receive loop (`recv()` returns Closed). */
    fun close() {
        closed = true
        receiveJob?.cancel()
        receiveJob = null
        tunnelSendJob?.cancel()
        tunnelSendJob = null
        val s = session
        session = null
        if (s != null) {
            scope.launch {
                try {
                    s.close()
                } catch (_: Throwable) {
                    // best-effort
                }
            }
        }
        val conn = tunneledConn
        tunneledConn = null
        if (conn != null) {
            scope.launch(Dispatchers.IO) {
                runCatching { conn.shutdown() }
            }
        }
        client.close()
        // The client was handed an engine instance, which it does not own.
        runCatching { engine.close() }
    }
}

private const val CLOSED_WHILE_CONNECTING = "the signaling client was closed"

/** Where the tunneled signaling socket dials and which request it sends. */
internal data class TunneledSignalingTarget(
    val host: String,
    val port: UShort,
    val innerUrl: String,
)

/**
 * The tunnel endpoint and the inner WebSocket URL for [baseUrl].
 *
 * The inner URL is plain `ws://` (the obfuscated tunnel provides the
 * encryption) and keeps the base URL's path: an SFU served under a prefix
 * (`wss://test.lakko.me/sfu`) is reached at `/sfu/v1/connect`, exactly like the
 * direct path. `null` when the base URL does not parse or has no host.
 */
internal fun tunneledSignalingTarget(baseUrl: String, jwt: String): TunneledSignalingTarget? {
    val uri = try {
        java.net.URI(baseUrl)
    } catch (_: Exception) {
        return null
    }
    val host = uri.host?.takeIf { it.isNotEmpty() } ?: return null
    val isSecure = baseUrl.startsWith("https") || baseUrl.startsWith("wss")
    val port = when {
        uri.port in 1..65535 -> uri.port
        isSecure -> 443
        else -> 80
    }.toUShort()
    val path = (uri.rawPath ?: "").trimEnd('/')
    return TunneledSignalingTarget(host, port, "ws://$host$path/v1/connect?token=$jwt")
}
