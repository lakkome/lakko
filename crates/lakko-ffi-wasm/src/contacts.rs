// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Mirror of `lakko-contacts`: pseudonym lifecycle and the
//! [`ContactRequest`] envelope that drives ADR-0005 scenario A
//! ("I know `@username`, add them"). Phase 1.9a only.

use lakko_contacts::call_invite::{
    call_sign_revocation as rust_call_sign_revocation, compose as rust_call_invite_compose,
    decode_and_verify as rust_call_invite_decode, CallInviteCapability as RustCallInviteCapability,
    CallInviteMedia as RustCallInviteMedia, CALL_ID_LEN, MAX_KEY_PACKAGE_LEN,
};
use lakko_contacts::call_kp_forward::{
    try_unwrap_call_kp_forward as rust_try_unwrap_call_kp_forward,
    wrap_call_kp_forward as rust_wrap_call_kp_forward, CALL_KP_FORWARD_MIN_PAYLOAD_LEN,
};
use lakko_contacts::call_welcome::{
    try_unwrap_call_welcome as rust_try_unwrap_call_welcome,
    wrap_call_welcome as rust_wrap_call_welcome, CallWelcomeError, CALL_WELCOME_MIN_PAYLOAD_LEN,
};
use lakko_contacts::group_invite::{
    compose as rust_group_invite_compose, decode_and_verify as rust_group_invite_decode,
    group_sign_revocation as rust_group_sign_revocation,
    GroupInviteCapability as RustGroupInviteCapability, MAX_GROUP_ID_LEN,
};
use lakko_contacts::group_join_attribution::{
    compose_attribution as rust_group_join_attribution_compose,
    decode_attribution as rust_group_join_attribution_decode,
    GroupJoinAttribution as RustGroupJoinAttribution,
};
use lakko_contacts::incoming_envelope::{
    dispatch_incoming_envelope as rust_dispatch_incoming_envelope, IncomingEnvelope,
};
use lakko_contacts::invite::{
    compose as rust_invite_compose, compute_joiner_tag as rust_compute_joiner_tag,
    decode_and_verify as rust_invite_decode, sign_owner_list as rust_sign_owner_list,
    sign_revocation as rust_sign_revocation, BLOB_ID_LEN,
};
use lakko_contacts::media_state::{
    compose_media_state as rust_compose_media_state, decode_media_state as rust_decode_media_state,
};
use lakko_contacts::push_routing::{
    compose_push_routing_secret as rust_compose_push_routing_secret,
    decode_push_routing_secret as rust_decode_push_routing_secret,
    derive_push_gid as rust_derive_push_gid,
    push_routing_secret_is_newer as rust_push_routing_secret_is_newer, PUSH_ROUTING_SECRET_LEN,
};
use lakko_contacts::ACCOUNT_ID_LEN;
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
        privacy_pass_token: &[u8],
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
            privacy_pass_token,
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

    /// Privacy Pass / App-Attest / Play-Integrity attestation
    /// blob the sender attached (Phase 1.9b2 — RFC 9576). Empty
    /// `Uint8Array` means "no token" — either composed without
    /// one (web / dev path) or a legacy v1/v2 envelope that
    /// pre-dates this field. The DS ingest path is the authority
    /// on what the token means (rate-limit tier lift); the local
    /// client surfaces the raw bytes here for transparency.
    #[wasm_bindgen(getter, js_name = privacyPassToken)]
    pub fn privacy_pass_token(&self) -> Vec<u8> {
        self.inner.privacy_pass_token.clone()
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
// ContactAck (1.9b1 — reply path)
// ============================================================================

/// Validated `ContactAck`. Mirror of [`lakko_contacts::ContactAck`].
///
/// Senders call [`ContactAck::compose`] to mint the wire bytes
/// they publish to the sender's mailbox (the per-relationship
/// pseudonym from the original `ContactRequest.fromPseudonym`).
/// Recipients (= the original request's sender) call
/// [`ContactAck::decodeAndVerify`] to validate the signature +
/// structure, then cross-check the `accepterPubkey` against
/// the recipient pubkey they recorded when composing the
/// original request.
#[wasm_bindgen]
pub struct ContactAck {
    inner: lakko_contacts::ContactAck,
}

#[wasm_bindgen]
impl ContactAck {
    /// Compose a signed `ContactAck` and return the wire bytes
    /// ready to deliver to the original requester's mailbox.
    /// `ackToPseudonym` is the same 32-byte pseudonym the
    /// original `ContactRequest.fromPseudonym` carried.
    ///
    /// # Errors
    ///
    /// `JsError` on wrong-length seed / account-id / x25519-pub /
    /// pseudonym, or on a malformed X25519 public key encoding.
    #[wasm_bindgen]
    pub fn compose(
        accepter_signing_seed: &[u8],
        accepter_account_id: &[u8],
        accepter_x25519_pub: &[u8],
        ack_to_pseudonym: &[u8],
        accepted_at_ms: u64,
    ) -> Result<Vec<u8>, JsError> {
        if accepter_signing_seed.len() != 32 {
            return Err(JsError::new("accepterSigningSeed must be 32 bytes"));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(accepter_signing_seed);
        let signing = SigningKey::from_seed(&seed);

        if accepter_account_id.len() != lakko_contacts::ACCOUNT_ID_LEN {
            return Err(JsError::new(&format!(
                "accepterAccountId must be {} bytes",
                lakko_contacts::ACCOUNT_ID_LEN,
            )));
        }
        let mut acct = [0u8; lakko_contacts::ACCOUNT_ID_LEN];
        acct.copy_from_slice(accepter_account_id);

        if accepter_x25519_pub.len() != PUBLIC_KEY_LEN {
            return Err(JsError::new(&format!(
                "accepterX25519Pub must be {PUBLIC_KEY_LEN} bytes",
            )));
        }
        let mut x25519_arr = [0u8; PUBLIC_KEY_LEN];
        x25519_arr.copy_from_slice(accepter_x25519_pub);
        let x25519_pub = X25519PublicKey::from_bytes(&x25519_arr).map_err(crypto_err)?;

        let pseudonym = decode_pseudonym_bytes(ack_to_pseudonym)?;

        lakko_contacts::compose_ack(&signing, &acct, &x25519_pub, pseudonym, accepted_at_ms)
            .map_err(contact_err)
    }

    /// Recipient-side: decode the wire bytes, verify the Ed25519
    /// signature, return the validated `ContactAck`.
    #[wasm_bindgen(js_name = decodeAndVerify)]
    pub fn decode_and_verify(wire: &[u8]) -> Result<ContactAck, JsError> {
        let inner = lakko_contacts::decode_and_verify_ack(wire).map_err(contact_err)?;
        Ok(Self { inner })
    }

    /// The 32-byte per-relationship pseudonym from the original
    /// `ContactRequest.fromPseudonym`. Sender's poll layer matches
    /// this against the pending-sent table to flip the local UI
    /// state from "pending" to "accepted".
    #[wasm_bindgen(getter, js_name = ackToPseudonym)]
    pub fn ack_to_pseudonym(&self) -> Vec<u8> {
        self.inner.ack_to_pseudonym.to_vec()
    }

    /// Accepter's Ed25519 identity public key (32 bytes).
    #[wasm_bindgen(getter, js_name = accepterPubkey)]
    pub fn accepter_pubkey(&self) -> Vec<u8> {
        self.inner.accepter_pubkey.to_bytes().to_vec()
    }

    /// Accepter's UUIDv7 account id (16 bytes).
    #[wasm_bindgen(getter, js_name = accepterAccountId)]
    pub fn accepter_account_id(&self) -> Vec<u8> {
        self.inner.accepter_account_id.to_vec()
    }

    /// Accepter's X25519 HPKE init key (32 bytes).
    #[wasm_bindgen(getter, js_name = accepterX25519Pubkey)]
    pub fn accepter_x25519_pubkey(&self) -> Vec<u8> {
        self.inner.accepter_x25519_pubkey.to_bytes().to_vec()
    }

    /// UNIX milliseconds at which the ack was minted.
    #[wasm_bindgen(getter, js_name = acceptedAtMs)]
    pub fn accepted_at_ms(&self) -> u64 {
        self.inner.accepted_at_ms
    }
}

// ============================================================================
// Safety numbers (1.9b3 — verify-existing-contact OOB compare)
// ============================================================================

/// Compute the canonical Signal-style safety number for two
/// Ed25519 identity public keys (Phase 1.9b3 — ADR-0005 §13.5).
///
/// The output is a 71-byte ASCII string: 12 groups of 5 decimal
/// digits separated by single spaces, e.g. `"12345 67890 …"`.
/// Both sides of a conversation compute the same string
/// regardless of input order, so the OOB compare ("read me your
/// number / mine reads X") works either way.
///
/// Each pubkey input is the raw 32-byte Ed25519 verifying key.
/// The inputs are NOT validated as on-curve points — the
/// function is purely a hash over the bytes; callers that need
/// on-curve validation should do it before invoking this helper.
///
/// # Errors
///
/// `JsError` when either input is not exactly 32 bytes long.
#[wasm_bindgen(js_name = computeSafetyNumber)]
pub fn compute_safety_number(my_pubkey: &[u8], their_pubkey: &[u8]) -> Result<String, JsError> {
    if my_pubkey.len() != PUBLIC_KEY_LEN {
        return Err(JsError::new(&format!(
            "myPubkey must be {PUBLIC_KEY_LEN} bytes",
        )));
    }
    if their_pubkey.len() != PUBLIC_KEY_LEN {
        return Err(JsError::new(&format!(
            "theirPubkey must be {PUBLIC_KEY_LEN} bytes",
        )));
    }
    let mut a = [0u8; PUBLIC_KEY_LEN];
    a.copy_from_slice(my_pubkey);
    let mut b = [0u8; PUBLIC_KEY_LEN];
    b.copy_from_slice(their_pubkey);
    Ok(lakko_contacts::safety_number_bytes(&a, &b))
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

    /// Signal-style safety number derived from the owner's
    /// Ed25519 identity key. Joiner UI shows it before
    /// Accept so the human cross-checks the digits against
    /// what the owner shared OOB. Defends against link-
    /// substitution attacks; identical format + value to
    /// `CallInviteCapability.ownerFingerprint` for the
    /// same owner.
    #[wasm_bindgen(getter, js_name = ownerFingerprint)]
    pub fn owner_fingerprint(&self) -> String {
        self.inner.owner_fingerprint()
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

/// Sign a **call-invite** link revocation request (Phase 3c3-revoke).
///
/// Sibling of [`signInviteRevocation`] for the
/// [`CallInviteCapability`] ecosystem. The web client calls this
/// just before posting `DELETE /discovery/call-invite/{blob_id}`
/// so the body's `signature` field carries proof that the
/// revocation came from the same Ed25519 key that signed the
/// capability.
///
/// The canonical bytes the signature covers — domain separator
/// `lakko-call-invite-revoke/v1\0` + `blob_id` + big-endian
/// `signed_at_ms` — live in
/// [`lakko_contacts::call_invite::call_revoke_canonical_bytes`].
/// The distinct domain separator (vs. contact-invite revocation)
/// ensures a captured contact-invite revoke signature cannot be
/// replayed as a call-invite revoke (and vice versa).
///
/// Returns the 64-byte raw Ed25519 signature. Caller
/// base64-encodes for the JSON body.
///
/// # Errors
///
/// `JsError` on any input-shape failure (32-byte signing seed,
/// 32-byte blob_id required).
#[wasm_bindgen(js_name = signCallInviteRevocation)]
pub fn sign_call_invite_revocation(
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

    Ok(rust_call_sign_revocation(&signing, &blob_id_arr, signed_at_ms).to_vec())
}

/// Sign an owner-list query for the
/// `POST /discovery/invite/by-owner` endpoint (Phase
/// 1.18d2b-list).
///
/// The web client calls this just before posting the list
/// request so the body's `signature` field carries proof that
/// the caller is the holder of the identity Ed25519 key bound
/// to `account_id` via the KT log. Canonical bytes layout:
/// see [`lakko_contacts::invite::owner_list_canonical_bytes`].
///
/// Distinct domain separator from
/// [`signInviteRevocation`] ensures a captured revoke signature
/// cannot be replayed as an owner-list authentication (and vice
/// versa).
///
/// Returns the 64-byte raw Ed25519 signature. Caller
/// base64-encodes for the JSON body.
///
/// # Errors
///
/// `JsError` on any input-shape failure (32-byte signing seed,
/// 16-byte account_id required).
#[wasm_bindgen(js_name = signOwnerListQuery)]
pub fn sign_owner_list_query(
    signing_seed: &[u8],
    owner_account_id: &[u8],
    signed_at_ms: u64,
) -> Result<Vec<u8>, JsError> {
    if signing_seed.len() != 32 {
        return Err(JsError::new("signingSeed must be 32 bytes"));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(signing_seed);
    let signing = SigningKey::from_seed(&seed);

    if owner_account_id.len() != ACCOUNT_ID_LEN {
        return Err(JsError::new(&format!(
            "ownerAccountId must be {ACCOUNT_ID_LEN} bytes",
        )));
    }
    let mut acct_arr = [0u8; ACCOUNT_ID_LEN];
    acct_arr.copy_from_slice(owner_account_id);

    Ok(rust_sign_owner_list(&signing, &acct_arr, signed_at_ms).to_vec())
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

// ============================================================================
// CallInviteCapability — ROADMAP 3c3
// ============================================================================

/// Compose + sign a call-invite capability (Phase 3c3).
/// Returns the wire bytes the platform base64url-encodes
/// into a `https://<host>/c/<...>` link.
///
/// Same shape as `composeInviteCapability` but for a
/// link-based group-call invite. The owner embeds a
/// one-time `KeyPackage` minted via `Identity.createKeyPackage`;
/// the joiner uses it to bootstrap a fresh MLS group for
/// the call.
#[wasm_bindgen(js_name = composeCallInviteCapability)]
#[allow(clippy::too_many_arguments)]
pub fn compose_call_invite_capability(
    owner_signing_seed: &[u8],
    owner_account_id: &[u8],
    call_id: &[u8],
    expiry_ms: u64,
    max_uses: u32,
    media_audio: bool,
    media_video: bool,
    one_time_key_package: &[u8],
    label: &str,
) -> Result<Vec<u8>, JsError> {
    if owner_signing_seed.len() != 32 {
        return Err(JsError::new("ownerSigningSeed must be 32 bytes"));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(owner_signing_seed);
    let signing = SigningKey::from_seed(&seed);

    if owner_account_id.len() != ACCOUNT_ID_LEN {
        return Err(JsError::new(&format!(
            "ownerAccountId must be {ACCOUNT_ID_LEN} bytes"
        )));
    }
    let mut acct = [0u8; ACCOUNT_ID_LEN];
    acct.copy_from_slice(owner_account_id);

    if call_id.len() != CALL_ID_LEN {
        return Err(JsError::new(&format!("callId must be {CALL_ID_LEN} bytes")));
    }
    let mut cid = [0u8; CALL_ID_LEN];
    cid.copy_from_slice(call_id);

    if one_time_key_package.is_empty() {
        return Err(JsError::new("oneTimeKeyPackage must be non-empty"));
    }
    if one_time_key_package.len() > MAX_KEY_PACKAGE_LEN {
        return Err(JsError::new(&format!(
            "oneTimeKeyPackage is {} bytes, max is {MAX_KEY_PACKAGE_LEN}",
            one_time_key_package.len()
        )));
    }

    let media = RustCallInviteMedia {
        audio: media_audio,
        video: media_video,
    };
    let wire = rust_call_invite_compose(
        &signing,
        &acct,
        &cid,
        expiry_ms,
        max_uses,
        media,
        one_time_key_package,
        label,
    )
    .map_err(|e| JsError::new(&format!("{e}")))?;
    Ok(wire)
}

/// Composed and verified call-invite capability (Phase
/// 3c3). Mirror of [`lakko_contacts::CallInviteCapability`].
#[wasm_bindgen]
pub struct CallInviteCapability {
    pub(crate) inner: RustCallInviteCapability,
}

#[wasm_bindgen]
impl CallInviteCapability {
    /// Joiner-side: decode wire bytes, verify the Ed25519
    /// signature, return the validated capability.
    #[wasm_bindgen(js_name = decodeAndVerify)]
    pub fn decode_and_verify(wire: &[u8]) -> Result<CallInviteCapability, JsError> {
        let inner = rust_call_invite_decode(wire).map_err(|e| JsError::new(&format!("{e}")))?;
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

    /// 16-byte random call id minted by the inviter.
    #[wasm_bindgen(getter, js_name = callId)]
    pub fn call_id(&self) -> Vec<u8> {
        self.inner.call_id.to_vec()
    }

    /// UNIX milliseconds when the invite stops being valid.
    /// `0` is the never-expires sentinel.
    #[wasm_bindgen(getter, js_name = expiryMs)]
    pub fn expiry_ms(&self) -> u64 {
        self.inner.expiry_ms
    }

    /// Max-uses hint. `0` is the single-use sentinel.
    #[wasm_bindgen(getter, js_name = maxUses)]
    pub fn max_uses(&self) -> u32 {
        self.inner.max_uses
    }

    /// `true` when the call supports audio.
    #[wasm_bindgen(getter, js_name = mediaAudio)]
    pub fn media_audio(&self) -> bool {
        self.inner.media.audio
    }

    /// `true` when the call supports video.
    #[wasm_bindgen(getter, js_name = mediaVideo)]
    pub fn media_video(&self) -> bool {
        self.inner.media.video
    }

    /// Owner's one-time `KeyPackage` wire bytes.
    #[wasm_bindgen(getter, js_name = oneTimeKeyPackage)]
    pub fn one_time_key_package(&self) -> Vec<u8> {
        self.inner.one_time_key_package.clone()
    }

    /// Optional human-visible label.
    #[wasm_bindgen(getter)]
    pub fn label(&self) -> String {
        self.inner.label.clone()
    }

    /// `true` iff the capability has not expired at `nowMs`.
    #[wasm_bindgen(js_name = isValidNow)]
    pub fn is_valid_now(&self, now_ms: u64) -> bool {
        self.inner.is_valid_now(now_ms)
    }

    /// Signal-style safety number derived from the owner's
    /// long-term Ed25519 identity key. Joiner UI shows this
    /// before Accept so the human cross-checks the digits
    /// against what the owner shared OOB. Defends against
    /// link-substitution attacks.
    #[wasm_bindgen(getter, js_name = ownerFingerprint)]
    pub fn owner_fingerprint(&self) -> String {
        self.inner.owner_fingerprint()
    }
}

// ============================================================================
// Call-Welcome envelope — Phase 3c3-welcome-ferry
// ============================================================================

/// Wrap MLS Welcome bytes in the call-welcome envelope's
/// domain separator (Phase 3c3-welcome-ferry). The joiner
/// hands the output to `ContactRequest.compose` as the intro-
/// message argument; the existing HPKE-seal + stranger-inbox
/// transport carries the wrapped bytes to the owner.
///
/// See [`lakko_contacts::call_welcome`] for the threat-model
/// rationale (reuse the contact-invite envelope's
/// confidentiality + authenticity + anti-abuse contracts
/// rather than spin up a fresh `/discovery/call-welcome`
/// endpoint).
#[wasm_bindgen(js_name = wrapCallWelcome)]
#[must_use]
pub fn wrap_call_welcome(welcome_bytes: &[u8]) -> Vec<u8> {
    rust_wrap_call_welcome(welcome_bytes)
}

/// Try to unwrap a call-welcome envelope from a HPKE-decrypted
/// plaintext (Phase 3c3-welcome-ferry).
///
/// Returns:
///   * `Uint8Array` with the MLS Welcome bytes — `plaintext`
///     started with the call-welcome domain separator AND
///     the trailing payload is at least
///     `CALL_WELCOME_MIN_PAYLOAD_LEN` bytes long. Caller
///     drives `Group::join_from_welcome(payload)`.
///   * `null` — `plaintext` is a plain intro message; caller
///     renders it as a free-text ContactRequest.
///
/// # Errors
///
/// `JsError` ONLY when the domain separator matched but the
/// payload is truncated (a malformed / hostile envelope; log
/// + drop on the caller side).
#[wasm_bindgen(js_name = tryUnwrapCallWelcome)]
pub fn try_unwrap_call_welcome(plaintext: &[u8]) -> Result<Option<Vec<u8>>, JsError> {
    match rust_try_unwrap_call_welcome(plaintext) {
        Ok(Some(payload)) => Ok(Some(payload.to_vec())),
        Ok(None) => Ok(None),
        Err(CallWelcomeError::TruncatedPayload { min, got }) => Err(JsError::new(&format!(
            "call-welcome payload is {got} bytes, minimum is {min}",
        ))),
    }
}

/// Re-export the floor constant so platform code can keep its
/// own size sanity-check (`welcome.length >=
/// CALL_WELCOME_MIN_PAYLOAD_LEN` before calling
/// `wrapCallWelcome`).
#[wasm_bindgen(js_name = callWelcomeMinPayloadLen)]
#[must_use]
pub fn call_welcome_min_payload_len() -> usize {
    CALL_WELCOME_MIN_PAYLOAD_LEN
}

// ============================================================================
// Call-KP-Forward envelope — Phase 3c3-kp-forward
// (subsequent-joiner KeyPackage transport for multi-party calls)
// ============================================================================

/// Wrap a fresh MLS KeyPackage in the call-KP-forward
/// envelope's domain separator (Phase 3c3-kp-forward).
///
/// The subsequent joiner (3rd, 4th, … participant in a
/// multi-party link call) hands the output to
/// `ContactRequest.compose` as the intro-message argument;
/// the existing HPKE-seal + stranger-inbox transport carries
/// the wrapped KP to the call group's current committer, who
/// runs an MLS Add proposal + commit and ferries a Welcome
/// back via `wrapCallWelcome`.
///
/// See [`lakko_contacts::call_kp_forward`] for the
/// threat-model rationale (reuse the contact-invite envelope
/// confidentiality + authenticity + anti-abuse contracts;
/// sibling of `wrapCallWelcome`).
#[wasm_bindgen(js_name = wrapCallKeyPackage)]
#[must_use]
pub fn wrap_call_keypackage(kp_bytes: &[u8]) -> Vec<u8> {
    rust_wrap_call_kp_forward(kp_bytes)
}

/// Try to unwrap a call-KP-forward envelope from a HPKE-
/// decrypted plaintext (Phase 3c3-kp-forward).
///
/// Returns:
///   * `Uint8Array` — `plaintext` started with the
///     call-KP-forward domain separator AND the trailing
///     payload is at least `CALL_KP_FORWARD_MIN_PAYLOAD_LEN`
///     bytes long. Caller drives `Group::add_member_from_wire(payload)`
///     against the call group and ferries the resulting
///     Welcome back via `wrapCallWelcome`.
///   * `null` — `plaintext` is not a KP-forward envelope;
///     caller falls through to `tryUnwrapCallWelcome` and
///     then to the intro-message rendering path.
///
/// # Errors
///
/// `JsError` ONLY when the domain separator matched but the
/// payload is truncated (a malformed / hostile envelope;
/// log + drop on the caller side).
#[wasm_bindgen(js_name = tryUnwrapCallKeyPackage)]
pub fn try_unwrap_call_keypackage(plaintext: &[u8]) -> Result<Option<Vec<u8>>, JsError> {
    match rust_try_unwrap_call_kp_forward(plaintext) {
        Ok(Some(payload)) => Ok(Some(payload.to_vec())),
        Ok(None) => Ok(None),
        Err(CallWelcomeError::TruncatedPayload { min, got }) => Err(JsError::new(&format!(
            "call-KP-forward payload is {got} bytes, minimum is {min}",
        ))),
    }
}

/// Re-export the floor constant so platform code can keep
/// its own size sanity-check (`kp.length >=
/// CALL_KP_FORWARD_MIN_PAYLOAD_LEN` before calling
/// `wrapCallKeyPackage`).
#[wasm_bindgen(js_name = callKeyPackageMinPayloadLen)]
#[must_use]
pub fn call_keypackage_min_payload_len() -> usize {
    CALL_KP_FORWARD_MIN_PAYLOAD_LEN
}

// ============================================================================
// Incoming-envelope dispatcher — Phase 3c3-incoming-dispatcher
// ============================================================================

/// Kind tag for [`WasmIncomingEnvelope`]. Mirror of the
/// underlying Rust enum variants. The numeric values are
/// stable wire tags — `MediaState` was appended as `3` so the
/// pre-existing `IntroMessage = 2` tag stays fixed. (The Rust
/// dispatch order is welcome → KP-forward → media-state →
/// intro; the numeric tag is independent of that order.)
#[wasm_bindgen(js_name = IncomingEnvelopeKind)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WasmIncomingEnvelopeKind {
    /// First-joiner Welcome bytes for a link-based call.
    CallWelcome = 0,
    /// Subsequent-joiner KeyPackage for a multi-party call.
    CallKeyPackage = 1,
    /// Plain free-text intro message (ADR-0005 scenario A).
    IntroMessage = 2,
    /// Camera on/off media-state signal (camera-off-cleanup).
    /// The decoded flag rides the `cameraOn` getter of
    /// [`WasmIncomingEnvelope`]; `payload` is empty.
    MediaState = 3,
    /// In-group push-routing-secret distribution envelope
    /// (ADR-0042, v2 per ADR-0044). The decoded 32-byte secret
    /// rides the `payload` getter of [`WasmIncomingEnvelope`]
    /// and the minter's recency stamp rides the `mintEpoch`
    /// getter; `cameraOn` is `undefined`. Appended as `4` so the
    /// pre-existing tags stay fixed.
    PushRoutingSecret = 4,
    /// Committer liveness beacon (ADR-0045 decision 2, variant 1).
    /// Carries **no** payload — `payload` is empty, `cameraOn` and
    /// `mintEpoch` are `undefined` — because the beacon has no
    /// fields: the sender comes from the MLS sender leaf and the age
    /// from the DS row. Appended as `5` so the pre-existing tags stay
    /// fixed.
    ///
    /// **This variant exists so a beacon is classified, not so it is
    /// consumed.** The fold that turns beacons into an
    /// acting-committer decision (`actingCommitterVerdict` /
    /// `shouldEmitBeacon` and their constants) is deliberately **not
    /// yet exposed** on this surface; until it is, a client that sees
    /// this kind should drop it. What the variant buys today is that
    /// a beacon can never fall through to `IntroMessage` and be
    /// rendered as a chat bubble.
    CommitterBeacon = 5,
}

/// FFI-portable result of [`dispatchIncomingEnvelope`]. The
/// payload bytes are materialised into an owned `Vec<u8>` at
/// the FFI boundary so the JavaScript caller doesn't have to
/// think about Rust borrow lifetimes.
#[wasm_bindgen(js_name = IncomingEnvelope)]
pub struct WasmIncomingEnvelope {
    kind: WasmIncomingEnvelopeKind,
    payload: Vec<u8>,
    camera_on: Option<bool>,
    mint_epoch: Option<u64>,
}

#[wasm_bindgen(js_class = IncomingEnvelope)]
impl WasmIncomingEnvelope {
    /// Which dispatch branch fired.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> WasmIncomingEnvelopeKind {
        self.kind
    }

    /// The dispatched payload — for `CallWelcome` and
    /// `CallKeyPackage` this is the bytes **after** the
    /// 22- / 25-byte domain separator; for `IntroMessage`
    /// it is the original plaintext verbatim; for
    /// `PushRoutingSecret` it is the decoded 32-byte secret;
    /// for `MediaState` it is empty.
    #[wasm_bindgen(getter)]
    pub fn payload(&self) -> Vec<u8> {
        self.payload.clone()
    }

    /// Set only for `MediaState`: whether the sender's camera
    /// is on. `undefined` (`None`) for every other kind.
    #[wasm_bindgen(getter, js_name = cameraOn)]
    pub fn camera_on(&self) -> Option<bool> {
        self.camera_on
    }

    /// Set only for `PushRoutingSecret` (ADR-0044): the minter's
    /// recency stamp, the primary key of the canonical total
    /// order. `undefined` (`None`) for every other kind. JS reads
    /// it as a `bigint | undefined` (the field is a `u64`),
    /// matching the `Group.epoch()` precedent.
    ///
    /// The caller MUST NOT store the secret unconditionally —
    /// run the ADR-0042 / ADR-0043 role gate, then
    /// `pushRoutingSecretIsNewer` against whatever is already
    /// stored. A stale re-broadcast carries a valid secret from
    /// an authorised sender and is refused only by the order.
    #[wasm_bindgen(getter, js_name = mintEpoch)]
    pub fn mint_epoch(&self) -> Option<u64> {
        self.mint_epoch
    }
}

/// Single-call dispatcher for opaque relay-payload plaintexts
/// (Phase 3c3-incoming-dispatcher). Runs the classification
/// helpers in dispatch order
/// (welcome → KP-forward → media-state → intro-message
/// fallthrough) and returns the typed variant.
///
/// See [`lakko_contacts::incoming_envelope`] for the full
/// dispatch contract.
///
/// # Errors
///
/// `JsError` when a domain separator matched but the payload
/// is malformed (below the Welcome / KP-forward minimum floor,
/// or an invalid media-state payload).
#[wasm_bindgen(js_name = dispatchIncomingEnvelope)]
pub fn dispatch_incoming_envelope(plaintext: &[u8]) -> Result<WasmIncomingEnvelope, JsError> {
    let parsed =
        rust_dispatch_incoming_envelope(plaintext).map_err(|err| JsError::new(&err.to_string()))?;
    Ok(match parsed {
        IncomingEnvelope::CallWelcome(p) => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::CallWelcome,
            payload: p.to_vec(),
            camera_on: None,
            mint_epoch: None,
        },
        IncomingEnvelope::CallKeyPackage(p) => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::CallKeyPackage,
            payload: p.to_vec(),
            camera_on: None,
            mint_epoch: None,
        },
        IncomingEnvelope::MediaState { camera_on } => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::MediaState,
            payload: Vec::new(),
            camera_on: Some(camera_on),
            mint_epoch: None,
        },
        IncomingEnvelope::PushRoutingSecret { mint_epoch, secret } => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::PushRoutingSecret,
            payload: secret.to_vec(),
            camera_on: None,
            mint_epoch: Some(mint_epoch),
        },
        IncomingEnvelope::CommitterBeacon => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::CommitterBeacon,
            payload: Vec::new(),
            camera_on: None,
            mint_epoch: None,
        },
        IncomingEnvelope::IntroMessage(p) => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::IntroMessage,
            payload: p.to_vec(),
            camera_on: None,
            mint_epoch: None,
        },
    })
}

