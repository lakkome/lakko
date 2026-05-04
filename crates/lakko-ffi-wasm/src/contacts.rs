// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Mirror of `lakko-contacts`: pseudonym lifecycle and the
//! [`ContactRequest`] envelope that drives ADR-0005 scenario A
//! ("I know `@username`, add them"). Phase 1.9a only.

use lakko_contacts::{
    compose as rust_compose, decode_and_verify as rust_decode, open_envelope as rust_open,
    ContactError, ContactRequest as RustContactRequest, Direction as RustDirection,
    IdentityPseudonym as RustIdentityPseudonym, PerRelationship as RustPerRelationship,
    StrangerInbox as RustStrangerInbox, PSEUDONYM_LEN,
};
use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use lakko_crypto::secret::SecretBytes;
use lakko_crypto::signing::{SigningKey, VerifyingKey, PUBLIC_KEY_LEN};
use wasm_bindgen::prelude::*;

fn contact_err(e: ContactError) -> JsError {
    JsError::new(&format!("{e}"))
}

fn crypto_err(e: lakko_crypto::error::Error) -> JsError {
    JsError::new(&format!("{e}"))
}

// ============================================================================
// Pseudonym types
// ============================================================================

/// Permanent identity pseudonym derived from the long-term Ed25519
/// public key. Mirror of [`lakko_contacts::IdentityPseudonym`].
#[wasm_bindgen]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct IdentityPseudonym {
    inner: RustIdentityPseudonym,
}

#[wasm_bindgen]
impl IdentityPseudonym {
    /// Derive from a 32-byte Ed25519 verifying key.
    #[wasm_bindgen(js_name = fromIdentity)]
    pub fn from_identity(pubkey: &[u8]) -> Result<IdentityPseudonym, JsError> {
        if pubkey.len() != PUBLIC_KEY_LEN {
            return Err(JsError::new(&format!(
                "identity pubkey must be {PUBLIC_KEY_LEN} bytes",
            )));
        }
        let vk = VerifyingKey::from_bytes(pubkey).map_err(crypto_err)?;
        Ok(Self {
            inner: RustIdentityPseudonym::from_identity(&vk),
        })
    }

    /// Wrap raw 32-byte pseudonym (e.g. parsed off a wire format).
    #[wasm_bindgen(js_name = fromBytes)]
    pub fn from_bytes(bytes: &[u8]) -> Result<IdentityPseudonym, JsError> {
        let arr = decode_pseudonym_bytes(bytes)?;
        Ok(Self {
            inner: RustIdentityPseudonym::from_bytes(arr),
        })
    }

    /// 32 raw bytes.
    #[wasm_bindgen(js_name = asBytes)]
    pub fn as_bytes(&self) -> Vec<u8> {
        self.inner.as_bytes().to_vec()
    }
}

/// Long-lived stranger-inbox pseudonym. Mirror of
/// [`lakko_contacts::StrangerInbox`].
#[wasm_bindgen]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct StrangerInbox {
    inner: RustStrangerInbox,
}

#[wasm_bindgen]
impl StrangerInbox {
    /// Derive from the owner's 32-byte stranger-inbox secret.
    #[wasm_bindgen(js_name = derive)]
    pub fn derive(secret: &[u8]) -> Result<StrangerInbox, JsError> {
        let arr = decode_pseudonym_bytes(secret)?;
        let s = SecretBytes::<32>::new(arr);
        Ok(Self {
            inner: RustStrangerInbox::derive(&s),
        })
    }

    /// 32 raw bytes.
    #[wasm_bindgen(js_name = asBytes)]
    pub fn as_bytes(&self) -> Vec<u8> {
        self.inner.as_bytes().to_vec()
    }
}

/// Direction of a per-relationship pseudonym. Distinct values
/// guarantee distinct pseudonyms for the same `seed`, breaking
/// trivial server-side pair correlation.
#[wasm_bindgen]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Direction {
    /// Self-to-Peer: addresses a message FROM us TO the contact.
    Outbound,
    /// Peer-to-Self: addresses a message FROM the contact TO us.
    Inbound,
}

