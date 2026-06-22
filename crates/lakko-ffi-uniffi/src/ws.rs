// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for a WebSocket over an obfuscated tunnel (ADR-0026).
//!
//! The native SFU signaling channel is a long-lived bidirectional WebSocket, so
//! it cannot use the request/response [`crate::transport::tunneled_http_request`]
//! path. [`open_tunneled_websocket`] dials the best obfuscated transport, runs an
//! RFC 6455 handshake over it ([`lakko_transport::tunneled_websocket`]), and
//! returns a [`TunneledWsConnection`] the native side drives with `send_text` /
//! `send_binary` / `recv` / `close`.
//!
//! ## Runtime
//!
//! Unlike the one-shot HTTP path, a WebSocket is long-lived, so it cannot ride a
//! per-call current-thread runtime. A process-wide **multi-thread** tokio runtime
//! ([`ws_runtime`]) owns the connection: the `ByteStreamConnection` pump and the
//! WS codec run on its worker threads while a synchronous `recv()` blocks the
//! caller's (native, off-main) thread. The sink + stream are split behind
//! `std::sync::Mutex`es so the native side can call `send_*` and `recv`
//! independently.
//!
//! Fail-closed throughout (invariant #4): a bad config / failed handshake is a
//! hard error; there is no direct-WS fallback in this path (the caller chooses
//! whether anti-censorship is required).

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use lakko_crypto::signing::VerifyingKey;
use lakko_transport::{
    tunneled_websocket, BootstrapConfig, BootstrapDialer, Connection, Endpoint, TunneledWebSocket,
};
use tokio::runtime::Runtime;
use tokio_tungstenite::tungstenite::Message;

use crate::FfiError;

/// Process-wide multi-thread runtime that owns every tunneled WebSocket. Built
/// lazily on first use; lives for the process so the connection's pump + codec
/// keep running between FFI calls.
static WS_RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn ws_runtime() -> Result<&'static Runtime, FfiError> {
    if let Some(rt) = WS_RUNTIME.get() {
        return Ok(rt);
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| FfiError::Transport(format!("ws runtime: {e}")))?;
    // If another thread won the race, its runtime is kept and ours is dropped.
    let _ = WS_RUNTIME.set(rt);
    WS_RUNTIME
        .get()
        .ok_or_else(|| FfiError::Transport("ws runtime init".into()))
}

/// One inbound WebSocket message surfaced to the native side.
#[derive(uniffi::Enum, Debug)]
pub enum FfiWsMessage {
    /// A UTF-8 text frame (the SFU signaling JSON).
    Text {
        /// The frame text.
        text: String,
    },
    /// A binary frame.
    Binary {
        /// The frame bytes.
        data: Vec<u8>,
    },
    /// The peer closed the connection (a Close frame or stream end).
    Closed,
}

/// A live WebSocket over an obfuscated tunnel. Held by the native side; drive it
/// with [`Self::send_text`] / [`Self::send_binary`] / [`Self::recv`] /
/// [`Self::close`]. Drop releases the connection (the runtime tears down the
/// pump).
#[derive(uniffi::Object)]
pub struct TunneledWsConnection {
    sink: Mutex<SplitSink<TunneledWebSocket, Message>>,
    stream: Mutex<SplitStream<TunneledWebSocket>>,
}

/// Build a [`TunneledWsConnection`] over an already-dialed framed connection.
/// Split out so a loopback test can exercise the object without the obfs dial.
pub(crate) fn open_over_connection(
    conn: Box<dyn Connection>,
    url: &str,
    subprotocol: Option<&str>,
) -> Result<Arc<TunneledWsConnection>, FfiError> {
    let rt = ws_runtime()?;
    let ws = rt
        .block_on(tunneled_websocket(conn, url, subprotocol))
        .map_err(|e| FfiError::Transport(format!("ws open: {e}")))?;
    let (sink, stream) = ws.split();
    Ok(Arc::new(TunneledWsConnection {
        sink: Mutex::new(sink),
        stream: Mutex::new(stream),
    }))
}

/// Dial the best obfuscated transport described by `config_json` and open a
/// WebSocket to `url` over it.
///
/// `trust_anchor` is the 32-byte Ed25519 verifying key the config is checked
/// against (build-time pinned). `endpoint_host` / `endpoint_port` name the dial
/// endpoint; `url` is the inner `ws://host/path` the origin expects (e.g. the
/// SFU's `/v1/connect`); `subprotocol`, when set, is sent as
/// `Sec-WebSocket-Protocol` (the SFU uses `lakko-sfu-v1`).
///
/// # Errors
///
/// [`FfiError::Transport`] for a bad anchor / config, a failed verification, a
/// fail-closed dial, or a failed handshake. No direct-WS fallback (invariant #4).
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn open_tunneled_websocket(
    config_json: Vec<u8>,
    trust_anchor: Vec<u8>,
    now_secs: u64,
    endpoint_host: String,
    endpoint_port: u16,
    deadline_ms: u64,
    url: String,
    subprotocol: Option<String>,
) -> Result<Arc<TunneledWsConnection>, FfiError> {
    let anchor = VerifyingKey::from_bytes(&trust_anchor)
        .map_err(|e| FfiError::Transport(format!("bad trust anchor: {e}")))?;
    let config: BootstrapConfig = serde_json::from_slice(&config_json)
        .map_err(|e| FfiError::Transport(format!("bad bootstrap config json: {e}")))?;
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(now_secs);
    let dialer = BootstrapDialer::from_signed_config(&config, &anchor, now)
        .map_err(|e| FfiError::Transport(format!("dialer build/verify: {e}")))?;

    let endpoint = Endpoint::Hostname {
        host: endpoint_host,
        port: endpoint_port,
    };
    let rt = ws_runtime()?;
    let (_kind, conn) = rt
        .block_on(dialer.dial(&endpoint, Duration::from_millis(deadline_ms)))
        .map_err(|e| FfiError::Transport(format!("ws dial: {e}")))?;

    open_over_connection(conn, &url, subprotocol.as_deref())
}