/// Compose a camera on/off media-state envelope
/// (camera-off-cleanup). The sender broadcasts the returned
/// bytes over the opaque SFU `signal-relay` payload; receivers
/// classify them (via `dispatchIncomingEnvelope` or
/// `decodeMediaState`) and reset the sender's tile `camOn`
/// state.
///
/// See [`lakko_contacts::media_state`] for the wire format and
/// threat-model rationale (the SFU never parses the payload —
/// invariant #2).
#[wasm_bindgen(js_name = composeMediaState)]
#[must_use]
pub fn compose_media_state(camera_on: bool) -> Vec<u8> {
    rust_compose_media_state(camera_on)
}

/// Strictly decode a media-state envelope, returning the
/// camera flag (camera-off-cleanup). Fail-closed: rejects a
/// plaintext that does not carry the media-state domain
/// separator or whose payload is malformed. Use
/// `dispatchIncomingEnvelope` instead when the plaintext might
/// be any relay-payload kind.
///
/// # Errors
///
/// `JsError` when `plaintext` is not a valid media-state
/// envelope (wrong domain / bad length / unknown version /
/// out-of-range flag byte).
#[wasm_bindgen(js_name = decodeMediaState)]
pub fn decode_media_state(plaintext: &[u8]) -> Result<bool, JsError> {
    rust_decode_media_state(plaintext)
        .map(|state| state.camera_on)
        .map_err(|err| JsError::new(&err.to_string()))
}

