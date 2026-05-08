// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Mirror of `lakko-contacts`: pseudonym lifecycle and the
//! [`ContactRequest`] envelope that drives ADR-0005 scenario A
//! ("I know `@username`, add them"). Phase 1.9a only.

use lakko_contacts::invite::{
    compose as rust_invite_compose, compute_joiner_tag as rust_compute_joiner_tag,
    decode_and_verify as rust_invite_decode, sign_revocation as rust_sign_revocation, BLOB_ID_LEN,
};
use lakko_contacts::{
    compose as rust_compose, decode_and_verify as rust_decode, open_envelope as rust_open,
    ContactError, ContactRequest as RustContactRequest, Direction as RustDirection,
    IdentityPseudonym as RustIdentityPseudonym, InviteCapability as RustInviteCapability,
    PerRelationship as RustPerRelationship, StrangerInbox as RustStrangerInbox, MAX_LABEL_LEN,
    PSEUDONYM_LEN,
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
        sender_account_id: &[u8],
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

        if sender_account_id.len() != lakko_contacts::ACCOUNT_ID_LEN {
            return Err(JsError::new(&format!(
                "senderAccountId must be {} bytes",
                lakko_contacts::ACCOUNT_ID_LEN,
            )));
        }
        let mut acct = [0u8; lakko_contacts::ACCOUNT_ID_LEN];
        acct.copy_from_slice(sender_account_id);

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
            &acct,
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

    /// Sender's `account_id` (16-byte UUIDv7) — present for v2
    /// envelopes, `null`/`undefined` for legacy v1 envelopes.
    /// `Phase 1.16e3b` always sends v2; v1 surfaces only when
    /// the sender pre-dates the deployment upgrade.
    #[wasm_bindgen(getter, js_name = fromAccountId)]
    pub fn from_account_id(&self) -> Option<Vec<u8>> {
        self.inner.from_account_id.map(|a| a.to_vec())
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

// ============================================================================
// Invite capability (Phase 1.18b)
// ============================================================================

/// Composed and verified invite capability — capability-based
/// contact-invite flow per ADR-0005 §13.3 / §13.4. Mirror of
/// [`lakko_contacts::InviteCapability`].
///
/// Owners drive [`InviteCapability::compose`] to produce signed
/// wire bytes; the JS layer base64url-encodes the bytes into a
/// `https://lakko.me/i/<...>` URL and shares it OOB. Joiners
/// drive [`InviteCapability::decodeAndVerify`] on the bytes
/// recovered from the URL slug, then read the getters to seed
/// the first ContactRequest back to the inviter.
///
/// Phase 1.18b ships the JS surface; the V1 wire format is the
/// 1.18a Rust struct verbatim — no server dependency, capability
/// bytes live in the URL itself.
#[wasm_bindgen]
pub struct InviteCapability {
    inner: RustInviteCapability,
}

#[wasm_bindgen]
impl InviteCapability {
    /// Compose a signed invite capability. Returns the wire bytes.
    /// Caller is responsible for base64url-encoding into a URL.
    ///
    /// `ownerSigningSeed` is the 32-byte Ed25519 seed that powers
    /// the `Identity` (see `mls.rs::Identity::ed25519Seed` for the
    /// already-wired-up source). `ownerAccountId` is 16 bytes
    /// (UUIDv7). `ownerX25519Pubkey` is the long-term HPKE init
    /// key (32 bytes — `Identity::x25519PublicKey`). `expiryMs`
    /// is UNIX millis; pass `0` for "never expires" (the JS
    /// layer SHOULD surface a stalking-risk warning before
    /// allowing this — see ADR-0005 §13.3). `maxUses` is a UI
    /// hint only in V1 (1.18d wires server-side enforcement);
    /// pass `0` for "single-use". `bootstrapReferral` flips on
    /// the signup-deep-link path (ADR-0005 §13.4). `label` is
    /// optional (empty allowed), capped at
    /// [`lakko_contacts::MAX_LABEL_LEN`] bytes UTF-8.
    ///
    /// # Errors
    ///
    /// `JsError` on any input-shape failure.
    #[wasm_bindgen]
    pub fn compose(
        owner_signing_seed: &[u8],
        owner_account_id: &[u8],
        owner_x25519_pubkey: &[u8],
        expiry_ms: u64,
        max_uses: u32,
        bootstrap_referral: bool,
        label: &str,
    ) -> Result<Vec<u8>, JsError> {
        if owner_signing_seed.len() != 32 {
            return Err(JsError::new("ownerSigningSeed must be 32 bytes"));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(owner_signing_seed);
        let signing = SigningKey::from_seed(&seed);

        if owner_account_id.len() != lakko_contacts::ACCOUNT_ID_LEN {
            return Err(JsError::new(&format!(
                "ownerAccountId must be {} bytes",
                lakko_contacts::ACCOUNT_ID_LEN,
            )));
        }
        let mut acct = [0u8; lakko_contacts::ACCOUNT_ID_LEN];
        acct.copy_from_slice(owner_account_id);

        if owner_x25519_pubkey.len() != PUBLIC_KEY_LEN {
            return Err(JsError::new(&format!(
                "ownerX25519Pubkey must be {PUBLIC_KEY_LEN} bytes",
            )));
        }
        let mut x_arr = [0u8; PUBLIC_KEY_LEN];
        x_arr.copy_from_slice(owner_x25519_pubkey);
        let x25519_pub = X25519PublicKey::from_bytes(&x_arr).map_err(crypto_err)?;

        if label.len() > MAX_LABEL_LEN {
            return Err(JsError::new(&format!(
                "label is {} bytes, max is {MAX_LABEL_LEN}",
                label.len(),
            )));
        }

        rust_invite_compose(
            &signing,
            &acct,
            &x25519_pub,
            expiry_ms,
            max_uses,
            bootstrap_referral,
            label,
        )
        .map_err(contact_err)
    }

    /// Joiner-side: decode wire bytes, verify the Ed25519
    /// signature, return the validated `InviteCapability`. Does
    /// NOT check expiry — call [`InviteCapability::isValidNow`]
    /// separately so the UI can render "this invite expired N
    /// minutes ago" rather than a generic failure.
    #[wasm_bindgen(js_name = decodeAndVerify)]
    pub fn decode_and_verify(wire: &[u8]) -> Result<InviteCapability, JsError> {
        let inner = rust_invite_decode(wire).map_err(contact_err)?;
        Ok(Self { inner })
    }

    /// Inviter's long-term Ed25519 verifying key (32 bytes).
    #[wasm_bindgen(getter, js_name = ownerSignaturePubkey)]
    pub fn owner_signature_pubkey(&self) -> Vec<u8> {
        self.inner.owner_signature_pubkey.to_bytes().to_vec()
    }

    /// Inviter's `account_id` (16-byte UUIDv7).
    #[wasm_bindgen(getter, js_name = ownerAccountId)]
    pub fn owner_account_id(&self) -> Vec<u8> {
        self.inner.owner_account_id.to_vec()
    }

    /// Inviter's long-term X25519 public key (32 bytes).
    #[wasm_bindgen(getter, js_name = ownerX25519Pubkey)]
    pub fn owner_x25519_pubkey(&self) -> Vec<u8> {
        self.inner.owner_x25519_pubkey.to_bytes().to_vec()
    }

    /// UNIX milliseconds when the invite stops being valid. `0`
    /// is the `NEVER_EXPIRES` sentinel — UI must warn before
    /// honouring (stalking risk, ADR-0005 §13.3).
    #[wasm_bindgen(getter, js_name = expiryMs)]
    pub fn expiry_ms(&self) -> u64 {
        self.inner.expiry_ms
    }

    /// Max-uses hint. `0` is the `SINGLE_USE` sentinel. V1 is a
    /// best-effort hint with no server enforcement — that lands
    /// in 1.18d.
    #[wasm_bindgen(getter, js_name = maxUses)]
    pub fn max_uses(&self) -> u32 {
        self.inner.max_uses
    }

    /// `true` when this is a referral link for a fresh signup
    /// (ADR-0005 §13.4).
    #[wasm_bindgen(getter, js_name = bootstrapReferral)]
    pub fn bootstrap_referral(&self) -> bool {
        self.inner.bootstrap_referral
    }

    /// Optional human-visible label, UTF-8, ≤
    /// `MAX_LABEL_LEN`. Empty string when the inviter didn't
    /// set one — UI fills in a default from the inviter's
    /// display name on lookup.
    #[wasm_bindgen(getter)]
    pub fn label(&self) -> String {
        self.inner.label.clone()
    }

    /// `true` iff the capability has not expired at `nowMs`.
    /// `NEVER_EXPIRES` capabilities always return `true`.
    #[wasm_bindgen(js_name = isValidNow)]
    pub fn is_valid_now(&self, now_ms: u64) -> bool {
        self.inner.is_valid_now(now_ms)
    }
}

/// Sign an invite-link revocation request (Phase 1.18d2b-web).
///
/// The web client calls this just before posting
/// `DELETE /discovery/invite/{blob_id}` so the body's
/// `signature` field carries proof that the revocation came
/// from the same Ed25519 key that signed the capability. The
/// canonical bytes the signature covers — domain separator +
/// `blob_id` + big-endian `signed_at_ms` — live in
/// [`lakko_contacts::invite::revoke_canonical_bytes`]; both the
/// server (`services/discovery`) and this helper drive
/// sign / verify against the same layout, so consolidating it
/// in `lakko-contacts` is what keeps the two sides in sync.
///
/// Returns the 64-byte raw Ed25519 signature. Caller
/// base64-encodes for the JSON body.
///
/// # Errors
///
/// `JsError` on any input-shape failure (32-byte signing seed,
/// 32-byte blob_id required).
#[wasm_bindgen(js_name = signInviteRevocation)]
pub fn sign_invite_revocation(
    signing_seed: &[u8],
    blob_id: &[u8],
    signed_at_ms: u64,
) -> Result<Vec<u8>, JsError> {
    if signing_seed.len() != 32 {
        return Err(JsError::new("signingSeed must be 32 bytes"));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(signing_seed);
    let signing = SigningKey::from_seed(&seed);

    if blob_id.len() != BLOB_ID_LEN {
        return Err(JsError::new(&format!("blobId must be {BLOB_ID_LEN} bytes")));
    }
    let mut blob_id_arr = [0u8; BLOB_ID_LEN];
    blob_id_arr.copy_from_slice(blob_id);

    Ok(rust_sign_revocation(&signing, &blob_id_arr, signed_at_ms).to_vec())
}

/// Compute the per-joiner consume-dedup tag for an invite-link
/// `(capability_bytes, joiner_account_id)` pair (Phase 1.18d3).
///
/// The web client calls this just before posting
/// `POST /discovery/invite/{blob_id}/consume` so the server
/// can dedup repeat consumes from the same joiner without
/// learning the joiner's `account_id` in plaintext. Layout +
/// privacy story: see
/// [`lakko_contacts::invite::compute_joiner_tag`].
///
/// Returns the 32-byte HMAC-SHA-256 tag. Caller base64-encodes
/// for the JSON body.
///
/// # Errors
///
/// `JsError` if `joinerAccountId` is the wrong length —
/// 16 bytes is the only supported shape today (UUIDv7 raw).
/// The capability slice is unbounded by length here; the
/// 1.18d1 server-side ceiling enforces the actual cap.
#[wasm_bindgen(js_name = computeJoinerInviteTag)]
pub fn compute_joiner_invite_tag(
    capability_bytes: &[u8],
    joiner_account_id: &[u8],
) -> Result<Vec<u8>, JsError> {
    if joiner_account_id.len() != lakko_contacts::ACCOUNT_ID_LEN {
        return Err(JsError::new(&format!(
            "joinerAccountId must be {} bytes",
            lakko_contacts::ACCOUNT_ID_LEN,
        )));
    }
    Ok(rust_compute_joiner_tag(capability_bytes, joiner_account_id).to_vec())
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
        let sender_account_id = [0x77u8; 16];
        let recipient_x25519 = X25519SecretKey::from_bytes([0xCD; 32]);
        let recipient_pub = recipient_x25519.public_key().to_bytes();
        let pseudonym = [0xAA; 32];

        let wire = ContactRequest::compose(
            &sender_seed,
            &sender_account_id,
            &recipient_pub,
            &pseudonym,
            1_000_000,
            b"hi bob",
        )
        .unwrap();

        let req = ContactRequest::decode_and_verify(&wire).unwrap();
        assert_eq!(req.from_pseudonym(), pseudonym.to_vec());
        assert_eq!(req.valid_after_ms(), 1_000_000);
        assert_eq!(req.from_account_id(), Some(sender_account_id.to_vec()));

        let priv_bytes = recipient_x25519.to_bytes();
        let plaintext = req.open_envelope(&priv_bytes).unwrap();
        assert_eq!(plaintext, b"hi bob");
    }

    #[test]
    fn invite_capability_roundtrip_happy_path() {
        let owner_seed = [9u8; 32];
        let owner_account_id = [0xA1u8; 16];
        let owner_x25519 = X25519SecretKey::from_bytes([0xB2; 32]);
        let owner_x25519_pub = owner_x25519.public_key().to_bytes();

        let wire = InviteCapability::compose(
            &owner_seed,
            &owner_account_id,
            &owner_x25519_pub,
            1_700_000_000_000,
            5,
            false,
            "alice@home",
        )
        .unwrap();

        let cap = InviteCapability::decode_and_verify(&wire).unwrap();
        assert_eq!(cap.owner_account_id(), owner_account_id.to_vec());
        assert_eq!(cap.owner_x25519_pubkey(), owner_x25519_pub.to_vec());
        assert_eq!(cap.expiry_ms(), 1_700_000_000_000);
        assert_eq!(cap.max_uses(), 5);
        assert!(!cap.bootstrap_referral());
        assert_eq!(cap.label(), "alice@home");
        // Ed25519 pubkey is derivable from the seed; assert non-empty.
        assert_eq!(cap.owner_signature_pubkey().len(), 32);
        // is_valid_now boundary check.
        assert!(cap.is_valid_now(0));
        assert!(!cap.is_valid_now(1_700_000_000_000));
    }

    #[test]
    fn sign_invite_revocation_round_trip() {
        use lakko_contacts::invite::revoke_canonical_bytes;
        use lakko_crypto::signing::{Signature, VerifyingKey};

        let seed = [0xAAu8; 32];
        let blob_id = [0xBBu8; 32];
        let signed_at_ms = 1_700_000_000_000u64;

        let sig_bytes = sign_invite_revocation(&seed, &blob_id, signed_at_ms).unwrap();
        assert_eq!(sig_bytes.len(), 64);

        // Reconstruct the public key the server-side `revoke`
        // handler would recover from the row, build the same
        // canonical bytes the helper signed, and verify.
        let signing = Ed25519::from_seed(&seed);
        let pubkey: VerifyingKey = signing.verifying_key();
        let canonical = revoke_canonical_bytes(&blob_id, signed_at_ms);
        let signature = Signature::from_bytes(&sig_bytes).unwrap();
        assert!(pubkey.verify(&canonical, &signature).is_ok());
    }

    #[test]
    fn compute_joiner_invite_tag_round_trip() {
        let cap = vec![0x01u8; 200];
        let acct = [0xA1u8; 16];

        let tag = compute_joiner_invite_tag(&cap, &acct).unwrap();
        assert_eq!(tag.len(), 32);

        // Determinism: same inputs → same output.
        let tag2 = compute_joiner_invite_tag(&cap, &acct).unwrap();
        assert_eq!(tag, tag2);

        // Injection: tweaking either side yields a different tag.
        let mut cap_b = cap.clone();
        cap_b[0] ^= 0x01;
        let tag_other_cap = compute_joiner_invite_tag(&cap_b, &acct).unwrap();
        assert_ne!(tag, tag_other_cap);

        let acct_b = [0xA2u8; 16];
        let tag_other_acct = compute_joiner_invite_tag(&cap, &acct_b).unwrap();
        assert_ne!(tag, tag_other_acct);
    }

    #[test]
    fn invite_capability_never_expires_sentinel() {
        let owner_seed = [3u8; 32];
        let owner_account_id = [0x11u8; 16];
        let owner_x25519 = X25519SecretKey::from_bytes([0x22; 32])
            .public_key()
            .to_bytes();

        let wire = InviteCapability::compose(
            &owner_seed,
            &owner_account_id,
            &owner_x25519,
            0, // NEVER_EXPIRES
            0, // SINGLE_USE
            true,
            "",
        )
        .unwrap();

        let cap = InviteCapability::decode_and_verify(&wire).unwrap();
        assert_eq!(cap.expiry_ms(), 0);
        assert_eq!(cap.max_uses(), 0);
        assert!(cap.bootstrap_referral());
        assert_eq!(cap.label(), "");
        assert!(cap.is_valid_now(u64::MAX));
    }
}
