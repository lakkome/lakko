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
    use crate::ech::ECH_VERSION_DRAFT_13;

    /// Build a valid one-config `ECHConfigList` on the wire,
    /// pinning X25519 + HKDF-SHA256 + ChaCha20-Poly1305 (one
    /// of the three suites our adapter exposes). Real ECH
    /// publishers MAY embed an actual X25519 public key
    /// here; we use a 32-byte placeholder since rustls
    /// validates structure + cipher-suite match, not the
    /// point itself, at this layer.
    ///
    /// Duplicated from `super::tests::build_one_config_list`
    /// because that helper is private to the parser tests.
    fn build_one_config_list() -> Vec<u8> {
        let config_id: u8 = 7;
        let kem_id: u16 = 0x0020; // X25519-HKDF-SHA256
                                  // 32-byte X25519 pubkey shape — rustls only checks
                                  // length + suite compatibility, not point validity,
                                  // when assembling `EchConfig`. A real publisher's
                                  // bytes would be a Curve25519 point.
        let public_key = vec![1u8; 32];
        let cipher_suites: Vec<u8> = vec![
            0x00, 0x01, // kdf: HKDF-SHA256
            0x00, 0x03, // aead: ChaCha20-Poly1305
        ];
        let max_name_len: u8 = 64;
        let public_name = b"cloudflare-ech.com";

        let mut body = Vec::new();
        body.push(config_id);
        body.extend_from_slice(&kem_id.to_be_bytes());
        body.extend_from_slice(
            &u16::try_from(public_key.len())
                .expect("len fits u16")
                .to_be_bytes(),
        );
        body.extend_from_slice(&public_key);
        body.extend_from_slice(
            &u16::try_from(cipher_suites.len())
                .expect("len fits u16")
                .to_be_bytes(),
        );
        body.extend_from_slice(&cipher_suites);
        body.push(max_name_len);
        body.push(u8::try_from(public_name.len()).expect("len fits u8"));
        body.extend_from_slice(public_name);
        // No extensions.
        body.extend_from_slice(&0u16.to_be_bytes());

        let mut config = Vec::new();
        config.extend_from_slice(&ECH_VERSION_DRAFT_13.to_be_bytes());
        config.extend_from_slice(
            &u16::try_from(body.len())
                .expect("len fits u16")
                .to_be_bytes(),
        );
        config.extend_from_slice(&body);

        let mut wire = Vec::new();
        wire.extend_from_slice(
            &u16::try_from(config.len())
                .expect("len fits u16")
                .to_be_bytes(),
        );
        wire.extend_from_slice(&config);
        wire
    }

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

    /// Positive integration — a wire-valid one-config list
    /// with an X25519-HKDF-SHA256 + ChaCha20-Poly1305 inner
    /// suite (one of the three suites Lakko advertises)
    /// must assemble into `EchMode::Enable(..)`. Confirms
    /// the parser → adapter suite-selection chain wires up
    /// end-to-end without an actual TLS handshake.
    #[test]
    fn valid_one_config_assembles_to_enable_mode() {
        let wire = build_one_config_list();
        let mode = build_ech_mode_from_bytes(&wire).expect("valid config assembles");
        // `EchMode::Enable(_)` is the expected variant —
        // `Grease(_)` would mean rustls fell back to a
        // synthetic placeholder, which our caller does NOT
        // want (silent downgrade).
        assert!(
            matches!(mode, rustls::client::EchMode::Enable(_)),
            "expected EchMode::Enable, got {mode:?}"
        );
    }

    /// A config-list that only advertises a KEM we do NOT
    /// support (DH-KEM-P-256, IANA 0x0010) must be refused
    /// by rustls's matcher — our `ALL_SUPPORTED_SUITES`
    /// only covers X25519. Confirms there is no silent
    /// downgrade onto an unintended suite.
    #[test]
    fn unsupported_kem_refused_by_rustls() {
        // Same shape as `build_one_config_list` but with
        // `kem_id = 0x0010` (DH-KEM-P-256-HKDF-SHA256),
        // which the adapter does not enable.
        let config_id: u8 = 7;
        let kem_id: u16 = 0x0010;
        // P-256 pubkey shape — 65-byte uncompressed point.
        let public_key = vec![4u8; 65];
        let cipher_suites: Vec<u8> = vec![0x00, 0x01, 0x00, 0x01]; // HKDF-SHA256 + AES-128-GCM
        let public_name = b"unused.example";

        let mut body = Vec::new();
        body.push(config_id);
        body.extend_from_slice(&kem_id.to_be_bytes());
        body.extend_from_slice(&65u16.to_be_bytes());
        body.extend_from_slice(&public_key);
        body.extend_from_slice(&4u16.to_be_bytes());
        body.extend_from_slice(&cipher_suites);
        body.push(64u8);
        body.push(u8::try_from(public_name.len()).unwrap());
        body.extend_from_slice(public_name);
        body.extend_from_slice(&0u16.to_be_bytes());

        let mut config = Vec::new();
        config.extend_from_slice(&ECH_VERSION_DRAFT_13.to_be_bytes());
        config.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
        config.extend_from_slice(&body);

        let mut wire = Vec::new();
        wire.extend_from_slice(&u16::try_from(config.len()).unwrap().to_be_bytes());
        wire.extend_from_slice(&config);

        let err = build_ech_mode_from_bytes(&wire).expect_err("unsupported KEM must be refused");
        match err {
            EchWireUpError::RustlsRefused(_) => (),
            EchWireUpError::ParserRefused(p) => {
                panic!("expected rustls refusal, got parser refusal: {p:?}")
            },
        }
    }
}
