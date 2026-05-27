// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `RealityTransport` — `Transport` trait impl for the
//! REALITY pluggable transport.
//!
//! Currently still returns
//! [`Error::NotImplemented`] on `connect`. The
//! cryptographic primitive (`reality::authenticator`)
//! and the typed config (`reality::config`) are both
//! shipped and tested; the missing piece is the TLS
//! handshake injection layer that:
//!
//! 1. Builds a TLS 1.3 ClientHello aimed at the
//!    cover-site SNI from [`RealityConfig::sni`].
//! 2. Overrides the 32-byte session_id slot with the
//!    output of [`crate::reality::authenticator::compute`].
//! 3. Verifies the server's response in ServerHello.Random
//!    (REALITY signals "I am the real REALITY backend, not
//!    the cover site") before any application data flows.
//!
//! rustls 0.23's public API does not expose a session_id
//! override hook today. The wire-up sprint either (a)
//! takes a dependency on a `rustls-hooks` fork OR (b)
//! writes the ClientHello payload by hand on top of a
//! raw TCP socket. Tracked in G5-1 closeout notes.

use async_trait::async_trait;

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::reality::config::RealityConfig;
use crate::transport::{Connection, Transport};

/// REALITY transport — typed wrapper around a parsed
/// [`RealityConfig`].
#[derive(Debug)]
pub struct RealityTransport {
    cfg: RealityConfig,
}

impl RealityTransport {
    /// Build the transport from a parsed config. Validates
    /// the config's lengths + formats upfront so a bad
    /// bootstrap config surfaces at construction, not at
    /// connect time.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] only when the
    /// underlying [`RealityConfig::parse`] fails. Other
    /// failure modes (network / TLS) surface from
    /// [`Self::connect`].
    pub fn new(cfg: RealityConfig) -> Result<Self> {
        // Validate the config eagerly so any wire-shape
        // surprise surfaces here, not at connect time.
        cfg.parse().map_err(|e| Error::NotImplemented {
            feature: "reality (bad config)",
            sprint: match e {
                crate::reality::config::RealityConfigError::BadTagSecretBase64 => {
                    "S-5-reality (tag_secret_b64 invalid)"
                },
                crate::reality::config::RealityConfigError::TagSecretLength { .. } => {
                    "S-5-reality (tag_secret wrong length)"
                },
                crate::reality::config::RealityConfigError::BadShortIdHex => {
                    "S-5-reality (short_id_hex invalid)"
                },
                crate::reality::config::RealityConfigError::ShortIdLength { .. } => {
                    "S-5-reality (short_id wrong length)"
                },
                crate::reality::config::RealityConfigError::EmptySni => "S-5-reality (sni empty)",
                crate::reality::config::RealityConfigError::EmptyRemote => {
                    "S-5-reality (remote empty)"
                },
            },
        })?;
        Ok(Self { cfg })
    }

    /// Borrow the underlying config — handy for tests and
    /// for the future TLS-handshake-injection layer that
    /// needs to read the SNI / remote endpoint.
    #[must_use]
    pub fn config(&self) -> &RealityConfig {
        &self.cfg
    }
}

#[async_trait]
impl Transport for RealityTransport {
    async fn connect(&self, _endpoint: &Endpoint) -> Result<Box<dyn Connection>> {
        // Foundation sprint (G5-1-foundation): the
        // authenticator primitive + config types are
        // shipped + tested. The TLS-handshake injection
        // is the next sub-sprint — see module-level docs
        // for the rustls-hook gap.
        Err(Error::NotImplemented {
            feature: "reality TLS handshake injection",
            sprint: "S-5-reality-handshake (rustls ClientHello hook)",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

    fn happy_config() -> RealityConfig {
        RealityConfig {
            sni: "www.microsoft.com".into(),
            tag_secret_b64: B64.encode([0xCA; 32]),
            short_id_hex: hex::encode([0xAB; 8]),
            allowed_inner_servers: vec!["lakko.me".into()],
            remote: "203.0.113.7:443".into(),
        }
    }

    #[test]
    fn new_accepts_valid_config() {
        let cfg = happy_config();
        let t = RealityTransport::new(cfg.clone()).expect("valid config");
        assert_eq!(t.config(), &cfg);
    }

    #[test]
    fn new_rejects_bad_tag_secret() {
        let mut cfg = happy_config();
        cfg.tag_secret_b64 = "!!!".into();
        let err = RealityTransport::new(cfg).expect_err("bad config must reject");
        match err {
            Error::NotImplemented { feature, sprint } => {
                assert_eq!(feature, "reality (bad config)");
                assert!(sprint.contains("tag_secret_b64 invalid"));
            },
            other => panic!("expected NotImplemented (bad config), got {other:?}"),
        }
    }

    #[test]
    fn new_rejects_empty_sni() {
        let mut cfg = happy_config();
        cfg.sni = String::new();
        let err = RealityTransport::new(cfg).expect_err("empty sni must reject");
        match err {
            Error::NotImplemented { sprint, .. } => {
                assert!(sprint.contains("sni empty"));
            },
            other => panic!("expected NotImplemented, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn connect_returns_not_implemented() {
        let t = RealityTransport::new(happy_config()).unwrap();
        let endpoint = crate::endpoint::Endpoint::hostname("203.0.113.7", 443);
        let err = t
            .connect(&endpoint)
            .await
            .err()
            .expect("connect must return NotImplemented");
        match err {
            Error::NotImplemented { feature, sprint } => {
                assert!(feature.contains("TLS handshake injection"));
                assert!(sprint.contains("rustls ClientHello hook"));
            },
            other => panic!("expected NotImplemented, got {other:?}"),
        }
    }
}
