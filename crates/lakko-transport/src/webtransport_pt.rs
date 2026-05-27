// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! WebTransport — Phase 5 G5-4 real implementation
//! per [ADR-0004](../../../docs/adr/0004-anti-censorship.md).
//!
//! Web-client first-class transport. The browser's native
//! WebTransport API rides over HTTP/3 (QUIC under-layer);
//! native targets (iOS / Android / desktop) reach the same
//! wire via the [`web-transport-quinn`](https://crates.io/crates/web-transport-quinn)
//! crate built on top of quinn (the same QUIC library the
//! rest of `lakko-transport` already depends on).
//!
//! ## Architecture
//!
//! - [`WebTransportConfig`] is the rendezvous-bootstrap-
//!   carried descriptor (URL + optional cert-hash pins).
//! - [`WebTransportPt`] is the `Transport` impl. Each
//!   `connect` opens a fresh WebTransport session and a
//!   single bidirectional stream, wrapped into a Lakko
//!   `Connection` over u32-BE length-prefixed framing.
//! - [`WebTransportConnection`] is the per-stream
//!   `Connection` impl. Frames are bounded by
//!   [`MAX_FRAME_BYTES`].
//!
//! ## Trust posture
//!
//! Two modes:
//!
//! - **Cert-hash pinning** (default): the bootstrap config
//!   carries SHA-256 hashes (base64-no-padded) of the
//!   server's full DER certificate. The client refuses any
//!   handshake whose presented cert hash is not in the
//!   set. This is the same trust shape the
//!   [W3C WebTransport spec](https://www.w3.org/TR/webtransport/)
//!   uses for `serverCertificateHashes` — the spec allows
//!   skipping the public CA chain entirely as long as the
//!   client knows the cert hash out-of-band, which the
//!   signed bootstrap rendezvous gives us.
//! - **System-roots fallback**: when `cert_hashes_b64` is
//!   empty, the client falls back to the platform CA chain.
//!   The pinning mode is preferred and recommended for
//!   production deployments.
//!
//! Note that this is **certificate-hash** pinning (SHA-256
//! of the full DER cert), not **SPKI** pinning (SHA-256 of
//! the SubjectPublicKeyInfo). The W3C WebTransport spec
//! defines this mode; do not confuse with the SPKI pinning
//! [`crate::pinning::PinnedVerifier`] uses for the HTTPS
//! REST clients.

use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD_NO_PAD as B64;
use base64::Engine as _;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use web_transport_quinn::{ClientBuilder, RecvStream, SendStream, Session};

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::transport::{Connection, Transport, MAX_FRAME_BYTES};

/// Length of the u32-BE frame length prefix on the wire.
const FRAME_LEN_PREFIX: usize = 4;

/// Length of a SHA-256 hash in bytes.
const CERT_HASH_LEN: usize = 32;

/// Bootstrap-config descriptor for a WebTransport endpoint.
///
/// Carried by the signed rendezvous bootstrap config — the
/// JSON field names are part of the protocol surface; a
/// rename breaks every previously-pinned config.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebTransportConfig {
    /// WebTransport URL (e.g. `https://rendezvous.lakko.me/wt`).
    /// Must be `https://` per the W3C spec.
    pub url: String,
    /// SHA-256 hashes of the server's full DER certificate,
    /// base64-encoded without padding (`STANDARD_NO_PAD`).
    /// Matches the W3C `serverCertificateHashes` mode. If
    /// empty, the client falls back to system CA roots —
    /// production deployments SHOULD populate this.
    pub cert_hashes_b64: Vec<String>,
}

/// Failure modes the WebTransport transport surfaces at
/// config-parse time. Connect-time failures fall through
/// to the existing [`Error`] surface.
#[derive(Debug, Error)]
pub enum WebTransportConfigError {
    /// URL was not parseable.
    #[error("URL parse failed: {0}")]
    BadUrl(String),
    /// URL scheme was not `https`.
    #[error("URL scheme must be https, got {got}")]
    NonHttpsScheme {
        /// The offending scheme.
        got: String,
    },
    /// One of the cert-hash entries did not base64-decode.
    #[error("cert_hashes_b64[{index}] base64 decode failed")]
    BadCertHashBase64 {
        /// Index into `cert_hashes_b64` that failed.
        index: usize,
    },
    /// One of the cert-hash entries decoded to a non-32-byte
    /// value. SHA-256 is 32 bytes; any other length is a
    /// publisher misconfiguration.
    #[error("cert_hashes_b64[{index}] is {got} bytes, expected {CERT_HASH_LEN}")]
    BadCertHashLength {
        /// Index into `cert_hashes_b64` that failed.
        index: usize,
        /// Number of bytes the decoded value carried.
        got: usize,
    },
    /// Client construction failed inside `web-transport-quinn`.
    #[error("client build failed: {0}")]
    ClientBuild(String),
}

