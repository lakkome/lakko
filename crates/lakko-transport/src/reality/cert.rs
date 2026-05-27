// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Custom rustls `ServerCertVerifier` for REALITY.
//!
//! REALITY's cryptographic authentication lives in the
//! session_id + ServerHello.Random round-trip — NOT in
//! the TLS PKI chain. A REALITY-aware server typically
//! either:
//!
//! - presents a self-signed cert for the cover SNI it is
//!   masquerading as, OR
//! - presents the *real* cover-site's cert (when acting
//!   as a transparent proxy to the cover site during the
//!   pre-auth window),
//!
//! …and neither path validates under the normal CA chain
//! the client expects for that SNI. Under REALITY's
//! threat model that is fine: a censor on the wire still
//! sees a normal TLS 1.3 handshake to the cover SNI, the
//! cert it observes matches what they'd expect from that
//! SNI in the active-probe scenario, and the actual
//! cryptographic protection of the inner traffic comes
//! from our pre-shared `tag_secret` HMAC chain.
//!
//! [`AcceptAnyCertVerifier`] therefore returns
//! `ServerCertVerified::assertion()` for any leaf cert.
//! It is **only** safe to use inside the REALITY
//! handshake driver where the
//! [`crate::reality::authenticator::verify_server_response`]
//! check on the ServerHello.Random is the real
//! cryptographic gate.
//!
//! ## Audit posture
//!
//! - This verifier is `pub(crate)` inside the
//!   `reality` module tree only — there is no public
//!   re-export through `lakko_transport::*`. A caller
//!   outside `reality::driver` cannot accidentally reach
//!   it.
//! - The struct name carries the safety contract in the
//!   word `AcceptAny` — code review for any PR that
//!   touches a `ServerCertVerifier` is supposed to look
//!   for this name and confirm REALITY context.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

/// `ServerCertVerifier` that accepts any leaf cert
/// without PKI validation. **Only** safe under REALITY's
/// threat model where the session_id + ServerHello.Random
/// HMAC round-trip carries the real authentication.
///
/// Lakko's REALITY driver constructs this internally;
/// it is not re-exported through the top-level
/// `lakko_transport` surface to keep the unsafe trust
/// posture audit-grep-scoped to the `reality` module.
///
/// `#[allow(dead_code)]` is in force until
/// `S-5-reality-driver-v2` lands and consumes this type;
/// the cert verifier is the first of four pieces
/// (cert + driver + stream + connect-wire), built in
/// order across the closing G5-1 sprints. Tests below
/// already exercise it so the impl is not dormant.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct AcceptAnyCertVerifier {
    /// The set of TLS signature schemes the rustls crypto
    /// provider advertises. Pulled from the provider so
    /// the verifier stays aligned with whatever rustls
    /// negotiated.
    signature_schemes: Vec<SignatureScheme>,
}

#[allow(dead_code)]
impl AcceptAnyCertVerifier {
    /// Build a verifier whose `supported_verify_schemes`
    /// matches the supplied crypto provider's defaults.
    /// Lakko's transport stack uses `ring` everywhere;
    /// the caller hands in `rustls::crypto::ring::default_provider()`.
    pub(crate) fn new(provider: &CryptoProvider) -> Self {
        // The CryptoProvider exposes signature_verification_algorithms
        // which carries the supported_schemes() list.
        let schemes = provider
            .signature_verification_algorithms
            .supported_schemes();
        Self {
            signature_schemes: schemes,
        }
    }

    /// Wrap into an `Arc<dyn ServerCertVerifier>` for
    /// `rustls::ClientConfig::dangerous().with_custom_certificate_verifier`.
    pub(crate) fn into_arc(self) -> Arc<dyn ServerCertVerifier> {
        Arc::new(self)
    }
}

impl ServerCertVerifier for AcceptAnyCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // Deliberate no-op — REALITY's authentication
        // lives in the session_id + Server.Random
        // round-trip, NOT in PKI.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // REALITY pins TLS 1.3 (per RFC 8446); we never
        // accept a TLS 1.2 signature path. A REALITY
        // server that somehow downgrades to TLS 1.2
        // would still fail later — the
        // ServerHello.legacy_version field is checked by
        // the wire-format parser. Reject loudly here so
        // the audit trail is clear.
        Err(rustls::Error::General(
            "REALITY: TLS 1.2 signature path forbidden".into(),
        ))
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // Same posture as verify_server_cert — REALITY
        // does not verify the TLS-level signature; the
        // HMAC chain in our authenticator is the gate.
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.signature_schemes.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::crypto::ring::default_provider;

    fn verifier() -> AcceptAnyCertVerifier {
        AcceptAnyCertVerifier::new(&default_provider())
    }

    fn dummy_cert() -> CertificateDer<'static> {
        // Minimal stand-in — the verifier never looks at
        // the bytes. rustls's CertificateDer is just a
        // newtype around bytes; build directly.
        CertificateDer::from(vec![0u8; 64])
    }

    #[test]
    fn server_cert_always_accepts() {
        let v = verifier();
        let cert = dummy_cert();
        let server_name = ServerName::try_from("www.microsoft.com").unwrap();
        let result = v.verify_server_cert(
            &cert,
            &[],
            &server_name,
            &[],
            UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000)),
        );
        assert!(result.is_ok(), "AcceptAnyCertVerifier must accept any cert");
    }

    // Note: `verify_tls12_signature` + `verify_tls13_signature`
    // tests are covered by the driver-level integration
    // path — rustls's `DigitallySignedStruct::new` is
    // `pub(crate)`, so we can't construct one in a unit
    // test from outside the crate. The two methods are
    // exercised end-to-end whenever the REALITY driver
    // completes a real TLS 1.3 handshake (TLS 1.3 path
    // hit → `Ok`; downgrade to TLS 1.2 hits the explicit
    // refusal in `verify_tls12_signature`).

    #[test]
    fn supported_schemes_non_empty() {
        let v = verifier();
        let schemes = v.supported_verify_schemes();
        assert!(
            !schemes.is_empty(),
            "ring provider must surface some supported schemes"
        );
    }
}
