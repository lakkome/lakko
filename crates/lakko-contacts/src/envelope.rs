// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Sealed-box envelope for cold ContactRequest payloads.
//!
//! Construction (HPKE-Base-equivalent, hand-rolled out of
//! `lakko-crypto` primitives):
//!
//! 1. Sender generates a single-use ephemeral X25519 keypair
//!    `(eph_priv, eph_pub)`.
//! 2. `shared = ECDH(eph_priv, recipient_pub)`.
//! 3. `key = HKDF-SHA-256(salt = "", ikm = shared, info = info_bytes)`
//!    → 32-byte ChaCha20-Poly1305 key.
//! 4. The 12-byte AEAD nonce is derived deterministically from the
//!    ephemeral pubkey (`HKDF` with a different `info`) — no
//!    nonce-reuse risk because the ephemeral key is single-use.
//! 5. AEAD `seal(key, nonce, aad, plaintext)`.
//!
//! On the wire: `eph_pub(32) ‖ ciphertext_with_tag`.
//!
//! Properties:
//!
//! - **Forward secrecy** (sender-side): the ephemeral private key is
//!   discarded after seal. A future compromise of the recipient's
//!   long-term private key still decrypts past envelopes — that is
//!   inherent to a one-shot envelope and matches HPKE-Base's
//!   security claim.
//! - **Sender authentication: NONE.** This is a sealed box, not an
//!   authenticated key exchange. ContactRequest layers an Ed25519
//!   signature on top (in `request.rs`) which provides the
//!   authentication.
//!
//! This is functionally equivalent to libsodium's
//! `crypto_box_seal` and to HPKE-Base mode with DHKEM-X25519,
//! HKDF-SHA-256, ChaCha20-Poly1305. We hand-roll it out of our own
//! primitives so we don't pull in a separate `hpke` or `crypto_box`
//! crate; the construction is small enough to read in one sitting.

use lakko_crypto::aead::{self, AeadKey, Algorithm, Nonce, KEY_LEN, NONCE_LEN};
use lakko_crypto::agreement::{PublicKey, SecretKey};
use lakko_crypto::kdf;
use lakko_crypto::secret::SecretBytes;

use crate::error::{ContactError, Result};

/// X25519 public key length (32 bytes).
pub const EPH_PUB_LEN: usize = 32;
/// AEAD authentication tag length (16 bytes).
pub const TAG_LEN: usize = 16;

const ALG: Algorithm = Algorithm::ChaCha20Poly1305;
const HKDF_INFO_KEY: &[u8] = b"lakko-contacts/v1/sealed-box/key";
const HKDF_INFO_NONCE: &[u8] = b"lakko-contacts/v1/sealed-box/nonce";

/// Seal `plaintext` to `recipient_pub`. `aad` is bound to the
/// ciphertext via AEAD; the recipient must supply the same bytes
/// (typically a transcript-binding label or the encoded ContactRequest
/// header) on `open`.
///
/// On success returns `eph_pub(32) ‖ ciphertext_with_tag`.
///
/// # Errors
///
/// Returns `ContactError::Crypto` on AEAD seal failure (essentially
/// impossible with valid inputs, but surfaced for completeness).
pub fn seal(recipient_pub: &PublicKey, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    seal_labeled(&CONTACT_LABELS, recipient_pub, aad, plaintext)
}

/// The two HKDF `info` prefixes one use of the sealed box derives its AEAD
/// key and nonce under. A use with its own labels (the ADR-0078 mailbox
/// envelope) can never open a box sealed for another use, even to the same
/// recipient key with the same AAD.
#[derive(Clone, Copy)]
pub(crate) struct SealLabels {
    pub(crate) key: &'static [u8],
    pub(crate) nonce: &'static [u8],
}

/// The labels [`seal`] / [`open`] use (the contact request's seal).
const CONTACT_LABELS: SealLabels = SealLabels {
    key: HKDF_INFO_KEY,
    nonce: HKDF_INFO_NONCE,
};

/// [`seal`] under `labels` instead of the contact-request labels.
pub(crate) fn seal_labeled(
    labels: &SealLabels,
    recipient_pub: &PublicKey,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let eph_priv = SecretKey::generate();
    let eph_pub = eph_priv.public_key();
    let shared = eph_priv.diffie_hellman(recipient_pub);

    let (key, nonce) = derive_key_and_nonce(labels, &shared, &eph_pub)?;
    let ciphertext = aead::seal(ALG, &key, &nonce, aad, plaintext)?;

    let mut out = Vec::with_capacity(EPH_PUB_LEN + ciphertext.len());
    out.extend_from_slice(&eph_pub.to_bytes());
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Open a sealed-box envelope addressed to `recipient_priv`. `aad`
/// must match what the sender supplied to [`seal`].
///
/// # Errors
///
/// Returns:
///
/// - [`ContactError::InvalidEncoding`] if the wire is too short.
/// - [`ContactError::WrongRecipient`] if AEAD authentication fails
///   (wrong key, wrong AAD, tampered ciphertext). Callers can
///   distinguish "not for me" from "tampered" by re-trying with a
///   different recipient key — but in practice the two are
///   indistinguishable on a sealed box, which is the expected
///   security property.
pub fn open(recipient_priv: &SecretKey, aad: &[u8], wire: &[u8]) -> Result<Vec<u8>> {
    open_labeled(&CONTACT_LABELS, recipient_priv, aad, wire)
}

/// [`open`] under `labels` instead of the contact-request labels.
pub(crate) fn open_labeled(
    labels: &SealLabels,
    recipient_priv: &SecretKey,
    aad: &[u8],
    wire: &[u8],
) -> Result<Vec<u8>> {
    if wire.len() < EPH_PUB_LEN + TAG_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "sealed envelope too short: {} bytes (need ≥ {})",
            wire.len(),
            EPH_PUB_LEN + TAG_LEN,
        )));
    }
    let (eph_pub_bytes, ciphertext) = wire.split_at(EPH_PUB_LEN);
    let mut eph_arr = [0u8; EPH_PUB_LEN];
    eph_arr.copy_from_slice(eph_pub_bytes);
    let eph_pub = PublicKey::from_bytes(&eph_arr).map_err(ContactError::Crypto)?;

    let shared = recipient_priv.diffie_hellman(&eph_pub);
    let (key, nonce) = derive_key_and_nonce(labels, &shared, &eph_pub)?;
    aead::open(ALG, &key, &nonce, aad, ciphertext).map_err(|_| ContactError::WrongRecipient)
}