impl WebTransportConfig {
    /// Parse the URL + decode the cert-hash pins. Validates
    /// every length / format constraint upfront so a bad
    /// bootstrap config surfaces at construction, not at
    /// connect time.
    ///
    /// # Errors
    ///
    /// See [`WebTransportConfigError`].
    pub fn parse(&self) -> std::result::Result<ParsedConfig, WebTransportConfigError> {
        let url =
            Url::parse(&self.url).map_err(|e| WebTransportConfigError::BadUrl(e.to_string()))?;
        if url.scheme() != "https" {
            return Err(WebTransportConfigError::NonHttpsScheme {
                got: url.scheme().to_string(),
            });
        }

        let mut hashes = Vec::with_capacity(self.cert_hashes_b64.len());
        for (index, h) in self.cert_hashes_b64.iter().enumerate() {
            let bytes = B64
                .decode(h.as_bytes())
                .map_err(|_| WebTransportConfigError::BadCertHashBase64 { index })?;
            if bytes.len() != CERT_HASH_LEN {
                return Err(WebTransportConfigError::BadCertHashLength {
                    index,
                    got: bytes.len(),
                });
            }
            let mut arr = [0u8; CERT_HASH_LEN];
            arr.copy_from_slice(&bytes);
            hashes.push(arr);
        }

        Ok(ParsedConfig { url, hashes })
    }
}

/// Typed config after `parse()`. Holds the validated URL
/// and the decoded cert hashes.
#[derive(Clone, Debug)]
pub struct ParsedConfig {
    /// Validated URL (scheme guaranteed `https`).
    pub url: Url,
    /// 32-byte SHA-256 cert-hash pins.
    pub hashes: Vec<[u8; CERT_HASH_LEN]>,
}

/// WebTransport `Transport` impl.
#[derive(Debug)]
pub struct WebTransportPt {
    parsed: ParsedConfig,
    cfg: WebTransportConfig,
}

impl WebTransportPt {
    /// Build the transport from a parsed config. Validates
    /// the config's URL + cert-hash pins upfront.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] (wrapping
    /// [`WebTransportConfigError`]) if the URL is not
    /// `https://` or a cert-hash pin is malformed.
    pub fn new(cfg: WebTransportConfig) -> Result<Self> {
        let parsed = cfg
            .parse()
            .map_err(|e| Error::Config(format!("webtransport config: {e}")))?;
        Ok(Self { parsed, cfg })
    }

    /// Borrow the parsed config — handy for tests + for
    /// observability surfaces.
    #[must_use]
    pub fn config(&self) -> &WebTransportConfig {
        &self.cfg
    }
}

#[async_trait]
impl Transport for WebTransportPt {
    async fn connect(&self, _endpoint: &Endpoint) -> Result<Box<dyn Connection>> {
        // The Lakko-side `endpoint` arg is ignored — the
        // canonical destination is in `cfg.url` which the
        // signed bootstrap rendezvous pinned. A future
        // sprint may take `endpoint` as a hint when the
        // selector races multiple candidates.
        let session = open_session(&self.parsed).await?;
        let (send, recv) = session
            .open_bi()
            .await
            .map_err(|e| Error::Io(format!("open_bi: {e}")))?;
        Ok(Box::new(WebTransportConnection {
            session,
            send,
            recv,
            closed: false,
        }))
    }
}

async fn open_session(parsed: &ParsedConfig) -> Result<Session> {
    let client = if parsed.hashes.is_empty() {
        // System-roots fallback — production deployments
        // SHOULD pin via `cert_hashes_b64`; this branch is
        // a convenience for development against a
        // well-known CA-signed origin.
        ClientBuilder::new()
            .with_system_roots()
            .map_err(|e| Error::Io(format!("with_system_roots: {e}")))?
    } else {
        // Cert-hash pinning — the W3C
        // `serverCertificateHashes` shape.
        let hashes: Vec<_> = parsed.hashes.iter().map(|h| h.to_vec()).collect();
        ClientBuilder::new()
            .with_server_certificate_hashes(hashes)
            .map_err(|e| Error::Io(format!("with_cert_hashes: {e}")))?
    };

    client
        .connect(parsed.url.clone())
        .await
        .map_err(|e| Error::Io(format!("connect: {e}")))
}

/// `Connection` impl over a single WebTransport
/// bidirectional stream. Frame layout is `u32-BE length
/// || payload`; the length prefix is rejected if it
/// exceeds [`MAX_FRAME_BYTES`].
struct WebTransportConnection {
    session: Session,
    send: SendStream,
    recv: RecvStream,
    closed: bool,
}

