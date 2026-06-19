// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Bootstrap transport dialer (ADR-0025).
//!
//! The runtime dispatcher that turns a **verified** [`BootstrapConfig`] into a
//! live, fail-closed dial. It is the operational glue between three pieces that
//! already existed but were never composed:
//!
//! 1. [`BootstrapConfig`] + its 4-stage `verify` (signature / version / expiry /
//!    clock) — the signed list of transports an operator publishes.
//! 2. The per-transport `Transport` implementations (REALITY, Shadowsocks-2022,
//!    WebTransport, MASQUE) constructed from each [`TransportSpec`].
//! 3. The [`HappyEyeballs`] selector — a fail-closed race that returns the first
//!    candidate to connect, or [`PluggableError::AllFailed`] (never a silent
//!    plaintext downgrade — invariant #4).
//!
//! # Fail-closed posture
//!
//! - A [`TransportSpec`] whose config is rejected by its `Transport::new` is
//!   **skipped** (one operator typo must not brick the rest), but if **zero**
//!   candidates build, [`BootstrapDialer::from_verified_config`] returns
//!   [`SelectorError::NoCandidates`] — no transport means no connection.
//! - [`TransportSpec`] has no `Direct` variant, so the candidate set is exactly
//!   the operator's obfuscated transports; the dialer never adds a plaintext /
//!   direct-QUIC fallback the signed config did not list.
//!
//! # Scope (ADR-0025)
//!
//! This is the transport-layer wiring only. Routing a client's HTTP stack
//! (browser `fetch`, `URLSession`, Ktor) through the returned
//! `Box<dyn Connection>` is per-platform connection-layer surgery, deferred to a
//! later Phase-5 arc. The dialer takes an already-fetched config so it stays
//! unit-testable without a network — the caller runs
//! [`crate::doh_fetch::DohFetcher::fetch_bootstrap_config`] first.

use std::time::{Duration, SystemTime};

use lakko_crypto::signing::VerifyingKey;
use thiserror::Error;

use crate::endpoint::Endpoint;
use crate::masque::MasqueTransport;
use crate::pluggable::{
    HappyEyeballs, PluggableCandidate, PluggableError, PluggableKind, PluggableTransport,
};
use crate::reality::RealityTransport;
use crate::rendezvous::{BootstrapConfig, RendezvousError, TransportSpec};
use crate::shadowsocks::ShadowsocksTransport;
use crate::transport::Connection;
use crate::webtransport_pt::WebTransportPt;

