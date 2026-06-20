// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the anti-censorship tunneled HTTP client (ADR-0026
//! Phase 3).
//!
//! Native clients (iOS / Android) call [`tunneled_http_request`] to make a
//! Lakko REST call through the best available obfuscated transport: it parses +
//! **verifies** the signed bootstrap config against a pinned trust anchor,
//! builds a [`BootstrapDialer`], races the configured transports
//! (fail-closed — no silent direct fallback, invariant #4), and runs one
//! HTTP/1.1 request/response over the winning tunnel.
//!
//! The call is synchronous at the FFI boundary (it drives a private
//! current-thread tokio runtime internally); the native side calls it off the
//! main thread. There is no wasm mirror — a browser sandbox cannot open the
//! raw sockets these transports need (ADR-0026).

use std::time::{Duration, SystemTime};

use lakko_crypto::signing::VerifyingKey;
use lakko_transport::{
    tunneled_request_via_dialer, BootstrapConfig, BootstrapDialer, DohFetcher, Endpoint,
    TunneledRequest,
};

use crate::FfiError;

/// Upper bound on a fetched bootstrap-config body. The signed config is a small
/// JSON document (a handful of transport descriptors); the cap stops a hostile
/// origin from streaming an unbounded body before the signature is ever checked.
const MAX_BOOTSTRAP_CONFIG_BYTES: usize = 64 * 1024;

/// One HTTP header (name, value).
#[derive(uniffi::Record, Debug)]
pub struct FfiHeader {
    /// Header name.
    pub name: String,
    /// Header value.
    pub value: String,
}

/// The result of a tunneled request.
#[derive(uniffi::Record, Debug)]
pub struct FfiTunneledResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers (names lower-cased).
    pub headers: Vec<FfiHeader>,
    /// Response body bytes.
    pub body: Vec<u8>,
    /// Which obfuscated transport won the race (e.g. `"Reality"`) — for
    /// client-side telemetry / debugging; never security-bearing.
    pub transport_kind: String,
}

/// Make one HTTP/1.1 request to `endpoint_host:endpoint_port` over the best
/// obfuscated transport described by the signed `config_json`.
///
/// `trust_anchor` is the 32-byte Ed25519 verifying key the config's signature
/// is checked against (build-time pinned by the caller). `now_secs` is the
/// current Unix time used for the config's expiry/clock gates. `deadline_ms`
/// bounds the dial race.
///
/// # Errors
///
/// [`FfiError::Transport`] for a bad trust anchor / config, a failed
/// verification, a fail-closed dial (no transport connected), or a
/// tunneled-request transport/parse failure.
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn tunneled_http_request(
    config_json: Vec<u8>,
    trust_anchor: Vec<u8>,
    now_secs: u64,
    endpoint_host: String,
    endpoint_port: u16,
    deadline_ms: u64,
    method: String,
    path: String,
    host: String,
    headers: Vec<FfiHeader>,
    body: Vec<u8>,
) -> Result<FfiTunneledResponse, FfiError> {
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
    let req = TunneledRequest {
        method,
        path,
        host,
        headers: headers.into_iter().map(|h| (h.name, h.value)).collect(),
        body,
    };

    // A private current-thread runtime drives the dial + request to completion;
    // the native caller invokes this off the main thread.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| FfiError::Transport(format!("runtime: {e}")))?;
    let (kind, resp) = rt
        .block_on(tunneled_request_via_dialer(
            &dialer,
            &endpoint,
            Duration::from_millis(deadline_ms),
            &req,
        ))
        .map_err(|e| FfiError::Transport(format!("tunneled request: {e}")))?;

    Ok(FfiTunneledResponse {
        status: resp.status,
        headers: resp
            .headers
            .into_iter()
            .map(|(name, value)| FfiHeader { name, value })
            .collect(),
        body: resp.body,
        transport_kind: format!("{kind:?}"),
    })
}

