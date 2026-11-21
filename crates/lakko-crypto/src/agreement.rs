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
    ///
    /// # Errors
    ///
    /// [`Error::NonContributory`] when the shared secret is all zeros: the
    /// peer key is a low-order point (RFC 7748 section 6.1), so the output
    /// does not depend on this secret and an attacker who chose the point
    /// knows it. Every use of the result would derive a key the attacker
    /// can compute, so the agreement is refused rather than returned.
    pub fn diffie_hellman(&self, peer: &PublicKey) -> Result<SecretBytes<32>> {
        let shared = self.0.diffie_hellman(&peer.0);
        if !shared.was_contributory() {
            return Err(Error::NonContributory);
        }
        Ok(SecretBytes::new(shared.to_bytes()))
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

    /// Build from a 32-byte array (any 32 bytes are a Montgomery
    /// `u`-coordinate X25519 accepts).
    #[must_use]
    pub fn from_array(bytes: [u8; PUBLIC_KEY_LEN]) -> Self {
        Self(DalekPublicKey::from(bytes))
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

        let alice_view = alice.diffie_hellman(&bob.public_key()).unwrap();
        let bob_view = bob.diffie_hellman(&alice.public_key()).unwrap();

        assert_eq!(alice_view, bob_view);
    }

    #[test]
    fn different_pairs_yield_different_secrets() {
        let alice = SecretKey::generate();
        let bob = SecretKey::generate();
        let eve = SecretKey::generate();

        let with_bob = alice.diffie_hellman(&bob.public_key()).unwrap();
        let with_eve = alice.diffie_hellman(&eve.public_key()).unwrap();

        assert_ne!(with_bob, with_eve);
    }

    #[test]
    fn key_serialization_roundtrip() {
        let sk = SecretKey::generate();
        let bytes = sk.to_bytes();
        let restored = SecretKey::from_bytes(bytes);

        assert_eq!(sk.public_key(), restored.public_key());
    }

    /// RFC 7748 section 6.1 / the libsodium blocklist: every low-order point
    /// on Curve25519 and its twist (as a `u`-coordinate, including the
    /// non-canonical encodings above the field prime) gives an all-zero
    /// shared secret, which is refused.
    #[test]
    fn a_low_order_peer_key_is_refused() {
        let mut low_order: Vec<[u8; 32]> = vec![[0u8; 32], {
            let mut one = [0u8; 32];
            one[0] = 1;
            one
        }];
        // 325606250916557431795983626356110631294008115727848805560023387167927233504
        let mut p8 = [0u8; 32];
        p8.copy_from_slice(&hex_32(
            "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800",
        ));
        low_order.push(p8);
        let mut p8b = [0u8; 32];
        p8b.copy_from_slice(&hex_32(
            "5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157",
        ));
        low_order.push(p8b);
        // p - 1, p, p + 1 (the twist points and non-canonical zero / one).
        low_order.push(hex_32(
            "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        ));
        low_order.push(hex_32(
            "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        ));
        low_order.push(hex_32(
            "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        ));
        for _ in 0..8 {
            let me = SecretKey::generate();
            for point in &low_order {
                assert!(
                    matches!(
                        me.diffie_hellman(&PublicKey::from_array(*point)),
                        Err(Error::NonContributory)
                    ),
                    "low-order point {point:02x?} must be refused",
                );
            }
        }
    }

    fn hex_32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        out
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
