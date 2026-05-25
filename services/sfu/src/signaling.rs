// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! WebSocket signaling endpoint + admission logic.
//!
//! Wire shape (Lakko-native, not LiveKit's JSON-RPC):
//!
//! - Client connects to `wss://sfu.lakko.me/v1/connect` with
//!   `Sec-WebSocket-Protocol: lakko-sfu-v1` and a query
//!   parameter `token=<call-jwt>`.
//! - The SFU verifies the JWT against the JWKS cache. On
//!   failure → HTTP 401 / 403 before the upgrade completes.
//! - On success the connection is upgraded; the peer is
//!   registered in [`crate::peer::PeerRegistry`] and the
//!   skeleton enters its frame loop. Each frame is
//!   currently counted via the `lakko_sfu_signaling_frames_total`
//!   metric — payload routing arrives in `S-3-B-routing`.
//!
//! Errors at every step return short, neutral HTTP status
//! codes — the SFU is assumed-compromised by clients; we do
//! not leak diagnostic detail.

use std::sync::Arc;

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
use tracing::{debug, info, warn};

use crate::jwks::{JwksCache, JwksError};
use crate::peer::{Peer, PeerId, PeerRegistry};
use crate::prom::names;

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

    let peer = Peer {
        id: PeerId::fresh(),
        call_id: claims.call_id.clone(),
        account_id: claims.sub.clone(),
    };

    let acct_pseudo = pseudonymise(&peer.account_id);
    let call_id = peer.call_id.clone();
    let peer_id = peer.id.clone();
    info!(peer = %peer_id, call = %call_id, account_pseudo = %acct_pseudo, "ws: admission ok");

    ws.protocols(["lakko-sfu-v1"])
        .on_upgrade(move |socket| handle_socket(socket, peer, state))
}

/// Per-connection task — owns the socket for the lifetime of
/// the WebSocket. Increments the active-peer gauge on entry,
/// decrements on exit; counts every frame for the signaling
/// metric.
async fn handle_socket(socket: WebSocket, peer: Peer, state: Arc<AppState>) {
    if !state.peers.register(peer.clone()).await {
        warn!(peer = %peer.id, "ws: peer registry collision; closing");
        return;
    }
    metrics::counter!(names::PEERS_ADMITTED_TOTAL).increment(1);
    metrics::gauge!(names::PEERS_ACTIVE).increment(1.0);

    let (mut tx, mut rx) = socket.split();

    while let Some(frame) = rx.next().await {
        match frame {
            Ok(Message::Text(_) | Message::Binary(_)) => {
                metrics::counter!(names::SIGNALING_FRAMES_TOTAL).increment(1);
                // S-3-B-routing replaces this echo with real
                // signaling-message dispatch. For the
                // skeleton, we keep the connection alive but
                // do not forward anything.
            },
            Ok(Message::Ping(p)) => {
                if let Err(e) = tx.send(Message::Pong(p)).await {
                    debug!(peer = %peer.id, error = %e, "ws: pong send failed");
                    break;
                }
            },
            Ok(Message::Pong(_)) => {},
            Ok(Message::Close(_)) | Err(_) => break,
        }
    }

    state.peers.unregister(&peer.id).await;
    metrics::gauge!(names::PEERS_ACTIVE).decrement(1.0);
    info!(peer = %peer.id, "ws: peer disconnected");
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
}
