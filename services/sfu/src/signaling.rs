// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! WebSocket signaling endpoint + admission logic.
//!
//! Wire shape (Lakko-native, not LiveKit's JSON-RPC) —
//! defined in [`crate::protocol`]. Each WebSocket text
//! frame is one `ClientFrame` JSON object inbound or one
//! `ServerFrame` outbound; binary frames are reserved for
//! a future RTP-over-WS fallback and are dropped here.
//!
//! - Client connects to `wss://sfu.lakko.me/v1/connect` offering two
//!   subprotocols in `Sec-WebSocket-Protocol`: `lakko-sfu-v1` and
//!   `lakko-sfu-token.<call-jwt>`. The token rides a header, never the
//!   URL, so no request line (and no access log, proxy log or crash
//!   report that records one) names who joined which call
//!   (invariant #5). The SFU answers with `lakko-sfu-v1` only.
//! - The SFU verifies the JWT against the JWKS cache. On
//!   failure → HTTP 401 / 403 before the upgrade completes.
//! - On success the connection is upgraded and the handler
//!   splits into two halves:
//!     * **reader half** — pulls inbound text frames,
//!       decodes them into `ClientFrame`, hands them to the
//!       router; the router fans them out into the call's
//!       per-peer mpsc senders.
//!     * **writer half** — drains the per-peer mpsc into the
//!       WS sink, one frame at a time.
//!
//! Errors at every step return short, neutral HTTP status
//! codes — the SFU is assumed-compromised by clients; we do
//! not leak diagnostic detail.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, State, WebSocketUpgrade,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::call_lifetime::{Admission, CallLifetimes, CallStatus};
use crate::config::Config;
use crate::jwks::{JwksCache, JwksError};
use crate::media::MediaTaskHandle;
use crate::peer::{Peer, PeerId, PeerRegistry};
use crate::prom::names;
use crate::protocol::{
    ClientFrame, ErrorCode, ServerFrame, MAX_INBOUND_FRAME_BYTES, SIGNALING_SUBPROTOCOL,
    TOKEN_SUBPROTOCOL_PREFIX,
};
use crate::router::Router;

/// Capacity of each peer's outbound mpsc. Must be large
/// enough to absorb a worst-case fanout burst (every other
/// peer in a 50-person call signals at once) but small
/// enough that a wedged recipient surfaces as
/// `BackpressureDrop` rather than unbounded memory growth.
const PEER_OUTBOUND_CAPACITY: usize = 256;

/// Shared application state handed to every axum route.
#[derive(Clone)]
pub struct AppState {
    /// JWKS cache the WS upgrade handler reads to verify
    /// the call-admission JWT.
    pub jwks: JwksCache,
    /// Live peer registry — every admitted WS connection
    /// inserts a [`Peer`] record here for the lifetime of
    /// the socket.
    pub peers: PeerRegistry,
    /// Optional handle to the media-routing task. `None`
    /// (when `LAKKO_SFU_MEDIA_BIND` is unset) runs the SFU
    /// as a signaling-only relay; `ClientFrame::SdpOffer`
    /// / `IceCandidate` frames surface as
    /// `RouterError::MediaNotWired`.
    pub media: Option<MediaTaskHandle>,
    /// How the signaling socket tells a live peer from one whose
    /// network went away without a close frame.
    pub liveness: SignalingLiveness,
    /// When each call is over, as this server sees it
    /// (`call_lifetime.rs`): fed by every admission and departure,
    /// read by the admission gate and the state route.
    pub calls: CallLifetimes,
}

/// Signaling-socket liveness: the SFU pings every peer on the
/// WebSocket protocol layer and drops one that has sent nothing
/// for the idle window.
///
/// Before this the only thing that ended a vanished peer's
/// registration was TCP giving up, which behind nginx's
/// hour-long `proxy_read_timeout` meant a phone that lost its
/// network kept its tile on every other participant's roster for
/// up to an hour, and the media task had long since pruned its
/// dead `Rtc`. Protocol pings need nothing from the clients:
/// URLSession, OkHttp, browsers and tungstenite all answer a
/// ping on their own, so a silent peer is one that is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalingLiveness {
    /// How often a protocol ping is sent to each peer.
    pub ping_interval: Duration,
    /// How long a peer may send nothing at all (a Pong counts)
    /// before it is dropped and announced as left. Exceeds
    /// `ping_interval`, enforced by [`Config::from_env`].
    pub idle_timeout: Duration,
}

impl From<&Config> for SignalingLiveness {
    fn from(cfg: &Config) -> Self {
        Self {
            ping_interval: cfg.signaling_ping_interval,
            idle_timeout: cfg.signaling_idle_timeout,
        }
    }
}