#[async_trait]
impl Connection for WebTransportConnection {
    async fn send(&mut self, frame: Bytes) -> Result<()> {
        if frame.len() > MAX_FRAME_BYTES {
            return Err(Error::FrameTooLarge {
                actual: frame.len(),
                limit: MAX_FRAME_BYTES,
            });
        }
        // u32 ≥ MAX_FRAME_BYTES (16 MiB) so the conversion
        // is lossless under the cap.
        let len = u32::try_from(frame.len()).expect("bounded by MAX_FRAME_BYTES check above");
        self.send
            .write_all(&len.to_be_bytes())
            .await
            .map_err(|e| Error::Io(format!("write_len: {e}")))?;
        self.send
            .write_all(&frame)
            .await
            .map_err(|e| Error::Io(format!("write_payload: {e}")))?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Bytes> {
        let mut len_buf = [0u8; FRAME_LEN_PREFIX];
        self.recv.read_exact(&mut len_buf).await.map_err(|e| {
            // Distinguish clean close from an I/O error so
            // the caller can branch.
            if is_clean_close(&e) {
                Error::PeerClosed
            } else {
                Error::Io(format!("read_len: {e}"))
            }
        })?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_BYTES {
            return Err(Error::FrameTooLarge {
                actual: len,
                limit: MAX_FRAME_BYTES,
            });
        }
        let mut buf = vec![0u8; len];
        self.recv
            .read_exact(&mut buf)
            .await
            .map_err(|e| Error::Io(format!("read_payload: {e}")))?;
        Ok(Bytes::from(buf))
    }

    async fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        // Finish flushes the send half cleanly. Errors
        // here are best-effort — the stream may already
        // be half-closed by the peer.
        let _ = self.send.finish();
        // Application-level close code 0 = clean close,
        // matching Lakko's `Error::PeerClosed` discriminant
        // on the receiving side.
        self.session.close(0u32, b"closed");
        self.closed = true;
        Ok(())
    }
}

/// Distinguish a clean WebTransport close from a
/// transport-layer I/O error so `recv()` can map to the
/// right Lakko variant.
///
/// `FinishedEarly` means the peer FIN'd the send half
/// before the requested byte count was filled — that is
/// the canonical "clean close" signal on a quinn /
/// WebTransport stream. Every other variant of
/// `ReadExactError` indicates a real I/O / session
/// failure.
fn is_clean_close(err: &web_transport_quinn::ReadExactError) -> bool {
    use web_transport_quinn::ReadExactError;
    matches!(err, ReadExactError::FinishedEarly(_))
}

/// Wraps an arc-cloneable handle for the rare case a
/// caller wants to share the `ClientBuilder`-derived
/// client across multiple connects (future optimisation).
#[allow(dead_code)]
struct SharedClient(Arc<()>);

#[cfg(test)]
mod tests {
    use super::*;

    fn happy_config() -> WebTransportConfig {
        WebTransportConfig {
            url: "https://lakko.me/wt".into(),
            cert_hashes_b64: vec![B64.encode([0u8; 32])],
        }
    }

    #[test]
    fn parse_happy_path() {
        let cfg = happy_config();
        let parsed = cfg.parse().expect("happy parse");
        assert_eq!(parsed.url.scheme(), "https");
        assert_eq!(parsed.hashes.len(), 1);
        assert_eq!(parsed.hashes[0], [0u8; 32]);
    }

    #[test]
    fn parse_rejects_non_https() {
        let mut cfg = happy_config();
        cfg.url = "ws://example.com/wt".into();
        match cfg.parse() {
            Err(WebTransportConfigError::NonHttpsScheme { got }) => assert_eq!(got, "ws"),
            other => panic!("expected NonHttpsScheme, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_bad_url() {
        let mut cfg = happy_config();
        cfg.url = "not a url".into();
        assert!(matches!(
            cfg.parse(),
            Err(WebTransportConfigError::BadUrl(_))
        ));
    }

    #[test]
    fn parse_rejects_bad_cert_hash_base64() {
        let mut cfg = happy_config();
        cfg.cert_hashes_b64 = vec!["!!!".into()];
        match cfg.parse() {
            Err(WebTransportConfigError::BadCertHashBase64 { index }) => assert_eq!(index, 0),
            other => panic!("expected BadCertHashBase64, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_wrong_cert_hash_length() {
        let mut cfg = happy_config();
        // 16 bytes instead of 32.
        cfg.cert_hashes_b64 = vec![B64.encode([0u8; 16])];
        match cfg.parse() {
            Err(WebTransportConfigError::BadCertHashLength { index, got }) => {
                assert_eq!(index, 0);
                assert_eq!(got, 16);
            },
            other => panic!("expected BadCertHashLength, got {other:?}"),
        }
    }

    #[test]
    fn parse_accepts_empty_hash_list() {
        // Empty hash list = system-roots fallback (allowed
        // but discouraged for production).
        let mut cfg = happy_config();
        cfg.cert_hashes_b64 = vec![];
        let parsed = cfg.parse().expect("empty hash list is allowed");
        assert!(parsed.hashes.is_empty());
    }

    #[test]
    fn new_validates_eagerly() {
        // The Transport ctor must reject bad config at
        // construction, not deferred to connect time.
        let mut cfg = happy_config();
        cfg.url = "ws://example.com/wt".into();
        match WebTransportPt::new(cfg) {
            Err(Error::Config(msg)) => assert!(msg.contains("https")),
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn config_round_trips_through_serde() {
        let cfg = happy_config();
        let json = serde_json::to_string(&cfg).expect("serialise");
        let back: WebTransportConfig = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(cfg, back);
    }
}