impl From<Direction> for RustDirection {
    fn from(d: Direction) -> Self {
        match d {
            Direction::Outbound => RustDirection::Outbound,
            Direction::Inbound => RustDirection::Inbound,
        }
    }
}

/// Per-relationship pseudonym. Mirror of [`lakko_contacts::PerRelationship`].
#[wasm_bindgen]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct PerRelationship {
    inner: RustPerRelationship,
}

#[wasm_bindgen]
impl PerRelationship {
    /// Derive from the shared 32-byte relationship seed and direction.
    #[wasm_bindgen(js_name = derive)]
    pub fn derive(seed: &[u8], direction: Direction) -> Result<PerRelationship, JsError> {
        let arr = decode_pseudonym_bytes(seed)?;
        let s = SecretBytes::<32>::new(arr);
        Ok(Self {
            inner: RustPerRelationship::derive(&s, direction.into()),
        })
    }

    /// 32 raw bytes.
    #[wasm_bindgen(js_name = asBytes)]
    pub fn as_bytes(&self) -> Vec<u8> {
        self.inner.as_bytes().to_vec()
    }
}

fn decode_pseudonym_bytes(bytes: &[u8]) -> Result<[u8; PSEUDONYM_LEN], JsError> {
    if bytes.len() != PSEUDONYM_LEN {
        return Err(JsError::new(&format!(
            "expected {PSEUDONYM_LEN} bytes, got {}",
            bytes.len(),
        )));
    }
    let mut out = [0u8; PSEUDONYM_LEN];
    out.copy_from_slice(bytes);
    Ok(out)
}

// ============================================================================
// ContactRequest
// ============================================================================

/// Composed and verified ContactRequest. Mirror of
/// [`lakko_contacts::ContactRequest`].
///
/// Senders use [`ContactRequest::compose`] to produce wire bytes;
/// recipients use [`ContactRequest::decode_and_verify`] to validate
/// and then [`ContactRequest::open_envelope`] (post-accept) to
/// recover the intro plaintext.
#[wasm_bindgen]
pub struct ContactRequest {
    inner: RustContactRequest,
}