#[uniffi::export]
impl TunneledWsConnection {
    /// Send a text frame over the tunnel.
    pub fn send_text(&self, text: String) -> Result<(), FfiError> {
        let rt = ws_runtime()?;
        let mut sink = self.sink.lock().map_err(|_| FfiError::LockPoisoned)?;
        rt.block_on(sink.send(Message::Text(text)))
            .map_err(|e| FfiError::Transport(format!("ws send: {e}")))
    }

    /// Send a binary frame over the tunnel.
    pub fn send_binary(&self, data: Vec<u8>) -> Result<(), FfiError> {
        let rt = ws_runtime()?;
        let mut sink = self.sink.lock().map_err(|_| FfiError::LockPoisoned)?;
        rt.block_on(sink.send(Message::Binary(data)))
            .map_err(|e| FfiError::Transport(format!("ws send: {e}")))
    }

    /// Block until the next inbound data frame (or a close). Control frames
    /// (ping / pong) are handled by the codec and skipped. The native side calls
    /// this in a loop on a background thread.
    pub fn recv(&self) -> Result<FfiWsMessage, FfiError> {
        let rt = ws_runtime()?;
        let mut stream = self.stream.lock().map_err(|_| FfiError::LockPoisoned)?;
        loop {
            match rt.block_on(stream.next()) {
                None | Some(Ok(Message::Close(_))) => return Ok(FfiWsMessage::Closed),
                Some(Ok(Message::Text(text))) => return Ok(FfiWsMessage::Text { text }),
                Some(Ok(Message::Binary(data))) => return Ok(FfiWsMessage::Binary { data }),
                // Ping / Pong / raw Frame — handled by the codec; wait for data.
                Some(Ok(_)) => {},
                Some(Err(e)) => return Err(FfiError::Transport(format!("ws recv: {e}"))),
            }
        }
    }

    /// Cleanly shut the WebSocket down (sends a Close frame + tears the tunnel
    /// down). Named `shutdown` rather than `close` to avoid clashing with the
    /// UniFFI object-disposal `close()` the foreign bindings generate (dropping
    /// the object also closes the connection, but without the clean Close frame).
    pub fn shutdown(&self) -> Result<(), FfiError> {
        let rt = ws_runtime()?;
        let mut sink = self.sink.lock().map_err(|_| FfiError::LockPoisoned)?;
        rt.block_on(sink.close())
            .map_err(|e| FfiError::Transport(format!("ws close: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_transport::in_memory::pair;
    use lakko_transport::ByteStreamConnection;

    #[test]
    fn open_tunneled_websocket_rejects_wrong_length_trust_anchor() {
        // The anchor gate fails before any dial — exercises the fail-closed
        // front door with no network.
        let result = open_tunneled_websocket(
            b"{}".to_vec(),
            vec![0u8; 16],
            0,
            "lakko.invalid".into(),
            443,
            1000,
            "ws://lakko.invalid/v1/connect".into(),
            Some("lakko-sfu-v1".into()),
        );
        match result {
            Err(FfiError::Transport(msg)) => assert!(msg.contains("trust anchor")),
            Err(other) => panic!("expected Transport, got {other:?}"),
            Ok(_) => panic!("expected an error, got a connection"),
        }
    }

    #[test]
    #[allow(clippy::result_large_err)] // the echo callback's Err type is fixed by tungstenite's API
    fn ws_object_round_trips_over_a_loopback_connection() {
        let rt = ws_runtime().unwrap();
        let (client_conn, server_conn) = pair();

        // Server WS echo over the peer half of the framed pair.
        rt.spawn(async move {
            use tokio_tungstenite::tungstenite::handshake::server::{
                Request as WsRequest, Response as WsResponse,
            };
            let server_stream = ByteStreamConnection::new(server_conn);
            let echo_proto = |req: &WsRequest, mut resp: WsResponse| {
                if let Some(proto) = req.headers().get("sec-websocket-protocol") {
                    resp.headers_mut()
                        .insert("sec-websocket-protocol", proto.clone());
                }
                Ok(resp)
            };
            let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(server_stream, echo_proto).await
            else {
                return;
            };
            while let Some(Ok(msg)) = ws.next().await {
                if msg.is_close() {
                    break;
                }
                if (msg.is_text() || msg.is_binary()) && ws.send(msg).await.is_err() {
                    break;
                }
            }
        });

        // Client drives the real FFI object over the other half.
        let conn = open_over_connection(
            client_conn,
            "ws://lakko.invalid/v1/connect",
            Some("lakko-sfu-v1"),
        )
        .expect("ws handshake over the loopback connection");

        conn.send_text("hello sfu".into()).expect("send text");
        match conn.recv().expect("recv text") {
            FfiWsMessage::Text { text } => assert_eq!(text, "hello sfu"),
            other => panic!("expected text, got {other:?}"),
        }

        conn.send_binary(vec![1, 2, 3, 4]).expect("send binary");
        match conn.recv().expect("recv binary") {
            FfiWsMessage::Binary { data } => assert_eq!(data, vec![1, 2, 3, 4]),
            other => panic!("expected binary, got {other:?}"),
        }

        conn.shutdown().expect("shutdown");
    }
}