impl AppState {
    /// Build a fresh state container without the media
    /// half. Used by integration tests and the signaling-
    /// only deployment mode.
    #[must_use]
    pub fn new(jwks: JwksCache, peers: PeerRegistry, liveness: SignalingLiveness) -> Arc<Self> {
        Arc::new(Self {
            jwks,
            peers,
            media: None,
            liveness,
            calls: CallLifetimes::new(),
        })
    }

    /// Build a fresh state container with the media half
    /// wired. The handle is what `spawn_media_task`
    /// returned in `main.rs`.
    #[must_use]
    pub fn new_with_media(
        jwks: JwksCache,
        peers: PeerRegistry,
        media: MediaTaskHandle,
        liveness: SignalingLiveness,
    ) -> Arc<Self> {
        Arc::new(Self {
            jwks,
            peers,
            media: Some(media),
            liveness,
            calls: CallLifetimes::new(),
        })
    }
}

/// The call-admission JWT offered as a `lakko-sfu-token.<jwt>`
/// subprotocol in `Sec-WebSocket-Protocol` (every header line, each a
/// comma-separated list). `None` when no such entry is offered, when the
/// entry is empty, or when more than one is offered: an ambiguous
/// offer is refused rather than guessed at.
fn token_from_subprotocols(headers: &HeaderMap) -> Option<&str> {
    let mut found: Option<&str> = None;
    for value in headers.get_all(header::SEC_WEBSOCKET_PROTOCOL) {
        let Ok(list) = value.to_str() else {
            return None;
        };
        for entry in list.split(',').map(str::trim) {
            if let Some(token) = entry.strip_prefix(TOKEN_SUBPROTOCOL_PREFIX) {
                if token.is_empty() || found.is_some() {
                    return None;
                }
                found = Some(token);
            }
        }
    }
    found
}

/// The token of an `Authorization: Bearer <jwt>` header. The scheme is
/// matched case-insensitively (RFC 9110 section 11.1); exactly one
/// Authorization header is accepted.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    let value = value.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

/// Axum handler — verify token + upgrade to WebSocket.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Response {
    let Some(token) = token_from_subprotocols(&headers) else {
        warn!("ws: no admission token offered");
        metrics::counter!(names::PEERS_REJECTED_TOTAL).increment(1);
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    };
    let claims = match state.jwks.verify(token).await {
        Ok(c) => c,
        Err(JwksError::Verify(e)) => {
            warn!(error = %e, "ws: jwt verify rejected");
            metrics::counter!(names::PEERS_REJECTED_TOTAL).increment(1);
            return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
        },
        Err(JwksError::UnknownKid { kid }) => {
            warn!(%kid, "ws: jwt presented unknown kid");
            metrics::counter!(names::PEERS_REJECTED_TOTAL).increment(1);
            return (StatusCode::UNAUTHORIZED, "unknown kid").into_response();
        },
        Err(e) => {
            warn!(error = %e, "ws: jwks cache error during verify");
            metrics::counter!(names::PEERS_REJECTED_TOTAL).increment(1);
            return (StatusCode::SERVICE_UNAVAILABLE, "auth unavailable").into_response();
        },
    };

    let (out_tx, out_rx) = mpsc::channel::<ServerFrame>(PEER_OUTBOUND_CAPACITY);
    let peer = Peer {
        id: PeerId::fresh(),
        call_id: claims.call_id.clone(),
        account_id: claims.sub.clone(),
        outbound: Some(out_tx),
    };

    // Nothing derived from the account id is logged: the peer id is
    // fresh per socket, so a log line cannot tie a call to an account.
    info!(peer = %peer.id, call = %peer.call_id, "ws: admission ok");

    // Only the signaling subprotocol is ever selected, so the token
    // entry the client offered is never echoed back.
    ws.protocols([SIGNALING_SUBPROTOCOL])
        .on_upgrade(move |socket| handle_socket(socket, peer, out_rx, state))
}