// ============================================================================
// Push-routing-secret + opaque push_gid — ADR-0042
// (invariant #5 — close the push social-graph leak)
// ============================================================================

/// Coerce a caller-supplied slice into the fixed 32-byte
/// `push_routing_secret` array, fail-closed on a wrong length.
fn push_routing_secret_array(secret: &[u8]) -> Result<[u8; PUSH_ROUTING_SECRET_LEN], JsError> {
    <[u8; PUSH_ROUTING_SECRET_LEN]>::try_from(secret).map_err(|_| {
        JsError::new(&format!(
            "push_routing_secret must be {PUSH_ROUTING_SECRET_LEN} bytes, got {}",
            secret.len(),
        ))
    })
}

/// Derive the stable opaque `push_gid` from a group's 32-byte
/// `push_routing_secret` (ADR-0042). Returns the 32-byte routing
/// id the client subscribes / notifies under (and reverse-indexes
/// to `(account, group_id)`), instead of the leaky DS journal
/// key.
///
/// See [`lakko_contacts::push_routing::derive_push_gid`] for the
/// derivation (keyed-BLAKE3, domain
/// `lakko/push/group-routing-id/v1`) and the invariant-#5
/// rationale.
///
/// # Errors
///
/// `JsError` when `secret` is not exactly 32 bytes.
#[wasm_bindgen(js_name = derivePushGid)]
pub fn derive_push_gid(secret: &[u8]) -> Result<Vec<u8>, JsError> {
    let arr = push_routing_secret_array(secret)?;
    Ok(rust_derive_push_gid(&arr).to_vec())
}