#[wasm_bindgen]
impl ContactRequest {
    /// Compose a signed ContactRequest. Returns the wire bytes ready
    /// to deliver to the recipient's stranger-inbox via
    /// `services/ds`. `senderSigningSeed` is a 32-byte Ed25519 seed
    /// (see `Identity` in the MLS surface for one already wired up
    /// to the device key); `recipientX25519Pub` is the recipient's
    /// X25519 public key bytes.
    ///
    /// # Errors
    ///
    /// `JsError` on any input-shape or AEAD failure.
    #[wasm_bindgen]
    pub fn compose(
        sender_signing_seed: &[u8],
        recipient_x25519_pub: &[u8],
        from_pseudonym: &[u8],
        valid_after_ms: u64,
        intro_message: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        if sender_signing_seed.len() != 32 {
            return Err(JsError::new("senderSigningSeed must be 32 bytes"));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(sender_signing_seed);
        let signing = SigningKey::from_seed(&seed);

        if recipient_x25519_pub.len() != PUBLIC_KEY_LEN {
            return Err(JsError::new(&format!(
                "recipientX25519Pub must be {PUBLIC_KEY_LEN} bytes",
            )));
        }
        let mut x25519_pub_arr = [0u8; PUBLIC_KEY_LEN];
        x25519_pub_arr.copy_from_slice(recipient_x25519_pub);
        let recipient_pub = X25519PublicKey::from_bytes(&x25519_pub_arr).map_err(crypto_err)?;

        let pseudonym = decode_pseudonym_bytes(from_pseudonym)?;

        rust_compose(
            &signing,
            &recipient_pub,
            pseudonym,
            valid_after_ms,
            intro_message,
        )
        .map_err(contact_err)
    }

    /// Recipient-side: decode wire bytes, verify the Ed25519
    /// signature, return the validated `ContactRequest`.
    #[wasm_bindgen(js_name = decodeAndVerify)]
    pub fn decode_and_verify(wire: &[u8]) -> Result<ContactRequest, JsError> {
        let inner = rust_decode(wire).map_err(contact_err)?;
        Ok(Self { inner })
    }

    /// Sender's identity public key (32-byte Ed25519 verifying key).
    #[wasm_bindgen(getter, js_name = fromPubkey)]
    pub fn from_pubkey(&self) -> Vec<u8> {
        self.inner.from_pubkey.to_bytes().to_vec()
    }

    /// Per-recipient pseudonym chosen by the sender (32 bytes).
    #[wasm_bindgen(getter, js_name = fromPseudonym)]
    pub fn from_pseudonym(&self) -> Vec<u8> {
        self.inner.from_pseudonym.to_vec()
    }

    /// UNIX milliseconds at which the request becomes valid (sender's
    /// freshness anchor — the application layer enforces an absolute
    /// staleness ceiling on top of this).
    #[wasm_bindgen(getter, js_name = validAfterMs)]
    pub fn valid_after_ms(&self) -> u64 {
        self.inner.valid_after_ms
    }

    /// Decrypt the intro envelope using the recipient's 32-byte
    /// X25519 private key. Returns the plaintext bytes.
    #[wasm_bindgen(js_name = openEnvelope)]
    pub fn open_envelope(&self, recipient_x25519_priv: &[u8]) -> Result<Vec<u8>, JsError> {
        if recipient_x25519_priv.len() != 32 {
            return Err(JsError::new("recipientX25519Priv must be 32 bytes"));
        }
        let mut sk = [0u8; 32];
        sk.copy_from_slice(recipient_x25519_priv);
        let priv_key = X25519SecretKey::from_bytes(sk);
        rust_open(&self.inner, &priv_key).map_err(contact_err)
    }
}

// Tests cover only happy paths — error paths invoke `JsError::new`,
// which is a wasm-bindgen-imported function and panics with
// "cannot call wasm-bindgen imported functions on non-wasm targets"
// when run with the host `cargo test`. Error-path coverage lives in
// the underlying `lakko-contacts` crate (it's the same logic). The
// tests here exist as a shape-regression sentinel against accidental
// API drift.
#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey as Ed25519;

    #[test]
    fn pseudonym_classes_diverge() {
        let secret = [0xCDu8; 32];
        let stranger = StrangerInbox::derive(&secret).unwrap();
        let rel_out = PerRelationship::derive(&secret, Direction::Outbound).unwrap();
        let rel_in = PerRelationship::derive(&secret, Direction::Inbound).unwrap();
        assert_ne!(stranger.as_bytes(), rel_out.as_bytes());
        assert_ne!(rel_out.as_bytes(), rel_in.as_bytes());
    }

    #[test]
    fn identity_pseudonym_from_valid_pubkey() {
        let id = Ed25519::from_seed(&[7u8; 32]);
        let pubkey = id.verifying_key().to_bytes();
        let p = IdentityPseudonym::from_identity(&pubkey).unwrap();
        assert_eq!(p.as_bytes().len(), 32);
    }

    #[test]
    fn contact_request_roundtrip_happy_path() {
        let sender_seed = [7u8; 32];
        let recipient_x25519 = X25519SecretKey::from_bytes([0xCD; 32]);
        let recipient_pub = recipient_x25519.public_key().to_bytes();
        let pseudonym = [0xAA; 32];

        let wire = ContactRequest::compose(
            &sender_seed,
            &recipient_pub,
            &pseudonym,
            1_000_000,
            b"hi bob",
        )
        .unwrap();

        let req = ContactRequest::decode_and_verify(&wire).unwrap();
        assert_eq!(req.from_pseudonym(), pseudonym.to_vec());
        assert_eq!(req.valid_after_ms(), 1_000_000);

        let priv_bytes = recipient_x25519.to_bytes();
        let plaintext = req.open_envelope(&priv_bytes).unwrap();
        assert_eq!(plaintext, b"hi bob");
    }
}
