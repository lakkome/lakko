// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MLS identity: signing keypair + credential + per-device storage provider.
//!
//! Phase 0 uses [`BasicCredential`] (an opaque user-chosen byte string).
//! X.509 credentials with Key Transparency log inclusion proofs land in
//! Phase 1+ per ADR-0001.

use openmls::credentials::{BasicCredential, CredentialWithKey};
use openmls::prelude::Ciphersuite;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::types::SignatureScheme;
use openmls_traits::OpenMlsProvider as _;

use crate::ciphersuite;
use crate::error::{Error, Result};

/// A user identity bound to a single device.
///
/// Owns:
/// - the device's MLS storage / crypto provider (in-memory for Phase 0)
/// - the long-term signing keypair
/// - the [`CredentialWithKey`] used as the MLS credential
///
/// Multi-device users have one [`Identity`] per device; the cross-device
/// linkage lives in the user-level DeviceGroup (Phase 2, see `lakko-sync`).
pub struct Identity {
    pub(crate) provider: OpenMlsRustCrypto,
    pub(crate) signature_keys: SignatureKeyPair,
    pub(crate) credential: CredentialWithKey,
    pub(crate) ciphersuite: Ciphersuite,
}

impl Identity {
    /// Generate a fresh identity with the given user-visible name.
    ///
    /// The name is wrapped in a [`BasicCredential`] and is opaque to the
    /// protocol; treat it as a user-display label, not as authentication.
    ///
    /// Uses [`ciphersuite::DEFAULT`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Identity`] if the signing keypair could not be
    /// generated or persisted to the provider's storage.
    pub fn generate(name: &str) -> Result<Self> {
        Self::generate_with_ciphersuite(name, ciphersuite::DEFAULT)
    }

    /// Generate a fresh identity bound to a specific (Lakko-supported)
    /// ciphersuite.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Identity`] for unsupported ciphersuites or any
    /// keypair / storage failure.
    pub fn generate_with_ciphersuite(name: &str, cs: Ciphersuite) -> Result<Self> {
        if !ciphersuite::is_supported(cs) {
            return Err(Error::Identity(format!("unsupported ciphersuite: {cs:?}")));
        }

        let provider = OpenMlsRustCrypto::default();
        let scheme = signature_scheme(cs);

        let signature_keys = SignatureKeyPair::new(scheme)
            .map_err(|e| Error::Identity(format!("keypair generation: {e}")))?;
        signature_keys
            .store(provider.storage())
            .map_err(|e| Error::Identity(format!("keypair storage: {e:?}")))?;

        let basic = BasicCredential::new(name.as_bytes().to_vec());
        let credential = CredentialWithKey {
            credential: basic.into(),
            signature_key: signature_keys.public().into(),
        };

        Ok(Self {
            provider,
            signature_keys,
            credential,
            ciphersuite: cs,
        })
    }

    /// Public signature key as raw bytes (Ed25519 or ECDSA-P384 depending on
    /// the ciphersuite).
    #[must_use]
    pub fn signature_public_key(&self) -> &[u8] {
        self.signature_keys.public()
    }

    /// Ciphersuite this identity is bound to.
    #[must_use]
    pub const fn ciphersuite(&self) -> Ciphersuite {
        self.ciphersuite
    }
}

/// Map a Lakko-supported ciphersuite to its MLS signature scheme.
///
/// Caller must already have validated `cs` via [`ciphersuite::is_supported`];
/// values outside the supported set fall through to the default Ed25519 path.
fn signature_scheme(cs: Ciphersuite) -> SignatureScheme {
    if cs == ciphersuite::FIPS {
        SignatureScheme::ECDSA_SECP384R1_SHA384
    } else {
        SignatureScheme::ED25519
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_default_identity() {
        let alice = Identity::generate("alice").unwrap();
        assert_eq!(alice.ciphersuite(), ciphersuite::DEFAULT);
        // Ed25519 public keys are 32 bytes.
        assert_eq!(alice.signature_public_key().len(), 32);
    }

    #[test]
    fn fips_path_not_yet_wired_in_phase_0() {
        // The FIPS suite (P-384 + ECDSA-P384) requires X.509 credentials and
        // ECDSA support in `openmls_basic_credential`, neither of which are
        // available in the 0.3 line. Phase 1+ will land both. For now we
        // verify the wrapper surfaces a clear error rather than panicking.
        let result = Identity::generate_with_ciphersuite("bob", ciphersuite::FIPS);
        assert!(matches!(result, Err(Error::Identity(_))));
    }

    #[test]
    fn unsupported_ciphersuite_rejected() {
        let result = Identity::generate_with_ciphersuite(
            "carol",
            Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519,
        );
        assert!(matches!(result, Err(Error::Identity(_))));
    }

    #[test]
    fn each_identity_has_distinct_keys() {
        let a = Identity::generate("alice").unwrap();
        let b = Identity::generate("alice").unwrap(); // same name, different keys
        assert_ne!(a.signature_public_key(), b.signature_public_key());
    }
}
