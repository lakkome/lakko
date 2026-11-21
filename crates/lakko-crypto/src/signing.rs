// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Ed25519 signing and verification.
//!
//! Lakko uses Ed25519 (RFC 8032) for identity keys, MLS credentials, invite
//! capabilities and Key Transparency log entries. P-384 ECDSA is reserved for
//! the FIPS-friendly MLS ciphersuite and lives in a separate module if/when
//! needed.
//!
//! # One key pair for signing and agreement
//!
//! [`SigningKey::to_x25519_secret`] and [`VerifyingKey::to_x25519_public`]
//! convert an Ed25519 key pair into the X25519 key pair on the birationally
//! equivalent Montgomery curve (the conversion libsodium names
//! `crypto_sign_ed25519_{sk,pk}_to_curve25519`): the secret is the unclamped
//! scalar half of `SHA-512(seed)`, which X25519 clamps on use exactly as
//! Ed25519 does, and the public key is the Edwards point mapped to its
//! Montgomery `u`-coordinate. So `x25519(to_x25519_secret(sk)) ==
//! to_x25519_public(pk(sk))` (pinned by a test and a golden vector). Used
//! where one registered key must both sign and receive sealed envelopes
//! (the ADR-0060 channel subscriber key).

use ed25519_dalek::{
    Signature as DalekSignature, Signer as _, SigningKey as DalekSigningKey, Verifier as _,
    VerifyingKey as DalekVerifyingKey,
};

use zeroize::Zeroize as _;

use crate::agreement;
use crate::error::{Error, Result};
use crate::random;

/// Length of an Ed25519 public key in bytes.
pub const PUBLIC_KEY_LEN: usize = 32;

/// Length of an Ed25519 signature in bytes.
pub const SIGNATURE_LEN: usize = 64;

/// Ed25519 secret signing key.
///
/// The underlying key material is zeroized on drop by `ed25519-dalek`.
#[derive(Clone)]
pub struct SigningKey(DalekSigningKey);

/// Ed25519 public verifying key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifyingKey(DalekVerifyingKey);

/// Ed25519 signature (64 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature(DalekSignature);

impl SigningKey {
    /// Generate a fresh signing key from OS randomness.
    #[must_use]
    pub fn generate() -> Self {
        Self(DalekSigningKey::generate(&mut random::rng()))
    }

    /// Reconstruct a signing key from its 32-byte seed.
    #[must_use]
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self(DalekSigningKey::from_bytes(seed))
    }

    /// Export the 32-byte seed. Use only for backup / serialization paths;
    /// the result is secret material.
    #[must_use]
    pub fn to_seed(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// Derive the public verifying key.
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        VerifyingKey(self.0.verifying_key())
    }

    /// Sign a message.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> Signature {
        Signature(self.0.sign(message))
    }

    /// The X25519 secret of the same key pair (see the module docs): the
    /// unclamped scalar half of `SHA-512(seed)`. Its public key is
    /// [`VerifyingKey::to_x25519_public`] of [`Self::verifying_key`].
    #[must_use]
    pub fn to_x25519_secret(&self) -> agreement::SecretKey {
        let mut scalar = self.0.to_scalar_bytes();
        let secret = agreement::SecretKey::from_bytes(scalar);
        scalar.zeroize();
        secret
    }
}

impl VerifyingKey {
    /// Decode a public key from its 32-byte canonical form.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLength`] for non-32-byte input or an
    /// [`Error::SignatureVerification`] if the bytes are not a valid encoding.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; PUBLIC_KEY_LEN] = bytes.try_into().map_err(|_| Error::InvalidLength {
            expected: PUBLIC_KEY_LEN,
            actual: bytes.len(),
        })?;
        let key = DalekVerifyingKey::from_bytes(&arr).map_err(|_| Error::SignatureVerification)?;
        Ok(Self(key))
    }

    /// Encode to 32-byte canonical form.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.0.to_bytes()
    }

    /// The X25519 public key of the same key pair (see the module docs): the
    /// Montgomery `u`-coordinate of this Edwards point.
    #[must_use]
    pub fn to_x25519_public(&self) -> agreement::PublicKey {
        agreement::PublicKey::from_array(self.0.to_montgomery().to_bytes())
    }

    /// Whether this key is of small order (one of the eight torsion points).
    ///
    /// A signature under such a key can be forged for any message, and its
    /// X25519 image yields an all-zero shared secret, so a caller that
    /// registers a key for signing or for agreement refuses it.
    #[must_use]
    pub fn is_weak(&self) -> bool {
        self.0.is_weak()
    }

    /// Verify `signature` against `message`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SignatureVerification`] if the signature is invalid.
    pub fn verify(&self, message: &[u8], signature: &Signature) -> Result<()> {
        self.0
            .verify(message, &signature.0)
            .map_err(|_| Error::SignatureVerification)
    }
}