/// A decoded push-routing-secret distribution envelope
/// (ADR-0042 v2, per ADR-0044). Output of
/// `decodePushRoutingSecret`.
///
/// TS-facing shape: `{ mintEpoch: bigint, secret: Uint8Array }`.
/// `mintEpoch` crosses as a `BigInt` (the field is a `u64`),
/// matching the `Group.epoch()` and `ParsedBody.target`
/// precedents.
#[wasm_bindgen(js_name = PushRoutingSecret)]
pub struct WasmPushRoutingSecret {
    mint_epoch: u64,
    secret: Vec<u8>,
}

#[wasm_bindgen(js_class = PushRoutingSecret)]
impl WasmPushRoutingSecret {
    /// The minter's recency stamp, the primary key of the
    /// canonical total order — a `bigint` on the JS side. Opaque
    /// to the core: it imposes an order and nothing else, and
    /// never reaches `derivePushGid`.
    #[wasm_bindgen(getter, js_name = mintEpoch)]
    #[must_use]
    pub fn mint_epoch(&self) -> u64 {
        self.mint_epoch
    }

    /// The decoded 32-byte per-group routing secret.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn secret(&self) -> Vec<u8> {
        self.secret.clone()
    }
}

/// Compose the in-group push-routing-secret distribution envelope
/// (ADR-0042 v2, per ADR-0044) for a group's 32-byte
/// `push_routing_secret`, stamped with `mintEpoch` (a `bigint`).
/// The founder / adder / rotator broadcasts the returned bytes as
/// an MLS application message on the group journal so members +
/// joiners learn the secret; receivers classify them (via
/// `dispatchIncomingEnvelope` or `decodePushRoutingSecret`) and
/// persist the secret — never render it as a chat bubble.
///
/// `mintEpoch` is stamped **at mint time** and then travels with
/// the secret forever: a re-broadcast MUST replay the epoch the
/// secret was minted under, never a fresh one. Re-stamping on
/// re-broadcast would make a stale secret out-rank the live one
/// and reintroduce the F-A replay (G-36 residual 9) this envelope
/// closes. The caller sources the stamp from the MLS group epoch
/// (`Group.epoch()`), which is shared state every receiver can
/// independently observe.
///
/// See [`lakko_contacts::push_routing`] for the wire format
/// (`domain || version || mint_epoch(8, BE) || secret(32)`) and
/// threat model (the DS sees ciphertext — invariant #2).
///
/// # Errors
///
/// `JsError` when `secret` is not exactly 32 bytes.
#[wasm_bindgen(js_name = composePushRoutingSecret)]
pub fn compose_push_routing_secret(mint_epoch: u64, secret: &[u8]) -> Result<Vec<u8>, JsError> {
    let arr = push_routing_secret_array(secret)?;
    Ok(rust_compose_push_routing_secret(mint_epoch, &arr))
}

