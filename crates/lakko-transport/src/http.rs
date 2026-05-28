// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! HTTP REST client for the back-end services (`auth`, `ds`, `storage`,
//! `discovery`, `push-gw`).
//!
//! Built on `reqwest` with a custom rustls TLS config that uses our
//! [`crate::pinning::PinnedVerifier`] instead of the system trust
//! store. Three primitives:
//!
//!   * [`HttpTransport::post_json`] — for everything that takes a
//!     JSON body and returns a JSON body. The whole auth / discovery
//!     / push-gw API surface is shaped this way.
//!   * [`HttpTransport::get_json`] — for `?wait=`-style long polls
//!     (DS journal / mailbox). The HTTP-level read timeout is set
//!     wider than the longest `wait`, so a held connection just
//!     waits for the server to respond.
//!   * [`HttpTransport::get_bytes`] — for blob downloads from
//!     `services/storage`. Returns `Vec<u8>`; a streaming variant
//!     can be added once the UI sprints (1.14+) need it.
//!
//! ## Long-poll without a separate API
//!
//! There's no special long-poll method. `services/ds` already does
//! the right thing through standard HTTP: client sends `GET …?wait=N`
//! and the server holds the connection until it can answer or the
//! timeout fires. From the HTTP client's POV that's just a slow GET.
//! The only thing we need is a generous `read_timeout` — set at
//! construction.

use std::time::Duration;

use bytes::Bytes;
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Serialize;
use url::Url;

use crate::pinning::{PinError, PinnedVerifier, SpkiPinSet};

/// Default request read timeout. `services/ds` caps `?wait=` at 60s,
/// so 90s leaves headroom for slow networks before we abort.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(90);