/// Failure modes of the bootstrap dialer.
#[derive(Debug, Error)]
pub enum SelectorError {
    /// The bootstrap config failed verification (signature / version / expiry /
    /// clock). Surfaced only by [`BootstrapDialer::from_signed_config`].
    #[error("bootstrap config verification failed: {0}")]
    Verify(#[from] RendezvousError),

    /// No transport candidate could be built from the config — either the
    /// `transports` list was empty, or every spec's config was rejected by its
    /// `Transport::new`. Fail-closed: no transport, no connection.
    #[error("no usable transport candidate in bootstrap config ({tried} spec(s) tried)")]
    NoCandidates {
        /// How many `TransportSpec`s were in the config.
        tried: usize,
    },

    /// The happy-eyeballs race failed — every candidate failed to connect
    /// within the deadline (or the candidate set was empty). Never a silent
    /// downgrade (invariant #4).
    #[error("transport dial failed: {0}")]
    Dial(#[from] PluggableError),
}

/// Dials the best available censorship-resistant transport from a verified
/// [`BootstrapConfig`] (ADR-0025).
pub struct BootstrapDialer {
    policy: HappyEyeballs,
}

impl BootstrapDialer {
    /// Build a dialer from a config the caller has **already verified** (via
    /// [`BootstrapConfig::verify`]). Prefer [`Self::from_signed_config`] unless
    /// verification genuinely happened elsewhere.
    ///
    /// # Errors
    ///
    /// [`SelectorError::NoCandidates`] if no spec produced a usable transport.
    pub fn from_verified_config(config: &BootstrapConfig) -> Result<Self, SelectorError> {
        let candidates = build_candidates(config);
        if candidates.is_empty() {
            return Err(SelectorError::NoCandidates {
                tried: config.transports.len(),
            });
        }
        Ok(Self {
            policy: HappyEyeballs::new(candidates),
        })
    }

    /// Verify the config (signature + version + expiry + clock) against
    /// `trust_anchor` at `now`, **then** build — the entry point that cannot
    /// skip the gate.
    ///
    /// # Errors
    ///
    /// [`SelectorError::Verify`] if the config fails verification;
    /// [`SelectorError::NoCandidates`] if it verifies but yields no transport.
    pub fn from_signed_config(
        config: &BootstrapConfig,
        trust_anchor: &VerifyingKey,
        now: SystemTime,
    ) -> Result<Self, SelectorError> {
        config.verify(trust_anchor, now)?;
        Self::from_verified_config(config)
    }

    /// Race the candidates and return the first to connect, or a fail-closed
    /// error. The candidate ORDER is the config's transport order.
    ///
    /// # Errors
    ///
    /// [`SelectorError::Dial`] if every candidate fails within `deadline`.
    pub async fn dial(
        &self,
        endpoint: &Endpoint,
        deadline: Duration,
    ) -> Result<(PluggableKind, Box<dyn Connection>), SelectorError> {
        self.policy
            .connect(endpoint, deadline)
            .await
            .map_err(SelectorError::Dial)
    }

    /// The candidate kinds in race (config) order — for logging / metrics.
    #[must_use]
    pub fn candidate_kinds(&self) -> Vec<PluggableKind> {
        self.policy.kinds()
    }
}

/// Map each [`TransportSpec`] to its `Transport`, skipping any whose config is
/// rejected (logged at `warn`, kind only — never the secret-bearing spec). The
/// caller treats an empty result as fail-closed.
fn build_candidates(config: &BootstrapConfig) -> Vec<PluggableCandidate> {
    let mut out = Vec::with_capacity(config.transports.len());
    for spec in &config.transports {
        let (kind, built) = match spec {
            TransportSpec::Reality(cfg) => (
                PluggableKind::Reality,
                RealityTransport::new(cfg.clone())
                    .map(|t| PluggableCandidate::new(PluggableKind::Reality, t)),
            ),
            TransportSpec::Shadowsocks2022(cfg) => (
                PluggableKind::Shadowsocks2022,
                ShadowsocksTransport::new(cfg.clone())
                    .map(|t| PluggableCandidate::new(PluggableKind::Shadowsocks2022, t)),
            ),
            TransportSpec::WebTransport(cfg) => (
                PluggableKind::WebTransport,
                WebTransportPt::new(cfg.clone())
                    .map(|t| PluggableCandidate::new(PluggableKind::WebTransport, t)),
            ),
            TransportSpec::Masque(cfg) => (
                PluggableKind::Masque,
                MasqueTransport::new(cfg.clone())
                    .map(|t| PluggableCandidate::new(PluggableKind::Masque, t)),
            ),
        };
        match built {
            Ok(candidate) => out.push(candidate),
            Err(error) => {
                // Log the KIND only — the spec carries secrets (tag_secret,
                // PSK, bearer) that must never reach the logs.
                tracing::warn!(
                    ?kind,
                    %error,
                    "skipping un-buildable transport spec from bootstrap config"
                );
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use lakko_crypto::signing::SigningKey;
    use std::time::UNIX_EPOCH;

    use crate::reality::RealityConfig;
    use crate::rendezvous::SCHEMA_VERSION;

    fn valid_reality() -> RealityConfig {
        RealityConfig {
            sni: "example.invalid".into(),
            tag_secret_b64: B64.encode([0xCA_u8; 32]),
            short_id_hex: hex::encode([0xAB_u8; 8]),
            allowed_inner_servers: vec!["inner.lakko.me".into()],
            remote: "1.2.3.4:443".into(),
            require_server_response: true,
        }
    }

    fn invalid_reality() -> RealityConfig {
        // Wrong tag_secret length (3 bytes, not 32) — RealityTransport::new
        // rejects it.
        RealityConfig {
            tag_secret_b64: "AAAA".into(),
            ..valid_reality()
        }
    }

    fn config_with(transports: Vec<TransportSpec>) -> BootstrapConfig {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        BootstrapConfig {
            version: SCHEMA_VERSION,
            issued_at: now,
            expires_at: now + 600,
            transports,
            signature_hex: String::new(),
        }
    }

    #[test]
    fn empty_transports_is_no_candidates() {
        let cfg = config_with(vec![]);
        match BootstrapDialer::from_verified_config(&cfg) {
            Err(SelectorError::NoCandidates { tried }) => assert_eq!(tried, 0),
            Err(e) => panic!("expected NoCandidates, got {e:?}"),
            Ok(_) => panic!("expected NoCandidates, got Ok"),
        }
    }

    #[test]
    fn all_specs_unbuildable_is_no_candidates() {
        // A single invalid Reality spec is skipped, leaving an empty set.
        let cfg = config_with(vec![TransportSpec::Reality(invalid_reality())]);
        match BootstrapDialer::from_verified_config(&cfg) {
            Err(SelectorError::NoCandidates { tried }) => assert_eq!(tried, 1),
            Err(e) => panic!("expected NoCandidates, got {e:?}"),
            Ok(_) => panic!("expected NoCandidates, got Ok"),
        }
    }

    #[test]
    fn valid_spec_builds_one_candidate() {
        let cfg = config_with(vec![TransportSpec::Reality(valid_reality())]);
        let dialer = BootstrapDialer::from_verified_config(&cfg).expect("builds");
        assert_eq!(dialer.candidate_kinds(), vec![PluggableKind::Reality]);
    }

    #[test]
    fn invalid_spec_is_skipped_valid_one_survives_in_order() {
        // [invalid Reality, valid Reality] → only the valid one survives.
        let cfg = config_with(vec![
            TransportSpec::Reality(invalid_reality()),
            TransportSpec::Reality(valid_reality()),
        ]);
        let dialer = BootstrapDialer::from_verified_config(&cfg).expect("builds");
        assert_eq!(dialer.candidate_kinds(), vec![PluggableKind::Reality]);
    }

    #[test]
    fn from_signed_config_rejects_wrong_trust_anchor() {
        let signer = SigningKey::generate();
        let attacker = SigningKey::generate();
        let mut cfg = config_with(vec![TransportSpec::Reality(valid_reality())]);
        cfg.sign(&signer).unwrap();
        match BootstrapDialer::from_signed_config(
            &cfg,
            &attacker.verifying_key(),
            SystemTime::now(),
        ) {
            Err(SelectorError::Verify(RendezvousError::SignatureRejected)) => {},
            Err(e) => panic!("expected Verify(SignatureRejected), got {e:?}"),
            Ok(_) => panic!("expected Verify(SignatureRejected), got Ok"),
        }
    }

    #[test]
    fn from_signed_config_accepts_valid_signature() {
        let signer = SigningKey::generate();
        let mut cfg = config_with(vec![TransportSpec::Reality(valid_reality())]);
        cfg.sign(&signer).unwrap();
        let dialer =
            BootstrapDialer::from_signed_config(&cfg, &signer.verifying_key(), SystemTime::now())
                .expect("verifies + builds");
        assert_eq!(dialer.candidate_kinds(), vec![PluggableKind::Reality]);
    }
}