/// `GET /v1/calls/{call_id}` with `Authorization: Bearer <jwt>` —
/// whether the call the token
/// names is still there (`call_lifetime.rs`). Gated by the same
/// call-admission JWT as the socket, and the token must name the call
/// asked about, so a member learns about a call it could join and
/// nothing else. A member whose chat shows a call in progress asks this
/// before offering Join, and closes the chat's record when the answer is
/// `ended`.
pub async fn call_state_handler(
    Path(call_id): Path<String>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Response {
    let Some(token) = bearer_token(&headers) else {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    };
    let claims = match state.jwks.verify(token).await {
        Ok(c) => c,
        Err(JwksError::Verify(_) | JwksError::UnknownKid { .. }) => {
            return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
        },
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "auth unavailable").into_response();
        },
    };
    if claims.call_id != call_id {
        return (StatusCode::FORBIDDEN, "token names another call").into_response();
    }
    let status = state.calls.status(&call_id, Instant::now());
    let peers = match status {
        CallStatus::Live { peers } => peers,
        CallStatus::Unknown | CallStatus::Closing | CallStatus::Ended => 0,
    };
    Json(serde_json::json!({ "state": status.as_str(), "peers": peers })).into_response()
}

/// Per-connection task — owns the socket for the lifetime
/// of the WebSocket. Registers the peer, broadcasts the
/// join, drives the reader / writer split, and on close
/// unregisters + announces the departure.
///
/// The function is intentionally a single linear procedure
/// rather than split into helpers — every step is part of
/// the per-connection state machine; splitting would
/// require passing the same five-tuple of locals through
/// each helper. The `too_many_lines` lint is silenced.
#[allow(clippy::too_many_lines)]
async fn handle_socket(
    socket: WebSocket,
    peer: Peer,
    mut out_rx: mpsc::Receiver<ServerFrame>,
    state: Arc<AppState>,
) {
    if !state.peers.register(peer.clone()).await {
        warn!(peer = %peer.id, "ws: peer registry collision; closing");
        return;
    }
    // A call that stood empty past its grace is over (`call_lifetime.rs`):
    // say so in place of the Welcome and close, so the member closes the
    // chat's record instead of founding a second call under the old id.
    if state.calls.admit(&peer.call_id, Instant::now()) == Admission::Ended {
        info!(peer = %peer.id, call = %peer.call_id, "ws: join refused, call ended");
        let refusal = ServerFrame::Error {
            code: ErrorCode::CallEnded,
            detail: "the call ended before you joined".to_owned(),
        };
        if let Err(e) = send_via_self(&peer, refusal) {
            warn!(peer = %peer.id, error = %e, "ws: refusal enqueue failed");
        }
        state.peers.unregister(&peer.id).await;
        drain_and_close(socket, out_rx).await;
        return;
    }
    metrics::counter!(names::PEERS_ADMITTED_TOTAL).increment(1);
    metrics::gauge!(names::PEERS_ACTIVE).increment(1.0);

    // Send the welcome frame BEFORE announcing the join to
    // the rest of the call — the new peer must see its own
    // roster (excluding itself) ahead of any join broadcast
    // that might reach them indirectly via cross-call
    // peer-list races.
    let router = {
        let r = Router::new(&state.peers);
        // Attach the media task only when configured —
        // when `state.media` is `None` SDP / ICE frames
        // surface as `RouterError::MediaNotWired` (the
        // signaling-only mode).
        match state.media.as_ref() {
            Some(m) => r.with_media(m),
            None => r,
        }
    };
    let server_ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map_or(0, |d| d.as_secs());
    let welcome = router.welcome_for(&peer, server_ts).await;
    // The mpsc receiver belongs to this task, so a try_send
    // on our own outbound is guaranteed not to block — but
    // we use the peer's tx via the registry, which is the
    // same channel. Push the welcome straight onto the rx
    // path so the writer half picks it up first.
    if let Err(e) = send_via_self(&peer, welcome) {
        warn!(peer = %peer.id, error = %e, "ws: welcome enqueue failed; closing");
        state.peers.unregister(&peer.id).await;
        state.calls.depart(&peer.call_id, Instant::now());
        metrics::gauge!(names::PEERS_ACTIVE).decrement(1.0);
        return;
    }

    // Now announce the join to everyone else in the call.
    router.announce_join(&peer).await;

    let (ws_tx, mut ws_rx) = socket.split();

    // Writer task — drain the outbound mpsc into the WS sink,
    // and ping the peer on the protocol layer every
    // `liveness.ping_interval` so a live peer is never silent
    // for the reader's idle window (its stack answers with a
    // Pong without any app-layer help).
    let writer_peer_id = peer.id.clone();
    let ping_interval = state.liveness.ping_interval;
    let writer = tokio::spawn(async move {
        let mut ws_tx = ws_tx;
        let mut pings = tokio::time::interval(ping_interval);
        pings.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick of an interval fires at once; a ping
        // straight after the welcome is pointless.
        pings.tick().await;
        loop {
            let message = tokio::select! {
                frame = out_rx.recv() => match frame {
                    Some(frame) => match serde_json::to_string(&frame) {
                        Ok(json) => Message::Text(json.into()),
                        Err(e) => {
                            warn!(peer = %writer_peer_id, error = %e, "ws: outbound serialise failed");
                            continue;
                        },
                    },
                    None => break,
                },
                _ = pings.tick() => Message::Ping(Vec::new().into()),
            };
            if let Err(e) = ws_tx.send(message).await {
                debug!(peer = %writer_peer_id, error = %e, "ws: writer send failed");
                break;
            }
        }
    });

    // Reader loop — process inbound frames until the peer
    // disconnects, violates the protocol, or falls silent for
    // the idle window. Any inbound frame restarts the window; a
    // live peer answers the writer's pings with a Pong, so only
    // a peer whose network is gone reaches the deadline.
    let idle_timeout = state.liveness.idle_timeout;
    loop {
        let frame = match tokio::time::timeout(idle_timeout, ws_rx.next()).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(_elapsed) => {
                warn!(
                    peer = %peer.id,
                    call = %peer.call_id,
                    idle_secs = idle_timeout.as_secs(),
                    "ws: peer silent past the idle window; dropping it and announcing the leave"
                );
                metrics::counter!(names::PEERS_DROPPED_IDLE_TOTAL).increment(1);
                break;
            },
        };
        match frame {
            Ok(Message::Text(text)) => {
                if text.len() > MAX_INBOUND_FRAME_BYTES {
                    let _ = send_via_self(
                        &peer,
                        ServerFrame::Error {
                            code: ErrorCode::FrameTooLarge,
                            detail: format!(
                                "frame size {} exceeds {MAX_INBOUND_FRAME_BYTES} bytes",
                                text.len()
                            ),
                        },
                    );
                    break;
                }
                match serde_json::from_str::<ClientFrame>(&text) {
                    Ok(parsed) => {
                        // Count the inbound frame by its typed kind. Captured
                        // before `parsed` is moved into the router.
                        metrics::counter!(
                            names::SIGNALING_FRAMES_TOTAL,
                            "direction" => "inbound",
                            "kind" => parsed.kind_str(),
                        )
                        .increment(1);
                        match router.handle_client_frame(&peer, parsed).await {
                            Ok(Some(reply)) => {
                                let _ = send_via_self(&peer, reply);
                            },
                            Ok(None) => {},
                            Err(e) => {
                                let _ = send_via_self(
                                    &peer,
                                    ServerFrame::Error {
                                        code: e.to_wire_code(),
                                        detail: e.to_string(),
                                    },
                                );
                            },
                        }
                    },
                    Err(decode_err) => {
                        // A frame that passed the size gate but failed to parse
                        // still counts as inbound traffic — bucket it as
                        // `invalid` so an undecodable-frame spike is visible.
                        metrics::counter!(
                            names::SIGNALING_FRAMES_TOTAL,
                            "direction" => "inbound",
                            "kind" => "invalid",
                        )
                        .increment(1);
                        let _ = send_via_self(
                            &peer,
                            ServerFrame::Error {
                                code: ErrorCode::BadFrame,
                                detail: decode_err.to_string(),
                            },
                        );
                    },
                }
            },
            Ok(Message::Binary(_)) => {
                // Reserved for a future RTP-over-WS fallback —
                // log + drop until the routing-media sprint
                // defines the shape.
                debug!(peer = %peer.id, "ws: binary frame dropped (no schema yet)");
            },
            Ok(Message::Ping(_)) => {
                // WS-protocol ping — axum auto-replies on
                // the WS layer; we hook in here only for the
                // debug log so a future rate-limiter has a
                // place to plug in.
                ack_ws_ping(&peer);
            },
            Ok(Message::Pong(_)) => {},
            Ok(Message::Close(_)) | Err(_) => break,
        }
    }

    // Cleanup: announce the departure, unregister, drop the
    // outbound sender so the writer task exits.
    router.announce_leave(&peer).await;
    state.peers.unregister(&peer.id).await;
    state.calls.depart(&peer.call_id, Instant::now());
    // The media half keeps its own per-peer state and learns of the
    // departure only from here; left alone, a peer whose ICE never
    // connected would stay in the router for the life of the process.
    if let Some(media) = state.media.as_ref() {
        if let Err(e) = media.remove_peer(peer.id.clone()) {
            warn!(peer = %peer.id, error = %e, "ws: could not ask the media task to drop the peer");
        }
    }
    metrics::gauge!(names::PEERS_ACTIVE).decrement(1.0);
    writer.abort();
    info!(peer = %peer.id, "ws: peer disconnected");
}