/// Strictly decode a push-routing-secret distribution envelope
/// (ADR-0042 v2, per ADR-0044), returning
/// `{ mintEpoch, secret }`. Fail-closed: rejects a plaintext that
/// does not carry the push-routing-secret domain separator or
/// whose payload is malformed — including a retired v1 envelope,
/// which carries no `mintEpoch` and so cannot be placed in the
/// total order. Use `dispatchIncomingEnvelope` instead when the
/// plaintext might be any relay-payload / in-group envelope kind.
///
/// # Errors
///
/// `JsError` when `plaintext` is not a valid push-routing-secret
/// envelope (wrong domain / bad length / unknown version — a v1
/// envelope lands here).
#[wasm_bindgen(js_name = decodePushRoutingSecret)]
pub fn decode_push_routing_secret(plaintext: &[u8]) -> Result<WasmPushRoutingSecret, JsError> {
    rust_decode_push_routing_secret(plaintext)
        .map(|envelope| WasmPushRoutingSecret {
            mint_epoch: envelope.mint_epoch,
            secret: envelope.secret.to_vec(),
        })
        .map_err(|err| JsError::new(&err.to_string()))
}

/// The canonical total order on push-routing secrets (ADR-0044):
/// `true` when the incoming envelope must supersede the stored
/// one. Epochs are `bigint`s on the JS side.
///
/// Ordered on the pair `(mintEpoch, secretBytes)` — epoch first,
/// then the 32 secret bytes lexicographically — and **strictly
/// greater wins; equal is NOT newer**, which is what keeps a
/// benign retry of the secret a device already holds from reading
/// as an update.
///
/// Exposed on the FFI precisely so no client reimplements the
/// comparison: three platforms each re-deriving it is the drift
/// class that produced the F-A stale-re-broadcast replay (G-36
/// residual 9). Run the ADR-0042 / ADR-0043 role gate first, then
/// route the accept/refuse decision through this function — an
/// authorised sender can still be replaying a superseded secret,
/// and only the order catches that.
///
/// Fail-closed on a wrong-length secret on **either** side:
/// returns `false` (not newer), so a malformed incoming secret
/// never displaces a stored one and a corrupt stored value is
/// left in place rather than overwritten. Never throws;
/// infallible by construction, hence no `Result`.
///
/// See [`lakko_contacts::push_routing::push_routing_secret_is_newer`].
#[wasm_bindgen(js_name = pushRoutingSecretIsNewer)]
#[must_use]
pub fn push_routing_secret_is_newer(
    incoming_epoch: u64,
    incoming_secret: &[u8],
    stored_epoch: u64,
    stored_secret: &[u8],
) -> bool {
    rust_push_routing_secret_is_newer(incoming_epoch, incoming_secret, stored_epoch, stored_secret)
}

