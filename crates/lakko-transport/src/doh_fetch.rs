// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! DoH-bootstrapped fetcher for the signed
//! [`crate::rendezvous::BootstrapConfig`] blob.
//!
//! Sprint G5-5 shipped the signed-rendezvous wire format
//! and the 4-stage [`BootstrapConfig::verify`] path. This
//! module is the missing client side: given a stable
//! innocuous domain (e.g. `bootstrap.lakko.me`), it
//!
//! 1. resolves the domain through one of four hardcoded
//!    JSON-DoH endpoints (Cloudflare, Google, Quad9,
//!    AdGuard) — happy-eyeballs first-success-wins,
//! 2. fetches the bootstrap blob over HTTPS, with the
//!    resolved IP **pinned into reqwest's resolver** so a
//!    censor-controlled recursive DNS cannot interpose
//!    between the DoH leg and the HTTPS leg,
//! 3. parses + verifies the blob through the existing
//!    4-stage [`BootstrapConfig::verify`] against the
//!    binary-pinned [`VerifyingKey`] trust anchor.
//!
//! ## Threat model
//!
//! The DoH legs use standard CA-chain TLS (Mozilla
//! webpki-roots, baked into the binary at build time).
//! A censor controlling the recursive DNS path can spoof
//! `bootstrap.lakko.me` to point at their own host; the
//! payload signature check at step 3 catches that — they
//! cannot forge an Ed25519 signature without the publisher
//! key. A censor controlling a public CA could mount a
//! valid TLS interception against the DoH endpoint
//! itself; this is the same trust shape every Tor client
//! uses for its directory authorities, and the signature
//! check at step 3 remains the cryptographic backstop.
//!
//! Lakko's own API trust (auth / ds / storage / channels)
//! continues to use SPKI pinning via
//! [`crate::pinning::PinnedVerifier`] — the two CA-trust
//! roots are intentionally independent.
//!
//! ## Wire shapes
//!
//! - **DoH query:** JSON dns-json per RFC 8484 §6.
//!   `GET https://{endpoint}/dns-query?name={domain}&type=A`
//!   with `Accept: application/dns-json`.
//! - **Bootstrap fetch:** plain HTTPS GET to the resolved
//!   IP, with the reqwest client's resolver overridden so
//!   the Host header + SNI carry the original domain
//!   string. The response body is the canonical signed-
//!   bytes JSON produced by
//!   [`BootstrapConfig::signing_bytes`].
//!
//! ## Why JSON-DoH and not DNS-over-HTTPS wire format
//!
//! The wire-format DoH (RFC 8484 §4) requires building a
//! DNS query packet + parsing the response. JSON-DoH is
//! the same protocol family but readable, and Cloudflare
//! / Google / Quad9 all ship it. The MVP picks JSON to
//! keep the parser surface tiny; a follow-up sprint can
//! add wire-format support behind a feature flag if a
//! censorship-lab test reveals a region where JSON-DoH is
//! specifically blocked.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, SystemTime};

use futures::StreamExt as _;
use lakko_crypto::signing::VerifyingKey;
use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;
use tracing::{debug, warn};

use crate::rendezvous::{BootstrapConfig, RendezvousError};

/// Hardcoded JSON-DoH endpoints, tried in order on every
/// fetch. The order is intentional: Cloudflare and Google
/// have the lowest latency for most of the planet;
/// Quad9 + AdGuard are fallbacks that still ship JSON-DoH
/// and run on different ASNs so a single-provider outage
/// (or block) does not strand bootstrap.
///
/// Stable across releases — these strings are part of the
/// reproducible-build artefact, audit-grep marker
/// `DOH_ENDPOINTS`.
pub const DOH_ENDPOINTS: &[&str] = &[
    "https://cloudflare-dns.com/dns-query",
    "https://dns.google/resolve",
    "https://dns.quad9.net:5053/dns-query",
    "https://dns.adguard-dns.com/resolve",
];

/// Per-leg timeout — both DoH resolve and bootstrap fetch
/// individually. Set generously so a slow link does not
/// fail the bootstrap entirely; the next-endpoint fallback
/// keeps the overall wall-clock bounded.
pub const PER_LEG_TIMEOUT: Duration = Duration::from_secs(10);

