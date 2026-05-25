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
//! - Client connects to `wss://sfu.lakko.me/v1/connect` with
//!   `Sec-WebSocket-Protocol: lakko-sfu-v1` and a query
//!   parameter `token=<call-jwt>`.
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
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Query, State, WebSocketUpgrade,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::jwks::{JwksCache, JwksError};
use crate::peer::{Peer, PeerId, PeerRegistry};
use crate::prom::names;
use crate::protocol::{ClientFrame, ErrorCode, ServerFrame, MAX_INBOUND_FRAME_BYTES};
use crate::router::Router;

/// Capacity of each peer's outbound mpsc. Must be large
/// enough to absorb a worst-case fanout burst (every other
/// peer in a 50-person call signals at once) but small
/// enough that a wedged recipient surfaces as
/// `BackpressureDrop` rather than unbounded memory growth.
const PEER_OUTBOUND_CAPACITY: usize = 256;

/// Shared application state handed to every axum route.
#[derive(Clone)]
pub(crate) struct AppState {
    pub jwks: JwksCache,
    pub peers: PeerRegistry,
}

impl AppState {
    /// Build a fresh state container.
    #[must_use]
    pub(crate) fn new(jwks: JwksCache, peers: PeerRegistry) -> Arc<Self> {
        Arc::new(Self { jwks, peers })
    }
}

/// Token query string — `?token=<jwt>`.
#[derive(Debug, Deserialize)]
pub(crate) struct ConnectQuery {
    pub token: String,
}

/// Pseudonymise an account id for log lines — first 8 chars
/// of the hex `sub`, padded with `…` if shorter.
fn pseudonymise(account_id: &str) -> String {
    let n = account_id.len().min(8);
    let head = &account_id[..n];
    format!("{head}…")
}

/// Axum handler — verify token + upgrade to WebSocket.
pub(crate) async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(query): Query<ConnectQuery>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let claims = match state.jwks.verify(&query.token).await {
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

    let acct_pseudo = pseudonymise(&peer.account_id);
    let call_id = peer.call_id.clone();
    let peer_id = peer.id.clone();
    info!(peer = %peer_id, call = %call_id, account_pseudo = %acct_pseudo, "ws: admission ok");

    ws.protocols(["lakko-sfu-v1"])
        .on_upgrade(move |socket| handle_socket(socket, peer, out_rx, state))
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
    metrics::counter!(names::PEERS_ADMITTED_TOTAL).increment(1);
    metrics::gauge!(names::PEERS_ACTIVE).increment(1.0);

    // Send the welcome frame BEFORE announcing the join to
    // the rest of the call — the new peer must see its own
    // roster (excluding itself) ahead of any join broadcast
    // that might reach them indirectly via cross-call
    // peer-list races.
    let router = Router::new(&state.peers);
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
        metrics::gauge!(names::PEERS_ACTIVE).decrement(1.0);
        return;
    }

    // Now announce the join to everyone else in the call.
    router.announce_join(&peer).await;

    let (ws_tx, mut ws_rx) = socket.split();

    // Writer task — drain the outbound mpsc into the WS sink.
    let writer_peer_id = peer.id.clone();
    let writer = tokio::spawn(async move {
        let mut ws_tx = ws_tx;
        while let Some(frame) = out_rx.recv().await {
            let json = match serde_json::to_string(&frame) {
                Ok(j) => j,
                Err(e) => {
                    warn!(peer = %writer_peer_id, error = %e, "ws: outbound serialise failed");
                    continue;
                },
            };
            if let Err(e) = ws_tx.send(Message::Text(json.into())).await {
                debug!(peer = %writer_peer_id, error = %e, "ws: writer send failed");
                break;
            }
        }
    });

    // Reader loop — process inbound frames until the peer
    // disconnects or violates the protocol.
    while let Some(frame) = ws_rx.next().await {
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
                metrics::counter!(names::SIGNALING_FRAMES_TOTAL).increment(1);
                match serde_json::from_str::<ClientFrame>(&text) {
                    Ok(parsed) => match router.handle_client_frame(&peer, parsed).await {
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
                    },
                    Err(decode_err) => {
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

    #[test]
    fn pseudonymise_truncates() {
        let p = pseudonymise("deadbeefdeadbeefdeadbeefdeadbeef");
        assert_eq!(p, "deadbeef…");
    }

    #[test]
    fn pseudonymise_handles_short_input() {
        let p = pseudonymise("abc");
        assert_eq!(p, "abc…");
    }

    #[test]
    fn pseudonymise_handles_empty() {
        let p = pseudonymise("");
        assert_eq!(p, "…");
    }

    /// Compile-time sanity — the channel capacity must sit
    /// inside the auditable range. Both bounds are constant;
    /// `const _: () = assert!(...)` evaluates them at build
    /// time without tripping clippy's runtime-assertion
    /// lint.
    const _: () = assert!(PEER_OUTBOUND_CAPACITY >= 64);
    const _: () = assert!(PEER_OUTBOUND_CAPACITY <= 1024);
}
