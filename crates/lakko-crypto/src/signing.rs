// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Ed25519 signing and verification.
//!
//! Lakko uses Ed25519 (RFC 8032) for identity keys, MLS credentials, invite
//! capabilities and Key Transparency log entries. P-384 ECDSA is reserved for
//! the FIPS-friendly MLS ciphersuite and lives in a separate module if/when
//! needed.

use ed25519_dalek::{
    Signature as DalekSignature, Signer as _, SigningKey as DalekSigningKey, Verifier as _,
    VerifyingKey as DalekVerifyingKey,
};

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
    fn signature_serialization_roundtrip() {
        let sk = SigningKey::generate();
        let sig = sk.sign(b"data");
        let bytes = sig.to_bytes();
        let restored = Signature::from_bytes(&bytes).unwrap();
        assert_eq!(sig, restored);
    }
}