/// Errors surfaced by the DoH fetcher. Branch on the
/// variant, not the message string — every variant maps
/// to a different operator response (retry, swap endpoint,
/// alert).
#[derive(Debug, Error)]
pub enum DohError {
    /// HTTP-layer failure (timeout, TCP reset, TLS reject,
    /// non-2xx response from a DoH endpoint).
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    /// Every DoH endpoint failed — either all timed out
    /// or all returned a parse-failed payload.
    #[error("all DoH endpoints failed; last error: {0}")]
    AllDohFailed(String),
    /// DoH JSON response was well-formed HTTP but the
    /// payload did not carry a usable A record.
    #[error("DoH response carried no A record")]
    NoAddressInResponse,
    /// JSON parse failed on either the DoH response or
    /// the bootstrap config payload.
    #[error("JSON parse failed: {0}")]
    Json(String),
    /// Bootstrap config did not pass the 4-stage
    /// [`BootstrapConfig::verify`].
    #[error("bootstrap verify failed: {0}")]
    Rendezvous(#[from] RendezvousError),
    /// Bootstrap fetch reached the right host but the
    /// HTTP status was non-2xx.
    #[error("bootstrap fetch returned HTTP {0}")]
    BootstrapHttp(u16),
    /// Response body exceeded the caller-supplied size
    /// cap. Surfaced by [`DohFetcher::fetch_pinned_bytes`]
    /// / [`DohFetcher::fetch_pinned_bytes_at`] when an
    /// attacker-controlled origin serves a larger payload
    /// than the caller is willing to buffer in memory.
    #[error("response body exceeded cap: {got} bytes > cap {cap}")]
    BodyTooLarge {
        /// Number of bytes read before the cap was hit.
        got: usize,
        /// Cap the caller asked us to enforce.
        cap: usize,
    },
}

/// Fetcher with two reqwest clients underneath — one for
/// the DoH legs (webpki-roots, standard CA chain), one
/// for the bootstrap HTTPS leg (same trust shape; the
/// signature check is the cryptographic backstop, not the
/// CA).
///
/// Cheap to clone — both clients are reference-counted
/// internally.
#[derive(Clone)]
pub struct DohFetcher {
    http: Client,
}

impl DohFetcher {
    /// Build a fetcher with the default reqwest config:
    /// rustls-tls + webpki-roots, HTTP/2, per-leg
    /// [`PER_LEG_TIMEOUT`].
    ///
    /// # Errors
    ///
    /// Returns the underlying [`reqwest::Error`] only on
    /// builder misconfiguration — in practice this is
    /// infallible on the stable API but the error type is
    /// surfaced for forward compatibility.
    pub fn new() -> Result<Self, DohError> {
        let http = Client::builder()
            .timeout(PER_LEG_TIMEOUT)
            .https_only(true)
            .use_rustls_tls()
            .build()?;
        Ok(Self { http })
    }

    /// Build with a pre-configured reqwest [`Client`] —
    /// useful for tests that inject a fake HTTP
    /// transport, or for callers that want to tune
    /// timeouts beyond [`PER_LEG_TIMEOUT`].
    #[must_use]
    pub fn with_client(http: Client) -> Self {
        Self { http }
    }

    /// Resolve `domain` to a single [`IpAddr`] via the
    /// first DoH endpoint that returns a usable A record.
    /// Happy-eyeballs over [`DOH_ENDPOINTS`] in order.
    ///
    /// # Errors
    ///
    /// [`DohError::AllDohFailed`] if every endpoint
    /// failed (timeout, non-2xx, no A record).
    pub async fn resolve(&self, domain: &str) -> Result<IpAddr, DohError> {
        let mut last_err: Option<String> = None;
        for endpoint in DOH_ENDPOINTS {
            match self.resolve_via_endpoint(endpoint, domain).await {
                Ok(addr) => {
                    debug!(%endpoint, %domain, %addr, "DoH resolve success");
                    return Ok(addr);
                },
                Err(e) => {
                    warn!(%endpoint, %domain, error = %e, "DoH endpoint failed");
                    last_err = Some(e.to_string());
                },
            }
        }
        Err(DohError::AllDohFailed(
            last_err.unwrap_or_else(|| "no endpoints tried".into()),
        ))
    }

