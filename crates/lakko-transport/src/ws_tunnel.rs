// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! WebSocket client over an obfuscated tunnel (ADR-0026).
//!
//! The SFU signaling channel (and any future long-lived bidirectional surface)
//! is a WebSocket, not a request/response HTTP call, so it cannot ride the
//! [`crate::tunnel_http`] path. [`tunneled_websocket`] runs a real RFC 6455
//! client handshake + frame codec ([`tokio_tungstenite`]) over a
//! [`ByteStreamConnection`], which itself sits on a framed [`Connection`] from
//! the [`crate::selector::BootstrapDialer`]. So the WebSocket bytes — handshake
//! and frames alike — travel inside the obfuscated, AEAD-protected tunnel; the
//! WS payload (the signaling JSON) stays opaque to this layer and to the
//! byte-relay proxy.
//!
//! No TLS is configured on the `tokio-tungstenite` side (the `handshake`-only
//! feature): the tunnel already authenticates + encrypts every byte, so a
//! `ws://` inner URL is correct (the encryption is the obfs layer, not an inner
//! TLS).

use thiserror::Error;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{client_async, WebSocketStream};

use crate::byte_stream::ByteStreamConnection;
use crate::transport::Connection;

/// A WebSocket stream running over an obfuscated tunnel. Use the
/// [`futures::SinkExt`] / [`futures::StreamExt`] APIs to send / receive
/// `tokio_tungstenite::tungstenite::Message`s.
pub type TunneledWebSocket = WebSocketStream<ByteStreamConnection>;

/// Failure opening a tunneled WebSocket.
#[derive(Debug, Error)]
pub enum WsTunnelError {
    /// The `url` / subprotocol could not be turned into a valid client request.
    #[error("invalid websocket request: {0}")]
    Request(String),
    /// The RFC 6455 handshake failed over the tunnel.
    #[error("websocket handshake failed: {0}")]
    Handshake(String),
}

/// Open a WebSocket to `url` over the framed `conn` (an obfuscated tunnel from
/// the dialer).
///
/// `url` is the inner `ws://host/path?query` the server expects (e.g. the SFU's
/// `ws://sfu.lakko.invalid/v1/connect?token=...`); the Host + path ride the
/// handshake through the byte-relay proxy to the internal WebSocket origin.
/// `subprotocol`, when set, is sent as `Sec-WebSocket-Protocol` (the SFU uses
/// `lakko-sfu-v1`).
///
/// # Errors
///
/// [`WsTunnelError::Request`] for a malformed URL / subprotocol;
/// [`WsTunnelError::Handshake`] if the RFC 6455 upgrade fails over the tunnel.
/// There is no fallback — a failed handshake is a hard error (invariant #4).
pub async fn tunneled_websocket(
    conn: Box<dyn Connection>,
    url: &str,
    subprotocol: Option<&str>,
) -> Result<TunneledWebSocket, WsTunnelError> {
    let mut request = url
        .into_client_request()
        .map_err(|e| WsTunnelError::Request(e.to_string()))?;
    if let Some(proto) = subprotocol {
        let value = HeaderValue::from_str(proto)
            .map_err(|e| WsTunnelError::Request(format!("bad subprotocol: {e}")))?;
        request
            .headers_mut()
            .insert("sec-websocket-protocol", value);
    }

    let stream = ByteStreamConnection::new(conn);
    let (ws, _response) = client_async(request, stream)
        .await
        .map_err(|e| WsTunnelError::Handshake(e.to_string()))?;
    Ok(ws)
}
