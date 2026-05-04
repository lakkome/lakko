// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! SPKI certificate pinning for the HTTP REST client.
//!
//! ## Why pin SPKI, not the certificate
//!
//! Pinning the whole leaf certificate ties the client to a specific
//! signed object — when the cert expires, every pinned client breaks
//! until they ship an update. Pinning the
//! `SubjectPublicKeyInfo` (SPKI) — the public-key portion — is stable
//! across cert rotations as long as the operator reuses the same
//! keypair, which is the normal Let's Encrypt / ACME flow.
//!
//! Phase 1.8 ships a static `SpkiPinSet`. Phase 5 (anti-censorship)
//! makes the pin set TUF-signed data delivered through the bootstrap
//! rendezvous service, so adversary-rotated CA-issued certificates
//! can't slip past silently.
//!
//! ## How the verifier works
//!
//! [`PinnedVerifier`] implements [`rustls::client::danger::ServerCertVerifier`]
//! and **fully replaces** rustls's normal X.509 chain validation.
//! It only checks: "does the leaf cert's SPKI hash match one of our
//! pins?". This is the right model for a closed-set client (we know
//! exactly which servers it talks to, and we burn-in their public
//! keys); using the system CA bundle would mean trusting any CA the
//! OS trusts, which for a security messenger is a much larger blast
//! radius than necessary.
//!
//! TLS-level signature verification on the handshake transcript still
//! runs inside rustls — pinning is in addition to (and replaces only
//! the chain-of-trust step of) TLS auth.

use std::collections::HashSet;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// SHA-256 of a leaf certificate's `SubjectPublicKeyInfo` (the
/// `tbsCertificate.subjectPublicKeyInfo` ASN.1 substructure, rendered
/// in DER).
pub type SpkiHash = [u8; 32];

/// A set of acceptable server SPKI hashes. The HTTP client refuses
/// to connect unless the leaf cert's SPKI hashes to one of these.
#[derive(Clone, Debug, Default)]
pub struct SpkiPinSet {
    pins: HashSet<SpkiHash>,
}

impl SpkiPinSet {
    /// Construct a pin set from a list of pre-computed SPKI hashes.
    /// This is the path Phase 5's TUF-delivered config will use —
    /// the bootstrap rendezvous service hands out signed JSON, the
    /// client decodes the hashes, plumbs them in here.
    #[must_use]
    pub fn from_hashes<I>(hashes: I) -> Self
    where
        I: IntoIterator<Item = SpkiHash>,
    {
        Self {
            pins: hashes.into_iter().collect(),
        }
    }

    /// Compute SPKI hash for a DER-encoded leaf certificate.
    ///
    /// # Errors
    ///
    /// Returns [`PinError::InvalidCertDer`] if the bytes aren't a
    /// well-formed X.509 certificate.
    pub fn spki_hash_of(cert_der: &[u8]) -> Result<SpkiHash, PinError> {
        let (_, parsed) = x509_parser::parse_x509_certificate(cert_der)
            .map_err(|e| PinError::InvalidCertDer(e.to_string()))?;
        // `subject_pki.raw` is the DER of the `SubjectPublicKeyInfo`
        // SEQUENCE — including the algorithm identifier. This is the
        // standard pin material per RFC 7469 §2.4.
        let spki_der = parsed.tbs_certificate.subject_pki.raw;
        let digest = Sha256::digest(spki_der);
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Ok(out)
    }

    /// `true` if the pin set contains the SHA-256 of `cert_der`'s SPKI.
    /// Returns `false` for malformed DER too — that's the
    /// security-correct default (untrusted bytes do not match).
    #[must_use]
    pub fn matches_der(&self, cert_der: &[u8]) -> bool {
        match Self::spki_hash_of(cert_der) {
            Ok(hash) => self.pins.contains(&hash),
            Err(_) => false,
        }
    }

    /// Number of distinct pins in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pins.len()
    }

    /// `true` if the set has no pins (which means **every** connection
    /// will be refused — `Default::default()` produces this state).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pins.is_empty()
    }
}

/// Errors when working with pinned certs.
#[derive(Error, Debug)]
pub enum PinError {
    /// Failed to parse the certificate DER.
    #[error("invalid certificate DER: {0}")]
    InvalidCertDer(String),
}

// ============================================================================
// rustls verifier
// ============================================================================