impl Signature {
    /// Decode a signature from its 64-byte canonical form.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLength`] for non-64-byte input.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; SIGNATURE_LEN] = bytes.try_into().map_err(|_| Error::InvalidLength {
            expected: SIGNATURE_LEN,
            actual: bytes.len(),
        })?;
        Ok(Self(DalekSignature::from_bytes(&arr)))
    }

    /// Encode to 64-byte canonical form.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; SIGNATURE_LEN] {
        self.0.to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip() {
        let sk = SigningKey::generate();
        let pk = sk.verifying_key();
        let msg = b"phase 0 phase 0 phase 0";

        let sig = sk.sign(msg);
        pk.verify(msg, &sig).unwrap();
    }

    #[test]
    fn wrong_message_fails() {
        let sk = SigningKey::generate();
        let pk = sk.verifying_key();
        let sig = sk.sign(b"original");

        assert!(matches!(
            pk.verify(b"tampered", &sig),
            Err(Error::SignatureVerification)
        ));
    }

    #[test]
    fn wrong_key_fails() {
        let sk1 = SigningKey::generate();
        let sk2 = SigningKey::generate();
        let sig = sk1.sign(b"msg");

        assert!(matches!(
            sk2.verifying_key().verify(b"msg", &sig),
            Err(Error::SignatureVerification)
        ));
    }

    #[test]
    fn key_seed_roundtrip() {
        let sk = SigningKey::generate();
        let seed = sk.to_seed();
        let restored = SigningKey::from_seed(&seed);

        // Same key produces same public.
        assert_eq!(sk.verifying_key(), restored.verifying_key());

        // Same key produces verifiable signatures (Ed25519 is deterministic).
        let sig1 = sk.sign(b"deterministic");
        let sig2 = restored.sign(b"deterministic");
        assert_eq!(sig1.to_bytes(), sig2.to_bytes());
    }

    #[test]
    fn public_key_serialization_roundtrip() {
        let pk = SigningKey::generate().verifying_key();
        let bytes = pk.to_bytes();
        let restored = VerifyingKey::from_bytes(&bytes).unwrap();
        assert_eq!(pk, restored);
    }

    #[test]
    fn public_key_from_wrong_length_fails() {
        let result = VerifyingKey::from_bytes(&[0u8; 16]);
        assert!(matches!(
            result,
            Err(Error::InvalidLength {
                expected: 32,
                actual: 16
            })
        ));
    }

    #[test]
    fn the_x25519_conversion_is_one_key_pair() {
        for _ in 0..16 {
            let sk = SigningKey::generate();
            let x_secret = sk.to_x25519_secret();
            let x_public = sk.verifying_key().to_x25519_public();
            assert_eq!(x_secret.public_key(), x_public);
            // And agreement works across the conversion.
            let peer = agreement::SecretKey::generate();
            assert_eq!(
                x_secret.diffie_hellman(&peer.public_key()).unwrap(),
                peer.diffie_hellman(&x_public).unwrap()
            );
        }
    }

    /// libsodium's own vector for `crypto_sign_ed25519_{pk,sk}_to_curve25519`
    /// (`test/default/ed25519_convert.c`): an independent implementation of
    /// the same mapping. libsodium returns the secret clamped; this crate
    /// returns it unclamped (X25519 clamps on use), so the secret is compared
    /// after clamping.
    #[test]
    fn the_x25519_conversion_matches_libsodium() {
        let seed: [u8; 32] =
            hex::decode("421151a459faeade3d247115f94aedae42318124095afabe4d1451a559faedee")
                .unwrap()
                .try_into()
                .unwrap();
        let sk = SigningKey::from_seed(&seed);
        assert_eq!(
            hex::encode(sk.verifying_key().to_x25519_public().to_bytes()),
            "f1814f0e8ff1043d8a44d25babff3cedcae6c22c3edaa48f857ae70de2baae50"
        );
        let mut secret = sk.to_x25519_secret().to_bytes();
        secret[0] &= 0xF8;
        secret[31] &= 0x7F;
        secret[31] |= 0x40;
        assert_eq!(
            hex::encode(secret),
            "8052030376d47112be7f73ed7a019293dd12ad910b654455798b4667d73de166"
        );
    }

    #[test]
    fn small_order_keys_are_weak() {
        // The identity point (y = 1) decodes but has order 1.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert!(VerifyingKey::from_bytes(&identity).unwrap().is_weak());
        assert!(!SigningKey::generate().verifying_key().is_weak());
    }

    #[test]
    fn signature_serialization_roundtrip() {
        let sk = SigningKey::generate();
        let sig = sk.sign(b"data");
        let bytes = sig.to_bytes();
        let restored = Signature::from_bytes(&bytes).unwrap();
        assert_eq!(sig, restored);
    }
}
