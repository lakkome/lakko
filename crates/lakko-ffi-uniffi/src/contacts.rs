// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI exposure of `lakko-contacts`.
//!
//! Mirror of `lakko-ffi-wasm/src/contacts.rs` (1.18b shape, scoped
//! down to the contact-request side; the invite-capability codec
//! + revocation helpers land in 1.15e6).
//!
//! Three pseudonym types — [`IdentityPseudonym`], [`StrangerInbox`],
//! [`PerRelationship`] — plus the [`Direction`] enum that disambiguates
//! per-relationship pseudonyms by inbound / outbound direction.
//!
//! The contact-request envelope is split into three operations:
//!
//!   * [`contact_request_compose`] — sender side. Top-level free
//!     function (rather than an associated function on
//!     [`ContactRequest`]) because UniFFI 0.28 rejects associated
//!     fns on `Object`-typed classes — same workaround as the
//!     `pow_solve` top-level in 1.15d1.
//!
//!   * [`ContactRequest::decode_and_verify`] — recipient side step
//!     1: validate the wire bytes + Ed25519 signature.
//!     `#[uniffi::constructor]`, surfaces on Kotlin as
//!     `ContactRequest.decodeAndVerify(wire)`.
//!
//!   * [`ContactRequest::open_envelope`] — recipient side step 2:
//!     open the sealed-box intro message with the recipient's
//!     X25519 private key. Method (not constructor) because it
//!     returns plaintext bytes, not another `ContactRequest`.

use lakko_contacts::{
    compose as rust_compose, decode_and_verify as rust_decode, open_envelope as rust_open,
    ContactError, ContactRequest as RustContactRequest, Direction as RustDirection,
    IdentityPseudonym as RustIdentityPseudonym, PerRelationship as RustPerRelationship,
    StrangerInbox as RustStrangerInbox, ACCOUNT_ID_LEN, PSEUDONYM_LEN,
};
use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use lakko_crypto::secret::SecretBytes;
use lakko_crypto::signing::{SigningKey, VerifyingKey, PUBLIC_KEY_LEN};

use crate::FfiError;

impl From<ContactError> for FfiError {
    fn from(e: ContactError) -> Self {
        Self::Contacts(e.to_string())
    }
}

impl From<lakko_crypto::error::Error> for FfiError {
    fn from(e: lakko_crypto::error::Error) -> Self {
        Self::Crypto(e.to_string())
    }
}

/// Helper: validate that `bytes` is exactly `PSEUDONYM_LEN` bytes
/// and copy into a stack-allocated array.
fn decode_pseudonym_bytes(bytes: &[u8]) -> Result<[u8; PSEUDONYM_LEN], FfiError> {
    if bytes.len() != PSEUDONYM_LEN {
        return Err(FfiError::Contacts(format!(
            "expected {PSEUDONYM_LEN} bytes, got {}",
            bytes.len(),
        )));
    }
    let mut out = [0u8; PSEUDONYM_LEN];
    out.copy_from_slice(bytes);
    Ok(out)
}

// ============================================================================
// Pseudonym types
// ============================================================================

/// Permanent identity pseudonym derived from the long-term Ed25519
/// public key. Mirror of [`lakko_contacts::IdentityPseudonym`].
///
/// `BLAKE3(domain_separator || identity_pk_bytes)` — deterministic
/// and one-way: anyone with the public key can compute it; no
/// information about the private key leaks.
#[derive(uniffi::Object)]
pub struct IdentityPseudonym {
    inner: RustIdentityPseudonym,
}

#[uniffi::export]
impl IdentityPseudonym {
    /// Derive from a 32-byte Ed25519 verifying key.
    #[uniffi::constructor]
    pub fn from_identity(pubkey: Vec<u8>) -> Result<Self, FfiError> {
        if pubkey.len() != PUBLIC_KEY_LEN {
            return Err(FfiError::Contacts(format!(
                "identity pubkey must be {PUBLIC_KEY_LEN} bytes",
            )));
        }
        let vk = VerifyingKey::from_bytes(&pubkey)?;
        Ok(Self {
            inner: RustIdentityPseudonym::from_identity(&vk),
        })
    }