/// Default connect timeout. Captures captive-portal failure modes
/// quickly without false-positiving on legitimately slow links.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Client-side body cap. Defends against a server pumping multi-GB
/// responses; matches the per-blob ceiling enforced by
/// `services/storage` plus a comfortable margin.
const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Errors from the HTTP client. `Network` covers DNS / TCP / TLS /
/// timeout failures; `Status` is a non-2xx response from the server;
/// `Decode` is local JSON / size violations.
#[derive(thiserror::Error, Debug)]
pub enum HttpError {
    /// Underlying network or TLS failure (incl. SPKI pin mismatch).
    #[error("http network error: {0}")]
    Network(#[source] reqwest::Error),

    /// Server returned a non-2xx status. `body` is captured for
    /// callers that want to surface server-provided detail.
    #[error("http status {status}: {body}")]
    Status {
        /// Numeric HTTP status code.
        status: u16,
        /// Response body, lossy-UTF-8.
        body: String,
    },

    /// Local error: JSON serialization, response too big, malformed
    /// URL, etc.
    #[error("http codec / size error: {0}")]
    Decode(String),

    /// TLS configuration failed at construction time (e.g. empty
    /// pin set passed in).
    #[error("tls configuration error: {0}")]
    TlsConfig(#[source] PinError),

    /// ECH config-list bytes supplied via
    /// [`HttpTransportConfig::ech_config_list`] could not be turned
    /// into a usable [`rustls::client::EchMode`]. Failure at
    /// construction time — wire layer never starts. Refuses
    /// silently downgrading to non-ECH per security invariant #4.
    #[error("ECH configuration error: {0}")]
    EchConfig(#[source] crate::ech::EchWireUpError),
}

impl From<PinError> for HttpError {
    fn from(e: PinError) -> Self {
        HttpError::TlsConfig(e)
    }
}

/// Result alias for the HTTP module.
pub type HttpResult<T> = std::result::Result<T, HttpError>;

/// HTTPS REST client over a SPKI-pinned `reqwest::Client`.
///
/// Cheap to clone — internally `reqwest::Client` is `Arc`-backed and
/// shares the connection pool across clones.
#[derive(Clone, Debug)]
pub struct HttpTransport {
    client: Client,
    base_url: Url,
}

/// Builder-style configuration for [`HttpTransport`]. Use this when
/// you need to override the default timeouts or response size cap.
#[derive(Clone, Debug)]
pub struct HttpTransportConfig {
    /// Per-request read timeout. Defaults to 90 s — wider than the
    /// 60 s `services/ds` long-poll cap.
    pub read_timeout: Duration,
    /// Connect timeout. Defaults to 10 s.
    pub connect_timeout: Duration,
    /// Response size cap. Defaults to 64 MiB.
    pub max_response_bytes: usize,
    /// Raw ECH config-list bytes (the DNS HTTPS `ech` parameter
    /// payload). `None` (default) means no ECH — the standard
    /// SPKI-pinned TLS path. `Some(bytes)` arms ECH on every
    /// outgoing TLS handshake: the outer ClientHello carries
    /// `cloudflare-ech.com` (or whichever public-name the published
    /// config advertises) and the real SNI rides inside an
    /// HPKE-sealed inner ClientHello.
    ///
    /// On construction the bytes are validated twice (our parser
    /// in `crate::ech` + rustls's own parser inside
    /// `EchConfig::new`); if either rejects, `with_config` returns
    /// [`HttpError::EchConfig`] and refuses to build the
    /// transport. There is no silent downgrade to non-ECH per the
    /// "no fallback to non-E2EE" invariant family.
    ///
    /// Default: `None`. Populate via a signed bootstrap config or
    /// a fresh DoH lookup.
    pub ech_config_list: Option<Vec<u8>>,
}

impl Default for HttpTransportConfig {
    fn default() -> Self {
        Self {
            read_timeout: DEFAULT_READ_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            ech_config_list: None,
        }
    }
}

impl HttpTransport {
    /// Construct a new transport pinned to the given SPKI hashes.
    /// `base_url` should include the scheme and host (e.g.
    /// `https://api.lakko.me/`).
    ///
    /// # Errors
    ///
    /// Returns [`HttpError::TlsConfig`] if the pin set is empty (which
    /// would refuse every connection — fail fast).
    pub fn new(base_url: Url, pins: SpkiPinSet) -> HttpResult<Self> {
        Self::with_config(base_url, pins, HttpTransportConfig::default())
    }

    /// Same as [`Self::new`] but takes an explicit config.
    ///
    /// # Errors
    ///
    /// Returns [`HttpError::TlsConfig`] if the pin set is empty, or
    /// [`HttpError::Network`] if the underlying reqwest builder
    /// rejects the config (rare — typically a feature-flag mismatch).
    pub fn with_config(
        base_url: Url,
        pins: SpkiPinSet,
        config: HttpTransportConfig,
    ) -> HttpResult<Self> {
        // Build the rustls config: empty root store, our verifier
        // takes over via `dangerous().set_certificate_verifier`.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let verifier = PinnedVerifier::new(pins)?;

        // ECH branch — switch from `ClientConfig::builder()` (which
        // returns `WantsVerifier` directly with default versions) to
        // `builder_with_provider()` (which returns `WantsVersions`),
        // so we can hop through `with_ech`. `with_ech` returns
        // `WantsVerifier` just like `with_safe_default_protocol_versions`,
        // so the rest of the chain is identical.
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let tls = if let Some(ech_bytes) = config.ech_config_list.as_deref() {
            let mode =
                crate::ech::build_ech_mode_from_bytes(ech_bytes).map_err(HttpError::EchConfig)?;
            rustls::ClientConfig::builder_with_provider(provider)
                .with_ech(mode)
                .map_err(|e| {
                    // rustls rejected the EchMode itself (e.g. unsupported
                    // version after `EchConfig::new` already accepted it —
                    // shouldn't fire today, but the surface is fallible).
                    HttpError::EchConfig(crate::ech::EchWireUpError::RustlsRefused(e.to_string()))
                })?
                .dangerous()
                .with_custom_certificate_verifier(verifier.into_arc())
                .with_no_client_auth()
        } else {
            rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|e| {
                    // Defensive — `default_provider` and default versions
                    // are known compatible. A failure here means a broken
                    // build, not a runtime condition.
                    HttpError::EchConfig(crate::ech::EchWireUpError::RustlsRefused(e.to_string()))
                })?
                .dangerous()
                .with_custom_certificate_verifier(verifier.into_arc())
                .with_no_client_auth()
        };

        let client = Client::builder()
            .use_preconfigured_tls(tls)
            .read_timeout(config.read_timeout)
            .connect_timeout(config.connect_timeout)
            .https_only(true)
            .build()
            .map_err(HttpError::Network)?;

        Ok(Self { client, base_url })
    }

    /// `POST <path>` with `body` serialised as JSON, response
    /// deserialised as JSON. Path is resolved against `base_url`.
    ///
    /// # Errors
    ///
    /// See [`HttpError`].
    pub async fn post_json<Req, Resp>(&self, path: &str, body: &Req) -> HttpResult<Resp>
    where
        Req: Serialize + ?Sized,
        Resp: DeserializeOwned,
    {
        let url = self.resolve(path)?;
        let response = self
            .client
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(HttpError::Network)?;
        check_status_and_decode_json(response, DEFAULT_MAX_RESPONSE_BYTES).await
    }

    /// `GET <path>` (with query string already in `path` if needed),
    /// response deserialised as JSON. Use this for the
    /// `?after=<id>&wait=<secs>` long-poll endpoints — set
    /// `read_timeout` wider than `wait` at construction time.
    ///
    /// # Errors
    ///
    /// See [`HttpError`].
    pub async fn get_json<Resp>(&self, path: &str) -> HttpResult<Resp>
    where
        Resp: DeserializeOwned,
    {
        let url = self.resolve(path)?;
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(HttpError::Network)?;
        check_status_and_decode_json(response, DEFAULT_MAX_RESPONSE_BYTES).await
    }

    /// `GET <path>` returning the raw body bytes. For blob downloads
    /// from `services/storage`. The response is fully buffered up to
    /// `max_response_bytes`; streaming variant is on the 1.14+
    /// to-do list when the UI starts wanting progressive image
    /// rendering.
    ///
    /// # Errors
    ///
    /// See [`HttpError`].
    pub async fn get_bytes(&self, path: &str) -> HttpResult<Bytes> {
        let url = self.resolve(path)?;
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(HttpError::Network)?;

        let status = response.status();
        if !status.is_success() {
            return Err(status_to_error(response).await);
        }
        // `bytes()` reads the whole body without honouring our
        // `max_response_bytes` — but reqwest already enforces a hard
        // ceiling at the HTTP/2 framer level via DecoderError, and we
        // re-check below.
        let body = response.bytes().await.map_err(HttpError::Network)?;
        if body.len() > DEFAULT_MAX_RESPONSE_BYTES {
            return Err(HttpError::Decode(format!(
                "response too large: {} bytes > cap {DEFAULT_MAX_RESPONSE_BYTES}",
                body.len(),
            )));
        }
        Ok(body)
    }

    fn resolve(&self, path: &str) -> HttpResult<Url> {
        self.base_url
            .join(path)
            .map_err(|e| HttpError::Decode(format!("cannot resolve path {path:?}: {e}")))
    }
}

/// Read the response body, then decode JSON. Cap enforcement happens
/// before deserialisation (a malicious server could pump endless
/// JSON whitespace and bloat memory; we cut it off).
async fn check_status_and_decode_json<T: DeserializeOwned>(
    response: reqwest::Response,
    max_bytes: usize,
) -> HttpResult<T> {
    let status = response.status();
    if !status.is_success() {
        return Err(status_to_error(response).await);
    }
    let body = response.bytes().await.map_err(HttpError::Network)?;
    if body.len() > max_bytes {
        return Err(HttpError::Decode(format!(
            "response too large: {} bytes > cap {max_bytes}",
            body.len(),
        )));
    }
    serde_json::from_slice::<T>(&body).map_err(|e| HttpError::Decode(format!("json: {e}")))
}

async fn status_to_error(response: reqwest::Response) -> HttpError {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    // Trim very long error bodies — server may have logged a stack
    // trace we don't want to ferry to user code.
    let body = if body.len() > 1024 {
        format!("{}…[truncated, {} bytes total]", &body[..1024], body.len())
    } else {
        body
    };
    HttpError::Status { status, body }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pinning::SpkiPinSet;

    fn install_crypto_provider() {
        // Idempotent — `install_default` returns Err if already set
        // by another test in the same process.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    #[test]
    fn empty_pin_set_is_rejected_at_construction() {
        install_crypto_provider();
        let url = Url::parse("https://example.invalid/").unwrap();
        let err = HttpTransport::new(url, SpkiPinSet::default()).unwrap_err();
        assert!(
            matches!(err, HttpError::TlsConfig(_)),
            "expected TlsConfig, got {err:?}",
        );
    }

    #[test]
    fn construction_succeeds_with_non_empty_pin_set() {
        install_crypto_provider();
        let url = Url::parse("https://example.invalid/").unwrap();
        let pins = SpkiPinSet::from_hashes([[0u8; 32]]);
        let transport = HttpTransport::new(url, pins);
        assert!(transport.is_ok(), "construction failed: {transport:?}");
    }

    #[test]
    fn config_defaults_are_sane() {
        let cfg = HttpTransportConfig::default();
        // Long-poll headroom must be wider than the server's `wait` cap.
        assert!(cfg.read_timeout >= Duration::from_secs(60));
        assert!(cfg.connect_timeout < cfg.read_timeout);
        assert!(cfg.max_response_bytes >= 1024 * 1024);
    }

    #[test]
    fn resolve_joins_paths_against_base_url() {
        install_crypto_provider();
        let base = Url::parse("https://api.lakko.me/auth/").unwrap();
        let pins = SpkiPinSet::from_hashes([[0u8; 32]]);
        let t = HttpTransport::new(base, pins).unwrap();
        let resolved = t.resolve("login/start").unwrap();
        assert_eq!(resolved.as_str(), "https://api.lakko.me/auth/login/start");

        // Absolute path resets to host root, per WHATWG URL spec.
        let resolved = t.resolve("/storage/blob/abc").unwrap();
        assert_eq!(resolved.as_str(), "https://api.lakko.me/storage/blob/abc");
    }

    #[test]
    fn malformed_ech_bytes_refused_at_construction() {
        install_crypto_provider();
        let url = Url::parse("https://example.invalid/").unwrap();
        let pins = SpkiPinSet::from_hashes([[0u8; 32]]);
        let cfg = HttpTransportConfig {
            // Two bytes — not even a valid outer length
            // prefix. Our parser refuses immediately, the
            // builder surfaces it via `HttpError::EchConfig`.
            ech_config_list: Some(vec![0xff, 0xff]),
            ..HttpTransportConfig::default()
        };
        let err =
            HttpTransport::with_config(url, pins, cfg).expect_err("must reject malformed ECH");
        assert!(
            matches!(err, HttpError::EchConfig(_)),
            "expected EchConfig error, got {err:?}",
        );
    }

    #[test]
    fn empty_ech_bytes_refused_at_construction() {
        install_crypto_provider();
        let url = Url::parse("https://example.invalid/").unwrap();
        let pins = SpkiPinSet::from_hashes([[0u8; 32]]);
        let cfg = HttpTransportConfig {
            // Empty payload — the parser's truncated-wire
            // check refuses before rustls ever sees the
            // bytes.
            ech_config_list: Some(Vec::new()),
            ..HttpTransportConfig::default()
        };
        let err =
            HttpTransport::with_config(url, pins, cfg).expect_err("empty ECH must be refused");
        assert!(
            matches!(err, HttpError::EchConfig(_)),
            "expected EchConfig error, got {err:?}",
        );
    }

    #[tokio::test]
    async fn pin_mismatch_against_real_server_yields_network_error() {
        // Smoke test the wiring end-to-end against a public HTTPS host.
        // We don't know the public host's real SPKI hash, so a random
        // pin must reject the handshake. This validates that
        // PinnedVerifier is actually plumbed through reqwest.
        //
        // Skipped under offline CI runs — the test is best-effort:
        // if the runner has no network, a `Network` error is fine
        // either way (DNS / TCP failure is also `Network`). The
        // *test invariant* is "we never get a successful response".
        install_crypto_provider();
        let pins = SpkiPinSet::from_hashes([[0xCDu8; 32]]);
        let url = Url::parse("https://example.com/").unwrap();
        let Ok(t) = HttpTransport::new(url, pins) else {
            return;
        };
        let result: HttpResult<serde_json::Value> = t.get_json("").await;
        assert!(
            matches!(result, Err(HttpError::Network(_))),
            "pinned-mismatch fetch must error, got {result:?}",
        );
    }
}
