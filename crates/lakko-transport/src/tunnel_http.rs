// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Minimal HTTP/1.1 over an obfuscated tunnel (ADR-0026, Phase 2).
//!
//! The censorship-resistant transports give a framed [`Connection`]
//! (`send(frame)` / `recv() -> frame`); the obfs proxy (`services/obfs-proxy`)
//! byte-relays it to the internal HTTP services. This module runs a Lakko REST
//! call over that tunnel: it encodes one HTTP/1.1 request, sends it as a frame,
//! accumulates the response frames until the server closes (the request asks
//! for `Connection: close`), and parses the result.
//!
//! It is deliberately tiny — request/response only, `Connection: close`,
//! `read-until-close` (no keep-alive, no chunked decoding). The inner HTTP is
//! plaintext because the obfs tunnel already encrypts + authenticates the
//! channel and the application payloads are MLS-E2EE. A streaming /
//! `AsyncRead`+`AsyncWrite` `ByteStreamConnection` (for WebSocket-over-tunnel)
//! is a later phase.

use bytes::BytesMut;

use crate::endpoint::Endpoint;
use crate::pluggable::PluggableKind;
use crate::selector::{BootstrapDialer, SelectorError};
use crate::transport::Connection;

/// Hard ceiling on an accumulated response. Bounds memory against a
/// misbehaving / hostile internal target (the proxy is trusted, but the bound
/// is cheap defence in depth).
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// A request to send over the tunnel.
pub struct TunneledRequest {
    /// HTTP method (`GET`, `POST`, …).
    pub method: String,
    /// Request target path (e.g. `/discovery/lookup`).
    pub path: String,
    /// `Host` header value.
    pub host: String,
    /// Extra request headers (name, value). `Host`, `Content-Length`, and
    /// `Connection` are set by the encoder and must not be duplicated here.
    pub headers: Vec<(String, String)>,
    /// Request body (empty for a bodyless GET).
    pub body: Vec<u8>,
}

/// A parsed response.
pub struct TunneledResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers (name, value), names lower-cased.
    pub headers: Vec<(String, String)>,
    /// Response body bytes.
    pub body: Vec<u8>,
}

impl TunneledResponse {
    /// First value of `name` (case-insensitive), if present.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        let lname = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(n, _)| *n == lname)
            .map(|(_, v)| v.as_str())
    }
}