    /// Wrap raw 32-byte pseudonym (e.g. parsed off a wire format).
    #[uniffi::constructor]
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, FfiError> {
        let arr = decode_pseudonym_bytes(&bytes)?;
        Ok(Self {
            inner: RustIdentityPseudonym::from_bytes(arr),
        })
    }

    /// 32 raw bytes.
    pub fn as_bytes(&self) -> Vec<u8> {
        self.inner.as_bytes().to_vec()
    }
}

/// Long-lived stranger-inbox pseudonym. Mirror of
/// [`lakko_contacts::StrangerInbox`].
#[derive(uniffi::Object)]
pub struct StrangerInbox {
    inner: RustStrangerInbox,
}

#[uniffi::export]
impl StrangerInbox {
    /// Derive from the owner's 32-byte stranger-inbox secret.
    ///
    /// The secret should be held in [`SecretBytes`]-equivalent
    /// storage on the caller side — at the FFI boundary it's a
    /// plain `Vec<u8>`, but the inner derivation immediately
    /// wraps it in `SecretBytes<32>` for zeroize-on-drop.
    #[uniffi::constructor]
    pub fn derive(secret: Vec<u8>) -> Result<Self, FfiError> {
        let arr = decode_pseudonym_bytes(&secret)?;
        let s = SecretBytes::<32>::new(arr);
        Ok(Self {
            inner: RustStrangerInbox::derive(&s),
        })
    }

    /// 32 raw bytes.
    pub fn as_bytes(&self) -> Vec<u8> {
        self.inner.as_bytes().to_vec()
    }
}

/// Direction of a per-relationship pseudonym. Distinct values
/// guarantee distinct pseudonyms for the same `seed`, breaking
/// trivial server-side pair correlation.
#[derive(uniffi::Enum)]
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

/// Per-relationship pseudonym. Mirror of
/// [`lakko_contacts::PerRelationship`].
#[derive(uniffi::Object)]
pub struct PerRelationship {
    inner: RustPerRelationship,
}

#[uniffi::export]
impl PerRelationship {
    /// Derive from the shared 32-byte relationship seed and direction.
    #[uniffi::constructor]
    pub fn derive(seed: Vec<u8>, direction: Direction) -> Result<Self, FfiError> {
        let arr = decode_pseudonym_bytes(&seed)?;
        let s = SecretBytes::<32>::new(arr);
        Ok(Self {
            inner: RustPerRelationship::derive(&s, direction.into()),
        })
    }

    /// 32 raw bytes.
    pub fn as_bytes(&self) -> Vec<u8> {
        self.inner.as_bytes().to_vec()
    }
}

// ============================================================================
// ContactRequest
// ============================================================================

