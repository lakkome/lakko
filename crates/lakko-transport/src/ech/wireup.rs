// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Wire-up helpers — bridge a raw ECH config-list byte
//! payload (from DNS HTTPS records or a signed bootstrap
//! config) into a fully-built
//! [`rustls::client::EchMode`] ready to feed into
//! [`rustls::ClientConfig::builder_with_provider(...).with_ech(mode)`].
//!
//! ## Two-stage validation
//!
//! 1. **Defense-in-depth pre-parse** — runs the bytes
//!    through [`super::parse_ech_config_list`] first.
//!    rustls's own parser inside `EchConfig::new` does
//!    its own validation, but our parser is version-pinned
//!    (0xfe0d only), bounds-checked (16 KiB outer, 4 KiB
//!    per inner, 16 inner max), and the audit-pinned T-12
//!    gate. A spoofed DNS answer that bypasses one parser
//!    can still be refused by the other.
//! 2. **rustls `EchConfig::new`** — selects a compatible
//!    HPKE suite from
//!    [`super::adapter::ALL_SUPPORTED_SUITES`] (the three
//!    DH-KEM-X25519-HKDF-SHA256 suites). If no suite
//!    matches the published config, we refuse loudly
//!    rather than fall through to GREASE — silent
//!    downgrade to fake-ECH would defeat the threat
//!    model.
//!
//! ## Per-transport wire-up status
//!
//! | Transport | ECH wire-up | Reason |
//! |-----------|-------------|--------|
//! | [`crate::http::HttpTransport`] | **landed** | `HttpTransportConfig::ech_config_list` is plumbed into `rustls::ClientConfig::builder_with_provider(...).with_ech(...)`; bytes validated twice (our parser + rustls's) before the handshake. |
//! | [`crate::webtransport_pt::WebTransportPt`] | **landed** | `WebTransportConfig::ech_config_list_b64` carries the bytes; when set, the connect path bypasses `web-transport-quinn::ClientBuilder` (which does not surface `with_ech`) and assembles the rustls + quinn::ClientConfig manually with a Lakko-side `CertHashVerifier` replicating the W3C `serverCertificateHashes` semantics. Refuses ECH-without-cert-pins at parse time per `WebTransportConfigError::EchRequiresCertHashes`. |
//! | [`crate::reality::transport::RealityTransport`] | **not applicable** | REALITY and ECH are mutually exclusive — both want to own the SNI / `legacy_session_id` slot of the outer ClientHello (REALITY embeds a HMAC authenticator there; ECH replaces SNI entirely with an HPKE-sealed inner ClientHello). A client cannot do both on the same handshake — and REALITY already provides SNI-hiding via its own mechanism, so there is no value in stacking ECH on top. The two transports give the caller two independent SNI-hiding options. |

use thiserror::Error;

use rustls::client::{EchConfig as RustlsEchConfig, EchMode};
use rustls::pki_types::EchConfigListBytes;

use crate::ech::{parse_ech_config_list, EchError};

use super::adapter::ALL_SUPPORTED_SUITES;

/// Failure modes the wire-up helper surfaces. Branch on
/// the variant — never on the message string.
#[derive(Debug, Error)]
pub enum EchWireUpError {
    /// The defense-in-depth parser refused the bytes (bad
    /// version, oversize, malformed inner config). See
    /// [`EchError`] for which check failed.
    #[error("ECH config-list parse failed: {0}")]
    ParserRefused(#[source] EchError),
    /// rustls's own parser inside `EchConfig::new`
    /// refused the bytes, or no inner config matched any
    /// HPKE suite we ship. The error string is opaque
    /// (rustls's own type) because the cases are not
    /// distinct from rustls's surface.
    #[error("rustls EchConfig::new refused payload: {0}")]
    RustlsRefused(String),
}

/// Build an [`EchMode::Enable`] from an attacker-touchable
/// ECH config-list byte payload.
///
/// The bytes come from a DNS HTTPS record (RFC 9460
/// section 7.2.2 `ech` parameter), which means a
/// recursive resolver or any cache between it and us
/// can spoof them. We parse twice (our parser + rustls's)
/// before they enter the TLS handshake.
///
/// # Errors
///
/// See [`EchWireUpError`].
pub fn build_ech_mode_from_bytes(bytes: &[u8]) -> Result<EchMode, EchWireUpError> {
    // Defense-in-depth: refuse obvious garbage before
    // handing the bytes to rustls. T-12 gate.
    parse_ech_config_list(bytes).map_err(EchWireUpError::ParserRefused)?;

    // rustls owns the canonical parser. `EchConfigListBytes`
    // is just a typed Vec<u8> wrapper around the raw payload.
    let ech_bytes = EchConfigListBytes::from(bytes.to_vec());
    let cfg = RustlsEchConfig::new(ech_bytes, ALL_SUPPORTED_SUITES)
        .map_err(|e| EchWireUpError::RustlsRefused(e.to_string()))?;
    Ok(EchMode::Enable(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty payload — our parser refuses first (truncated
    /// wire header before any inner config). Confirms the
    /// defense-in-depth gate fires on degenerate input.
    #[test]
    fn empty_payload_refused_by_parser() {
        let err = build_ech_mode_from_bytes(&[]).expect_err("must refuse empty");
        match err {
            EchWireUpError::ParserRefused(_) => (),
            other @ EchWireUpError::RustlsRefused(_) => {
                panic!("wrong variant: {other:?}")
            },
        }
    }

    /// Junk bytes that look like a valid outer length
    /// prefix but contain no recognisable inner config —
    /// rustls's own parser refuses (after our parser
    /// passes the outer envelope check).
    ///
    /// The exact branch (parser vs rustls) depends on
    /// where the junk fails; the test just confirms we
    /// refuse rather than silently constructing a useless
    /// `EchMode`.
    #[test]
    fn malformed_inner_config_refused() {
        // 2-byte outer length = 4, then 4 bytes of junk that
        // doesn't look like any ECH version.
        let payload = vec![0x00, 0x04, 0xff, 0xff, 0x00, 0x00];
        let err = build_ech_mode_from_bytes(&payload).expect_err("must refuse junk");
        let _ = err; // either parser or rustls is acceptable.
    }
}