    /// JSON-DoH (RFC 8484 §6) query against a single
    /// endpoint. Returns the first A record from the
    /// answer section, or an error if none.
    async fn resolve_via_endpoint(
        &self,
        endpoint: &str,
        domain: &str,
    ) -> Result<IpAddr, DohError> {
        let url = format!("{endpoint}?name={domain}&type=A");
        let resp = self
            .http
            .get(&url)
            .header("accept", "application/dns-json")
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(DohError::AllDohFailed(format!(
                "{endpoint}: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let body: DnsJson = resp
            .json()
            .await
            .map_err(|e| DohError::Json(e.to_string()))?;
        // Walk the answer section for the first A record
        // we can parse as an IPv4 address. A future
        // sprint can extend to AAAA / type=28.
        for ans in body.answer.iter().flatten() {
            // RFC 8484 §6: type 1 = A. Skip CNAMEs and
            // other types.
            if ans.type_ == 1 {
                if let Ok(addr) = ans.data.parse::<Ipv4Addr>() {
                    return Ok(IpAddr::V4(addr));
                }
            }
        }
        Err(DohError::NoAddressInResponse)
    }

    /// Fetch a signed [`BootstrapConfig`] from `domain` +
    /// `path`, with the host resolved through the DoH
    /// fan-out and the resolved IP pinned into reqwest's
    /// resolver so the HTTPS leg cannot be re-resolved
    /// through a censor-controlled recursive DNS.
    ///
    /// The HTTPS leg targets `https://{domain}{path}`
    /// (Host header + SNI carry the original string; the
    /// pinned IP is what TCP actually connects to).
    ///
    /// Verifies the response payload against
    /// `trust_anchor` + the current wall-clock through
    /// [`BootstrapConfig::verify`]. The 4-stage validation
    /// (schema version → expiry → issued-at clock skew →
    /// Ed25519) is the cryptographic backstop against a
    /// compromised DoH or CA chain.
    ///
    /// # Errors
    ///
    /// - [`DohError::AllDohFailed`] — none of
    ///   [`DOH_ENDPOINTS`] resolved `domain`.
    /// - [`DohError::BootstrapHttp`] — DoH succeeded but
    ///   the HTTPS GET returned non-2xx.
    /// - [`DohError::Http`] — TCP / TLS-layer failure on
    ///   either leg.
    /// - [`DohError::Json`] — bootstrap payload was not
    ///   parseable JSON.
    /// - [`DohError::Rendezvous`] — signature / expiry /
    ///   schema check failed.
    pub async fn fetch_bootstrap(
        &self,
        domain: &str,
        path: &str,
        trust_anchor: &VerifyingKey,
    ) -> Result<BootstrapConfig, DohError> {
        let ip = self.resolve(domain).await?;
        self.fetch_bootstrap_at(ip, domain, path, trust_anchor).await
    }

    /// Variant of [`Self::fetch_bootstrap`] that takes an
    /// already-resolved IP — useful for tests that pin a
    /// known mock-server address, or for operators that
    /// want to skip the DoH leg entirely (e.g. when a
    /// signed config is shipped through a sidechannel).
    ///
    /// `domain` is still used for SNI + Host header so
    /// the TLS cert validation works.
    pub async fn fetch_bootstrap_at(
        &self,
        ip: IpAddr,
        domain: &str,
        path: &str,
        trust_anchor: &VerifyingKey,
    ) -> Result<BootstrapConfig, DohError> {
        let url = format!("https://{domain}{path}");
        // reqwest's resolve_to_addrs API takes an Iter of
        // SocketAddrs; we use a single resolved IP on port
        // 443. A future variant can take an explicit port
        // when the bootstrap host runs on a non-standard
        // one.
        let resolved = SocketAddr::new(ip, 443);
        let pinned_client = Client::builder()
            .timeout(PER_LEG_TIMEOUT)
            .https_only(true)
            .use_rustls_tls()
            .resolve(domain, resolved)
            .build()?;
        let resp = pinned_client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(DohError::BootstrapHttp(resp.status().as_u16()));
        }
        let cfg: BootstrapConfig = resp
            .json()
            .await
            .map_err(|e| DohError::Json(e.to_string()))?;
        cfg.verify(trust_anchor, SystemTime::now())?;
        Ok(cfg)
    }

    /// Generic IP-pinned HTTPS GET — DoH-resolve `domain`,
    /// HTTPS GET `https://{domain}{path}` against the
    /// resolved IP, return the body bytes up to `max_bytes`.
    /// Refuses payloads larger than the cap (the call
    /// surfaces [`DohError::BodyTooLarge`] rather than
    /// silently truncating).
    ///
    /// Designed for downstream callers that need to pull
    /// signed artefacts over the same IP-pin + DoH pipeline
    /// the bootstrap rendezvous uses — TUF metadata
    /// (`{base}/timestamp.json` / `{base}/{n}.root.json` /
    /// etc.), KeyPackage bundles, signed identity blobs.
    /// The crate stays unaware of those payload shapes; the
    /// caller decodes the bytes (e.g.
    /// `lakko_tuf::Signed::<T>::from_json`) and runs the
    /// signature / chain checks.
    ///
    /// # Threat model
    ///
    /// Same shape as [`Self::fetch_bootstrap`]: a censor
    /// controlling DNS cannot interpose because the
    /// resolved IP is pinned into reqwest's resolver, and
    /// the signature check happens on the bytes downstream
    /// (TUF: each role envelope signed by Ed25519 under the
    /// binary-pinned trust anchor).
    ///
    /// # Errors
    ///
    /// - [`DohError::AllDohFailed`] / [`DohError::NoAddressInResponse`]
    ///   — every DoH endpoint failed to resolve `domain`.
    /// - [`DohError::BootstrapHttp`] — HTTPS GET returned
    ///   non-2xx (variant name is historical; covers
    ///   every IP-pinned GET in this module).
    /// - [`DohError::BodyTooLarge`] — origin served a
    ///   payload larger than `max_bytes`.
    /// - [`DohError::Http`] — TCP / TLS-layer failure.
    pub async fn fetch_pinned_bytes(
        &self,
        domain: &str,
        path: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, DohError> {
        let ip = self.resolve(domain).await?;
        self.fetch_pinned_bytes_at(ip, domain, path, max_bytes).await
    }

    /// Variant of [`Self::fetch_pinned_bytes`] that takes
    /// an already-resolved IP. Useful for tests pinning a
    /// known mock-server address and for callers that
    /// already cached a previous DoH resolution.
    ///
    /// # Errors
    ///
    /// Same as [`Self::fetch_pinned_bytes`], minus the
    /// DoH-leg failures.
    pub async fn fetch_pinned_bytes_at(
        &self,
        ip: IpAddr,
        domain: &str,
        path: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, DohError> {
        let url = format!("https://{domain}{path}");
        let resolved = SocketAddr::new(ip, 443);
        let pinned_client = Client::builder()
            .timeout(PER_LEG_TIMEOUT)
            .https_only(true)
            .use_rustls_tls()
            .resolve(domain, resolved)
            .build()?;
        let resp = pinned_client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(DohError::BootstrapHttp(resp.status().as_u16()));
        }

        // Stream the body chunk-by-chunk so we can bail
        // out the moment we cross max_bytes — `bytes()`
        // would buffer the entire response before we get
        // a chance to inspect length. Content-Length is
        // attacker-controllable so we also check the
        // running total on every chunk.
        let mut buf: Vec<u8> = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if buf.len().saturating_add(chunk.len()) > max_bytes {
                return Err(DohError::BodyTooLarge {
                    got: buf.len() + chunk.len(),
                    cap: max_bytes,
                });
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(buf)
    }
}

// ---------------------------------------------------------
// JSON-DoH response wire shape (Cloudflare / Google /
// Quad9 / AdGuard all share it). Documented at
// https://developers.cloudflare.com/1.1.1.1/encryption/dns-over-https/make-api-requests/dns-json/
// ---------------------------------------------------------

#[derive(Debug, Deserialize)]
struct DnsJson {
    /// "Answer" section — an array of records. Absent
    /// when the query name does not exist; serde's
    /// Option treatment maps that to `None`.
    #[serde(rename = "Answer", default)]
    answer: Option<Vec<DnsAnswer>>,
}

#[derive(Debug, Deserialize)]
struct DnsAnswer {
    /// Record type — `1` = A, `28` = AAAA, `5` = CNAME.
    /// We only consume A records for the MVP.
    #[serde(rename = "type")]
    type_: u16,
    /// The record data — for A records, a dotted-quad
    /// IPv4 string.
    #[serde(rename = "data")]
    data: String,
}

// ---------------------------------------------------------
// Tests
// ---------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doh_endpoints_pinned_and_https() {
        // Wire-contract pin — the audit-grep marker
        // `DOH_ENDPOINTS` lists every public DoH host
        // the binary trusts. A change here is part of
        // the reproducible-build artefact.
        assert!(!DOH_ENDPOINTS.is_empty());
        assert_eq!(DOH_ENDPOINTS.len(), 4);
        for ep in DOH_ENDPOINTS {
            assert!(
                ep.starts_with("https://"),
                "DoH endpoint must be HTTPS: {ep}"
            );
        }
    }

    #[test]
    fn dns_json_parses_cloudflare_shape() {
        // Real Cloudflare response shape (trimmed). Tests
        // that we walk Answer + type filter correctly.
        let raw = r#"{
            "Status": 0,
            "Answer": [
                {"name":"bootstrap.lakko.me","type":5,"TTL":300,"data":"alias.lakko.me"},
                {"name":"alias.lakko.me","type":1,"TTL":60,"data":"203.0.113.42"}
            ]
        }"#;
        let parsed: DnsJson = serde_json::from_str(raw).unwrap();
        let answers = parsed.answer.unwrap();
        assert_eq!(answers.len(), 2);
        // CNAME (type 5) comes first; A (type 1) second.
        let a = answers.iter().find(|a| a.type_ == 1).unwrap();
        assert_eq!(a.data, "203.0.113.42");
        let parsed_addr: Ipv4Addr = a.data.parse().unwrap();
        assert_eq!(parsed_addr, Ipv4Addr::new(203, 0, 113, 42));
    }

    #[test]
    fn dns_json_handles_empty_answer() {
        // NXDOMAIN-style — no "Answer" key at all.
        let raw = r#"{"Status": 3}"#;
        let parsed: DnsJson = serde_json::from_str(raw).unwrap();
        assert!(parsed.answer.is_none());
    }

    #[test]
    fn doh_fetcher_constructs_without_network() {
        // Pure smoke — DohFetcher::new should not touch
        // the network, only build the reqwest::Client.
        let _ = DohFetcher::new().unwrap();
    }

    #[test]
    fn per_leg_timeout_is_sane() {
        // 10s gives a slow mobile link a chance but
        // bounds the cold-start budget at 4×10s = 40s
        // worst case across all DoH endpoints.
        assert!(PER_LEG_TIMEOUT.as_secs() <= 30);
        assert!(PER_LEG_TIMEOUT.as_secs() >= 5);
    }

    // Live-network tests against real DoH endpoints are
    // gated behind `--ignored` so the default `cargo
    // test` run stays hermetic. Run manually with
    //   cargo test -p lakko-transport -- --ignored doh
    // when wiring is needed.
    #[tokio::test]
    #[ignore = "live network — DoH resolve against Cloudflare"]
    async fn live_resolve_example_com() {
        let f = DohFetcher::new().unwrap();
        let addr = f.resolve("example.com").await.unwrap();
        // We don't pin example.com's IP (it changes), but
        // it must be a non-loopback IPv4 if the resolver
        // is sane.
        assert!(matches!(addr, IpAddr::V4(_)));
        if let IpAddr::V4(v4) = addr {
            assert!(!v4.is_loopback());
            assert!(!v4.is_unspecified());
        }
    }
}