/// Compose a signed `ContactRequest` and return its wire bytes.
///
/// Top-level free function rather than an associated function on
/// [`ContactRequest`] because UniFFI 0.28 rejects associated fns
/// on `Object`-typed classes (same workaround as `pow_solve` in
/// 1.15d1). On Kotlin this surfaces as a top-level
/// `composeContactRequest(...)`; on Swift as a free function.
///
/// `sender_signing_seed` is a 32-byte Ed25519 seed (the device
/// identity's signing seed — `Identity` in the MLS surface holds
/// the canonical source). `sender_account_id` is the sender's
/// 16-byte UUIDv7. `recipient_x25519_pub` is the recipient's
/// long-term X25519 public key (32 bytes). `from_pseudonym` is a
/// fresh per-recipient pseudonym — 32 random bytes or a derive
/// from [`PerRelationship`] with a one-shot seed.
///
/// `valid_after_ms` is the sender's UNIX-ms freshness anchor;
/// the application layer enforces an absolute staleness ceiling
/// on top of this on the receive side.
///
/// `intro_message` is the optional plaintext payload (sealed in
/// an HPKE-equivalent envelope to `recipient_x25519_pub`).
///
/// # Errors
///
/// * [`FfiError::Contacts`] on wrong-length seed / account-id /
///   pseudonym, or any AEAD failure inside the sealed envelope.
/// * [`FfiError::Crypto`] when `recipient_x25519_pub` is not a
///   valid X25519 public key (canonical encoding).
#[uniffi::export]
pub fn compose_contact_request(
    sender_signing_seed: Vec<u8>,
    sender_account_id: Vec<u8>,
    recipient_x25519_pub: Vec<u8>,
    from_pseudonym: Vec<u8>,
    valid_after_ms: u64,
    intro_message: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    if sender_signing_seed.len() != 32 {
        return Err(FfiError::Contacts(
            "senderSigningSeed must be 32 bytes".to_string(),
        ));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&sender_signing_seed);
    let signing = SigningKey::from_seed(&seed);

    if sender_account_id.len() != ACCOUNT_ID_LEN {
        return Err(FfiError::Contacts(format!(
            "senderAccountId must be {ACCOUNT_ID_LEN} bytes",
        )));
    }
    let mut acct = [0u8; ACCOUNT_ID_LEN];
    acct.copy_from_slice(&sender_account_id);

    if recipient_x25519_pub.len() != PUBLIC_KEY_LEN {
        return Err(FfiError::Contacts(format!(
            "recipientX25519Pub must be {PUBLIC_KEY_LEN} bytes",
        )));
    }
    let mut x25519_pub_arr = [0u8; PUBLIC_KEY_LEN];
    x25519_pub_arr.copy_from_slice(&recipient_x25519_pub);
    let recipient_pub = X25519PublicKey::from_bytes(&x25519_pub_arr)?;

    let pseudonym = decode_pseudonym_bytes(&from_pseudonym)?;

    let wire = rust_compose(
        &signing,
        &acct,
        &recipient_pub,
        pseudonym,
        valid_after_ms,
        &intro_message,
    )?;
    Ok(wire)
}

/// Composed and verified ContactRequest. Mirror of
/// [`lakko_contacts::ContactRequest`].
///
/// Senders produce wire bytes via [`compose_contact_request`];
/// recipients use [`ContactRequest::decode_and_verify`] to
/// validate the signature + structure, then
/// [`ContactRequest::open_envelope`] (post-accept) to recover
/// the intro plaintext.
#[derive(uniffi::Object)]
pub struct ContactRequest {
    inner: RustContactRequest,
}

#[uniffi::export]
impl ContactRequest {
    /// Recipient-side: decode wire bytes, verify the Ed25519
    /// signature, return the validated [`ContactRequest`].
    ///
    /// Does NOT open the sealed envelope — call [`open_envelope`]
    /// after the user decides to accept. Splitting lets the UI
    /// render "pending request from <fingerprint>" before paying
    /// the X25519 + AEAD cost on accept.
    ///
    /// # Errors
    ///
    /// * [`FfiError::Contacts`] — wire format malformed or Ed25519
    ///   signature failed.
    #[uniffi::constructor]
    pub fn decode_and_verify(wire: Vec<u8>) -> Result<Self, FfiError> {
        let inner = rust_decode(&wire)?;
        Ok(Self { inner })
    }

    /// Sender's identity public key (32-byte Ed25519 verifying
    /// key).
    pub fn from_pubkey(&self) -> Vec<u8> {
        self.inner.from_pubkey.to_bytes().to_vec()
    }

    /// Sender's `account_id` (16-byte UUIDv7) — present for v2
    /// envelopes, `None` for legacy v1.
    pub fn from_account_id(&self) -> Option<Vec<u8>> {
        self.inner.from_account_id.map(|a| a.to_vec())
    }

