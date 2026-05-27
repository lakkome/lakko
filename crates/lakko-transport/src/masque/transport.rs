// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `MasqueTransport` — `Transport` trait impl for the
//! MASQUE / CONNECT-UDP pluggable transport.
//!
//! Currently still returns
//! [`Error::NotImplemented`] on `connect`. The
//! cryptographic / wire-format foundations
//! ([`super::config`], [`super::request`],
//! [`super::datagram`]) are all shipped + tested; the
//! missing piece is the h3 + quinn integration that:
//!
//! 1. Opens a quinn HTTP/3 connection to
//!    [`super::config::MasqueProxyUrl`].
//! 2. Sends the [`super::request::ConnectUdpRequest`] over
//!    an extended-CONNECT request stream via the `h3`
//!    crate.
//! 3. On `2xx` response, multiplexes outbound UDP payloads
//!    through [`super::datagram::encode_udp_payload`] +
//!    `quinn::Connection::send_datagram`, and demultiplexes
//!    inbound datagrams through
//!    [`super::datagram::decode_udp_payload`].
//!
//! The `h3-datagram` crate is at 0.0.2 (very early) on
//! crates.io; the handshake sub-sprint either pins to it
//! or writes the QUIC DATAGRAM frame handling directly on
//! top of `quinn`'s `send_datagram` / `read_datagram` API.
//! Tracked as `S-5-masque-handshake`.

use async_trait::async_trait;

use crate::endpoint::Endpoint;
use crate::error::{Error, Result};
use crate::masque::config::MasqueConfig;
use crate::transport::{Connection, Transport};

/// MASQUE transport — typed wrapper around a parsed
/// [`MasqueConfig`].
#[derive(Debug)]
pub struct MasqueTransport {
    cfg: MasqueConfig,
}

impl MasqueTransport {
    /// Build the transport from a parsed config. Validates
    /// every URL / target / bearer constraint upfront so a
    /// bad bootstrap config surfaces at construction, not
    /// at connect time.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when
    /// [`MasqueConfig::parse`] fails.
    pub fn new(cfg: MasqueConfig) -> Result<Self> {
        cfg.parse()
            .map_err(|e| Error::Config(format!("masque config: {e}")))?;
        Ok(Self { cfg })
    }

    /// Borrow the underlying config — handy for tests + for
    /// the future h3+quinn integration layer that needs to
    /// read the proxy URL / inner target / bearer.
    #[must_use]
    pub fn config(&self) -> &MasqueConfig {
        &self.cfg
    }
}

#[async_trait]
impl Transport for MasqueTransport {
    async fn connect(&self, _endpoint: &Endpoint) -> Result<Box<dyn Connection>> {
        Err(Error::NotImplemented {
            feature: "masque h3+quinn handshake",
            sprint: "S-5-masque-handshake",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn happy_config() -> MasqueConfig {
        MasqueConfig {
            proxy_url: "https://masque.example.com/".into(),
            inner_target: "lakko.me:443".into(),
            bearer: Some("opaque-token".into()),
        }
    }

    #[test]
    fn new_accepts_valid_config() {
        let cfg = happy_config();
        let t = MasqueTransport::new(cfg.clone()).expect("valid config");
        assert_eq!(t.config(), &cfg);
    }

    #[test]
    fn new_rejects_non_https_proxy() {
        let mut cfg = happy_config();
        cfg.proxy_url = "http://example.com/".into();
        let err = MasqueTransport::new(cfg).expect_err("non-https must reject");
        match err {
            Error::Config(msg) => assert!(msg.contains("https"), "msg: {msg}"),
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn new_rejects_bad_inner_target() {
        let mut cfg = happy_config();
        cfg.inner_target = "lakko.me".into();
        let err = MasqueTransport::new(cfg).expect_err("missing-port must reject");
        match err {
            Error::Config(_) => {},
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn connect_returns_not_implemented() {
        let t = MasqueTransport::new(happy_config()).unwrap();
        let endpoint = Endpoint::hostname("masque.example.com", 443);
        let err = t
            .connect(&endpoint)
            .await
            .err()
            .expect("connect must return NotImplemented");
        match err {
            Error::NotImplemented { feature, sprint } => {
                assert!(feature.contains("h3+quinn"));
                assert!(sprint.contains("S-5-masque-handshake"));
            },
            other => panic!("expected NotImplemented, got {other:?}"),
        }
    }
}
