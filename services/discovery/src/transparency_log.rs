// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! KT-log S2 — best-effort append to the transparency log on every
//! identity-key CHANGE.
//!
//! The discovery service is the only writer of the key-transparency log:
//! whenever an account's published Ed25519 identity key CHANGES (first
//! publish, a proven rotation, or an unproven new-key reset), discovery
//! appends a leaf to `services/transparency` so a client can later prove
//! the operator never equivocated about that account's key history.
//!
//! ## Account-key derivation (decision C)
//!
//! The log leaf's `username_hash` field carries
//!
//! ```text
//! BLAKE3("lakko/transparency/account/v1" || account_id)
//! ```
//!
//! — a 32-byte key derivable by BOTH discovery (on append) AND a client
//! (on verify), because `account_id` is already public. The transparency
//! service treats this as 32 opaque bytes and never learns the
//! account_id-to-username mapping, so the log leaks nothing the public
//! `account_id` did not already.
//!
//! ## Consistency model (decision E) — best-effort, fail-open
//!
//! The identity bundle is published FIRST (the discovery upsert). The
//! append here is BEST-EFFORT: on any failure (transparency down,
//! non-2xx, network error) we `warn!` and PROCEED — we NEVER fail the
//! discovery upload because of a transparency-append failure. A missed
//! append is NOT silently lost: it is closed by the S2.5 reconciliation
//! sweep (future work), which re-derives the expected log state from the
//! `identity_bundles` table and back-fills any gaps.
//!
//! When `LAKKO_TRANSPARENCY_URL` is unset the append is skipped entirely
//! (the log integration is off — logged once at startup, not per
//! request).

use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::Serialize;

/// Domain-separation string for the account-key derivation (decision C).
/// Both discovery (append) and the client (verify) MUST use this exact
/// string so the derived `username_hash` matches. Changing it is a wire
/// break.
pub(crate) const ACCOUNT_KEY_DOMAIN: &str = "lakko/transparency/account/v1";

/// Derive the transparency-log `username_hash` for an account:
/// `BLAKE3("lakko/transparency/account/v1" || account_id)`.
///
/// The domain string is PREPENDED to the raw `account_id` bytes and the
/// concatenation is hashed with plain (un-keyed) BLAKE3 — `account_id`
/// is public, so the result reveals nothing new; the domain separator
/// only stops this hash from colliding with any other BLAKE3 use of the
/// same `account_id`.
#[must_use]
pub(crate) fn account_key(account_id: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(ACCOUNT_KEY_DOMAIN.len() + account_id.len());
    input.extend_from_slice(ACCOUNT_KEY_DOMAIN.as_bytes());
    input.extend_from_slice(account_id);
    lakko_crypto::hash::blake3(&input)
}

/// Configuration + HTTP client for the transparency append integration.
/// `None` in `AppState` when `LAKKO_TRANSPARENCY_URL` is unset (the
/// integration is off).
#[derive(Clone)]
pub(crate) struct TransparencyClient {
    http: reqwest::Client,
    /// Base URL of the transparency service, e.g.
    /// `http://127.0.0.1:8087`. No trailing slash.
    base_url: Arc<str>,
    /// The shared service secret presented as
    /// `Authorization: Bearer <secret-b64>` — the SAME value the
    /// transparency service holds in `LAKKO_TRANSPARENCY_APPEND_SECRET`.
    /// Stored already base64-encoded so the header is cheap to build.
    secret_b64: Arc<str>,
}

/// The `POST /transparency/append` request body (decision B — the
/// version is assigned server-side, so it is NOT sent).
#[derive(Serialize)]
struct AppendReq<'a> {
    username_hash_b64: &'a str,
    identity_pubkey_b64: &'a str,
    is_reset: bool,
}

impl TransparencyClient {
    /// Build a client from the raw env values. `base_url` is the
    /// `LAKKO_TRANSPARENCY_URL` value; `secret_b64` is the
    /// `LAKKO_TRANSPARENCY_APPEND_SECRET` value (passed through verbatim
    /// — the transparency side base64-decodes it).
    ///
    /// # Errors
    ///
    /// Returns an error if the reqwest client cannot be built (TLS
    /// backend init failure).
    pub(crate) fn new(base_url: &str, secret_b64: &str) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self {
            http,
            base_url: Arc::from(base_url.trim_end_matches('/')),
            secret_b64: Arc::from(secret_b64),
        })
    }

    /// Best-effort append of an identity-key CHANGE to the transparency
    /// log. NEVER returns an error — every failure mode is logged at
    /// `warn` and swallowed (decision E, fail-open). A missed append is
    /// closed by the S2.5 reconciliation sweep (future work), not lost.
    pub(crate) async fn append_key_change(
        &self,
        account_id: &[u8],
        signature_pubkey: &[u8],
        is_reset: bool,
    ) {
        let username_hash = account_key(account_id);
        let body = AppendReq {
            username_hash_b64: &B64.encode(username_hash),
            identity_pubkey_b64: &B64.encode(signature_pubkey),
            is_reset,
        };
        let url = format!("{}/transparency/append", self.base_url);

        let result = self
            .http
            .post(&url)
            .bearer_auth(&*self.secret_b64)
            .json(&body)
            .send()
            .await;

        match result {
            Ok(resp) if resp.status().is_success() => {
                tracing::debug!(
                    account_id = %B64.encode(account_id),
                    is_reset,
                    "transparency append ok",
                );
            },
            Ok(resp) => {
                // Non-2xx — log the status; the upload already succeeded so
                // we proceed. Reconciliation (S2.5) will back-fill.
                tracing::warn!(
                    account_id = %B64.encode(account_id),
                    status = %resp.status(),
                    "transparency append non-2xx — proceeding (S2.5 reconciliation will back-fill)",
                );
            },
            Err(e) => {
                // Network / timeout / connect error. Same posture.
                tracing::warn!(
                    account_id = %B64.encode(account_id),
                    error = %e,
                    "transparency append failed — proceeding (S2.5 reconciliation will back-fill)",
                );
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_key_is_domain_separated_and_deterministic() {
        let id = [0xABu8; 16];
        let k1 = account_key(&id);
        let k2 = account_key(&id);
        // Deterministic.
        assert_eq!(k1, k2);

        // Domain-separated: hashing the bare account_id (no domain
        // prefix) must NOT collide with the derived key.
        let bare = lakko_crypto::hash::blake3(&id);
        assert_ne!(k1, bare);

        // Pinned domain string — a change here is a wire break with the
        // client verifier.
        assert_eq!(ACCOUNT_KEY_DOMAIN, "lakko/transparency/account/v1");

        // Sanity: matches an explicit "domain || id" hash.
        let mut expected_input = Vec::new();
        expected_input.extend_from_slice(b"lakko/transparency/account/v1");
        expected_input.extend_from_slice(&id);
        assert_eq!(k1, lakko_crypto::hash::blake3(&expected_input));
    }

    #[test]
    fn distinct_accounts_derive_distinct_keys() {
        assert_ne!(account_key(&[0u8; 16]), account_key(&[1u8; 16]));
    }
}
