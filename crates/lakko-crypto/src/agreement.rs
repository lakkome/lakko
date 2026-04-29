// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! X25519 Diffie-Hellman key agreement.
//!
//! Used wherever a static or ephemeral DH is required outside the MLS path
//! (e.g. invite-capability bootstrap, device-handshake fast path). The MLS
//! ciphersuite-internal X25519 lives inside OpenMLS and is not surfaced here.

use x25519_dalek::{PublicKey as DalekPublicKey, StaticSecret as DalekStaticSecret};

use crate::error::{Error, Result};
use crate::random;
use crate::secret::SecretBytes;

/// Length of an X25519 public key in bytes.
pub const PUBLIC_KEY_LEN: usize = 32;

/// X25519 secret key.
///
/// Wraps `x25519-dalek::StaticSecret`, which zeroes its bytes on drop.
#[derive(Clone)]
pub struct SecretKey(DalekStaticSecret);

/// X25519 public key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicKey(DalekPublicKey);

impl SecretKey {
    /// Generate a fresh X25519 secret from OS randomness.
    #[must_use]
    pub fn generate() -> Self {
        Self(DalekStaticSecret::random_from_rng(random::rng()))
    }

    /// Reconstruct from a 32-byte representation.
    ///
    /// Equivalent to `from_seed`; X25519 secrets are unstructured 32 bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(DalekStaticSecret::from(bytes))
    }

    /// Export the 32-byte secret. Secret material.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// Derive the corresponding public key.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey(DalekPublicKey::from(&self.0))
    }

    /// Compute the shared secret with `peer`.
    ///
    /// Output is 32 bytes; callers are expected to feed it to an HKDF
    /// (per X25519 best practice — never use the raw DH output as a key).
    #[must_use]
    pub fn diffie_hellman(&self, peer: &PublicKey) -> SecretBytes<32> {
        SecretBytes::new(self.0.diffie_hellman(&peer.0).to_bytes())
    }
}

impl PublicKey {
    /// Decode a public key from 32 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLength`] for non-32-byte input.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; PUBLIC_KEY_LEN] = bytes.try_into().map_err(|_| Error::InvalidLength {
            expected: PUBLIC_KEY_LEN,
            actual: bytes.len(),
        })?;
        Ok(Self(DalekPublicKey::from(arr)))
    }

    /// Encode to 32 bytes.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.0.to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dh_agreement_is_symmetric() {
        let alice = SecretKey::generate();
        let bob = SecretKey::generate();

        let alice_view = alice.diffie_hellman(&bob.public_key());
        let bob_view = bob.diffie_hellman(&alice.public_key());

        assert_eq!(alice_view, bob_view);
    }

    #[test]
    fn different_pairs_yield_different_secrets() {
        let alice = SecretKey::generate();
        let bob = SecretKey::generate();
        let eve = SecretKey::generate();

        let with_bob = alice.diffie_hellman(&bob.public_key());
        let with_eve = alice.diffie_hellman(&eve.public_key());

        assert_ne!(with_bob, with_eve);
    }

    #[test]
    fn key_serialization_roundtrip() {
        let sk = SecretKey::generate();
        let bytes = sk.to_bytes();
        let restored = SecretKey::from_bytes(bytes);

        assert_eq!(sk.public_key(), restored.public_key());
    }

    #[test]
    fn public_key_from_wrong_length_fails() {
        let result = PublicKey::from_bytes(&[0u8; 16]);
        assert!(matches!(
            result,
            Err(Error::InvalidLength {
                expected: 32,
                actual: 16
            })
        ));
    }
}
