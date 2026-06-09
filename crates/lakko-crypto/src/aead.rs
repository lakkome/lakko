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
use crate::random;
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

/// Seal `plaintext` under a raw 32-byte `key` with a fresh random nonce,
/// returning a self-describing wire blob `nonce(12) || ciphertext+tag`.
///
/// Unlike [`seal`], this is a turnkey envelope: the nonce is generated
/// internally and prepended to the output, so the caller stores / transmits
/// one opaque byte string and never has to manage a nonce out of band. Uses
/// ChaCha20-Poly1305 with empty associated data. Used by the ADR-0018 Part 2a
/// "another-device" identity transfer to seal `(grant_token || phrase_seed)`
/// under a key both devices derive via [`crate::aead`]-keyed MLS exporter, but
/// the helper is fully generic — any caller with a shared 32-byte key may use
/// it.
///
/// The nonce is fresh-random per call (no counter / reuse risk under the same
/// key, given the RFC 8439 birthday bound for the number of messages a single
/// transfer produces — one).
///
/// [ADR-0018]: ../../../docs/adr/0018-recovery-factor-cascade.md
///
/// # Errors
///
/// Returns [`Error::Aead`] on internal cipher failure (essentially impossible
/// for valid inputs, but the API surfaces it).
pub fn seal_with_key(key: &[u8; KEY_LEN], plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut nonce: Nonce = [0u8; NONCE_LEN];
    random::fill(&mut nonce);
    let aead_key = AeadKey::new(*key);
    let ciphertext = seal(
        Algorithm::ChaCha20Poly1305,
        &aead_key,
        &nonce,
        &[],
        plaintext,
    )?;
    let mut wire = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    wire.extend_from_slice(&nonce);
    wire.extend_from_slice(&ciphertext);
    Ok(wire)
}

/// Inverse of [`seal_with_key`]: given the `nonce(12) || ciphertext+tag` wire
/// blob and the same raw 32-byte `key`, slice off the leading nonce and
/// AEAD-open the remainder.
///
/// # Errors
///
/// - [`Error::InvalidLength`] when `wire` is too short to contain a nonce plus
///   a non-empty authentication tag.
/// - [`Error::Aead`] when authentication fails (wrong key / tampered blob).
pub fn open_with_key(key: &[u8; KEY_LEN], wire: &[u8]) -> Result<Vec<u8>> {
    // Must hold at least the nonce and a full tag; an all-empty plaintext
    // still carries TAG_LEN bytes after the nonce.
    if wire.len() < NONCE_LEN + TAG_LEN {
        return Err(Error::InvalidLength {
            expected: NONCE_LEN + TAG_LEN,
            actual: wire.len(),
        });
    }
    let (nonce_bytes, ciphertext) = wire.split_at(NONCE_LEN);
    let mut nonce: Nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(nonce_bytes);
    let aead_key = AeadKey::new(*key);
    open(
        Algorithm::ChaCha20Poly1305,
        &aead_key,
        &nonce,
        &[],
        ciphertext,
    )
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

    fn fresh_raw_key() -> [u8; KEY_LEN] {
        let mut k = [0u8; KEY_LEN];
        random::fill(&mut k);
        k
    }

    #[test]
    fn seal_open_with_key_roundtrips() {
        let key = fresh_raw_key();
        let pt = b"grant_token || phrase_seed";
        let wire = seal_with_key(&key, pt).unwrap();
        // Wire = nonce(12) || ciphertext+tag.
        assert_eq!(wire.len(), NONCE_LEN + pt.len() + TAG_LEN);
        let recovered = open_with_key(&key, &wire).unwrap();
        assert_eq!(recovered, pt);
    }

    #[test]
    fn seal_with_key_uses_a_fresh_nonce_each_call() {
        let key = fresh_raw_key();
        let a = seal_with_key(&key, b"same plaintext").unwrap();
        let b = seal_with_key(&key, b"same plaintext").unwrap();
        // Different nonce -> different leading 12 bytes -> different ciphertext.
        assert_ne!(
            a, b,
            "two seals of the same plaintext collided (nonce reuse?)"
        );
    }

    #[test]
    fn open_with_key_rejects_the_wrong_key() {
        let key = fresh_raw_key();
        let other = fresh_raw_key();
        let wire = seal_with_key(&key, b"secret").unwrap();
        assert!(matches!(open_with_key(&other, &wire), Err(Error::Aead)));
    }

    #[test]
    fn open_with_key_rejects_a_tampered_blob() {
        let key = fresh_raw_key();
        let mut wire = seal_with_key(&key, b"secret").unwrap();
        // Flip a bit in the ciphertext body (past the 12-byte nonce).
        let last = wire.len() - 1;
        wire[last] ^= 0x01;
        assert!(matches!(open_with_key(&key, &wire), Err(Error::Aead)));
    }

    #[test]
    fn open_with_key_rejects_a_short_blob() {
        let key = fresh_raw_key();
        // Shorter than nonce + tag — cannot possibly carry a sealed message.
        let too_short = vec![0u8; NONCE_LEN + TAG_LEN - 1];
        assert!(matches!(
            open_with_key(&key, &too_short),
            Err(Error::InvalidLength { .. })
        ));
    }

    #[test]
    fn seal_open_with_key_handles_empty_plaintext() {
        let key = fresh_raw_key();
        let wire = seal_with_key(&key, b"").unwrap();
        assert_eq!(wire.len(), NONCE_LEN + TAG_LEN);
        assert_eq!(open_with_key(&key, &wire).unwrap(), b"");
    }
}