/// `ServerCertVerifier` that accepts the connection iff the leaf
/// cert's SPKI is in the configured [`SpkiPinSet`].
///
/// All chain validation is bypassed — we don't care about the OS
/// trust store, intermediate CAs, OCSP, etc. The only authority is
/// the pin set.
#[derive(Debug)]
pub struct PinnedVerifier {
    pins: SpkiPinSet,
    /// Signature algorithms supported by the rustls crypto provider.
    /// rustls 0.23 wants a list to validate the signed handshake
    /// transcript with (orthogonal to chain validation).
    signature_algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl PinnedVerifier {
    /// Build a verifier from a pin set. The crypto provider is taken
    /// from the rustls process default — callers configure that
    /// elsewhere (typically `ring`).
    ///
    /// # Errors
    ///
    /// Returns an error if the pin set is empty (all connections
    /// would fail — almost certainly a misconfiguration, fail fast).
    pub fn new(pins: SpkiPinSet) -> Result<Self, PinError> {
        if pins.is_empty() {
            return Err(PinError::InvalidCertDer(
                "empty pin set rejects every connection — refusing to construct".to_string(),
            ));
        }
        let signature_algorithms = match rustls::crypto::CryptoProvider::get_default() {
            Some(p) => p.signature_verification_algorithms,
            None => rustls::crypto::ring::default_provider().signature_verification_algorithms,
        };
        Ok(Self {
            pins,
            signature_algorithms,
        })
    }

    /// Wrap into an `Arc<dyn ServerCertVerifier>` for plumbing into
    /// `rustls::ClientConfig::builder()`.
    #[must_use]
    pub fn into_arc(self) -> Arc<dyn ServerCertVerifier> {
        Arc::new(self)
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if self.pins.matches_der(end_entity.as_ref()) {
            Ok(ServerCertVerified::assertion())
        } else {
            // We deliberately don't include the cert hash in the
            // error message — log it separately if you need it.
            // Embedding it here would surface in TLS-level errors.
            tracing::warn!("SPKI pin mismatch — refusing TLS handshake");
            Err(rustls::Error::General(
                "lakko-transport: SPKI pin mismatch".to_string(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.signature_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.signature_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.signature_algorithms.supported_schemes()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertificateParams, KeyPair};

    /// Generate a self-signed cert via rcgen and return its DER.
    /// rcgen is already a workspace dep for the QUIC backend.
    fn fresh_self_signed() -> Vec<u8> {
        let params =
            CertificateParams::new(vec!["localhost".to_string()]).expect("certificate params");
        let key_pair = KeyPair::generate().expect("rcgen keypair");
        let cert = params.self_signed(&key_pair).expect("rcgen self-sign");
        cert.der().to_vec()
    }

    #[test]
    fn spki_hash_is_deterministic_for_same_cert() {
        let der = fresh_self_signed();
        let h1 = SpkiPinSet::spki_hash_of(&der).unwrap();
        let h2 = SpkiPinSet::spki_hash_of(&der).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn spki_hash_differs_for_different_certs() {
        let h1 = SpkiPinSet::spki_hash_of(&fresh_self_signed()).unwrap();
        let h2 = SpkiPinSet::spki_hash_of(&fresh_self_signed()).unwrap();
        assert_ne!(h1, h2, "fresh keypairs must yield distinct SPKI hashes");
    }

    #[test]
    fn matches_der_returns_true_for_pinned_cert() {
        let der = fresh_self_signed();
        let hash = SpkiPinSet::spki_hash_of(&der).unwrap();
        let pins = SpkiPinSet::from_hashes([hash]);
        assert!(pins.matches_der(&der));
    }

    #[test]
    fn matches_der_returns_false_for_unpinned_cert() {
        let pinned_der = fresh_self_signed();
        let other_der = fresh_self_signed();
        let pins = SpkiPinSet::from_hashes([SpkiPinSet::spki_hash_of(&pinned_der).unwrap()]);
        assert!(pins.matches_der(&pinned_der));
        assert!(!pins.matches_der(&other_der));
    }

    #[test]
    fn matches_der_returns_false_for_malformed_input() {
        let pins = SpkiPinSet::from_hashes([[0u8; 32]]);
        // Truncated / random bytes are not valid DER → no match.
        assert!(!pins.matches_der(&[]));
        assert!(!pins.matches_der(b"not a certificate"));
    }

    #[test]
    fn pin_set_can_hold_multiple_hashes() {
        let der1 = fresh_self_signed();
        let der2 = fresh_self_signed();
        let pins = SpkiPinSet::from_hashes([
            SpkiPinSet::spki_hash_of(&der1).unwrap(),
            SpkiPinSet::spki_hash_of(&der2).unwrap(),
        ]);
        assert_eq!(pins.len(), 2);
        assert!(pins.matches_der(&der1));
        assert!(pins.matches_der(&der2));
    }

    #[test]
    fn pinned_verifier_construction_rejects_empty_pinset() {
        // Install a default crypto provider for this test (idempotent
        // — already done by other tests in the same process).
        let _ = rustls::crypto::ring::default_provider().install_default();
        let err = PinnedVerifier::new(SpkiPinSet::default()).unwrap_err();
        assert!(matches!(err, PinError::InvalidCertDer(_)));
    }

    #[test]
    fn pinned_verifier_constructs_with_non_empty_set() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let der = fresh_self_signed();
        let pins = SpkiPinSet::from_hashes([SpkiPinSet::spki_hash_of(&der).unwrap()]);
        let verifier = PinnedVerifier::new(pins).expect("verifier construction");
        // Sanity: it advertises some signature schemes (depends on
        // crypto provider, ring exposes a non-empty list).
        assert!(!verifier.supported_verify_schemes().is_empty());
    }
}