/// Fetch + verify the signed bootstrap config used to arm the obfuscated
/// tunnel (ADR-0026 component 6).
///
/// Resolves `doh_domain` over JSON-DoH, IP-pinned-HTTPS-GETs
/// `https://{doh_domain}{path}` against the resolved address, then parses the
/// body as a [`BootstrapConfig`] and verifies its Ed25519 signature + expiry /
/// clock gates against `trust_anchor` (the build-time-pinned 32-byte verifying
/// key) at `now_secs`. On success it returns the **verified raw JSON bytes**, so
/// the caller stashes them and later passes them to [`tunneled_http_request`] as
/// `config_json` (which re-parses + re-verifies independently).
///
/// A censor controlling DNS cannot interpose: the resolved IP is pinned into the
/// HTTPS client, and acceptance hinges on the Ed25519 signature over the config,
/// not on the network path.
///
/// # Errors
///
/// [`FfiError::Transport`] for a bad trust anchor, a DoH / HTTPS failure, an
/// over-cap body, a malformed config, or a failed signature / expiry check.
/// There is no silent fallback — a caller that cannot obtain a verified config
/// must leave anti-censorship unavailable (loud), never armed with an
/// unverified config (invariant #4).
#[uniffi::export]
pub fn fetch_bootstrap_config(
    doh_domain: String,
    path: String,
    trust_anchor: Vec<u8>,
    now_secs: u64,
) -> Result<Vec<u8>, FfiError> {
    let anchor = VerifyingKey::from_bytes(&trust_anchor)
        .map_err(|e| FfiError::Transport(format!("bad trust anchor: {e}")))?;
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(now_secs);

    // A private current-thread runtime drives the DoH + HTTPS fetch to
    // completion; the native caller invokes this off the main thread.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| FfiError::Transport(format!("runtime: {e}")))?;

    rt.block_on(async {
        let fetcher =
            DohFetcher::new().map_err(|e| FfiError::Transport(format!("doh fetcher: {e}")))?;
        let bytes = fetcher
            .fetch_pinned_bytes(&doh_domain, &path, MAX_BOOTSTRAP_CONFIG_BYTES)
            .await
            .map_err(|e| FfiError::Transport(format!("bootstrap fetch: {e}")))?;
        // Parse + verify BEFORE returning the bytes: an unverified config never
        // leaves this function (fail-closed).
        let config: BootstrapConfig = serde_json::from_slice(&bytes)
            .map_err(|e| FfiError::Transport(format!("bad bootstrap config json: {e}")))?;
        config
            .verify(&anchor, now)
            .map_err(|e| FfiError::Transport(format!("bootstrap verify: {e}")))?;
        Ok(bytes)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(anchor: Vec<u8>, config: Vec<u8>) -> Result<FfiTunneledResponse, FfiError> {
        tunneled_http_request(
            config,
            anchor,
            0,
            "lakko.invalid".into(),
            443,
            1000,
            "GET".into(),
            "/x".into(),
            "lakko.invalid".into(),
            vec![],
            vec![],
        )
    }

    #[test]
    fn rejects_wrong_length_trust_anchor() {
        // 16 bytes, not 32 — fails before any dial.
        let err = call(vec![0u8; 16], b"{}".to_vec()).unwrap_err();
        assert!(matches!(err, FfiError::Transport(_)));
    }

    #[test]
    fn rejects_malformed_config_json() {
        let err = call(vec![0u8; 32], b"not json".to_vec()).unwrap_err();
        match err {
            FfiError::Transport(msg) => assert!(msg.contains("config")),
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[test]
    fn fetch_bootstrap_config_rejects_wrong_length_trust_anchor() {
        // 16 bytes, not 32 — the anchor gate fails before any network leg, so
        // the fail-closed front door is exercised without a live fetcher.
        let err = fetch_bootstrap_config(
            "rendezvous.lakko.invalid".into(),
            "/bootstrap.json".into(),
            vec![0u8; 16],
            0,
        )
        .unwrap_err();
        match err {
            FfiError::Transport(msg) => assert!(msg.contains("trust anchor")),
            other => panic!("expected Transport, got {other:?}"),
        }
    }
}
