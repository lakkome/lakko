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
//! - Every transport dials the address its signed spec names. REALITY and
//!   Shadowsocks-2022 take the `Endpoint` they are handed, so the dialer pins
//!   each of them to its signed `remote`: the endpoint a caller passes is the
//!   server it wants to reach, usually the blocked one, and the obfuscated
//!   listener lives elsewhere.
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

use async_trait::async_trait;

use crate::endpoint::Endpoint;
use crate::error::{Error, Result as TransportResult};
use crate::masque::MasqueTransport;
use crate::pluggable::{
    HappyEyeballs, PluggableCandidate, PluggableError, PluggableKind, PluggableTransport,
};
use crate::reality::RealityTransport;
use crate::rendezvous::{BootstrapConfig, RendezvousError, TransportSpec};
use crate::shadowsocks::ShadowsocksTransport;
use crate::transport::{Connection, Transport};
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
    /// Each candidate dials the address in its signed spec, so `endpoint` does
    /// not choose where a tunnel goes.
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

/// A transport that dials its signed address whatever endpoint it is handed.
struct AtSignedRemote<T> {
    inner: T,
    remote: Endpoint,
}

#[async_trait]
impl<T: Transport> Transport for AtSignedRemote<T> {
    async fn connect(&self, _endpoint: &Endpoint) -> TransportResult<Box<dyn Connection>> {
        self.inner.connect(&self.remote).await
    }
}

/// Parse a spec's `host:port`. An IP literal needs no resolver, which is the
/// point of signing one: a censor's DNS has no say in it.
fn parse_remote(remote: &str) -> TransportResult<Endpoint> {
    if let Ok(addr) = remote.parse() {
        return Ok(Endpoint::Direct(addr));
    }
    let bad = || Error::Config("transport remote is not host:port".to_owned());
    let (host, port) = remote.rsplit_once(':').ok_or_else(bad)?;
    if host.is_empty() || host.contains(':') {
        return Err(bad());
    }
    let port = port.parse().map_err(|_| bad())?;
    Ok(Endpoint::Hostname {
        host: host.to_owned(),
        port,
    })
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
                parse_remote(&cfg.remote).and_then(|remote| {
                    RealityTransport::new(cfg.clone()).map(|inner| {
                        PluggableCandidate::new(
                            PluggableKind::Reality,
                            AtSignedRemote { inner, remote },
                        )
                    })
                }),
            ),
            TransportSpec::Shadowsocks2022(cfg) => (
                PluggableKind::Shadowsocks2022,
                parse_remote(&cfg.remote).and_then(|remote| {
                    ShadowsocksTransport::new(cfg.clone()).map(|inner| {
                        PluggableCandidate::new(
                            PluggableKind::Shadowsocks2022,
                            AtSignedRemote { inner, remote },
                        )
                    })
                }),
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
    #[test]
    fn a_remote_parses_as_an_address_or_a_name() {
        assert!(matches!(
            parse_remote("1.2.3.4:443"),
            Ok(Endpoint::Direct(_))
        ));
        assert!(matches!(parse_remote("[::1]:443"), Ok(Endpoint::Direct(_))));
        match parse_remote("edge.example:8443") {
            Ok(Endpoint::Hostname { host, port }) => {
                assert_eq!(host, "edge.example");
                assert_eq!(port, 8443);
            },
            other => panic!("expected a hostname, got {other:?}"),
        }
        for bad in ["", "edge.example", ":443", "edge.example:port", "::1:443"] {
            assert!(parse_remote(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_spec_whose_remote_is_not_an_address_is_skipped() {
        let cfg = config_with(vec![
            TransportSpec::Reality(RealityConfig {
                remote: "no-port".into(),
                ..valid_reality()
            }),
            TransportSpec::Reality(valid_reality()),
        ]);
        let dialer = BootstrapDialer::from_verified_config(&cfg).expect("one candidate survives");
        assert_eq!(dialer.candidate_kinds(), vec![PluggableKind::Reality]);
    }

    /// The bug this pins: the two TCP transports used to dial whatever the
    /// caller named (the API host, blocked or not) and never read `remote`.
    #[tokio::test]
    async fn a_tcp_transport_dials_its_signed_remote_not_the_callers_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound = listener.local_addr().unwrap();
        let cfg = config_with(vec![TransportSpec::Reality(RealityConfig {
            remote: bound.to_string(),
            ..valid_reality()
        })]);
        let dialer = BootstrapDialer::from_verified_config(&cfg).unwrap();
        let reached = tokio::spawn(async move { listener.accept().await.is_ok() });

        // Port 1 on loopback accepts nothing: a dial that honoured the
        // endpoint would never reach the listener.
        let elsewhere = Endpoint::direct("127.0.0.1:1".parse().unwrap());
        let _ = dialer.dial(&elsewhere, Duration::from_secs(2)).await;

        let reached = tokio::time::timeout(Duration::from_secs(2), reached)
            .await
            .expect("the signed remote got a connection")
            .unwrap();
        assert!(reached);
    }
}