/// Push a frame to the peer's own outbound channel. The
/// signaling task uses this for control frames it generates
/// itself (welcome, errors, pongs). Returns an error if the
/// outbound channel is full or closed — the connection is
/// almost certainly about to close, so the caller breaks
/// out of the read loop.
/// Write whatever is queued for a socket that was refused (the one
/// `Error` frame), then close it. The writer half never started for
/// this socket, so the queue is drained here.
async fn drain_and_close(mut socket: WebSocket, mut out_rx: mpsc::Receiver<ServerFrame>) {
    while let Ok(frame) = out_rx.try_recv() {
        let Ok(json) = serde_json::to_string(&frame) else {
            break;
        };
        if socket.send(Message::Text(json.into())).await.is_err() {
            break;
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

fn send_via_self(peer: &Peer, frame: ServerFrame) -> Result<(), &'static str> {
    let Some(tx) = peer.outbound.as_ref() else {
        return Err("no outbound channel");
    };
    match tx.try_send(frame) {
        Ok(()) => Ok(()),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Err("outbound queue full"),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Err("outbound closed"),
    }
}

/// Acknowledge a WS-protocol ping. axum auto-replies to
/// pings on the WS layer; this is a debug hook so a future
/// rate-limiter has a place to plug in.
fn ack_ws_ping(peer: &Peer) {
    debug!(peer = %peer.id, "ws: protocol ping acknowledged");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_protocols(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for v in values {
            headers.append(
                header::SEC_WEBSOCKET_PROTOCOL,
                axum::http::HeaderValue::from_str(v).expect("header value"),
            );
        }
        headers
    }

    #[test]
    fn token_is_read_from_the_offered_subprotocols() {
        let h = with_protocols(&["lakko-sfu-v1, lakko-sfu-token.aaa.bbb.ccc"]);
        assert_eq!(token_from_subprotocols(&h), Some("aaa.bbb.ccc"));
        let h = with_protocols(&["lakko-sfu-v1,lakko-sfu-token.aaa.bbb.ccc"]);
        assert_eq!(token_from_subprotocols(&h), Some("aaa.bbb.ccc"));
        let h = with_protocols(&["lakko-sfu-v1", "lakko-sfu-token.x.y.z"]);
        assert_eq!(token_from_subprotocols(&h), Some("x.y.z"));
    }

    #[test]
    fn a_missing_empty_or_repeated_token_is_refused() {
        assert_eq!(token_from_subprotocols(&HeaderMap::new()), None);
        let h = with_protocols(&["lakko-sfu-v1"]);
        assert_eq!(token_from_subprotocols(&h), None);
        let h = with_protocols(&["lakko-sfu-v1, lakko-sfu-token."]);
        assert_eq!(token_from_subprotocols(&h), None);
        let h = with_protocols(&["lakko-sfu-token.a.b.c, lakko-sfu-token.d.e.f"]);
        assert_eq!(token_from_subprotocols(&h), None);
        let h = with_protocols(&["lakko-sfu-token.a.b.c", "lakko-sfu-token.a.b.c"]);
        assert_eq!(token_from_subprotocols(&h), None);
    }

    fn with_authorization(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for v in values {
            headers.append(
                header::AUTHORIZATION,
                axum::http::HeaderValue::from_str(v).expect("header value"),
            );
        }
        headers
    }

    #[test]
    fn bearer_token_is_read_from_authorization() {
        let h = with_authorization(&["Bearer a.b.c"]);
        assert_eq!(bearer_token(&h), Some("a.b.c"));
        let h = with_authorization(&["bearer a.b.c"]);
        assert_eq!(bearer_token(&h), Some("a.b.c"));
    }

    #[test]
    fn a_malformed_authorization_is_refused() {
        assert_eq!(bearer_token(&HeaderMap::new()), None);
        assert_eq!(bearer_token(&with_authorization(&["Basic a.b.c"])), None);
        assert_eq!(bearer_token(&with_authorization(&["Bearer"])), None);
        assert_eq!(bearer_token(&with_authorization(&["Bearer  "])), None);
        assert_eq!(
            bearer_token(&with_authorization(&["Bearer a.b.c", "Bearer d.e.f"])),
            None
        );
    }

    /// Compile-time sanity — the channel capacity must sit
    /// inside the auditable range. Both bounds are constant;
    /// `const _: () = assert!(...)` evaluates them at build
    /// time without tripping clippy's runtime-assertion
    /// lint.
    const _: () = assert!(PEER_OUTBOUND_CAPACITY >= 64);
    const _: () = assert!(PEER_OUTBOUND_CAPACITY <= 1024);
}