    /// Per-recipient pseudonym chosen by the sender (32 bytes).
    pub fn from_pseudonym(&self) -> Vec<u8> {
        self.inner.from_pseudonym.to_vec()
    }

    /// UNIX milliseconds at which the request becomes valid.
    /// The application layer enforces an absolute staleness
    /// ceiling on top of this on the receive side.
    pub fn valid_after_ms(&self) -> u64 {
        self.inner.valid_after_ms
    }

    /// Decrypt the intro envelope using the recipient's 32-byte
    /// X25519 private key. Returns the plaintext bytes.
    ///
    /// # Errors
    ///
    /// * [`FfiError::Contacts`] when the AEAD open fails
    ///   (wrong key or tampered ciphertext).
    pub fn open_envelope(&self, recipient_x25519_priv: Vec<u8>) -> Result<Vec<u8>, FfiError> {
        if recipient_x25519_priv.len() != 32 {
            return Err(FfiError::Contacts(
                "recipientX25519Priv must be 32 bytes".to_string(),
            ));
        }
        let mut sk = [0u8; 32];
        sk.copy_from_slice(&recipient_x25519_priv);
        let priv_key = X25519SecretKey::from_bytes(sk);
        let plaintext = rust_open(&self.inner, &priv_key)?;
        Ok(plaintext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::agreement::SecretKey as InnerX25519SecretKey;

    // ===================================================================
    // Pseudonym surface
    // ===================================================================

    #[test]
    fn identity_pseudonym_from_identity_is_32_bytes() {
        // Any valid Ed25519 verifying key — derive one from a
        // fixed seed for determinism.
        let signing = SigningKey::from_seed(&[1u8; 32]);
        let pubkey = signing.verifying_key().to_bytes().to_vec();
        let pseudonym = IdentityPseudonym::from_identity(pubkey).expect("from_identity");
        assert_eq!(pseudonym.as_bytes().len(), PSEUDONYM_LEN);
    }

    #[test]
    fn identity_pseudonym_rejects_wrong_length() {
        // `expect_err` would need `Debug` on the `Object`-typed
        // Ok variant — UniFFI doesn't auto-derive it. Match on
        // the Err arm explicitly and assert against the Ok arm
        // without trying to format it.
        match IdentityPseudonym::from_identity(vec![0u8; 31]) {
            Err(FfiError::Contacts(msg)) => assert!(msg.contains("32 bytes")),
            Err(other) => panic!("expected Contacts error, got {other:?}"),
            Ok(_) => panic!("expected wrong-length rejection"),
        }
    }

    #[test]
    fn identity_pseudonym_from_bytes_round_trips() {
        let bytes = vec![7u8; PSEUDONYM_LEN];
        let pseudonym = IdentityPseudonym::from_bytes(bytes.clone()).expect("from_bytes");
        assert_eq!(pseudonym.as_bytes(), bytes);
    }

    #[test]
    fn stranger_inbox_derive_is_32_bytes() {
        let secret = vec![5u8; PSEUDONYM_LEN];
        let inbox = StrangerInbox::derive(secret).expect("derive");
        assert_eq!(inbox.as_bytes().len(), PSEUDONYM_LEN);
    }

    #[test]
    fn per_relationship_directions_differ() {
        // Same seed, different directions — pseudonyms must
        // differ by construction (the whole point of the
        // direction tag).
        let seed = vec![9u8; PSEUDONYM_LEN];
        let outbound =
            PerRelationship::derive(seed.clone(), Direction::Outbound).expect("derive outbound");
        let inbound = PerRelationship::derive(seed, Direction::Inbound).expect("derive inbound");
        assert_ne!(outbound.as_bytes(), inbound.as_bytes());
    }

    // ===================================================================
    // ContactRequest end-to-end
    // ===================================================================

    /// Compose → decode_and_verify → open_envelope round trip.
    /// Exercises the full happy path through the FFI boundary
    /// without involving the network or any persistent state.
    #[test]
    fn contact_request_round_trip() {
        // Alice signing key (sender).
        let alice_signing = SigningKey::from_seed(&[0xAAu8; 32]);
        let alice_account_id = [0xA1u8; ACCOUNT_ID_LEN];

        // Bob X25519 keypair (recipient).
        let bob_priv = InnerX25519SecretKey::from_bytes([0xBBu8; 32]);
        let bob_pub = bob_priv.public_key();

        let from_pseudonym = vec![0x55u8; PSEUDONYM_LEN];
        let intro = b"hello from alice".to_vec();

        let wire = compose_contact_request(
            alice_signing.to_seed().to_vec(),
            alice_account_id.to_vec(),
            bob_pub.to_bytes().to_vec(),
            from_pseudonym.clone(),
            1_700_000_000_000,
            intro.clone(),
        )
        .expect("compose");

        let req = ContactRequest::decode_and_verify(wire).expect("decode");
        // Sender identity round-trips.
        assert_eq!(
            req.from_pubkey(),
            alice_signing.verifying_key().to_bytes().to_vec(),
        );
        assert_eq!(req.from_account_id(), Some(alice_account_id.to_vec()));
        assert_eq!(req.from_pseudonym(), from_pseudonym);
        assert_eq!(req.valid_after_ms(), 1_700_000_000_000);

        // Open the sealed envelope with Bob's private key.
        let plaintext = req
            .open_envelope(bob_priv.to_bytes().to_vec())
            .expect("open");
        assert_eq!(plaintext, intro);
    }

    #[test]
    fn contact_request_rejects_wrong_seed_length() {
        // The Ok arm of `compose_contact_request` is `Vec<u8>`
        // which has `Debug`, so `expect_err` works here.
        let err = compose_contact_request(
            vec![0u8; 16], // <-- too short
            vec![0u8; ACCOUNT_ID_LEN],
            vec![0u8; PUBLIC_KEY_LEN],
            vec![0u8; PSEUDONYM_LEN],
            0,
            vec![],
        )
        .expect_err("expected wrong-length rejection");
        match err {
            FfiError::Contacts(msg) => assert!(msg.contains("32 bytes")),
            other => panic!("expected Contacts error, got {other:?}"),
        }
    }

    #[test]
    fn contact_request_rejects_wrong_account_id_length() {
        let err = compose_contact_request(
            vec![0u8; 32],
            vec![0u8; 8], // <-- too short
            vec![0u8; PUBLIC_KEY_LEN],
            vec![0u8; PSEUDONYM_LEN],
            0,
            vec![],
        )
        .expect_err("expected wrong-length rejection");
        match err {
            FfiError::Contacts(msg) => assert!(msg.contains("16 bytes")),
            other => panic!("expected Contacts error, got {other:?}"),
        }
    }

    #[test]
    fn open_envelope_rejects_wrong_recipient_key() {
        // Compose to Bob, try to open with Mallory's key — the
        // AEAD tag check fails and `rust_open` returns
        // `ContactError`, which we surface as `FfiError::Contacts`.
        let alice_signing = SigningKey::from_seed(&[0xAAu8; 32]);
        let bob_priv = InnerX25519SecretKey::from_bytes([0xBBu8; 32]);
        let bob_pub = bob_priv.public_key();
        let mallory_priv = InnerX25519SecretKey::from_bytes([0xCCu8; 32]);

        let wire = compose_contact_request(
            alice_signing.to_seed().to_vec(),
            vec![0u8; ACCOUNT_ID_LEN],
            bob_pub.to_bytes().to_vec(),
            vec![0u8; PSEUDONYM_LEN],
            0,
            b"secret".to_vec(),
        )
        .expect("compose");
        let req = ContactRequest::decode_and_verify(wire).expect("decode");
        match req.open_envelope(mallory_priv.to_bytes().to_vec()) {
            Err(FfiError::Contacts(_)) => {},
            other => panic!("expected Contacts error, got {other:?}"),
        }
    }
}