/// HKDF the AEAD key and nonce from the ECDH shared secret. The
/// ephemeral public key is mixed into the `info` so two envelopes
/// against the same recipient — even with the (astronomically
/// unlikely) same shared secret — derive distinct AEAD nonces.
fn derive_key_and_nonce(
    labels: &SealLabels,
    shared: &SecretBytes<32>,
    eph_pub: &PublicKey,
) -> Result<(AeadKey, Nonce)> {
    let mut info_key = Vec::with_capacity(labels.key.len() + EPH_PUB_LEN);
    info_key.extend_from_slice(labels.key);
    info_key.extend_from_slice(&eph_pub.to_bytes());
    let derived_key: SecretBytes<KEY_LEN> = kdf::hkdf_sha256(shared.as_bytes(), &[], &info_key)?;
    let mut key_bytes = [0u8; KEY_LEN];
    key_bytes.copy_from_slice(derived_key.as_bytes());
    let key = AeadKey::new(key_bytes);

    let mut info_nonce = Vec::with_capacity(labels.nonce.len() + EPH_PUB_LEN);
    info_nonce.extend_from_slice(labels.nonce);
    info_nonce.extend_from_slice(&eph_pub.to_bytes());
    let derived_nonce: SecretBytes<NONCE_LEN> =
        kdf::hkdf_sha256(shared.as_bytes(), &[], &info_nonce)?;
    let mut nonce: Nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(derived_nonce.as_bytes());

    Ok((key, nonce))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let recipient = SecretKey::generate();
        let pt = b"hello bob";
        let aad = b"contact-request-v1";

        let wire = seal(&recipient.public_key(), aad, pt).unwrap();
        let recovered = open(&recipient, aad, &wire).unwrap();
        assert_eq!(recovered, pt);
    }

    #[test]
    fn seal_produces_distinct_wires_each_time() {
        let recipient = SecretKey::generate();
        let pt = b"same plaintext";
        let aad = b"same aad";

        let w1 = seal(&recipient.public_key(), aad, pt).unwrap();
        let w2 = seal(&recipient.public_key(), aad, pt).unwrap();
        assert_ne!(
            w1, w2,
            "fresh ephemeral keypair must yield distinct envelopes",
        );
    }

    #[test]
    fn wrong_recipient_fails_open() {
        let recipient = SecretKey::generate();
        let attacker = SecretKey::generate();
        let pt = b"secret";

        let wire = seal(&recipient.public_key(), b"", pt).unwrap();
        let err = open(&attacker, b"", &wire).unwrap_err();
        assert!(matches!(err, ContactError::WrongRecipient));
    }

    #[test]
    fn wrong_aad_fails_open() {
        let recipient = SecretKey::generate();
        let pt = b"secret";

        let wire = seal(&recipient.public_key(), b"intended-aad", pt).unwrap();
        let err = open(&recipient, b"different-aad", &wire).unwrap_err();
        assert!(matches!(err, ContactError::WrongRecipient));
    }

    #[test]
    fn tampered_ciphertext_fails_open() {
        let recipient = SecretKey::generate();
        let pt = b"secret";

        let mut wire = seal(&recipient.public_key(), b"", pt).unwrap();
        // Flip a bit in the ciphertext (after the 32-byte ephemeral pub).
        wire[EPH_PUB_LEN] ^= 0x01;
        let err = open(&recipient, b"", &wire).unwrap_err();
        assert!(matches!(err, ContactError::WrongRecipient));
    }

    #[test]
    fn truncated_wire_fails_open() {
        let recipient = SecretKey::generate();
        let pt = b"x";
        let wire = seal(&recipient.public_key(), b"", pt).unwrap();

        // Cut off the AEAD tag.
        let truncated = &wire[..wire.len() - TAG_LEN - 1];
        let err = open(&recipient, b"", truncated).unwrap_err();
        // Either too-short wire or AEAD failure — both are acceptable
        // signals.
        assert!(matches!(
            err,
            ContactError::InvalidEncoding(_) | ContactError::WrongRecipient,
        ));
    }

    #[test]
    fn empty_wire_fails_open() {
        let recipient = SecretKey::generate();
        let err = open(&recipient, b"", &[]).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn wire_starts_with_ephemeral_pub() {
        let recipient = SecretKey::generate();
        let wire = seal(&recipient.public_key(), b"", b"x").unwrap();
        assert_eq!(wire.len(), EPH_PUB_LEN + 1 /* pt */ + TAG_LEN);
    }
}
