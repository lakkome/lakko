// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Authenticated encryption with associated data (AEAD).
//!
//! Lakko uses two AEAD ciphers:
//!
//! - [`Algorithm::ChaCha20Poly1305`] — the default for application-layer
//!   encryption (HPKE envelopes, attachment encryption, backup blobs).
//!   Software-only constant-time, fast on platforms without AES hardware.
//! - [`Algorithm::Aes256Gcm`] — used when interop or hardware acceleration
//!   matters (FIPS-friendly MLS ciphersuite, SFrame on devices with AES-NI).
//!
//! Key sizes are 256 bits / 32 bytes for both.

use aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm as AesGcmCipher;
use chacha20poly1305::ChaCha20Poly1305 as ChaChaCipher;

use crate::error::{Error, Result};
use crate::secret::SecretBytes;

/// AEAD key length in bytes for both supported ciphers.
pub const KEY_LEN: usize = 32;

/// AEAD nonce length in bytes for both supported ciphers.
pub const NONCE_LEN: usize = 12;

/// AEAD authentication tag length in bytes for both supported ciphers.
pub const TAG_LEN: usize = 16;

/// AEAD key.
pub type AeadKey = SecretBytes<KEY_LEN>;

/// AEAD nonce — the caller is responsible for uniqueness per key.
pub type Nonce = [u8; NONCE_LEN];

/// AEAD cipher selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
    /// ChaCha20-Poly1305 (RFC 8439).
    ChaCha20Poly1305,
    /// AES-256-GCM (NIST SP 800-38D).
    Aes256Gcm,
}

/// Encrypt `plaintext` with associated data `aad` under `key` using `algorithm`.
///
/// The returned vector is `ciphertext || tag`. The caller must persist `nonce`
/// next to the ciphertext (or reconstruct it deterministically).
///
/// # Errors
///
/// Returns [`Error::Aead`] on internal cipher failure (essentially impossible
/// for these algorithms with valid inputs, but the API surfaces it).
pub fn seal(
    algorithm: Algorithm,
    key: &AeadKey,
    nonce: &Nonce,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let payload = Payload {
        msg: plaintext,
        aad,
    };
    match algorithm {
        Algorithm::ChaCha20Poly1305 => {
            let cipher = ChaChaCipher::new_from_slice(key.as_bytes()).map_err(|_| Error::Aead)?;
            cipher
                .encrypt(nonce.into(), payload)
                .map_err(|_| Error::Aead)
        },
        Algorithm::Aes256Gcm => {
            let cipher = AesGcmCipher::new_from_slice(key.as_bytes()).map_err(|_| Error::Aead)?;
            cipher
                .encrypt(nonce.into(), payload)
                .map_err(|_| Error::Aead)
        },
    }
}

/// Decrypt `ciphertext` (which must include the trailing tag) with associated
/// data `aad` under `key` using `algorithm`.
///
/// # Errors
///
/// Returns [`Error::Aead`] if authentication fails (wrong key / wrong AAD /
/// tampered ciphertext / wrong nonce).
pub fn open(
    algorithm: Algorithm,
    key: &AeadKey,
    nonce: &Nonce,
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    let payload = Payload {
        msg: ciphertext,
        aad,
    };
    match algorithm {
        Algorithm::ChaCha20Poly1305 => {
            let cipher = ChaChaCipher::new_from_slice(key.as_bytes()).map_err(|_| Error::Aead)?;
            cipher
                .decrypt(nonce.into(), payload)
                .map_err(|_| Error::Aead)
        },
        Algorithm::Aes256Gcm => {
            let cipher = AesGcmCipher::new_from_slice(key.as_bytes()).map_err(|_| Error::Aead)?;
            cipher
                .decrypt(nonce.into(), payload)
                .map_err(|_| Error::Aead)
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::random;

    fn fresh_key() -> AeadKey {
        random::secret_bytes::<KEY_LEN>()
    }

    fn fresh_nonce() -> Nonce {
        let mut n = [0u8; NONCE_LEN];
        random::fill(&mut n);
        n
    }

    #[test]
    fn chacha_roundtrip() {
        let key = fresh_key();
        let nonce = fresh_nonce();
        let pt = b"the future is encrypted";
        let aad = b"context";

        let ct = seal(Algorithm::ChaCha20Poly1305, &key, &nonce, aad, pt).unwrap();
        let recovered = open(Algorithm::ChaCha20Poly1305, &key, &nonce, aad, &ct).unwrap();
        assert_eq!(recovered, pt);
    }

    #[test]
    fn aes_roundtrip() {
        let key = fresh_key();
        let nonce = fresh_nonce();
        let pt = b"interop payload";
        let aad = b"";

        let ct = seal(Algorithm::Aes256Gcm, &key, &nonce, aad, pt).unwrap();
        let recovered = open(Algorithm::Aes256Gcm, &key, &nonce, aad, &ct).unwrap();
        assert_eq!(recovered, pt);
    }

    #[test]
    fn ciphertext_includes_tag() {
        let key = fresh_key();
        let nonce = fresh_nonce();
        let pt = b"abc";

        let ct = seal(Algorithm::ChaCha20Poly1305, &key, &nonce, &[], pt).unwrap();
        assert_eq!(ct.len(), pt.len() + TAG_LEN);
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let key = fresh_key();
        let nonce = fresh_nonce();
        let pt = b"sensitive";

        let mut ct = seal(Algorithm::ChaCha20Poly1305, &key, &nonce, &[], pt).unwrap();
        ct[0] ^= 0x01; // flip a bit
        let result = open(Algorithm::ChaCha20Poly1305, &key, &nonce, &[], &ct);
        assert!(matches!(result, Err(Error::Aead)));
    }

    #[test]
    fn wrong_aad_fails() {
        let key = fresh_key();
        let nonce = fresh_nonce();
        let ct = seal(
            Algorithm::ChaCha20Poly1305,
            &key,
            &nonce,
            b"correct-aad",
            b"msg",
        )
        .unwrap();
        let result = open(Algorithm::ChaCha20Poly1305, &key, &nonce, b"wrong-aad", &ct);
        assert!(matches!(result, Err(Error::Aead)));
    }

    #[test]
    fn wrong_key_fails() {
        let nonce = fresh_nonce();
        let ct = seal(Algorithm::Aes256Gcm, &fresh_key(), &nonce, &[], b"msg").unwrap();
        let result = open(Algorithm::Aes256Gcm, &fresh_key(), &nonce, &[], &ct);
        assert!(matches!(result, Err(Error::Aead)));
    }
}