// ============================================================================
// GroupInviteCapability — ADR-0028 (group join-ticket, G-3)
// ============================================================================

/// Owner-side: compose + sign a group join-ticket capability (ADR-0028).
/// `issuerSigningSeed` is the 32-byte Ed25519 seed; `groupId` is the target
/// group's opaque MLS GroupId bytes; `expiryMs` is UNIX millis (`0` = never).
/// Returns the wire bytes the web client base64url-encodes into `/g/<slug>`.
#[wasm_bindgen(js_name = composeGroupInviteCapability)]
pub fn compose_group_invite_capability(
    issuer_signing_seed: &[u8],
    group_id: &[u8],
    expiry_ms: u64,
) -> Result<Vec<u8>, JsError> {
    if issuer_signing_seed.len() != 32 {
        return Err(JsError::new("issuerSigningSeed must be 32 bytes"));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(issuer_signing_seed);
    let signing = SigningKey::from_seed(&seed);
    if group_id.is_empty() || group_id.len() > MAX_GROUP_ID_LEN {
        return Err(JsError::new(&format!(
            "groupId must be 1..={MAX_GROUP_ID_LEN} bytes"
        )));
    }
    rust_group_invite_compose(&signing, group_id, expiry_ms)
        .map_err(|e| JsError::new(&format!("{e}")))
}

/// Owner-side: sign a group-invite revocation (ADR-0028 revoke). Returns the
/// 64-byte Ed25519 signature over the group-invite revoke canonical bytes (the
/// `DELETE /discovery/group-invite/{blob_id}` body). `issuerSigningSeed` is the
/// 32-byte Ed25519 seed that minted the link; `blobId` is the 32-byte
/// content-addressed id.
#[wasm_bindgen(js_name = signGroupInviteRevocation)]
pub fn sign_group_invite_revocation(
    issuer_signing_seed: &[u8],
    blob_id: &[u8],
    signed_at_ms: u64,
) -> Result<Vec<u8>, JsError> {
    if issuer_signing_seed.len() != 32 {
        return Err(JsError::new("issuerSigningSeed must be 32 bytes"));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(issuer_signing_seed);
    let signing = SigningKey::from_seed(&seed);
    if blob_id.len() != BLOB_ID_LEN {
        return Err(JsError::new(&format!("blobId must be {BLOB_ID_LEN} bytes")));
    }
    let mut blob_id_arr = [0u8; BLOB_ID_LEN];
    blob_id_arr.copy_from_slice(blob_id);
    Ok(rust_group_sign_revocation(&signing, &blob_id_arr, signed_at_ms).to_vec())
}

/// Composed + verified group join-ticket capability (ADR-0028). Mirror of
/// [`lakko_contacts::GroupInviteCapability`].
#[wasm_bindgen]
pub struct GroupInviteCapability {
    pub(crate) inner: RustGroupInviteCapability,
}

#[wasm_bindgen]
impl GroupInviteCapability {
    /// Joiner-side: decode wire bytes, verify the Ed25519 signature, return the
    /// validated capability. Does **not** check expiry — call `isValidNow`.
    #[wasm_bindgen(js_name = decodeAndVerify)]
    pub fn decode_and_verify(wire: &[u8]) -> Result<GroupInviteCapability, JsError> {
        let inner = rust_group_invite_decode(wire).map_err(|e| JsError::new(&format!("{e}")))?;
        Ok(Self { inner })
    }

    /// The target group's opaque MLS GroupId bytes — pass to
    /// `Identity.createExternalJoinProposal`.
    #[wasm_bindgen(getter, js_name = groupId)]
    pub fn group_id(&self) -> Vec<u8> {
        self.inner.group_id.clone()
    }

    /// The issuer's Ed25519 verifying key (32 bytes).
    #[wasm_bindgen(getter, js_name = issuerSignaturePubkey)]
    pub fn issuer_signature_pubkey(&self) -> Vec<u8> {
        self.inner.issuer_signature_pubkey.to_bytes().to_vec()
    }

    /// UNIX milliseconds when the link stops being valid. `0` = never expires.
    #[wasm_bindgen(getter, js_name = expiryMs)]
    pub fn expiry_ms(&self) -> u64 {
        self.inner.expiry_ms
    }

    /// `true` iff the link has not expired at `nowMs`.
    #[wasm_bindgen(js_name = isValidNow)]
    pub fn is_valid_now(&self, now_ms: u64) -> bool {
        self.inner.is_valid_now(now_ms)
    }

    /// Signal-style anti-phishing fingerprint of the issuer's key.
    #[wasm_bindgen(js_name = issuerFingerprint)]
    pub fn issuer_fingerprint(&self) -> String {
        self.inner.issuer_fingerprint()
    }
}

// ============================================================================
// GroupJoinAttribution — ADR-0028 OQ2 (issuer-in-roster surfacing, G-3)
// ============================================================================

/// Joiner-side: compose + self-sign the attribution envelope binding the
/// joiner's `GroupInviteCapability` to its MLS leaf signature key (ADR-0028
/// OQ2). `joinerSigningSeed` is the joiner's 32-byte Ed25519 identity seed
/// (`Identity.ed25519Seed`); the envelope embeds the matching verifying key and
/// is signed with it, so the admin can verify the attribution is authentic. The
/// joiner posts this to the group journal alongside its external-join proposal.
#[wasm_bindgen(js_name = composeGroupJoinAttribution)]
pub fn compose_group_join_attribution(
    joiner_signing_seed: &[u8],
    capability_bytes: &[u8],
) -> Result<Vec<u8>, JsError> {
    rust_group_join_attribution_compose(joiner_signing_seed, capability_bytes)
        .map_err(|e| JsError::new(&format!("{e}")))
}

/// Decoded join-attribution (ADR-0028 OQ2) — which member's `/g/` link a join
/// request arrived on. Produced by `GroupJoinAttribution.parse`; the embedded
/// capability is re-verified (Ed25519) on decode.
#[wasm_bindgen]
pub struct GroupJoinAttribution {
    pub(crate) inner: RustGroupJoinAttribution,
}

#[wasm_bindgen]
impl GroupJoinAttribution {
    /// Admin-side: decode + verify an attribution envelope from the group
    /// journal. A malformed / forged envelope (or a capability whose signature
    /// fails) is rejected.
    #[wasm_bindgen(js_name = parse)]
    pub fn parse(bytes: &[u8]) -> Result<GroupJoinAttribution, JsError> {
        let inner =
            rust_group_join_attribution_decode(bytes).map_err(|e| JsError::new(&format!("{e}")))?;
        Ok(Self { inner })
    }

    /// The joiner's MLS leaf signature key — the tag matching the pending
    /// external-join request.
    #[wasm_bindgen(getter, js_name = joinerSignatureKey)]
    pub fn joiner_signature_key(&self) -> Vec<u8> {
        self.inner.joiner_signature_key.clone()
    }

    /// The target group's opaque MLS GroupId bytes — the admin checks this
    /// equals its own group before trusting the attribution.
    #[wasm_bindgen(getter, js_name = groupId)]
    pub fn group_id(&self) -> Vec<u8> {
        self.inner.capability.group_id.clone()
    }

    /// The issuer's Ed25519 verifying key (32 bytes) — checked against the
    /// roster (OQ2).
    #[wasm_bindgen(getter, js_name = issuerSignaturePubkey)]
    pub fn issuer_signature_pubkey(&self) -> Vec<u8> {
        self.inner
            .capability
            .issuer_signature_pubkey
            .to_bytes()
            .to_vec()
    }

    /// Signal-style anti-phishing fingerprint of the issuer's key.
    #[wasm_bindgen(getter, js_name = issuerFingerprint)]
    pub fn issuer_fingerprint(&self) -> String {
        self.inner.capability.issuer_fingerprint()
    }

    /// UNIX milliseconds when the link stops being valid. `0` = never expires.
    #[wasm_bindgen(getter, js_name = expiryMs)]
    pub fn expiry_ms(&self) -> u64 {
        self.inner.capability.expiry_ms
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
            b"",
            b"hi bob",
        )
        .unwrap();

        let req = ContactRequest::decode_and_verify(&wire).unwrap();
        assert_eq!(req.from_pseudonym(), pseudonym.to_vec());
        assert_eq!(req.valid_after_ms(), 1_000_000);
        assert_eq!(req.from_account_id(), Some(sender_account_id.to_vec()));
        assert!(req.privacy_pass_token().is_empty());

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
    fn sign_call_invite_revocation_round_trip() {
        use lakko_contacts::call_invite::call_revoke_canonical_bytes;
        use lakko_crypto::signing::{Signature, VerifyingKey};

        let seed = [0xCCu8; 32];
        let blob_id = [0xDDu8; 32];
        let signed_at_ms = 1_700_000_000_000u64;

        let sig_bytes = sign_call_invite_revocation(&seed, &blob_id, signed_at_ms).unwrap();
        assert_eq!(sig_bytes.len(), 64);

        // Server-side: recover pubkey from the stored capability,
        // build the call-invite-revoke canonical bytes, verify.
        let signing = Ed25519::from_seed(&seed);
        let pubkey: VerifyingKey = signing.verifying_key();
        let canonical = call_revoke_canonical_bytes(&blob_id, signed_at_ms);
        let signature = Signature::from_bytes(&sig_bytes).unwrap();
        assert!(pubkey.verify(&canonical, &signature).is_ok());
    }

    #[test]
    fn sign_call_invite_revocation_isolated_from_contact_invite() {
        // Cross-kind isolation: a signature minted via
        // `signCallInviteRevocation` MUST NOT verify under the
        // contact-invite revoke canonical bytes (different
        // domain separator). Catches a wiring mistake where one
        // side accidentally uses the other domain.
        use lakko_contacts::invite::revoke_canonical_bytes as contact_revoke_canonical_bytes;
        use lakko_crypto::signing::{Signature, VerifyingKey};

        let seed = [0xEEu8; 32];
        let blob_id = [0xFFu8; 32];
        let signed_at_ms = 1_700_000_000_000u64;

        let sig_bytes = sign_call_invite_revocation(&seed, &blob_id, signed_at_ms).unwrap();
        let pubkey: VerifyingKey = Ed25519::from_seed(&seed).verifying_key();
        let wrong_canonical = contact_revoke_canonical_bytes(&blob_id, signed_at_ms);
        let signature = Signature::from_bytes(&sig_bytes).unwrap();
        assert!(pubkey.verify(&wrong_canonical, &signature).is_err());
    }

    // Negative-path coverage (wrong-length blob_id / seed) lives
    // on the UniFFI side: `JsError::new` here calls into the
    // wasm-bindgen runtime, which panics on the native target the
    // `cargo test` harness runs on. Positive + cross-kind tests
    // above already prove the wrapper plumbs through to
    // `call_sign_revocation` correctly.

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

    // -------------------------------------------------------------------
    // Push-routing-secret v2 — ADR-0044
    //
    // Happy paths only, per the module note above: the error paths
    // construct a `JsError`, which cannot run on a non-wasm target.
    // `pushRoutingSecretIsNewer` is infallible, so it is covered in
    // full here.
    // -------------------------------------------------------------------

    #[test]
    fn push_routing_compose_then_decode_round_trips() {
        let secret = [0x42u8; 32];
        let wrapped = compose_push_routing_secret(7, &secret).unwrap();
        let decoded = decode_push_routing_secret(&wrapped).unwrap();
        assert_eq!(decoded.mint_epoch(), 7);
        assert_eq!(decoded.secret(), secret.to_vec());
    }

    #[test]
    fn push_routing_round_trips_at_the_mint_epoch_extremes() {
        // The 8-byte big-endian field must survive the boundary
        // un-truncated. `u64` crosses to JS as a `BigInt`, so a
        // reader that narrowed it to a `number` would lose
        // precision above 2^53 — pinned here at the extremes.
        let secret = [0x42u8; 32];
        for epoch in [0u64, 1, u64::from(u32::MAX), u64::MAX - 1, u64::MAX] {
            let wrapped = compose_push_routing_secret(epoch, &secret).unwrap();
            let decoded = decode_push_routing_secret(&wrapped).unwrap();
            assert_eq!(decoded.mint_epoch(), epoch);
            assert_eq!(decoded.secret(), secret.to_vec());
        }
    }

    #[test]
    fn dispatch_routes_push_routing_secret_envelope() {
        let secret = [0x42u8; 32];
        let wrapped = compose_push_routing_secret(9, &secret).unwrap();
        let result = dispatch_incoming_envelope(&wrapped).unwrap();
        assert_eq!(result.kind(), WasmIncomingEnvelopeKind::PushRoutingSecret);
        assert_eq!(result.payload(), secret.to_vec());
        assert_eq!(result.mint_epoch(), Some(9));
        assert_eq!(result.camera_on(), None);
    }

    #[test]
    fn dispatch_mint_epoch_field_is_none_for_other_kinds() {
        // `mintEpoch` is meaningful ONLY on `PushRoutingSecret`, so
        // a JS reader must see `undefined` on any other kind rather
        // than a stale or defaulted stamp.
        let media = compose_media_state(true);
        let result = dispatch_incoming_envelope(&media).unwrap();
        assert_eq!(result.kind(), WasmIncomingEnvelopeKind::MediaState);
        assert_eq!(result.mint_epoch(), None);

        let intro = dispatch_incoming_envelope(b"hello alice").unwrap();
        assert_eq!(intro.kind(), WasmIncomingEnvelopeKind::IntroMessage);
        assert_eq!(intro.mint_epoch(), None);
    }

    #[test]
    fn is_newer_orders_on_the_epoch_then_the_secret_bytes() {
        let low = [0x11u8; 32];
        let high = [0x99u8; 32];
        // Epoch is the primary key and dominates the tiebreak.
        assert!(push_routing_secret_is_newer(9, &low, 8, &high));
        assert!(!push_routing_secret_is_newer(7, &high, 8, &low));
        // Exact epoch tie: the secret bytes decide, lexicographically.
        assert!(push_routing_secret_is_newer(5, &high, 5, &low));
        assert!(!push_routing_secret_is_newer(5, &low, 5, &high));
    }

    #[test]
    fn is_newer_says_an_identical_pair_is_not_newer() {
        // THE F-A property: a benign retry of the secret a device
        // already holds must not read as an update.
        let secret = [0x42u8; 32];
        assert!(!push_routing_secret_is_newer(5, &secret, 5, &secret));
    }

    #[test]
    fn is_newer_refuses_a_stale_rebroadcast_after_a_rotation() {
        // The end-to-end F-A scenario at the FFI boundary, for BOTH
        // tiebreak directions so the result cannot hinge on which
        // secret happens to sort higher.
        let low = [0x11u8; 32];
        let high = [0x99u8; 32];
        for (older, newer) in [(&low, &high), (&high, &low)] {
            assert!(!push_routing_secret_is_newer(10, older, 11, newer));
            assert!(push_routing_secret_is_newer(11, newer, 10, older));
        }
    }

    #[test]
    fn is_newer_is_fail_closed_on_a_wrong_length_secret() {
        // Neither side may panic or displace on a malformed value.
        let good = [0x42u8; 32];
        assert!(!push_routing_secret_is_newer(99, &[0u8; 31], 1, &good));
        assert!(!push_routing_secret_is_newer(99, &[0u8; 33], 1, &good));
        assert!(!push_routing_secret_is_newer(99, &good, 1, &[0u8; 31]));
        assert!(!push_routing_secret_is_newer(99, &good, 1, &[]));
    }
}
