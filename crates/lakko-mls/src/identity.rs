// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MLS identity: signing keypair + credential + per-device storage provider.
//!
//! Phase 0 uses [`BasicCredential`] (an opaque user-chosen byte string).
//! X.509 credentials with Key Transparency log inclusion proofs land in
//! Phase 1+ per ADR-0001.
//!
//! ## X25519 envelope keypair (Phase 1.16d3b)
//!
//! Each Identity also owns a long-lived X25519 keypair used as the
//! HPKE init_key for `lakko-contacts` sealed envelopes
//! (ContactRequest intro messages, future invite-capability blobs).
//! It's separate from the X25519 keys MLS uses internally for
//! DHKEM — those are scoped to a specific MLS group and rotated
//! on every commit; the Identity-level X25519 has a much longer
//! lifetime and is published to the discovery service's
//! identity-bundle directory (1.16d3a).

use openmls::credentials::{BasicCredential, CredentialWithKey};
use openmls::prelude::Ciphersuite;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::types::SignatureScheme;
use openmls_traits::OpenMlsProvider as _;

use lakko_crypto::agreement;

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
    /// Long-lived X25519 secret for HPKE-Base sealed envelopes
    /// targeted at this identity (Phase 1.16d3b). Distinct from
    /// the per-MLS-group DHKEM keys OpenMLS manages internally;
    /// see the file-level docstring.
    pub(crate) x25519_secret: agreement::SecretKey,
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

        // Long-lived X25519 keypair for HPKE-Base sealed envelopes.
        // Generated from the OS CSPRNG; the secret stays inside the
        // Identity — accessors below let downstream layers extract
        // raw bytes when persisting (encrypted by the device root key
        // — Phase 1.16c3c2 / 1.16d3b client wiring).
        let x25519_secret = agreement::SecretKey::generate();

        Ok(Self {
            provider,
            signature_keys,
            credential,
            ciphersuite: cs,
            x25519_secret,
        })
    }

    /// Public signature key as raw bytes (Ed25519 or ECDSA-P384 depending on
    /// the ciphersuite).
    #[must_use]
    pub fn signature_public_key(&self) -> &[u8] {
        self.signature_keys.public()
    }

    /// Public X25519 key (32 bytes) — the HPKE init_key callers
    /// of `lakko-contacts::compose` target when sealing an
    /// envelope to this identity.
    ///
    /// This is the value the discovery service stores as
    /// `x25519_pubkey` in `identity_bundles` (1.16d3a).
    #[must_use]
    pub fn x25519_public_key(&self) -> [u8; 32] {
        self.x25519_secret.public_key().to_bytes()
    }

    /// Raw X25519 secret bytes (32 bytes). The caller is
    /// responsible for protecting them — wrap in WebCrypto via
    /// the device root key (1.16c3c2) before persisting.
    ///
    /// Returns a copy each call so the caller controls the
    /// lifetime; the in-Identity secret is unaffected.
    #[must_use]
    pub fn x25519_secret_bytes(&self) -> [u8; 32] {
        self.x25519_secret.to_bytes()
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
        assert_ne!(a.x25519_public_key(), b.x25519_public_key());
        assert_ne!(a.x25519_secret_bytes(), b.x25519_secret_bytes());
    }

    /// Phase 1.16d3b — every Identity exposes a 32-byte X25519
    /// public key intended for HPKE-Base sealed envelopes (the
    /// `lakko-contacts::compose` recipient_x25519_pub argument).
    #[test]
    fn x25519_public_key_is_32_bytes() {
        let alice = Identity::generate("alice").unwrap();
        let pk = alice.x25519_public_key();
        assert_eq!(pk.len(), 32);
        // Sanity: not all zero (would indicate the secret was
        // never generated).
        assert!(pk.iter().any(|&b| b != 0));
    }

    /// The X25519 secret round-trips through the
    /// `agreement::SecretKey::from_bytes` constructor — useful
    /// because the FFI layer hands the secret to JS as raw bytes
    /// for envelope decryption (`ContactRequest.openEnvelope`)
    /// and may eventually reconstruct an Identity from a
    /// persisted secret + signature key.
    #[test]
    fn x25519_secret_bytes_round_trip_to_public() {
        let alice = Identity::generate("alice").unwrap();
        let secret_bytes = alice.x25519_secret_bytes();
        let public_bytes = alice.x25519_public_key();

        // Reconstruct the secret from raw bytes and recompute the
        // public key — must match the one Identity returned.
        let restored = lakko_crypto::agreement::SecretKey::from_bytes(secret_bytes);
        assert_eq!(
            restored.public_key().to_bytes(),
            public_bytes,
            "x25519 secret round-trip must produce the same public key",
        );
    }

    /// Calling `x25519_secret_bytes()` twice produces the same
    /// bytes — the accessor returns a copy, the in-Identity
    /// secret is stable across reads.
    #[test]
    fn x25519_secret_accessor_is_idempotent() {
        let alice = Identity::generate("alice").unwrap();
        let a = alice.x25519_secret_bytes();
        let b = alice.x25519_secret_bytes();
        assert_eq!(a, b);
    }
}