/// Errors from a tunneled request.
#[derive(Debug, thiserror::Error)]
pub enum TunnelHttpError {
    /// The transport dial / selector failed.
    #[error("tunnel dial failed: {0}")]
    Dial(#[from] SelectorError),
    /// A send/recv on the tunnel failed before a full response arrived.
    #[error("tunnel transport error: {0}")]
    Transport(#[from] crate::error::Error),
    /// The accumulated response exceeded [`MAX_RESPONSE_BYTES`].
    #[error("response exceeded {MAX_RESPONSE_BYTES} bytes")]
    ResponseTooLarge,
    /// The response could not be parsed as HTTP/1.x.
    #[error("malformed HTTP response: {0}")]
    BadResponse(&'static str),
}

/// Encode `req` as HTTP/1.1 wire bytes with `Connection: close`.
fn encode_request(req: &TunneledRequest) -> Vec<u8> {
    let mut out = format!("{} {} HTTP/1.1\r\n", req.method, req.path).into_bytes();
    let mut push = |line: String| out.extend_from_slice(line.as_bytes());
    push(format!("Host: {}\r\n", req.host));
    for (name, value) in &req.headers {
        push(format!("{name}: {value}\r\n"));
    }
    push(format!("Content-Length: {}\r\n", req.body.len()));
    push("Connection: close\r\n\r\n".to_string());
    out.extend_from_slice(&req.body);
    out
}

/// Parse an accumulated HTTP/1.x response (status line + headers + body).
fn parse_response(buf: &[u8]) -> Result<TunneledResponse, TunnelHttpError> {
    // Split headers from body at the first CRLFCRLF.
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or(TunnelHttpError::BadResponse("no header terminator"))?;
    let head = &buf[..split];
    let body = buf[split + 4..].to_vec();

    let head_str =
        std::str::from_utf8(head).map_err(|_| TunnelHttpError::BadResponse("non-UTF-8 headers"))?;
    let mut lines = head_str.split("\r\n");

    // Status line: `HTTP/1.1 200 OK`.
    let status_line = lines
        .next()
        .ok_or(TunnelHttpError::BadResponse("empty response"))?;
    let mut parts = status_line.split(' ');
    let _version = parts
        .next()
        .ok_or(TunnelHttpError::BadResponse("no version"))?;
    let status: u16 = parts
        .next()
        .ok_or(TunnelHttpError::BadResponse("no status code"))?
        .parse()
        .map_err(|_| TunnelHttpError::BadResponse("status code not an integer"))?;

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }

    Ok(TunneledResponse {
        status,
        headers,
        body,
    })
}

/// Send `req` over an already-dialed tunnel `conn` and read the response.
///
/// The request carries `Connection: close`, so the response is read until the
/// internal server closes (propagated as a tunnel close); the accumulated
/// bytes are then parsed. Bounded by [`MAX_RESPONSE_BYTES`].
///
/// # Errors
///
/// [`TunnelHttpError::Transport`] on a tunnel send/recv failure before any
/// response, [`TunnelHttpError::ResponseTooLarge`] past the cap, or
/// [`TunnelHttpError::BadResponse`] on an unparseable response.
pub async fn tunneled_request(
    mut conn: Box<dyn Connection>,
    req: &TunneledRequest,
) -> Result<TunneledResponse, TunnelHttpError> {
    conn.send(bytes::Bytes::from(encode_request(req))).await?;

    let mut acc = BytesMut::new();
    // A clean close (or any transport end) terminates the response —
    // `Connection: close` means the server closes when done.
    while let Ok(frame) = conn.recv().await {
        if acc.len() + frame.len() > MAX_RESPONSE_BYTES {
            return Err(TunnelHttpError::ResponseTooLarge);
        }
        acc.extend_from_slice(&frame);
    }
    let _ = conn.close().await;
    parse_response(&acc)
}

/// Dial the best transport from `dialer` to `endpoint`, then send `req` over it.
/// Returns the winning transport kind alongside the response.
///
/// # Errors
///
/// [`TunnelHttpError::Dial`] if no transport connects (fail-closed — no direct
/// fallback), else the [`tunneled_request`] errors.
pub async fn tunneled_request_via_dialer(
    dialer: &BootstrapDialer,
    endpoint: &Endpoint,
    deadline: std::time::Duration,
    req: &TunneledRequest,
) -> Result<(PluggableKind, TunneledResponse), TunnelHttpError> {
    let (kind, conn) = dialer.dial(endpoint, deadline).await?;
    let resp = tunneled_request(conn, req).await?;
    Ok((kind, resp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory;
    use bytes::Bytes;

    #[test]
    fn encode_request_sets_host_length_and_close() {
        let req = TunneledRequest {
            method: "POST".into(),
            path: "/x".into(),
            host: "lakko.invalid".into(),
            headers: vec![("X-Test".into(), "1".into())],
            body: b"hi".to_vec(),
        };
        let wire = String::from_utf8(encode_request(&req)).unwrap();
        assert!(wire.starts_with("POST /x HTTP/1.1\r\n"));
        assert!(wire.contains("Host: lakko.invalid\r\n"));
        assert!(wire.contains("X-Test: 1\r\n"));
        assert!(wire.contains("Content-Length: 2\r\n"));
        assert!(wire.contains("Connection: close\r\n"));
        assert!(wire.ends_with("\r\n\r\nhi"));
    }

    #[test]
    fn parse_response_extracts_status_headers_body() {
        let raw = b"HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nX-A: b\r\n\r\n{\"ok\":true}";
        let resp = parse_response(raw).unwrap();
        assert_eq!(resp.status, 201);
        assert_eq!(resp.header("content-type"), Some("application/json"));
        assert_eq!(resp.header("X-A"), Some("b")); // case-insensitive lookup
        assert_eq!(resp.body, b"{\"ok\":true}");
    }

    #[test]
    fn parse_response_rejects_no_terminator() {
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nfoo").is_err());
    }

    #[tokio::test]
    async fn tunneled_request_round_trips_over_a_framed_pair() {
        // `client` runs tunneled_request; `server` mimics the relay+origin:
        // read the request frame, reply with a canned HTTP response, then close.
        let (client, mut server) = in_memory::pair();
        let server_task = tokio::spawn(async move {
            let _req = server.recv().await.unwrap();
            server
                .send(Bytes::from_static(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\npong",
                ))
                .await
                .unwrap();
            server.close().await.unwrap();
        });

        let req = TunneledRequest {
            method: "GET".into(),
            path: "/ping".into(),
            host: "lakko.invalid".into(),
            headers: vec![],
            body: vec![],
        };
        let resp = tunneled_request(client, &req).await.unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"pong");
        server_task.await.unwrap();
    }
}
