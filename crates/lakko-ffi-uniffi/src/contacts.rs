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
use lakko_contacts::incoming_envelope::{
    dispatch_incoming_envelope as rust_dispatch_incoming_envelope, IncomingEnvelope,
};
use lakko_contacts::invite::{
    compose as rust_invite_compose, compute_joiner_tag as rust_compute_joiner_tag,
    decode_and_verify as rust_invite_decode, sign_owner_list as rust_sign_owner_list,
    sign_revocation as rust_sign_revocation, BLOB_ID_LEN,
};
use lakko_contacts::{
    compose as rust_compose, decode_and_verify as rust_decode, open_envelope as rust_open,
    ContactError, ContactRequest as RustContactRequest, Direction as RustDirection,
    IdentityPseudonym as RustIdentityPseudonym, InviteCapability as RustInviteCapability,
    PerRelationship as RustPerRelationship, StrangerInbox as RustStrangerInbox, ACCOUNT_ID_LEN,
    MAX_LABEL_LEN, PSEUDONYM_LEN,
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
/// `privacy_pass_token` is an optional Privacy Pass / App-Attest /
/// Play-Integrity attestation blob (RFC 9576 — Phase 1.9b2). Pass
/// an **empty array** to omit it; the DS ingest path treats
/// missing tokens as "no attestation tier" (lowest rate-limit
/// ceiling). Mobile production builds populate this from the
/// platform attestation API; web + dev builds send empty. Capped
/// at [`lakko_contacts::request::MAX_PRIVACY_PASS_TOKEN_LEN`] —
/// over-cap returns [`FfiError::Contacts`].
///
/// `intro_message` is the optional plaintext payload (sealed in
/// an HPKE-equivalent envelope to `recipient_x25519_pub`).
///
/// # Errors
///
/// * [`FfiError::Contacts`] on wrong-length seed / account-id /
///   pseudonym, over-cap `privacy_pass_token`, or any AEAD
///   failure inside the sealed envelope.
/// * [`FfiError::Crypto`] when `recipient_x25519_pub` is not a
///   valid X25519 public key (canonical encoding).
#[uniffi::export]
pub fn compose_contact_request(
    sender_signing_seed: Vec<u8>,
    sender_account_id: Vec<u8>,
    recipient_x25519_pub: Vec<u8>,
    from_pseudonym: Vec<u8>,
    valid_after_ms: u64,
    privacy_pass_token: Vec<u8>,
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
        &privacy_pass_token,
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

    /// Privacy Pass / App-Attest / Play-Integrity attestation
    /// blob the sender attached (Phase 1.9b2 — RFC 9576). Empty
    /// `Vec` means "no token" — the wire envelope either was
    /// composed without one (web / dev path) or arrived from a
    /// v1 / v2 legacy sender that pre-dates this field. The DS
    /// ingest path is the authority on what the token means
    /// (rate-limit tier lift); the local client surfaces the
    /// raw bytes here for transparency + future verification.
    pub fn privacy_pass_token(&self) -> Vec<u8> {
        self.inner.privacy_pass_token.clone()
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

// ============================================================================
// InviteCapability — ADR-0005 §13.3 / §13.4
// ============================================================================

/// Compose a signed invite capability. Returns the wire bytes.
///
/// Top-level free function rather than an associated function on
/// [`InviteCapability`] for the same reason as
/// [`compose_contact_request`]: UniFFI 0.28 rejects associated
/// fns on `Object`-typed classes.
///
/// `owner_signing_seed` is the 32-byte Ed25519 seed the
/// inviter's [`crate::Identity`] holds (see
/// [`crate::Identity::ed25519_seed`] for the canonical source).
/// `owner_account_id` is 16 bytes (UUIDv7).
/// `owner_x25519_pubkey` is the inviter's long-term HPKE init
/// key (32 bytes — [`crate::Identity::x25519_public_key`]).
///
/// `expiry_ms` is UNIX millis; pass `0` for the
/// `NEVER_EXPIRES` sentinel — the UI **must** warn before
/// honouring (stalking risk; ADR-0005 §13.3). `max_uses` is a
/// UI hint only in the v1 wire format (server-side enforcement
/// is the discovery service's job — 1.18d on the web side, the
/// Kotlin invite-flow sub-sprint will pick this up). `0` means
/// the `SINGLE_USE` sentinel. `bootstrap_referral = true` flips
/// on the fresh-signup deep-link path (ADR-0005 §13.4).
/// `label` is an optional human-visible UTF-8 string capped at
/// [`MAX_LABEL_LEN`] bytes — empty string is fine.
///
/// # Errors
///
/// * [`FfiError::Contacts`] on wrong-length seed / account-id /
///   x25519 pubkey, or on a label exceeding `MAX_LABEL_LEN`
///   bytes.
/// * [`FfiError::Crypto`] when `owner_x25519_pubkey` is not a
///   valid X25519 public key (canonical encoding).
#[uniffi::export]
pub fn compose_invite_capability(
    owner_signing_seed: Vec<u8>,
    owner_account_id: Vec<u8>,
    owner_x25519_pubkey: Vec<u8>,
    expiry_ms: u64,
    max_uses: u32,
    bootstrap_referral: bool,
    label: String,
) -> Result<Vec<u8>, FfiError> {
    if owner_signing_seed.len() != 32 {
        return Err(FfiError::Contacts(
            "ownerSigningSeed must be 32 bytes".to_string(),
        ));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&owner_signing_seed);
    let signing = SigningKey::from_seed(&seed);

    if owner_account_id.len() != ACCOUNT_ID_LEN {
        return Err(FfiError::Contacts(format!(
            "ownerAccountId must be {ACCOUNT_ID_LEN} bytes",
        )));
    }
    let mut acct = [0u8; ACCOUNT_ID_LEN];
    acct.copy_from_slice(&owner_account_id);

    if owner_x25519_pubkey.len() != PUBLIC_KEY_LEN {
        return Err(FfiError::Contacts(format!(
            "ownerX25519Pubkey must be {PUBLIC_KEY_LEN} bytes",
        )));
    }
    let mut x_arr = [0u8; PUBLIC_KEY_LEN];
    x_arr.copy_from_slice(&owner_x25519_pubkey);
    let x25519_pub = X25519PublicKey::from_bytes(&x_arr)?;

    if label.len() > MAX_LABEL_LEN {
        return Err(FfiError::Contacts(format!(
            "label is {} bytes, max is {MAX_LABEL_LEN}",
            label.len(),
        )));
    }

    let wire = rust_invite_compose(
        &signing,
        &acct,
        &x25519_pub,
        expiry_ms,
        max_uses,
        bootstrap_referral,
        &label,
    )?;
    Ok(wire)
}

/// Composed and verified invite capability — capability-based
/// contact-invite flow per ADR-0005 §13.3 / §13.4. Mirror of
/// [`lakko_contacts::InviteCapability`].
///
/// Owners drive [`compose_invite_capability`] to produce signed
/// wire bytes; the Kotlin layer base64url-encodes the bytes
/// into a `https://<host>/i/<...>` URL and shares it OOB.
/// Joiners drive [`InviteCapability::decode_and_verify`] on
/// the bytes recovered from the URL slug, then read the
/// getters to seed the first ContactRequest back to the
/// inviter via the Kotlin-side `sendContactRequestToBundle`
/// orchestrator (1.15e9-side work).
///
/// The v1 wire format is the 1.18a Rust struct verbatim — no
/// server dependency, capability bytes live in the URL itself
/// (server-side `/discovery/invite` storage is the discovery
/// service's job, deferred 1.15e9-side work).
#[derive(uniffi::Object)]
pub struct InviteCapability {
    inner: RustInviteCapability,
}

#[uniffi::export]
impl InviteCapability {
    /// Joiner-side: decode wire bytes, verify the Ed25519
    /// signature, return the validated [`InviteCapability`].
    ///
    /// Does **not** check expiry — call [`is_valid_now`]
    /// separately so the UI can render "this invite expired N
    /// minutes ago" rather than a generic verification failure.
    #[uniffi::constructor]
    pub fn decode_and_verify(wire: Vec<u8>) -> Result<Self, FfiError> {
        let inner = rust_invite_decode(&wire)?;
        Ok(Self { inner })
    }

    /// Inviter's long-term Ed25519 verifying key (32 bytes).
    pub fn owner_signature_pubkey(&self) -> Vec<u8> {
        self.inner.owner_signature_pubkey.to_bytes().to_vec()
    }

    /// Inviter's `account_id` (16-byte UUIDv7).
    pub fn owner_account_id(&self) -> Vec<u8> {
        self.inner.owner_account_id.to_vec()
    }

    /// Inviter's long-term X25519 public key (32 bytes).
    pub fn owner_x25519_pubkey(&self) -> Vec<u8> {
        self.inner.owner_x25519_pubkey.to_bytes().to_vec()
    }

    /// UNIX milliseconds when the invite stops being valid.
    /// `0` is the `NEVER_EXPIRES` sentinel — the UI must warn
    /// before honouring (stalking risk; ADR-0005 §13.3).
    pub fn expiry_ms(&self) -> u64 {
        self.inner.expiry_ms
    }

    /// Max-uses hint. `0` is the `SINGLE_USE` sentinel. v1 is
    /// a best-effort hint with no server enforcement — the
    /// discovery service consume-counter (web 1.18d-side work,
    /// 1.15e9 on the Kotlin side) lands the wire enforcement.
    pub fn max_uses(&self) -> u32 {
        self.inner.max_uses
    }

    /// `true` when this is a referral link for a fresh signup
    /// (ADR-0005 §13.4) — the joiner's signup flow auto-sends
    /// a ContactRequest back to the inviter on successful
    /// account creation.
    pub fn bootstrap_referral(&self) -> bool {
        self.inner.bootstrap_referral
    }

    /// Optional human-visible label, UTF-8, ≤ [`MAX_LABEL_LEN`].
    /// Empty string when the inviter didn't set one — the UI
    /// fills in a default from the inviter's display name on
    /// lookup.
    pub fn label(&self) -> String {
        self.inner.label.clone()
    }

    /// `true` iff the capability has not expired at `now_ms`.
    /// `NEVER_EXPIRES` capabilities always return `true`.
    pub fn is_valid_now(&self, now_ms: u64) -> bool {
        self.inner.is_valid_now(now_ms)
    }

    /// Signal-style safety number derived from the owner's
    /// Ed25519 identity key. Joiner UI shows it before
    /// Accept so the human cross-checks the digits against
    /// what the owner shared OOB. Defends against link-
    /// substitution attacks; identical format + value to
    /// the call-invite fingerprint for the same owner.
    pub fn owner_fingerprint(&self) -> String {
        self.inner.owner_fingerprint()
    }
}

/// Sign an invite-link revocation request.
///
/// The Kotlin client calls this just before posting
/// `DELETE /discovery/invite/{blob_id}` so the body's
/// `signature` field carries proof that the revocation came
/// from the same Ed25519 key that signed the capability. The
/// canonical bytes the signature covers — domain separator +
/// `blob_id` + big-endian `signed_at_ms` — live in
/// [`lakko_contacts::invite::revoke_canonical_bytes`]; both
/// the server (`services/discovery`) and this helper drive
/// sign / verify against the same layout.
///
/// Returns the 64-byte raw Ed25519 signature. Caller
/// base64-encodes for the JSON body.
///
/// # Errors
///
/// * [`FfiError::Contacts`] on wrong-length signing seed
///   (32 bytes required) or blob_id ([`BLOB_ID_LEN`] required).
#[uniffi::export]
pub fn sign_invite_revocation(
    signing_seed: Vec<u8>,
    blob_id: Vec<u8>,
    signed_at_ms: u64,
) -> Result<Vec<u8>, FfiError> {
    if signing_seed.len() != 32 {
        return Err(FfiError::Contacts(
            "signingSeed must be 32 bytes".to_string(),
        ));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&signing_seed);
    let signing = SigningKey::from_seed(&seed);

    if blob_id.len() != BLOB_ID_LEN {
        return Err(FfiError::Contacts(format!(
            "blobId must be {BLOB_ID_LEN} bytes",
        )));
    }
    let mut blob_id_arr = [0u8; BLOB_ID_LEN];
    blob_id_arr.copy_from_slice(&blob_id);

    Ok(rust_sign_revocation(&signing, &blob_id_arr, signed_at_ms).to_vec())
}

/// Sign a **call-invite** link revocation request (Phase 3c3-revoke).
///
/// Sibling of [`sign_invite_revocation`] for the
/// [`CallInviteCapability`] ecosystem. The iOS / Android client
/// calls this just before posting
/// `DELETE /discovery/call-invite/{blob_id}` so the body's
/// `signature` field carries proof that the revocation came
/// from the same Ed25519 key that signed the capability.
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
/// * [`FfiError::Contacts`] on wrong-length signing seed
///   (32 bytes required) or blob_id ([`BLOB_ID_LEN`] required).
#[uniffi::export]
pub fn sign_call_invite_revocation(
    signing_seed: Vec<u8>,
    blob_id: Vec<u8>,
    signed_at_ms: u64,
) -> Result<Vec<u8>, FfiError> {
    if signing_seed.len() != 32 {
        return Err(FfiError::Contacts(
            "signingSeed must be 32 bytes".to_string(),
        ));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&signing_seed);
    let signing = SigningKey::from_seed(&seed);

    if blob_id.len() != BLOB_ID_LEN {
        return Err(FfiError::Contacts(format!(
            "blobId must be {BLOB_ID_LEN} bytes",
        )));
    }
    let mut blob_id_arr = [0u8; BLOB_ID_LEN];
    blob_id_arr.copy_from_slice(&blob_id);

    Ok(rust_call_sign_revocation(&signing, &blob_id_arr, signed_at_ms).to_vec())
}

/// Sign an owner-list query for the
/// `POST /discovery/invite/by-owner` endpoint (Phase
/// 1.18d2b-list).
///
/// Each client calls this just before posting the list request
/// so the body's `signature` field carries proof that the
/// caller is the holder of the identity Ed25519 key bound to
/// `account_id` via the KT log. The canonical bytes the
/// signature covers — domain separator + `account_id` +
/// big-endian `signed_at_ms` — live in
/// [`lakko_contacts::invite::owner_list_canonical_bytes`]; both
/// the server (`services/discovery::invite::list_by_owner`) and
/// this helper drive sign / verify against the same layout.
///
/// Distinct domain separator from
/// [`sign_invite_revocation`] ensures a captured revoke
/// signature cannot be replayed as an owner-list authentication
/// (and vice versa).
///
/// Returns the 64-byte raw Ed25519 signature. Caller
/// base64-encodes for the JSON body.
///
/// # Errors
///
/// * [`FfiError::Contacts`] on wrong-length signing seed (32
///   bytes required) or account_id ([`ACCOUNT_ID_LEN`] required).
#[uniffi::export]
pub fn sign_owner_list_query(
    signing_seed: Vec<u8>,
    owner_account_id: Vec<u8>,
    signed_at_ms: u64,
) -> Result<Vec<u8>, FfiError> {
    if signing_seed.len() != 32 {
        return Err(FfiError::Contacts(
            "signingSeed must be 32 bytes".to_string(),
        ));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&signing_seed);
    let signing = SigningKey::from_seed(&seed);

    if owner_account_id.len() != ACCOUNT_ID_LEN {
        return Err(FfiError::Contacts(format!(
            "ownerAccountId must be {ACCOUNT_ID_LEN} bytes",
        )));
    }
    let mut acct_arr = [0u8; ACCOUNT_ID_LEN];
    acct_arr.copy_from_slice(&owner_account_id);

    Ok(rust_sign_owner_list(&signing, &acct_arr, signed_at_ms).to_vec())
}

/// Compute the per-joiner consume-dedup tag for an invite-link
/// `(capability_bytes, joiner_account_id)` pair.
///
/// The Kotlin client calls this just before posting
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
/// * [`FfiError::Contacts`] if `joiner_account_id` is the
///   wrong length — 16 bytes is the only supported shape
///   today (UUIDv7 raw). The capability slice is unbounded by
///   length here; the server-side ceiling enforces the actual
///   cap.
#[uniffi::export]
pub fn compute_joiner_invite_tag(
    capability_bytes: Vec<u8>,
    joiner_account_id: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    if joiner_account_id.len() != ACCOUNT_ID_LEN {
        return Err(FfiError::Contacts(format!(
            "joinerAccountId must be {ACCOUNT_ID_LEN} bytes",
        )));
    }
    Ok(rust_compute_joiner_tag(&capability_bytes, &joiner_account_id).to_vec())
}

// ============================================================================
// ContactAck (1.9b1 — reply path)
// ============================================================================

/// Compose a signed `ContactAck` and return its wire bytes.
///
/// The recipient of an inbound [`ContactRequest`] who has decided
/// to accept produces this and publishes it to the sender's
/// mailbox at `ackToPseudonym` (the same per-relationship
/// pseudonym the sender embedded in the original request as
/// `from_pseudonym`).
///
/// `accepterSigningSeed` — recipient's 32-byte Ed25519 seed
/// (same shape every other contact-flow call uses).
/// `accepterAccountId` — recipient's 16-byte UUIDv7.
/// `accepterX25519Pub` — recipient's HPKE init key (32 bytes;
/// sender already knows it from discovery, included again so the
/// ack is a self-contained authorisation token for the future
/// fast-path).
/// `ackToPseudonym` — 32-byte routing handle picked off the
/// original `ContactRequest.from_pseudonym`.
/// `acceptedAtMs` — UNIX millisecond timestamp; sender uses it
/// for staleness display.
///
/// # Errors
///
/// * [`FfiError::Contacts`] on wrong-length seed / account-id /
///   x25519-pub / pseudonym.
/// * [`FfiError::Crypto`] when `accepterX25519Pub` is not a valid
///   X25519 public key (canonical encoding).
#[uniffi::export]
pub fn compose_contact_ack(
    accepter_signing_seed: Vec<u8>,
    accepter_account_id: Vec<u8>,
    accepter_x25519_pub: Vec<u8>,
    ack_to_pseudonym: Vec<u8>,
    accepted_at_ms: u64,
) -> Result<Vec<u8>, FfiError> {
    if accepter_signing_seed.len() != 32 {
        return Err(FfiError::Contacts(
            "accepterSigningSeed must be 32 bytes".to_string(),
        ));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&accepter_signing_seed);
    let signing = SigningKey::from_seed(&seed);

    if accepter_account_id.len() != ACCOUNT_ID_LEN {
        return Err(FfiError::Contacts(format!(
            "accepterAccountId must be {ACCOUNT_ID_LEN} bytes",
        )));
    }
    let mut acct = [0u8; ACCOUNT_ID_LEN];
    acct.copy_from_slice(&accepter_account_id);

    if accepter_x25519_pub.len() != PUBLIC_KEY_LEN {
        return Err(FfiError::Contacts(format!(
            "accepterX25519Pub must be {PUBLIC_KEY_LEN} bytes",
        )));
    }
    let x25519_pub = X25519PublicKey::from_bytes(&accepter_x25519_pub)?;
    let pseudonym = decode_pseudonym_bytes(&ack_to_pseudonym)?;

    let wire =
        lakko_contacts::compose_ack(&signing, &acct, &x25519_pub, pseudonym, accepted_at_ms)?;
    Ok(wire)
}

/// Validated `ContactAck`. Mirror of [`lakko_contacts::ContactAck`].
///
/// The sender's poll layer pulls one of these off their mailbox,
/// matches `accepterPubkey` + `ackToPseudonym` against an open
/// row in the pending-sent table, and flips the local UI state
/// to "accepted". A signature-valid ack whose `accepterPubkey`
/// doesn't match any pending request is dropped silently (spam
/// containment — anyone with PoW can publish to the mailbox).
#[derive(uniffi::Object)]
pub struct ContactAck {
    inner: lakko_contacts::ContactAck,
}

#[uniffi::export]
impl ContactAck {
    /// Decode wire bytes, verify the Ed25519 signature, return
    /// the validated ack.
    ///
    /// # Errors
    ///
    /// * [`FfiError::Contacts`] — wire format malformed or Ed25519
    ///   signature failed.
    #[uniffi::constructor]
    pub fn decode_and_verify(wire: Vec<u8>) -> Result<Self, FfiError> {
        let inner = lakko_contacts::decode_and_verify_ack(&wire)?;
        Ok(Self { inner })
    }

    /// Per-relationship pseudonym from the original request — the
    /// caller's pending-sent table key. The sender's poll layer
    /// looks this up to flip "pending → accepted" without revealing
    /// the recipient identity to the server.
    pub fn ack_to_pseudonym(&self) -> Vec<u8> {
        self.inner.ack_to_pseudonym.to_vec()
    }

    /// Recipient's Ed25519 identity public key (32 bytes).
    pub fn accepter_pubkey(&self) -> Vec<u8> {
        self.inner.accepter_pubkey.to_bytes().to_vec()
    }

    /// Recipient's UUIDv7 account id (16 bytes).
    pub fn accepter_account_id(&self) -> Vec<u8> {
        self.inner.accepter_account_id.to_vec()
    }

    /// Recipient's X25519 HPKE init key (32 bytes).
    pub fn accepter_x25519_pubkey(&self) -> Vec<u8> {
        self.inner.accepter_x25519_pubkey.to_bytes().to_vec()
    }

    /// UNIX milliseconds at which the ack was minted.
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
/// digits separated by single spaces, e.g.
/// `"12345 67890 12345 67890 12345 67890 12345 67890 12345 67890
/// 12345 67890"`. Both sides of a conversation compute the same
/// string regardless of order, so the OOB compare ("read me your
/// number / mine reads X") works either way.
///
/// Each pubkey input is the raw 32-byte Ed25519 verifying key
/// (the same shape every other contact-flow call uses). The
/// inputs are NOT validated as on-curve points — the function is
/// purely a hash over the bytes; callers that need on-curve
/// validation should do it before invoking this helper.
///
/// # Errors
///
/// * [`FfiError::Contacts`] when either input is not exactly
///   32 bytes long.
#[uniffi::export]
pub fn compute_safety_number(
    my_pubkey: Vec<u8>,
    their_pubkey: Vec<u8>,
) -> Result<String, FfiError> {
    if my_pubkey.len() != PUBLIC_KEY_LEN {
        return Err(FfiError::Contacts(format!(
            "myPubkey must be {PUBLIC_KEY_LEN} bytes",
        )));
    }
    if their_pubkey.len() != PUBLIC_KEY_LEN {
        return Err(FfiError::Contacts(format!(
            "theirPubkey must be {PUBLIC_KEY_LEN} bytes",
        )));
    }
    let mut a = [0u8; PUBLIC_KEY_LEN];
    a.copy_from_slice(&my_pubkey);
    let mut b = [0u8; PUBLIC_KEY_LEN];
    b.copy_from_slice(&their_pubkey);
    Ok(lakko_contacts::safety_number_bytes(&a, &b))
}

// ============================================================================
// CallInviteCapability — ROADMAP 3c3
// ============================================================================
//
// Sibling of `InviteCapability` for link-based group-call
// invites. The owner mints a one-time `KeyPackage` via
// `Identity::create_key_package` (already in this crate's
// FFI surface), embeds the resulting wire bytes here, signs
// the whole thing with their Ed25519 seed, ships out the
// resulting `wire` bytes as a `https://<host>/c/<base64url>`
// link. Joiner decodes + verifies + boots a fresh MLS group
// using the embedded KP.

/// Media flags mirror of `lakko_contacts::CallInviteMedia`.
/// Wire bitmap: `bit 0 = audio`, `bit 1 = video`.
#[derive(uniffi::Record)]
pub struct FfiCallInviteMedia {
    /// `true` when the call supports audio.
    pub audio: bool,
    /// `true` when the call supports video.
    pub video: bool,
}

impl FfiCallInviteMedia {
    fn into_inner(self) -> RustCallInviteMedia {
        RustCallInviteMedia {
            audio: self.audio,
            video: self.video,
        }
    }

    fn from_inner(inner: RustCallInviteMedia) -> Self {
        Self {
            audio: inner.audio,
            video: inner.video,
        }
    }
}

/// Compose + sign a call-invite capability (Phase 3c3).
/// Returns the wire bytes the platform base64url-encodes
/// into a `https://<host>/c/<...>` link.
///
/// `owner_signing_seed` is the 32-byte Ed25519 seed (same
/// source as `compose_invite_capability`).
/// `owner_account_id` is the 16-byte UUIDv7.
/// `call_id` is 16 random bytes the platform mints with
/// `crypto.getRandomValues` / `SecRandomCopyBytes` /
/// `SecureRandom.nextBytes`. Every joiner sees the same
/// `call_id` so the call ledger
/// (`lakko_calls::GroupCallSession`) shares membership
/// across all joiners.
/// `one_time_key_package` is the opaque wire bytes minted
/// via `Identity::create_key_package`.
///
/// # Errors
///
/// * [`FfiError::Contacts`] on wrong-length seed /
///   account-id / call-id, empty or oversized KP,
///   oversized label.
#[uniffi::export]
pub fn compose_call_invite_capability(
    owner_signing_seed: Vec<u8>,
    owner_account_id: Vec<u8>,
    call_id: Vec<u8>,
    expiry_ms: u64,
    max_uses: u32,
    media: FfiCallInviteMedia,
    one_time_key_package: Vec<u8>,
    label: String,
) -> Result<Vec<u8>, FfiError> {
    if owner_signing_seed.len() != 32 {
        return Err(FfiError::Contacts(
            "ownerSigningSeed must be 32 bytes".to_string(),
        ));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&owner_signing_seed);
    let signing = SigningKey::from_seed(&seed);

    if owner_account_id.len() != ACCOUNT_ID_LEN {
        return Err(FfiError::Contacts(format!(
            "ownerAccountId must be {ACCOUNT_ID_LEN} bytes",
        )));
    }
    let mut acct = [0u8; ACCOUNT_ID_LEN];
    acct.copy_from_slice(&owner_account_id);

    if call_id.len() != CALL_ID_LEN {
        return Err(FfiError::Contacts(format!(
            "callId must be {CALL_ID_LEN} bytes",
        )));
    }
    let mut cid = [0u8; CALL_ID_LEN];
    cid.copy_from_slice(&call_id);

    if one_time_key_package.is_empty() {
        return Err(FfiError::Contacts(
            "oneTimeKeyPackage must be non-empty".into(),
        ));
    }
    if one_time_key_package.len() > MAX_KEY_PACKAGE_LEN {
        return Err(FfiError::Contacts(format!(
            "oneTimeKeyPackage is {} bytes, max is {MAX_KEY_PACKAGE_LEN}",
            one_time_key_package.len(),
        )));
    }

    let wire = rust_call_invite_compose(
        &signing,
        &acct,
        &cid,
        expiry_ms,
        max_uses,
        media.into_inner(),
        &one_time_key_package,
        &label,
    )?;
    Ok(wire)
}

/// Composed and verified call-invite capability (Phase 3c3).
/// Mirror of [`lakko_contacts::CallInviteCapability`].
#[derive(uniffi::Object)]
pub struct CallInviteCapability {
    pub(crate) inner: RustCallInviteCapability,
}

#[uniffi::export]
impl CallInviteCapability {
    /// Joiner-side: decode wire bytes, verify the Ed25519
    /// signature, return the validated capability. Does
    /// **not** check expiry — call [`Self::is_valid_now`]
    /// separately.
    #[uniffi::constructor]
    pub fn decode_and_verify(wire: Vec<u8>) -> Result<Self, FfiError> {
        let inner = rust_call_invite_decode(&wire)?;
        Ok(Self { inner })
    }

    /// Inviter's long-term Ed25519 verifying key (32 bytes).
    pub fn owner_signature_pubkey(&self) -> Vec<u8> {
        self.inner.owner_signature_pubkey.to_bytes().to_vec()
    }

    /// Inviter's `account_id` (16-byte UUIDv7).
    pub fn owner_account_id(&self) -> Vec<u8> {
        self.inner.owner_account_id.to_vec()
    }

    /// 16-byte random call id minted by the inviter. Every
    /// joiner sees the same value.
    pub fn call_id(&self) -> Vec<u8> {
        self.inner.call_id.to_vec()
    }

    /// UNIX milliseconds when the invite stops being valid.
    /// `0` is the never-expires sentinel.
    pub fn expiry_ms(&self) -> u64 {
        self.inner.expiry_ms
    }

    /// Max-uses hint. `0` is the single-use sentinel.
    pub fn max_uses(&self) -> u32 {
        self.inner.max_uses
    }

    /// Audio / video flags.
    pub fn media(&self) -> FfiCallInviteMedia {
        FfiCallInviteMedia::from_inner(self.inner.media)
    }

    /// Owner's one-time `KeyPackage` wire bytes — joiner
    /// hands these to `Group::join_from_welcome` /
    /// equivalent to bootstrap the call MLS group.
    pub fn one_time_key_package(&self) -> Vec<u8> {
        self.inner.one_time_key_package.clone()
    }

    /// Optional human-visible label. Empty string when the
    /// inviter didn't set one.
    pub fn label(&self) -> String {
        self.inner.label.clone()
    }

    /// `true` iff the capability has not expired at `now_ms`.
    pub fn is_valid_now(&self, now_ms: u64) -> bool {
        self.inner.is_valid_now(now_ms)
    }

    /// Signal-style safety number derived from the owner's
    /// long-term Ed25519 identity key. Joiner UI shows this
    /// before letting the human Accept so they can cross-
    /// check the digits against what the owner shared OOB.
    /// Defends against link-substitution attacks.
    pub fn owner_fingerprint(&self) -> String {
        self.inner.owner_fingerprint()
    }
}

// ===========================================================================
// Call-Welcome envelope — Phase 3c3-welcome-ferry
// ===========================================================================

/// Wrap MLS Welcome bytes in the call-welcome envelope's
/// domain separator (Phase 3c3-welcome-ferry). The joiner
/// hands the output to `contact_request_compose` as the
/// `intro_message` argument; the existing HPKE-seal +
/// stranger-inbox transport carries the wrapped bytes to the
/// owner.
///
/// See [`lakko_contacts::call_welcome`] for the threat-model
/// rationale (reuse the contact-invite envelope's
/// confidentiality + authenticity + anti-abuse contracts).
///
/// # Errors
///
/// Infallible — returns `Ok` unconditionally; the `Result`
/// shape matches the rest of the FFI surface so callers don't
/// have to special-case the call-welcome path.
#[uniffi::export]
#[must_use]
pub fn wrap_call_welcome(welcome_bytes: Vec<u8>) -> Vec<u8> {
    rust_wrap_call_welcome(&welcome_bytes)
}

/// Try to unwrap a call-welcome envelope from a HPKE-
/// decrypted plaintext (Phase 3c3-welcome-ferry).
///
/// Returns:
///   * `Ok(Some(welcome_bytes))` — `plaintext` started with
///     the call-welcome domain separator AND the trailing
///     payload is at least `CALL_WELCOME_MIN_PAYLOAD_LEN`
///     bytes. Caller drives
///     `Group::join_from_welcome(payload)`.
///   * `Ok(None)` — `plaintext` is a plain intro message;
///     caller renders it as a free-text ContactRequest.
///
/// # Errors
///
/// * [`FfiError::Contacts`] ONLY when the domain separator
///   matched but the payload is truncated (a malformed /
///   hostile envelope). Owner-side polling logs + drops.
#[uniffi::export]
pub fn try_unwrap_call_welcome(plaintext: Vec<u8>) -> Result<Option<Vec<u8>>, FfiError> {
    match rust_try_unwrap_call_welcome(&plaintext) {
        Ok(Some(payload)) => Ok(Some(payload.to_vec())),
        Ok(None) => Ok(None),
        Err(CallWelcomeError::TruncatedPayload { min, got }) => Err(FfiError::Contacts(format!(
            "call-welcome payload is {got} bytes, minimum is {min}",
        ))),
    }
}

/// Floor constant the platform code keeps for its own
/// pre-FFI sanity check (`welcome.size >=
/// callWelcomeMinPayloadLen()` before calling
/// `wrapCallWelcome`).
#[uniffi::export]
#[must_use]
pub fn call_welcome_min_payload_len() -> u32 {
    // Wire-stable u32 for FFI portability; the underlying
    // constant fits comfortably (32).
    u32::try_from(CALL_WELCOME_MIN_PAYLOAD_LEN).unwrap_or(u32::MAX)
}

// ===========================================================================
// Call-KP-Forward envelope — Phase 3c3-kp-forward
// (subsequent-joiner KeyPackage transport for multi-party calls)
// ===========================================================================

/// Wrap a fresh MLS KeyPackage in the call-KP-forward
/// envelope's domain separator (Phase 3c3-kp-forward).
///
/// Sibling of [`wrap_call_welcome`] for the subsequent-
/// joiner (3rd, 4th, … participant) branch of multi-party
/// link calls. The joiner hands the output to
/// `contact_request_compose` as the `intro_message`
/// argument; the existing HPKE-seal + stranger-inbox
/// transport carries the wrapped KP to the call group's
/// current committer.
///
/// See [`lakko_contacts::call_kp_forward`] for the threat-
/// model rationale.
///
/// # Errors
///
/// Infallible — returns `Ok` unconditionally for shape
/// symmetry with [`try_unwrap_call_kp_forward`].
#[uniffi::export]
#[must_use]
pub fn wrap_call_key_package(kp_bytes: Vec<u8>) -> Vec<u8> {
    rust_wrap_call_kp_forward(&kp_bytes)
}

/// Try to unwrap a call-KP-forward envelope from a HPKE-
/// decrypted plaintext (Phase 3c3-kp-forward).
///
/// Returns:
///   * `Ok(Some(kp_bytes))` — `plaintext` started with the
///     call-KP-forward domain separator AND the trailing
///     payload is at least `CALL_KP_FORWARD_MIN_PAYLOAD_LEN`
///     bytes. Caller drives `Group::add_member_from_wire(payload)`
///     against the call group and ferries the resulting
///     Welcome back via [`wrap_call_welcome`].
///   * `Ok(None)` — `plaintext` is not a KP-forward
///     envelope; caller falls through to
///     [`try_unwrap_call_welcome`] and then to the intro-
///     message rendering path.
///
/// # Errors
///
/// * [`FfiError::Contacts`] ONLY when the domain separator
///   matched but the payload is truncated.
#[uniffi::export]
pub fn try_unwrap_call_key_package(plaintext: Vec<u8>) -> Result<Option<Vec<u8>>, FfiError> {
    match rust_try_unwrap_call_kp_forward(&plaintext) {
        Ok(Some(payload)) => Ok(Some(payload.to_vec())),
        Ok(None) => Ok(None),
        Err(CallWelcomeError::TruncatedPayload { min, got }) => Err(FfiError::Contacts(format!(
            "call-KP-forward payload is {got} bytes, minimum is {min}",
        ))),
    }
}

/// Floor constant the platform code keeps for its own
/// pre-FFI sanity check (`kp.size >=
/// callKeyPackageMinPayloadLen()` before calling
/// `wrapCallKeyPackage`).
#[uniffi::export]
#[must_use]
pub fn call_key_package_min_payload_len() -> u32 {
    u32::try_from(CALL_KP_FORWARD_MIN_PAYLOAD_LEN).unwrap_or(u32::MAX)
}

// ===========================================================================
// Incoming-envelope dispatcher — Phase 3c3-incoming-dispatcher
// ===========================================================================

/// Kind tag for a dispatched incoming envelope. Mirror of
/// the variant set on
/// [`lakko_contacts::incoming_envelope::IncomingEnvelope`],
/// scoped to an FFI-portable enum (no borrow lifetime).
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiIncomingEnvelopeKind {
    /// First-joiner Welcome bytes for a link-based call.
    CallWelcome,
    /// Subsequent-joiner KeyPackage for a multi-party call.
    CallKeyPackage,
    /// Plain free-text intro message (ADR-0005 scenario A).
    IntroMessage,
}

/// FFI-portable result of [`dispatch_incoming_envelope`].
/// The `payload` slice is materialised into an owned
/// `Vec<u8>` at the FFI boundary so the platform side
/// doesn't have to think about Rust borrow lifetimes.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiIncomingEnvelope {
    /// Which dispatch branch fired.
    pub kind: FfiIncomingEnvelopeKind,
    /// The dispatched payload — for `CallWelcome` and
    /// `CallKeyPackage` this is the bytes **after** the
    /// 22- / 25-byte domain separator; for `IntroMessage`
    /// it is the original plaintext verbatim.
    pub payload: Vec<u8>,
}

/// Single-call dispatcher for HPKE-decrypted
/// `ContactRequest` plaintexts. Runs the two call-side
/// unwrap helpers in dispatch order
/// (welcome → KP-forward → intro-message fallthrough) and
/// returns the typed variant.
///
/// See [`lakko_contacts::incoming_envelope`] for the full
/// dispatch contract.
///
/// # Errors
///
/// * [`FfiError::Contacts`] when a domain separator matched
///   but the payload is shorter than the kind's minimum
///   floor (32 bytes for Welcome, 256 bytes for KP-forward).
#[uniffi::export]
pub fn dispatch_incoming_envelope(plaintext: Vec<u8>) -> Result<FfiIncomingEnvelope, FfiError> {
    let parsed = rust_dispatch_incoming_envelope(&plaintext).map_err(|err| match err {
        CallWelcomeError::TruncatedPayload { min, got } => FfiError::Contacts(format!(
            "incoming envelope payload is {got} bytes, minimum is {min}",
        )),
    })?;
    let (kind, payload) = match parsed {
        IncomingEnvelope::CallWelcome(p) => (FfiIncomingEnvelopeKind::CallWelcome, p.to_vec()),
        IncomingEnvelope::CallKeyPackage(p) => {
            (FfiIncomingEnvelopeKind::CallKeyPackage, p.to_vec())
        },
        IncomingEnvelope::IntroMessage(p) => (FfiIncomingEnvelopeKind::IntroMessage, p.to_vec()),
    };
    Ok(FfiIncomingEnvelope { kind, payload })
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
            vec![], // privacy_pass_token — empty for unit test
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
        assert!(req.privacy_pass_token().is_empty());

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
            vec![], // privacy_pass_token
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
            vec![], // privacy_pass_token
            vec![],
        )
        .expect_err("expected wrong-length rejection");
        match err {
            FfiError::Contacts(msg) => assert!(msg.contains("16 bytes")),
            other => panic!("expected Contacts error, got {other:?}"),
        }
    }

    // ===================================================================
    // InviteCapability — ADR-0005 §13.3 / §13.4
    // ===================================================================

    /// `composeInviteCapability` → `decodeAndVerify` round-trips
    /// every getter cleanly. Exercises the full v1 wire format
    /// through the FFI boundary; the underlying Rust struct
    /// already has comprehensive coverage in `lakko-contacts`'s
    /// own tests, so we just verify the FFI-side bridging.
    #[test]
    fn invite_capability_round_trip() {
        let alice_signing = SigningKey::from_seed(&[0xAAu8; 32]);
        let alice_account_id = [0xA1u8; ACCOUNT_ID_LEN];
        let alice_x25519 = InnerX25519SecretKey::from_bytes([0xBBu8; 32]).public_key();

        // Positional args — Rust function calls don't have
        // named-arg syntax; the parameter order matches the
        // signature on `compose_invite_capability` above.
        let wire = compose_invite_capability(
            alice_signing.to_seed().to_vec(),
            alice_account_id.to_vec(),
            alice_x25519.to_bytes().to_vec(),
            1_900_000_000_000, // expiry_ms
            5,                 // max_uses
            true,              // bootstrap_referral
            "lunch group".to_string(),
        )
        .expect("compose");

        let cap = InviteCapability::decode_and_verify(wire).expect("decode");
        assert_eq!(
            cap.owner_signature_pubkey(),
            alice_signing.verifying_key().to_bytes().to_vec(),
        );
        assert_eq!(cap.owner_account_id(), alice_account_id.to_vec());
        assert_eq!(cap.owner_x25519_pubkey(), alice_x25519.to_bytes().to_vec());
        assert_eq!(cap.expiry_ms(), 1_900_000_000_000);
        assert_eq!(cap.max_uses(), 5);
        assert!(cap.bootstrap_referral());
        assert_eq!(cap.label(), "lunch group");

        // is_valid_now: well before expiry → true; way after → false.
        assert!(cap.is_valid_now(1_700_000_000_000));
        assert!(!cap.is_valid_now(2_000_000_000_000));
    }

    #[test]
    fn invite_capability_rejects_oversize_label() {
        let alice_signing = SigningKey::from_seed(&[0xAAu8; 32]);
        let alice_x25519 = InnerX25519SecretKey::from_bytes([0xBBu8; 32]).public_key();
        // MAX_LABEL_LEN + 1 byte — should bounce.
        let too_long = "x".repeat(super::MAX_LABEL_LEN + 1);
        let err = compose_invite_capability(
            alice_signing.to_seed().to_vec(),
            vec![0u8; ACCOUNT_ID_LEN],
            alice_x25519.to_bytes().to_vec(),
            0,
            0,
            false,
            too_long,
        )
        .expect_err("expected oversize-label rejection");
        match err {
            FfiError::Contacts(msg) => assert!(msg.contains("max is")),
            other => panic!("expected Contacts, got {other:?}"),
        }
    }

    #[test]
    fn sign_invite_revocation_round_trip() {
        let alice_signing = SigningKey::from_seed(&[0x77u8; 32]);
        let blob_id = [0x99u8; BLOB_ID_LEN];
        let sig = sign_invite_revocation(
            alice_signing.to_seed().to_vec(),
            blob_id.to_vec(),
            1_700_000_000_000,
        )
        .expect("sign");
        assert_eq!(sig.len(), 64, "Ed25519 signature must be 64 bytes");
    }

    #[test]
    fn sign_invite_revocation_rejects_wrong_blob_id_length() {
        let alice_signing = SigningKey::from_seed(&[0x77u8; 32]);
        let err = sign_invite_revocation(
            alice_signing.to_seed().to_vec(),
            vec![0u8; 16], // wrong: BLOB_ID_LEN is 32
            0,
        )
        .expect_err("expected wrong-length rejection");
        match err {
            FfiError::Contacts(msg) => assert!(msg.contains("bytes")),
            other => panic!("expected Contacts, got {other:?}"),
        }
    }

    #[test]
    fn sign_call_invite_revocation_round_trip() {
        use lakko_contacts::call_invite::call_revoke_canonical_bytes;
        use lakko_crypto::signing::Signature;

        let alice_signing = SigningKey::from_seed(&[0xC7u8; 32]);
        let blob_id = [0xD9u8; BLOB_ID_LEN];
        let signed_at_ms = 1_700_000_000_000u64;

        let sig = sign_call_invite_revocation(
            alice_signing.to_seed().to_vec(),
            blob_id.to_vec(),
            signed_at_ms,
        )
        .expect("sign");
        assert_eq!(sig.len(), 64, "Ed25519 signature must be 64 bytes");

        // Round-trip via the same canonical bytes the server uses
        // to authorise `DELETE /discovery/call-invite/{blob_id}`.
        let pubkey = alice_signing.verifying_key();
        let canonical = call_revoke_canonical_bytes(&blob_id, signed_at_ms);
        let signature = Signature::from_bytes(&sig).unwrap();
        assert!(pubkey.verify(&canonical, &signature).is_ok());
    }

    #[test]
    fn sign_call_invite_revocation_isolated_from_contact_invite() {
        // Cross-kind isolation: signature minted via
        // `sign_call_invite_revocation` MUST NOT verify under
        // the contact-invite revoke canonical bytes (different
        // domain separator).
        use lakko_contacts::invite::revoke_canonical_bytes as contact_revoke_canonical_bytes;
        use lakko_crypto::signing::Signature;

        let alice_signing = SigningKey::from_seed(&[0xE3u8; 32]);
        let blob_id = [0xF1u8; BLOB_ID_LEN];
        let signed_at_ms = 1_700_000_000_000u64;

        let sig = sign_call_invite_revocation(
            alice_signing.to_seed().to_vec(),
            blob_id.to_vec(),
            signed_at_ms,
        )
        .expect("sign");
        let pubkey = alice_signing.verifying_key();
        let wrong_canonical = contact_revoke_canonical_bytes(&blob_id, signed_at_ms);
        let signature = Signature::from_bytes(&sig).unwrap();
        assert!(pubkey.verify(&wrong_canonical, &signature).is_err());
    }

    #[test]
    fn sign_call_invite_revocation_rejects_wrong_blob_id_length() {
        let alice_signing = SigningKey::from_seed(&[0x55u8; 32]);
        let err = sign_call_invite_revocation(
            alice_signing.to_seed().to_vec(),
            vec![0u8; 16], // wrong: BLOB_ID_LEN is 32
            0,
        )
        .expect_err("expected wrong-length rejection");
        match err {
            FfiError::Contacts(msg) => assert!(msg.contains("bytes")),
            other => panic!("expected Contacts, got {other:?}"),
        }
    }

    #[test]
    fn sign_owner_list_query_round_trip() {
        let alice_signing = SigningKey::from_seed(&[0x88u8; 32]);
        let acct = [0xA1u8; ACCOUNT_ID_LEN];
        let sig = sign_owner_list_query(
            alice_signing.to_seed().to_vec(),
            acct.to_vec(),
            1_700_000_000_000,
        )
        .expect("sign");
        assert_eq!(sig.len(), 64, "Ed25519 signature must be 64 bytes");
    }

    #[test]
    fn sign_owner_list_query_rejects_wrong_account_id_length() {
        let alice_signing = SigningKey::from_seed(&[0x88u8; 32]);
        let err = sign_owner_list_query(
            alice_signing.to_seed().to_vec(),
            vec![0u8; 8], // wrong: ACCOUNT_ID_LEN is 16
            0,
        )
        .expect_err("expected wrong-length rejection");
        match err {
            FfiError::Contacts(msg) => assert!(msg.contains("16 bytes")),
            other => panic!("expected Contacts, got {other:?}"),
        }
    }

    #[test]
    fn compute_joiner_invite_tag_round_trip() {
        // Build a real capability + a fixed joiner account id;
        // assert the tag is 32 bytes and is **deterministic**
        // for the same inputs (the dedup property the server
        // relies on).
        let alice_signing = SigningKey::from_seed(&[0xAAu8; 32]);
        let alice_x25519 = InnerX25519SecretKey::from_bytes([0xBBu8; 32]).public_key();
        let wire = compose_invite_capability(
            alice_signing.to_seed().to_vec(),
            vec![0u8; ACCOUNT_ID_LEN],
            alice_x25519.to_bytes().to_vec(),
            0,
            0,
            false,
            String::new(),
        )
        .expect("compose");
        let joiner_account_id = vec![0xCCu8; ACCOUNT_ID_LEN];

        let tag1 = compute_joiner_invite_tag(wire.clone(), joiner_account_id.clone()).expect("tag");
        assert_eq!(tag1.len(), 32);
        let tag2 = compute_joiner_invite_tag(wire, joiner_account_id).expect("tag");
        assert_eq!(
            tag1, tag2,
            "same inputs must yield same tag (dedup property)"
        );
    }

    #[test]
    fn compute_joiner_invite_tag_rejects_wrong_account_id_length() {
        let err = compute_joiner_invite_tag(vec![1u8, 2, 3], vec![0u8; 8])
            .expect_err("expected wrong-length rejection");
        match err {
            FfiError::Contacts(msg) => assert!(msg.contains("16")),
            other => panic!("expected Contacts, got {other:?}"),
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
            vec![], // privacy_pass_token
            b"secret".to_vec(),
        )
        .expect("compose");
        let req = ContactRequest::decode_and_verify(wire).expect("decode");
        match req.open_envelope(mallory_priv.to_bytes().to_vec()) {
            Err(FfiError::Contacts(_)) => {},
            other => panic!("expected Contacts error, got {other:?}"),
        }
    }

    // ========================================================
    // 1.9b2 — Privacy Pass token round-trip
    // ========================================================

    /// A v3 envelope with a populated `privacyPassToken` round-
    /// trips cleanly through the FFI boundary — the inner
    /// `lakko-contacts` crate has the wire-format coverage; this
    /// gates that the UniFFI getter surfaces the bytes
    /// unchanged.
    #[test]
    fn contact_request_privacy_pass_token_round_trip() {
        let alice_signing = SigningKey::from_seed(&[0xAAu8; 32]);
        let alice_account_id = [0xA1u8; ACCOUNT_ID_LEN];
        let bob_priv = InnerX25519SecretKey::from_bytes([0xBBu8; 32]);
        let bob_pub = bob_priv.public_key();
        let token = b"\x99\x88\x77 attestation \x66\x55".to_vec();

        let wire = compose_contact_request(
            alice_signing.to_seed().to_vec(),
            alice_account_id.to_vec(),
            bob_pub.to_bytes().to_vec(),
            vec![0xC9u8; PSEUDONYM_LEN],
            0,
            token.clone(),
            b"".to_vec(),
        )
        .expect("compose");
        let req = ContactRequest::decode_and_verify(wire).expect("decode");
        assert_eq!(req.privacy_pass_token(), token);
    }

    // -------------------------------------------------------------------
    // Call-Welcome envelope (3c3-welcome-ferry)
    // -------------------------------------------------------------------

    #[test]
    fn call_welcome_wrap_then_unwrap_round_trips() {
        let welcome = vec![0xCDu8; 256];
        let wrapped = wrap_call_welcome(welcome.clone());
        let unwrapped = try_unwrap_call_welcome(wrapped)
            .expect("unwrap should not error")
            .expect("Some(welcome) expected");
        assert_eq!(unwrapped, welcome);
    }

    #[test]
    fn call_welcome_unwrap_returns_none_for_intro_message() {
        let intro = b"hi bob it's alice".to_vec();
        let result = try_unwrap_call_welcome(intro).expect("unwrap should not error");
        assert!(result.is_none(), "intro message must surface as None");
    }

    #[test]
    fn call_welcome_unwrap_rejects_truncated_payload() {
        // Prefix matches but trailing bytes < 32-byte floor.
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(b"lakko/call-welcome/v1\x00");
        wrapped.extend_from_slice(&[0u8; 8]); // 8 < 32
        match try_unwrap_call_welcome(wrapped) {
            Err(FfiError::Contacts(msg)) => assert!(msg.contains('8') && msg.contains("32")),
            Err(other) => panic!("expected Contacts error, got {other:?}"),
            Ok(_) => panic!("truncated payload must surface as Err"),
        }
    }

    #[test]
    fn call_welcome_min_payload_len_matches_rust_constant() {
        assert_eq!(call_welcome_min_payload_len() as usize, 32);
    }

    // -------------------------------------------------------------------
    // Call-KP-Forward envelope (3c3-kp-forward)
    // -------------------------------------------------------------------

    #[test]
    fn call_kp_forward_wrap_then_unwrap_round_trips() {
        let kp = vec![0xCDu8; 512];
        let wrapped = wrap_call_key_package(kp.clone());
        let unwrapped = try_unwrap_call_key_package(wrapped)
            .expect("unwrap should not error")
            .expect("Some(kp) expected");
        assert_eq!(unwrapped, kp);
    }

    #[test]
    fn call_kp_forward_unwrap_returns_none_for_intro_message() {
        let intro = b"hi alice it is bob".to_vec();
        let result = try_unwrap_call_key_package(intro).expect("unwrap should not error");
        assert!(result.is_none(), "intro message must surface as None");
    }

    #[test]
    fn call_kp_forward_unwrap_returns_none_for_welcome_envelope() {
        // Cross-dispatch guard: a call-welcome envelope MUST
        // NOT decode as a KP-forward when fed to the wrong
        // unwrap helper.
        let welcome = vec![0xABu8; 256];
        let wrapped_welcome = wrap_call_welcome(welcome);
        let result = try_unwrap_call_key_package(wrapped_welcome).expect("unwrap should not error");
        assert!(
            result.is_none(),
            "call-welcome must NOT cross-dispatch as KP-forward",
        );
    }

    #[test]
    fn call_kp_forward_unwrap_rejects_truncated_payload() {
        // Prefix matches but trailing bytes < 256-byte floor.
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(b"lakko/call-keypackage/v1\x00");
        wrapped.extend_from_slice(&[0u8; 100]); // 100 < 256
        match try_unwrap_call_key_package(wrapped) {
            Err(FfiError::Contacts(msg)) => {
                assert!(msg.contains("100") && msg.contains("256"));
            },
            Err(other) => panic!("expected Contacts error, got {other:?}"),
            Ok(_) => panic!("truncated payload must surface as Err"),
        }
    }

    #[test]
    fn call_kp_forward_min_payload_len_matches_rust_constant() {
        assert_eq!(call_key_package_min_payload_len() as usize, 256);
    }

    // -------------------------------------------------------------------
    // Incoming-envelope dispatcher (3c3-incoming-dispatcher)
    // -------------------------------------------------------------------

    #[test]
    fn dispatch_routes_call_welcome_envelope() {
        let welcome = vec![0xABu8; 256];
        let wrapped = wrap_call_welcome(welcome.clone());
        let result = dispatch_incoming_envelope(wrapped).expect("dispatch should succeed");
        assert_eq!(result.kind, FfiIncomingEnvelopeKind::CallWelcome);
        assert_eq!(result.payload, welcome);
    }

    #[test]
    fn dispatch_routes_call_kp_forward_envelope() {
        let kp = vec![0xCDu8; 512];
        let wrapped = wrap_call_key_package(kp.clone());
        let result = dispatch_incoming_envelope(wrapped).expect("dispatch should succeed");
        assert_eq!(result.kind, FfiIncomingEnvelopeKind::CallKeyPackage);
        assert_eq!(result.payload, kp);
    }

    #[test]
    fn dispatch_falls_through_to_intro_message() {
        let intro = b"hello alice".to_vec();
        let result = dispatch_incoming_envelope(intro.clone()).expect("dispatch should succeed");
        assert_eq!(result.kind, FfiIncomingEnvelopeKind::IntroMessage);
        assert_eq!(result.payload, intro);
    }

    #[test]
    fn dispatch_treats_empty_plaintext_as_intro_message() {
        let result = dispatch_incoming_envelope(Vec::new()).expect("dispatch should succeed");
        assert_eq!(result.kind, FfiIncomingEnvelopeKind::IntroMessage);
        assert!(result.payload.is_empty());
    }

    #[test]
    fn dispatch_surfaces_truncated_welcome_as_error() {
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(b"lakko/call-welcome/v1\x00");
        wrapped.extend_from_slice(&[0u8; 4]);
        match dispatch_incoming_envelope(wrapped) {
            Err(FfiError::Contacts(msg)) => {
                assert!(msg.contains('4') && msg.contains("32"));
            },
            Err(other) => panic!("expected Contacts error, got {other:?}"),
            Ok(_) => panic!("truncated welcome must surface as Err"),
        }
    }
}
