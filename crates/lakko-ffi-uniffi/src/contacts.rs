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
//!     open the sealed box with the recipient's X25519 private key,
//!     returning the sender's profile (ADR-0048) and the intro
//!     message as an [`OpenedContactRequest`]. Method (not
//!     constructor) because it returns plaintext, not another
//!     `ContactRequest`.

use lakko_contacts::call_invite::{
    call_invite_expiry_ms as rust_call_invite_expiry_ms,
    CALL_INVITE_MAX_USES as RUST_CALL_INVITE_MAX_USES,
};
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
use lakko_contacts::committer_beacon::{
    acting_committer_verdict as rust_acting_committer_verdict,
    compose_committer_beacon as rust_compose_committer_beacon,
    decode_committer_beacon as rust_decode_committer_beacon,
    should_emit_beacon as rust_should_emit_beacon, ActingCommitter as RustActingCommitter,
    BeaconObservation as RustBeaconObservation, COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
    COMMITTER_BEACON_FRESHNESS_WINDOW_MS, COMMITTER_BEACON_INTERVAL_MS,
    COMMITTER_BEACON_PAYLOAD_LEN, COMMITTER_BEACON_WORST_CASE_POLL_MS,
};
use lakko_contacts::group_invite::{
    compose as rust_group_invite_compose,
    compose_with_preview as rust_group_invite_compose_with_preview,
    decode_and_verify as rust_group_invite_decode, generate_link_key as rust_generate_link_key,
    group_sign_revocation as rust_group_sign_revocation,
    GroupInviteCapability as RustGroupInviteCapability,
    GroupInvitePreview as RustGroupInvitePreview,
    LINK_SECRET_B64URL_LEN as GROUP_INVITE_LINK_SECRET_B64URL_LEN,
    LINK_SECRET_LEN as GROUP_INVITE_LINK_SECRET_LEN, MAX_GROUP_ID_LEN,
};
use lakko_contacts::group_join_attribution::{
    compose_attribution as rust_group_join_attribution_compose,
    decode_attribution as rust_group_join_attribution_decode,
};
use lakko_contacts::group_name::{
    compose_group_name as rust_compose_group_name, decode_group_name as rust_decode_group_name,
    group_name_is_newer as rust_group_name_is_newer,
    normalize_group_name as rust_normalize_group_name, GroupName as RustGroupName,
};
use lakko_contacts::incoming_envelope::{
    dispatch_incoming_envelope as rust_dispatch_incoming_envelope, IncomingEnvelope,
};
use lakko_contacts::invite::{
    compose as rust_invite_compose, compute_joiner_tag as rust_compute_joiner_tag,
    decode_and_verify as rust_invite_decode, sign_owner_list as rust_sign_owner_list,
    sign_revocation as rust_sign_revocation, BLOB_ID_B64URL_LEN, BLOB_ID_LEN,
};
use lakko_contacts::media_state::{
    compose_media_state as rust_compose_media_state, decode_media_state as rust_decode_media_state,
};
use lakko_contacts::pin::{
    compose_pin as rust_compose_pin, decode_pin as rust_decode_pin,
    pin_is_newer as rust_pin_is_newer, PinAction as RustPinAction, PinOp as RustPinOp,
};
use lakko_contacts::profile::{
    compose_profile as rust_compose_profile, decode_profile as rust_decode_profile,
    normalize_display_name as rust_normalize_display_name,
    profile_is_newer as rust_profile_is_newer, Profile as RustProfile,
};
use lakko_contacts::profile_request::{
    compose_profile_request as rust_compose_profile_request,
    decode_profile_request as rust_decode_profile_request,
    should_request_profiles as rust_should_request_profiles, PROFILE_REQUEST_ENVELOPE_LEN,
    PROFILE_REQUEST_MIN_INTERVAL_MS,
};
use lakko_contacts::push_routing::{
    compose_push_routing_secret as rust_compose_push_routing_secret,
    decode_push_routing_secret as rust_decode_push_routing_secret,
    derive_push_gid as rust_derive_push_gid,
    push_routing_secret_is_newer as rust_push_routing_secret_is_newer, PUSH_ROUTING_SECRET_LEN,
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
/// `profile` is the sender's own profile (ADR-0048), or `None` to send
/// none. It is sealed alongside the intro, so the recipient can show who
/// is asking before accepting; the DS still sees only ciphertext. It is
/// prepared exactly as [`compose_profile_envelope`] prepares it (display
/// name trimmed, blank = no name; username normalised and checked).
///
/// `intro_message` is the optional plaintext payload (sealed in
/// an HPKE-equivalent envelope to `recipient_x25519_pub`).
///
/// # Errors
///
/// * [`FfiError::Contacts`] on wrong-length seed / account-id /
///   pseudonym, over-cap `privacy_pass_token`, or any AEAD
///   failure inside the sealed envelope.
/// * A `Username*` / `DisplayName*` variant when `profile` violates a
///   bound — nothing is composed, so no recipient ever drops it.
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
    profile: Option<FfiProfile>,
    intro_message: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let profile = profile.map(prepare_profile).transpose()?;
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
        profile.as_ref(),
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

    /// Decrypt the sealed envelope using the recipient's 32-byte
    /// X25519 private key. Returns the sender's profile (if any) and
    /// the intro message.
    ///
    /// The profile is the sender's **claim**, bound to
    /// [`Self::from_pubkey`] (which the request signature
    /// authenticates): store it under that key, and show its username
    /// only after verifying it through discovery (ADR-0048 §3). A profile
    /// that fails its bounds arrives as `None` with the intro intact —
    /// the request is still authentic and still worth showing.
    ///
    /// # Errors
    ///
    /// * [`FfiError::Contacts`] when the AEAD open fails
    ///   (wrong key or tampered ciphertext), or the sealed plaintext's
    ///   profile framing is broken.
    pub fn open_envelope(
        &self,
        recipient_x25519_priv: Vec<u8>,
    ) -> Result<OpenedContactRequest, FfiError> {
        if recipient_x25519_priv.len() != 32 {
            return Err(FfiError::Contacts(
                "recipientX25519Priv must be 32 bytes".to_string(),
            ));
        }
        let mut sk = [0u8; 32];
        sk.copy_from_slice(&recipient_x25519_priv);
        let priv_key = X25519SecretKey::from_bytes(sk);
        let (profile, intro_message) = rust_open(&self.inner, &priv_key)?;
        Ok(OpenedContactRequest {
            profile: profile.map(Into::into),
            intro_message,
        })
    }
}

/// Result of [`ContactRequest::open_envelope`] (ADR-0048 §2).
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct OpenedContactRequest {
    /// The sender's profile, or `None` when the request carried none or
    /// carried one that failed its bounds. A claim bound to the request's
    /// `from_pubkey` — see [`ContactRequest::open_envelope`].
    pub profile: Option<FfiProfile>,
    /// The intro message, possibly empty. May itself be a call envelope —
    /// classify it with [`dispatch_incoming_envelope`] as before.
    pub intro_message: Vec<u8>,
}

// ============================================================================
// Profile envelope — ADR-0048
// ============================================================================

/// A person's self-stated profile (ADR-0048). Mirror of
/// [`lakko_contacts::profile::Profile`].
///
/// **Every field received from someone else is that person's claim.** Store
/// it under the sender's signature key resolved at receipt (C-68), keep it
/// only when [`profile_is_newer`] than the stored one, and show `username`
/// only once discovery resolves it to that same key.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiProfile {
    /// Display name, or `None` for no name.
    pub display_name: Option<String>,
    /// Claimed username in canonical form, or `None` for no username.
    pub username: Option<String>,
    /// Sender-clock UNIX milliseconds at which the profile was set.
    pub updated_at_ms: u64,
    /// The sender's 16-byte auth account id, or `None`. On compose, exactly
    /// 16 bytes or absent; anything else is refused. On receipt, the value
    /// the call roster derives the ADR-0021 peer tag from (ADR-0048 v2).
    pub account_id: Option<Vec<u8>>,
}

impl From<RustProfile> for FfiProfile {
    fn from(p: RustProfile) -> Self {
        Self {
            display_name: p.display_name,
            username: p.username,
            updated_at_ms: p.updated_at_ms,
            account_id: p.account_id.map(|id| id.to_vec()),
        }
    }
}

/// Turn a caller-supplied profile into the canonical core value: trim the
/// display name (blank = no name) and normalise + check the username
/// (`None` or empty = no username). Anything else out of bounds is refused
/// here with a typed variant, before any bytes exist.
fn prepare_profile(p: FfiProfile) -> Result<RustProfile, FfiError> {
    let display_name = match p.display_name {
        Some(name) => rust_normalize_display_name(&name)?,
        None => None,
    };
    let username = match p.username {
        Some(u) if !u.is_empty() => Some(lakko_discovery_client::validate_username(&u)?),
        _ => None,
    };
    let account_id = match p.account_id {
        None => None,
        Some(bytes) => Some(<[u8; 16]>::try_from(bytes.as_slice()).map_err(|_| {
            FfiError::Contacts(format!(
                "profile account id must be 16 bytes, got {}",
                bytes.len()
            ))
        })?),
    };
    Ok(RustProfile {
        display_name,
        username,
        updated_at_ms: p.updated_at_ms,
        account_id,
        // ADR-0058 fields are not surfaced on this FFI until sprint S3.
        username_certificate: None,
        grants: Vec::new(),
    })
}

/// Compose a profile envelope (ADR-0048) — the inner plaintext to publish
/// as an MLS application message in a group.
///
/// Prepares the profile first (see [`FfiProfile`]): the display name is
/// trimmed and a blank one becomes "no name"; a non-empty username is
/// normalised (`@Alice` -> `alice`) and must pass the ADR-0048 §4 rule. The
/// envelope produced is always one a receiver accepts.
///
/// # Errors
///
/// * `DisplayNameLineBreak` / `DisplayNameControlOrFormatCharacter` /
///   `DisplayNameTooLong` / `DisplayNameTooManyBytes` — the name.
/// * `UsernameEmpty` / `UsernameTooShort` / `UsernameTooLong` /
///   `UsernameInvalidCharacters` — the username.
#[uniffi::export]
pub fn compose_profile_envelope(profile: FfiProfile) -> Result<Vec<u8>, FfiError> {
    let prepared = prepare_profile(profile)?;
    rust_compose_profile(&prepared).map_err(Into::into)
}

/// Strictly decode a profile envelope (ADR-0048). Fail-closed: a plaintext
/// without the profile domain separator, malformed, or with any field out of
/// bounds is refused as a whole.
///
/// Use [`dispatch_incoming_envelope`] when the plaintext might be any
/// in-group envelope kind.
///
/// # Errors
///
/// * [`FfiError::Contacts`] for a wrong domain / truncation / trailing bytes
///   / unknown version / non-UTF-8 name.
/// * A `DisplayName*` or `Username*` variant for an out-of-bounds field.
#[uniffi::export]
pub fn decode_profile_envelope(plaintext: Vec<u8>) -> Result<FfiProfile, FfiError> {
    rust_decode_profile(&plaintext)
        .map(Into::into)
        .map_err(Into::into)
}

/// `true` when a profile stamped `incoming_updated_at_ms` must replace the
/// stored one **from the same signature key**. Strictly greater wins; equal
/// is not newer, so a re-broadcast is not an update.
#[uniffi::export]
#[must_use]
pub fn profile_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_profile_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// Prepare what a person typed as their display name: trim, map blank to
/// `None` ("no name"), and refuse anything out of bounds with the reason.
///
/// For the Settings -> Profile and signup fields, so the same rule the
/// envelope enforces is shown as the user types — no client re-implements
/// the control / format-character table.
///
/// # Errors
///
/// `DisplayNameLineBreak` / `DisplayNameControlOrFormatCharacter` /
/// `DisplayNameTooLong` / `DisplayNameTooManyBytes`.
#[uniffi::export]
pub fn normalize_display_name(input: String) -> Result<Option<String>, FfiError> {
    rust_normalize_display_name(&input).map_err(Into::into)
}

// ============================================================================
// Shared-pin envelope — ADR-0052 decision 3
// ============================================================================

/// One shared-pin operation on one message (ADR-0052 decision 3). Mirror of
/// [`lakko_contacts::pin::PinOp`].
///
/// **A pin received from someone else is that person's assertion about the
/// whole conversation, and the sender's authority is the RECEIVER's check**:
/// resolve the MLS sender leaf to the account key at receipt (C-68), read the
/// live role table, accept only from the owner or an admin (any member when
/// the group carries no role table; either participant of a one-to-one), then
/// keep it only when [`pin_is_newer`] than the stored stamp **for that
/// message** — the whole read-decide-write under the per-group lock (C-62
/// sub-invariant 5). Persist the stamp and re-emit it VERBATIM on a re-send.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiPinOp {
    /// DS `messageId` of the message in this conversation.
    pub message_id: i64,
    /// `true` = pinned for everyone; `false` = no longer pinned.
    pub pinned: bool,
    /// Sender-clock UNIX milliseconds at which the action was taken.
    pub updated_at_ms: u64,
}

impl From<RustPinOp> for FfiPinOp {
    fn from(op: RustPinOp) -> Self {
        Self {
            message_id: op.message_id,
            pinned: matches!(op.action, RustPinAction::Pin),
            updated_at_ms: op.updated_at_ms,
        }
    }
}

impl From<FfiPinOp> for RustPinOp {
    fn from(op: FfiPinOp) -> Self {
        Self {
            message_id: op.message_id,
            action: if op.pinned {
                RustPinAction::Pin
            } else {
                RustPinAction::Unpin
            },
            updated_at_ms: op.updated_at_ms,
        }
    }
}

/// Compose a shared-pin envelope (ADR-0052 decision 3) — the inner plaintext
/// to publish as an MLS application message on the conversation's journal.
/// Infallible: the envelope has no variable field.
#[uniffi::export]
#[must_use]
pub fn compose_pin_envelope(op: FfiPinOp) -> Vec<u8> {
    rust_compose_pin(&op.into())
}

/// Strictly decode a shared-pin envelope (ADR-0052 decision 3). Fail-closed:
/// a plaintext without the pin domain separator, of the wrong length, with an
/// unknown version or an unknown action is refused as a whole.
///
/// Use [`dispatch_incoming_envelope`] when the plaintext might be any
/// in-group envelope kind.
///
/// # Errors
///
/// * [`FfiError::Contacts`] for a wrong domain / bad length / unknown
///   version / unknown action.
#[uniffi::export]
pub fn decode_pin_envelope(plaintext: Vec<u8>) -> Result<FfiPinOp, FfiError> {
    rust_decode_pin(&plaintext)
        .map(Into::into)
        .map_err(|e| FfiError::Contacts(e.to_string()))
}

/// `true` when a pin action stamped `incoming_updated_at_ms` must replace the
/// stored action **for the same message**. Strictly greater wins; equal is
/// not newer, so a re-send of the stored action is not an update.
#[uniffi::export]
#[must_use]
pub fn pin_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_pin_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

// ============================================================================
// Group-name envelope — ADR-0051 §1
// ============================================================================

/// A group's shared name (ADR-0051). Mirror of
/// [`lakko_contacts::group_name::GroupName`].
///
/// **A name received from someone else is that person's assertion about the
/// whole group, and the sender's authority is the RECEIVER's check** (ADR-0051
/// §2): resolve the MLS sender leaf to the account key at receipt (C-68), read
/// the live role table, accept only from the owner or an admin (or from any
/// member when the group carries no role table), then keep it only when
/// [`group_name_is_newer`] than the stored stamp for that group. Persist the
/// stamp and re-emit it VERBATIM on a growth re-send (the ADR-0044 lesson).
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiGroupName {
    /// The name. **Empty means "clear the name"** — receivers fall back to
    /// the member-derived title.
    pub name: String,
    /// Setter-clock UNIX milliseconds at which the name was set.
    pub updated_at_ms: u64,
}

impl From<RustGroupName> for FfiGroupName {
    fn from(g: RustGroupName) -> Self {
        Self {
            name: g.name,
            updated_at_ms: g.updated_at_ms,
        }
    }
}

/// Turn a caller-supplied group name into the canonical core value: trim the
/// name (blank = clear the name). Anything else out of bounds is refused here
/// with a typed variant, before any bytes exist.
fn prepare_group_name(g: FfiGroupName) -> Result<RustGroupName, FfiError> {
    Ok(RustGroupName {
        name: rust_normalize_group_name(&g.name)?,
        updated_at_ms: g.updated_at_ms,
    })
}

/// Compose a group-name envelope (ADR-0051 §1) — the inner plaintext to
/// publish as an MLS application message on the group's journal.
///
/// Prepares the name first (see [`FfiGroupName`]): it is trimmed and a blank
/// one becomes "clear the name". The envelope produced is always one a
/// receiver accepts (the profile display-name rule, reused).
///
/// # Errors
///
/// * `DisplayNameLineBreak` / `DisplayNameControlOrFormatCharacter` /
///   `DisplayNameTooLong` / `DisplayNameTooManyBytes` — the name.
#[uniffi::export]
pub fn compose_group_name_envelope(group_name: FfiGroupName) -> Result<Vec<u8>, FfiError> {
    let prepared = prepare_group_name(group_name)?;
    rust_compose_group_name(&prepared).map_err(Into::into)
}

/// Strictly decode a group-name envelope (ADR-0051 §1). Fail-closed: a
/// plaintext without the group-name domain separator, malformed, or with the
/// name out of bounds is refused as a whole.
///
/// Use [`dispatch_incoming_envelope`] when the plaintext might be any
/// in-group envelope kind.
///
/// # Errors
///
/// * [`FfiError::Contacts`] for a wrong domain / truncation / trailing bytes
///   / unknown version / non-UTF-8 name.
/// * A `DisplayName*` variant for an out-of-bounds name.
#[uniffi::export]
pub fn decode_group_name_envelope(plaintext: Vec<u8>) -> Result<FfiGroupName, FfiError> {
    rust_decode_group_name(&plaintext)
        .map(Into::into)
        .map_err(Into::into)
}

/// `true` when a group name stamped `incoming_updated_at_ms` must replace
/// the stored one **for the same group**. Strictly greater wins; equal is not
/// newer, so a growth re-send of the stored name is not an update.
#[uniffi::export]
#[must_use]
pub fn group_name_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_group_name_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
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
/// The v1 wire format is the 1.18a Rust struct verbatim. This
/// FFI surface always returns the full capability bytes, which
/// can ride directly in the URL with no server dependency.
/// Alternatively `services/discovery` content-addresses an
/// uploaded capability and serves it from
/// `GET /discovery/invite/{blob_id}` (with `max_uses`
/// enforcement on `/consume`), so the client may instead embed
/// the short `/i/<blob_id>` pointer — that is a client transport
/// choice, not a gap in this codec.
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
    /// Camera on/off media-state signal (camera-off-cleanup).
    /// The decoded flag rides the `camera_on` field of
    /// [`FfiIncomingEnvelope`]; `payload` is empty.
    MediaState,
    /// In-group push-routing-secret distribution envelope
    /// (ADR-0042, v2 per ADR-0044). The decoded 32-byte secret
    /// rides the `payload` field of [`FfiIncomingEnvelope`] and
    /// the minter's recency stamp rides `mint_epoch`;
    /// `camera_on` is `None`.
    PushRoutingSecret,
    /// Committer liveness beacon (ADR-0045 decision 2, variant 1).
    /// Carries **no** payload — `payload` is empty, `camera_on` and
    /// `mint_epoch` are `None` — because the beacon has no fields:
    /// the sender comes from the MLS sender leaf and the age from
    /// the DS row.
    ///
    /// **This variant exists so a beacon is classified, not so it is
    /// consumed.** The fold that turns beacons into an
    /// acting-committer decision
    /// (`acting_committer_verdict` / `should_emit_beacon` and their
    /// constants) is deliberately **not yet exposed** on this
    /// surface; until it is, a client that sees this kind should
    /// drop it. What the variant buys today is that a beacon can
    /// never fall through to `IntroMessage` and be rendered as a
    /// chat bubble.
    CommitterBeacon,
    /// A member's profile (ADR-0048). The decoded, bounds-checked profile
    /// rides the `profile` field of [`FfiIncomingEnvelope`]; `payload` is
    /// empty. Never render it as a chat bubble. Appended last so every
    /// earlier variant keeps its ordinal.
    ///
    /// Every field is the sender's claim: store it under the sender's
    /// signature key resolved from the MLS sender leaf at receipt (C-68),
    /// keep it only when [`profile_is_newer`], and verify the username
    /// through discovery before showing it.
    Profile,
    /// The group's shared name (ADR-0051 §1). The decoded, bounds-checked
    /// name rides the `group_name` field of [`FfiIncomingEnvelope`];
    /// `payload` is empty. Never render it as a chat bubble. Appended last
    /// so every earlier variant keeps its ordinal.
    ///
    /// The sender's authority is the receiver's check (ADR-0051 §2): resolve
    /// the sender leaf to the account key (C-68), consult the live role
    /// table (owner / admin, or any member when there is no table), then
    /// keep it only when [`group_name_is_newer`]. An empty name clears the
    /// name.
    GroupName,
    /// A member asks the group to introduce itself (ADR-0048 §2 trigger 4).
    /// `payload` is empty. Answer by publishing the own profile through the
    /// throttled growth path; never render it. Appended last so every
    /// earlier variant keeps its ordinal.
    ProfileRequest,
    /// A shared pin or unpin of one message (ADR-0052 decision 3). The
    /// decoded operation rides the `pin` field of [`FfiIncomingEnvelope`];
    /// `payload` is empty. Never render it as a chat bubble. Appended last
    /// so every earlier variant keeps its ordinal.
    ///
    /// The sender's authority is the receiver's check: resolve the sender
    /// leaf to the account key (C-68), consult the live role table (owner /
    /// admin, or any member when there is no table, or either participant
    /// of a one-to-one), then keep it only when [`pin_is_newer`] for that
    /// message.
    Pin,
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
    /// it is the original plaintext verbatim; for
    /// `PushRoutingSecret` it is the decoded 32-byte secret;
    /// for `MediaState` it is empty.
    pub payload: Vec<u8>,
    /// Set only for `MediaState`: whether the sender's camera
    /// is on. `None` for every other kind.
    pub camera_on: Option<bool>,
    /// Set only for `PushRoutingSecret` (ADR-0044): the
    /// minter's recency stamp, the primary key of the canonical
    /// total order. `None` for every other kind.
    ///
    /// The caller MUST NOT store the secret unconditionally —
    /// run the ADR-0042 / ADR-0043 role gate, then
    /// [`push_routing_secret_is_newer`] against whatever is
    /// already stored. A stale re-broadcast carries a valid
    /// secret from an authorised sender and is refused only by
    /// the order.
    pub mint_epoch: Option<u64>,
    /// Set only for `Profile` (ADR-0048): the decoded profile. `None` for
    /// every other kind.
    pub profile: Option<FfiProfile>,
    /// Set only for `GroupName` (ADR-0051): the decoded group name. `None`
    /// for every other kind.
    pub group_name: Option<FfiGroupName>,
    /// Set only for `Pin` (ADR-0052): the decoded pin operation. `None` for
    /// every other kind.
    pub pin: Option<FfiPinOp>,
}

/// Single-call dispatcher for opaque relay-payload plaintexts.
/// Runs the classification helpers in dispatch order
/// (welcome → KP-forward → media-state → intro-message
/// fallthrough) and returns the typed variant.
///
/// See [`lakko_contacts::incoming_envelope`] for the full
/// dispatch contract.
///
/// # Errors
///
/// * [`FfiError::Contacts`] when a domain separator matched
///   but the payload is malformed (below the Welcome /
///   KP-forward minimum floor, or an invalid media-state
///   payload).
#[uniffi::export]
pub fn dispatch_incoming_envelope(plaintext: Vec<u8>) -> Result<FfiIncomingEnvelope, FfiError> {
    let parsed = rust_dispatch_incoming_envelope(&plaintext)
        .map_err(|err| FfiError::Contacts(err.to_string()))?;
    let mut profile = None;
    let mut group_name = None;
    let mut pin = None;
    let (kind, payload, camera_on, mint_epoch) = match parsed {
        IncomingEnvelope::CallWelcome(p) => {
            (FfiIncomingEnvelopeKind::CallWelcome, p.to_vec(), None, None)
        },
        IncomingEnvelope::CallKeyPackage(p) => (
            FfiIncomingEnvelopeKind::CallKeyPackage,
            p.to_vec(),
            None,
            None,
        ),
        IncomingEnvelope::MediaState { camera_on } => (
            FfiIncomingEnvelopeKind::MediaState,
            Vec::new(),
            Some(camera_on),
            None,
        ),
        IncomingEnvelope::PushRoutingSecret { mint_epoch, secret } => (
            FfiIncomingEnvelopeKind::PushRoutingSecret,
            secret.to_vec(),
            None,
            Some(mint_epoch),
        ),
        IncomingEnvelope::CommitterBeacon => (
            FfiIncomingEnvelopeKind::CommitterBeacon,
            Vec::new(),
            None,
            None,
        ),
        IncomingEnvelope::Profile(p) => {
            profile = Some(p.into());
            (FfiIncomingEnvelopeKind::Profile, Vec::new(), None, None)
        },
        IncomingEnvelope::GroupName(g) => {
            group_name = Some(g.into());
            (FfiIncomingEnvelopeKind::GroupName, Vec::new(), None, None)
        },
        IncomingEnvelope::ProfileRequest => (
            FfiIncomingEnvelopeKind::ProfileRequest,
            Vec::new(),
            None,
            None,
        ),
        IncomingEnvelope::Pin(op) => {
            pin = Some(op.into());
            (FfiIncomingEnvelopeKind::Pin, Vec::new(), None, None)
        },
        // ADR-0058 envelopes are classified by the core (so they never fall
        // through to the intro-message path) but not yet surfaced on this
        // FFI; that is sprint S3. Until then they are refused, which the
        // caller treats as "drop, never render".
        IncomingEnvelope::IntroductionOffer(_)
        | IncomingEnvelope::Introductions(_)
        | IncomingEnvelope::Visibility(_)
        | IncomingEnvelope::ReachState(_) => {
            return Err(FfiError::Contacts(
                "envelope kind not yet surfaced on this FFI (ADR-0058 S3)".to_string(),
            ));
        },
        IncomingEnvelope::IntroMessage(p) => (
            FfiIncomingEnvelopeKind::IntroMessage,
            p.to_vec(),
            None,
            None,
        ),
    };
    Ok(FfiIncomingEnvelope {
        kind,
        payload,
        camera_on,
        mint_epoch,
        profile,
        group_name,
        pin,
    })
}

// ===========================================================================
// Media-state envelope — camera-off-cleanup (Phase 1)
// (camera on/off signal over the opaque relay transport)
// ===========================================================================

/// Compose a camera on/off media-state envelope
/// (camera-off-cleanup). The sender broadcasts the returned
/// bytes over the opaque SFU `signal-relay` payload; receivers
/// classify them (via [`dispatch_incoming_envelope`] or
/// [`decode_media_state`]) and reset the sender's tile `camOn`
/// state.
///
/// See [`lakko_contacts::media_state`] for the wire format and
/// threat-model rationale (reuse the opaque relay-payload
/// pattern; the SFU never parses the payload — invariant #2).
///
/// # Errors
///
/// Infallible — returns `Ok` unconditionally for shape
/// symmetry with [`decode_media_state`].
#[uniffi::export]
#[must_use]
pub fn compose_media_state(camera_on: bool) -> Vec<u8> {
    rust_compose_media_state(camera_on)
}

/// Strictly decode a media-state envelope, returning the
/// camera flag (camera-off-cleanup). Fail-closed: rejects a
/// plaintext that does not carry the media-state domain
/// separator or whose payload is malformed.
///
/// Use [`dispatch_incoming_envelope`] instead when the
/// plaintext might be any relay-payload kind (it falls through
/// to the other kinds rather than erroring).
///
/// # Errors
///
/// * [`FfiError::Contacts`] when `plaintext` is not a valid
///   media-state envelope (wrong domain / bad length / unknown
///   version / out-of-range flag byte).
#[uniffi::export]
pub fn decode_media_state(plaintext: Vec<u8>) -> Result<bool, FfiError> {
    rust_decode_media_state(&plaintext)
        .map(|state| state.camera_on)
        .map_err(|err| FfiError::Contacts(err.to_string()))
}

// ===========================================================================
// Push-routing-secret + opaque push_gid — ADR-0042
// (invariant #5 — close the push social-graph leak)
// ===========================================================================

/// Coerce a caller-supplied byte vector into the fixed 32-byte
/// `push_routing_secret` array, fail-closed on a wrong length.
fn push_routing_secret_array(secret: &[u8]) -> Result<[u8; PUSH_ROUTING_SECRET_LEN], FfiError> {
    <[u8; PUSH_ROUTING_SECRET_LEN]>::try_from(secret).map_err(|_| {
        FfiError::Contacts(format!(
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
/// * [`FfiError::Contacts`] when `secret` is not exactly 32
///   bytes.
#[uniffi::export]
pub fn derive_push_gid(secret: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let arr = push_routing_secret_array(&secret)?;
    Ok(rust_derive_push_gid(&arr).to_vec())
}

/// A decoded push-routing-secret distribution envelope
/// (ADR-0042 v2, per ADR-0044). Output of
/// [`decode_push_routing_secret`].
///
/// Field order mirrors the wire and, deliberately, the
/// [`push_routing_secret_is_newer`] total order: `mint_epoch`
/// first, then the secret bytes.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiPushRoutingSecret {
    /// The minter's recency stamp, the primary key of the
    /// canonical total order. Opaque to the core: it imposes an
    /// order and nothing else, and never reaches
    /// [`derive_push_gid`].
    pub mint_epoch: u64,
    /// The decoded 32-byte per-group routing secret.
    pub secret: Vec<u8>,
}

/// Compose the in-group push-routing-secret distribution envelope
/// (ADR-0042 v2, per ADR-0044) for a group's 32-byte
/// `push_routing_secret`, stamped with `mint_epoch`. The founder /
/// adder / rotator broadcasts the returned bytes as an MLS
/// application message on the group journal so members + joiners
/// learn the secret; receivers classify them (via
/// [`dispatch_incoming_envelope`] or [`decode_push_routing_secret`])
/// and persist the secret — never render it as a chat bubble.
///
/// `mint_epoch` is stamped **at mint time** and then travels with
/// the secret forever: a re-broadcast MUST replay the epoch the
/// secret was minted under, never a fresh one. Re-stamping on
/// re-broadcast would make a stale secret out-rank the live one and
/// reintroduce the F-A replay (G-36 residual 9) this envelope
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
/// * [`FfiError::Contacts`] when `secret` is not exactly 32
///   bytes.
#[uniffi::export]
pub fn compose_push_routing_secret(mint_epoch: u64, secret: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let arr = push_routing_secret_array(&secret)?;
    Ok(rust_compose_push_routing_secret(mint_epoch, &arr))
}

/// Strictly decode a push-routing-secret distribution envelope
/// (ADR-0042 v2, per ADR-0044), returning the mint epoch + the
/// 32-byte secret. Fail-closed: rejects a plaintext that does not
/// carry the push-routing-secret domain separator or whose payload
/// is malformed — including a retired v1 envelope, which carries no
/// `mint_epoch` and so cannot be placed in the total order.
///
/// Use [`dispatch_incoming_envelope`] instead when the plaintext
/// might be any relay-payload / in-group envelope kind (it falls
/// through to the other kinds rather than erroring).
///
/// # Errors
///
/// * [`FfiError::Contacts`] when `plaintext` is not a valid
///   push-routing-secret envelope (wrong domain / bad length /
///   unknown version — a v1 envelope lands here).
#[uniffi::export]
pub fn decode_push_routing_secret(plaintext: Vec<u8>) -> Result<FfiPushRoutingSecret, FfiError> {
    rust_decode_push_routing_secret(&plaintext)
        .map(|envelope| FfiPushRoutingSecret {
            mint_epoch: envelope.mint_epoch,
            secret: envelope.secret.to_vec(),
        })
        .map_err(|err| FfiError::Contacts(err.to_string()))
}

/// The canonical total order on push-routing secrets (ADR-0044):
/// `true` when the incoming envelope must supersede the stored one.
///
/// Ordered on the pair `(mint_epoch, secret_bytes)` — epoch first,
/// then the 32 secret bytes lexicographically — and **strictly
/// greater wins; equal is NOT newer**, which is what keeps a benign
/// retry of the secret a device already holds from reading as an
/// update.
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
/// never displaces a stored one and a corrupt stored value is left
/// in place rather than overwritten. Never panics; infallible by
/// construction, hence no `Result`.
///
/// See [`lakko_contacts::push_routing::push_routing_secret_is_newer`].
#[uniffi::export]
#[must_use]
pub fn push_routing_secret_is_newer(
    incoming_epoch: u64,
    incoming_secret: Vec<u8>,
    stored_epoch: u64,
    stored_secret: Vec<u8>,
) -> bool {
    rust_push_routing_secret_is_newer(
        incoming_epoch,
        &incoming_secret,
        stored_epoch,
        &stored_secret,
    )
}

// ============================================================================
// Committer liveness beacon — ADR-0045 decision 2, variant 1
// ============================================================================

/// One observed committer beacon, reduced to the only two things
/// [`acting_committer_verdict`] reads. Mirror of
/// [`lakko_contacts::committer_beacon::BeaconObservation`].
///
/// **Both fields are the observer's own re-derivation, never anything the
/// beacon claimed.** The beacon payload is field-less: the sender comes from
/// the MLS sender leaf and the age from the DS row's server-assigned
/// `created_at`, so a sender can neither name itself nor assert its own
/// freshness.
#[derive(uniffi::Record, Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeaconObservation {
    /// The committer rank the observer re-derived for this beacon's
    /// MLS-authenticated sender, or `None` when that sender is not a candidate
    /// at all (a plain member, or a leaf that no longer resolves).
    ///
    /// **Derive it with `Group.committerRank(signatureKey)` at the moment the
    /// verdict is taken, never at the moment the beacon arrived.** A rank is a
    /// *position* in the role table's canonical order — `0` for the owner,
    /// `k + 1` for the `k`-th admin — so promoting, demoting or removing any
    /// admin renumbers every admin that sorts after it. A rank captured under
    /// one role table therefore cannot be compared against a rank taken under
    /// another, and [`acting_committer_verdict`] compares each observation
    /// against this device's own rank, which is necessarily current. Persist
    /// the sender's **32-byte signature key** alongside the row timestamp and
    /// rank it here; do not persist a leaf index, which OpenMLS reuses for the
    /// next member added.
    pub rank: Option<u32>,
    /// Age of the beacon's DS row in milliseconds — `now - created_at`, on the
    /// **server-assigned** `TIMESTAMP(3)` every member reads identically on the
    /// identical row. The sender's own clock never enters this.
    ///
    /// Clamp a row that appears to come from the future to `0`; never wrap.
    /// That is also the safe direction: an age that reads too fresh makes this
    /// device defer and the request wait a little longer, while an age that
    /// reads too stale makes it act and risks a double sweep.
    pub age_ms: u64,
}

impl From<BeaconObservation> for RustBeaconObservation {
    fn from(o: BeaconObservation) -> Self {
        Self {
            rank: o.rank,
            age_ms: o.age_ms,
        }
    }
}

/// The fold's answer to *am I the acting committer right now?* Mirror of
/// [`lakko_contacts::committer_beacon::ActingCommitter`].
///
/// A named verdict rather than a `bool` so a caller cannot collapse "not a
/// candidate" and "deferring to someone better placed" into one branch — they
/// are different situations with different follow-ups.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActingCommitter {
    /// This device is the acting committer: it is a candidate and has heard no
    /// fresh beacon from any lower-ranked candidate — **including when it has
    /// heard nothing at all**, which means nobody better placed is running.
    ///
    /// Still only a licence to *attempt* the sweep. ADR-0045's stand-down
    /// journal re-read and the staged-commit rollback
    /// (`stagePendingProposals` / `mergeStagedSweep` / `abandonStagedSweep`)
    /// both remain required: two candidates can still both reach this verdict,
    /// because each folds over its own poll-timed view.
    Act,
    /// A lower-ranked candidate beaconed inside the freshness window, so this
    /// device stands down and re-evaluates on its next poll.
    Defer {
        /// The lowest rank observed active. Purely informational — the decision
        /// is already made — but it is what lets a log line answer *deferring
        /// to whom*.
        to_rank: u32,
    },
    /// This device is not an owner or admin, so it can never sweep. Distinct
    /// from [`ActingCommitter::Defer`]: there is nothing to re-evaluate, and no
    /// beacon this device should be emitting.
    NotACandidate,
}

impl From<RustActingCommitter> for ActingCommitter {
    fn from(v: RustActingCommitter) -> Self {
        match v {
            RustActingCommitter::Act => Self::Act,
            RustActingCommitter::Defer { to_rank } => Self::Defer { to_rank },
            RustActingCommitter::NotACandidate => Self::NotACandidate,
        }
    }
}

/// Compose a committer-liveness beacon (ADR-0045 decision 2, variant 1).
///
/// The returned bytes are the **inner** plaintext handed to the MLS
/// application-message send path, which seals them before they reach the DS.
/// A fixed 27 bytes, identical on every call and for every sender, because the
/// beacon carries no fields at all: naming the sender would add an
/// unauthenticated identity claim next to the MLS-authenticated one, a
/// self-reported timestamp would let a candidate hold the committer duty by
/// asserting its own freshness, and a self-reported rank is one every correct
/// receiver must ignore.
///
/// Gate the call on [`should_emit_beacon`], and only from a **foregrounded**
/// client that is actually polling — a beacon means *able to sweep*, and the
/// ADR-0041 background decrypt structurally cannot commit (C-59), so a beacon
/// emitted from the background is a lie that costs the group a takeover.
#[uniffi::export]
#[must_use]
pub fn compose_committer_beacon() -> Vec<u8> {
    rust_compose_committer_beacon()
}

/// Strictly decode a committer-liveness beacon (ADR-0045). Fail-closed:
/// rejects a plaintext that does not carry the beacon domain separator, or
/// whose payload is the wrong length or an unknown schema version.
///
/// **Returning without throwing is the entire result.** There is deliberately
/// no payload to hand back — everything a receiver needs (who sent it, how old
/// it is) comes from the MLS sender leaf and the DS row, not from the bytes.
///
/// Use [`dispatch_incoming_envelope`] instead when the plaintext might be any
/// relay-payload / in-group envelope kind; it classifies a beacon as
/// [`FfiIncomingEnvelopeKind::CommitterBeacon`] and falls through to the other
/// kinds rather than erroring.
///
/// # Errors
///
/// * [`FfiError::Contacts`] when `plaintext` is not a valid committer beacon
///   (wrong domain / bad length / unknown version).
#[uniffi::export]
pub fn decode_committer_beacon(plaintext: Vec<u8>) -> Result<(), FfiError> {
    rust_decode_committer_beacon(&plaintext)
        .map(|_| ())
        .map_err(|err| FfiError::Contacts(err.to_string()))
}

/// ADR-0048 §2 trigger 4 — the newcomer's "introduce yourselves" envelope.
/// Publish it as an MLS application message into the group right after
/// joining, and on opening a group with an unnamed member when
/// [`should_request_profiles`] says so. Field-less; byte-identical every
/// call.
#[uniffi::export]
#[must_use]
pub fn compose_profile_request() -> Vec<u8> {
    rust_compose_profile_request()
}

/// Recognise a profile request. `Ok(())` for a well-formed one, an error
/// for anything else (a foreign domain or a malformed payload). The
/// dispatcher already classifies it as
/// [`FfiIncomingEnvelopeKind::ProfileRequest`]; this is the standalone
/// check for a receive path that peeks before dispatching.
#[uniffi::export]
pub fn decode_profile_request(plaintext: Vec<u8>) -> Result<(), FfiError> {
    rust_decode_profile_request(&plaintext)
        .map(|_| ())
        .map_err(|err| FfiError::Contacts(err.to_string()))
}

/// Length of the profile-request wire, for a cheap pre-filter.
#[uniffi::export]
#[must_use]
pub fn profile_request_envelope_len() -> u32 {
    u32::try_from(PROFILE_REQUEST_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

/// The daily floor between two requests to one group outside a join.
#[uniffi::export]
#[must_use]
pub const fn profile_request_min_interval_ms() -> u64 {
    PROFILE_REQUEST_MIN_INTERVAL_MS
}

/// Should this device ask the group to introduce itself now? See
/// `lakko_contacts::profile_request::should_request_profiles`: always on a
/// join; otherwise only with an unnamed member and no sooner than
/// [`profile_request_min_interval_ms`] after the last request. `None` for
/// `last_requested_at_ms` means never asked.
#[uniffi::export]
#[must_use]
pub fn should_request_profiles(
    joined_just_now: bool,
    unnamed_members: u32,
    last_requested_at_ms: Option<u64>,
    now_ms: u64,
) -> bool {
    rust_should_request_profiles(
        joined_just_now,
        unnamed_members as usize,
        last_requested_at_ms,
        now_ms,
    )
}

/// The fold: decide whether this device is the acting committer (ADR-0045
/// decision 2, variant 1).
///
/// Pure, total and clock-free — the caller supplies `now` implicitly, as
/// already-computed ages. Exposed on the FFI precisely so no client
/// reimplements the rules: three platforms each re-deriving them is the drift
/// class that produced ADR-0044's F-A replay.
///
/// The rules, in the order they apply:
///
/// 1. `own_rank == None` is [`ActingCommitter::NotACandidate`], decided without
///    reading a single beacon.
/// 2. Rank `0` — the owner — is [`ActingCommitter::Act`] immediately; no rank
///    is lower, so no beacon could outrank it and none is consulted.
/// 3. An observation whose `rank` is `None` is ignored (its sender is not a
///    candidate).
/// 4. An observation older than `freshness_window_ms` is ignored.
/// 5. Only a **strictly** lower rank defers this device — equal rank never
///    does, which is what makes a device's own echoed-back beacon harmless.
/// 6. Surviving all of that with nothing left means [`ActingCommitter::Act`]:
///    hearing nothing is the correct answer when nobody better placed is
///    running, not a failure to elect.
///
/// **This does not close the race.** Each candidate folds over its own
/// poll-timed view and a beacon is stale by up to one
/// [`committer_beacon_interval_ms`] even when its sender is perfectly healthy,
/// so two candidates can both reach [`ActingCommitter::Act`]. ADR-0045's
/// stand-down journal re-read and its staged-commit rollback both remain
/// required; deleting either on the strength of this verdict is a mis-reading.
///
/// Pass [`committer_beacon_freshness_window_ms`] for `freshness_window_ms`
/// unless there is a measured reason not to — the interval and the window are
/// two halves of one budget.
#[uniffi::export]
#[must_use]
pub fn acting_committer_verdict(
    own_rank: Option<u32>,
    observed: Vec<BeaconObservation>,
    freshness_window_ms: u64,
) -> ActingCommitter {
    let rust_observed: Vec<RustBeaconObservation> = observed.into_iter().map(Into::into).collect();
    rust_acting_committer_verdict(own_rank, &rust_observed, freshness_window_ms).into()
}

/// The send-side throttle: should this device emit a beacon now (ADR-0045)?
///
/// 1. **Only candidates emit** — `own_rank == None` is `false`. A plain
///    member's beacon can never make it the acting committer (the fold drops
///    it), so emitting one is pure journal litter and a pure privacy leak.
/// 2. **The first beacon of a session is immediate** — `last_emitted_at_ms ==
///    None` is `true`, so a candidate that has just opened the app announces
///    itself at once rather than staying invisible for a whole interval, which
///    is exactly when a stale view is likeliest to hand the duty elsewhere.
/// 3. Otherwise, one per [`committer_beacon_interval_ms`].
///
/// Both timestamps come from this device's **own local** clock, which is
/// legitimate here and nowhere else in the beacon design: it governs only this
/// device's send cadence and feeds no judgement about anybody else. A backward
/// clock jump suppresses one beacon and self-heals on the next tick.
///
/// **Foregroundedness is a caller obligation this function cannot check.** See
/// [`compose_committer_beacon`].
#[uniffi::export]
#[must_use]
pub fn should_emit_beacon(
    own_rank: Option<u32>,
    last_emitted_at_ms: Option<u64>,
    now_ms: u64,
) -> bool {
    rust_should_emit_beacon(own_rank, last_emitted_at_ms, now_ms)
}

/// How often a candidate re-emits its beacon, in milliseconds (ADR-0045).
///
/// Exposed rather than hand-copied: this and
/// [`committer_beacon_freshness_window_ms`] are two halves of one budget, and a
/// client that re-derives its emit cadence locally can silently fall out of
/// step with the window every other client is judging it against.
///
/// **The number is CHOSEN, not measured.** It is sized for journal litter and
/// for privacy — every beacon is a journal row that persists to the 30-day
/// prune and publishes that this candidate had the app foregrounded at that
/// moment — not for responsiveness, because nothing interactive waits on it.
/// Takeover latency is bounded by the freshness window, not by this.
#[uniffi::export]
#[must_use]
pub const fn committer_beacon_interval_ms() -> u64 {
    COMMITTER_BEACON_INTERVAL_MS
}

/// The slowest group-poll cadence in the client tree, in milliseconds — a term
/// in the freshness window's construction (ADR-0045).
///
/// Exposed because it is a **constraint on the caller**, not decoration: a
/// client that polls its group journal slower than this invalidates the
/// window's derivation, so the cadence has to be checkable against one shared
/// number instead of against a comment.
#[uniffi::export]
#[must_use]
pub const fn committer_beacon_worst_case_poll_ms() -> u64 {
    COMMITTER_BEACON_WORST_CASE_POLL_MS
}

/// How long a beacon keeps counting its sender as an active candidate, in
/// milliseconds, measured against the **DS row's** age (ADR-0045). Pass this to
/// [`acting_committer_verdict`].
///
/// It is `2 * interval + worst_case_poll`, and each term earns its place: one
/// interval because a perfectly healthy candidate's most recent beacon is
/// already that old the moment anyone looks; a second to tolerate exactly one
/// lost or delayed beacon, so a single dropped journal round does not evict a
/// live candidate; and one poll cadence because a beacon is only *observed* at
/// the observer's next poll.
#[uniffi::export]
#[must_use]
pub const fn committer_beacon_freshness_window_ms() -> u64 {
    COMMITTER_BEACON_FRESHNESS_WINDOW_MS
}

/// Exact byte length of a composed committer beacon (ADR-0045) — the domain
/// separator plus the one-byte payload.
///
/// Exposed for the cheap length pre-filter a receive path runs before the
/// authoritative [`decode_committer_beacon`], so that length is not hardcoded
/// on three platforms. Because the beacon is field-less, every beacon is
/// byte-identical, so an exact-equality pre-filter against
/// [`compose_committer_beacon`] is also correct.
#[uniffi::export]
#[must_use]
pub fn committer_beacon_envelope_len() -> u32 {
    u32::try_from(COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.len() + COMMITTER_BEACON_PAYLOAD_LEN)
        .unwrap_or(u32::MAX)
}

// ============================================================================
// GroupInviteCapability — ADR-0028 (group join-ticket, G-3)
// ============================================================================

/// Owner-side: compose + sign a group join-ticket capability (ADR-0028).
///
/// `issuer_signing_seed` is the 32-byte Ed25519 seed of the owner/admin minting
/// the link; `group_id` is the target group's opaque MLS `GroupId` bytes;
/// `expiry_ms` is UNIX millis (`0` = never expires). Returns the wire bytes the
/// client base64url-encodes into the `/g/<slug>` link.
///
/// # Errors
///
/// [`FfiError::Contacts`] for a wrong-length seed, an empty / oversized
/// `group_id`, or a compose failure.
#[uniffi::export]
pub fn compose_group_invite_capability(
    issuer_signing_seed: Vec<u8>,
    group_id: Vec<u8>,
    expiry_ms: u64,
) -> Result<Vec<u8>, FfiError> {
    if issuer_signing_seed.len() != 32 {
        return Err(FfiError::Contacts(
            "issuerSigningSeed must be 32 bytes".to_string(),
        ));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&issuer_signing_seed);
    let signing = SigningKey::from_seed(&seed);
    if group_id.is_empty() || group_id.len() > MAX_GROUP_ID_LEN {
        return Err(FfiError::Contacts(format!(
            "groupId must be 1..={MAX_GROUP_ID_LEN} bytes",
        )));
    }
    Ok(rust_group_invite_compose(&signing, &group_id, expiry_ms)?)
}

/// Owner-side: sign a group-invite revocation (ADR-0028 revoke). Returns the
/// 64-byte Ed25519 signature over the group-invite revoke canonical bytes
/// (`DELETE /discovery/group-invite/{blob_id}` body). `issuer_signing_seed` is
/// the 32-byte Ed25519 seed that minted the link; `blob_id` is the
/// [`BLOB_ID_LEN`]-byte (6) id the discovery upload returned.
///
/// # Errors
///
/// [`FfiError::Contacts`] for a wrong-length seed or `blob_id`.
#[uniffi::export]
pub fn sign_group_invite_revocation(
    issuer_signing_seed: Vec<u8>,
    blob_id: Vec<u8>,
    signed_at_ms: u64,
) -> Result<Vec<u8>, FfiError> {
    if issuer_signing_seed.len() != 32 {
        return Err(FfiError::Contacts(
            "issuerSigningSeed must be 32 bytes".to_string(),
        ));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&issuer_signing_seed);
    let signing = SigningKey::from_seed(&seed);
    if blob_id.len() != BLOB_ID_LEN {
        return Err(FfiError::Contacts(format!(
            "blobId must be {BLOB_ID_LEN} bytes",
        )));
    }
    let mut blob_id_arr = [0u8; BLOB_ID_LEN];
    blob_id_arr.copy_from_slice(&blob_id);
    Ok(rust_group_sign_revocation(&signing, &blob_id_arr, signed_at_ms).to_vec())
}

/// Width in bytes of a discovery-ledger id (`/i/`, `/c/`, `/g/` pointer
/// links): 6. The server mints it; a client only carries it (ADR-0028
/// short-link amendment, 2026-09-27).
#[uniffi::export]
#[must_use]
pub fn invite_blob_id_len() -> u32 {
    u32::try_from(BLOB_ID_LEN).unwrap_or(u32::MAX)
}

/// Length of a ledger id in a link: base64url without padding, 8 characters.
/// A pointer path segment of any other length is not a pointer id.
#[uniffi::export]
#[must_use]
pub fn invite_blob_id_url_len() -> u32 {
    u32::try_from(BLOB_ID_B64URL_LEN).unwrap_or(u32::MAX)
}

/// Width in bytes of a `/g/` link secret (the URL fragment): 12.
#[uniffi::export]
#[must_use]
pub fn group_invite_link_secret_len() -> u32 {
    u32::try_from(GROUP_INVITE_LINK_SECRET_LEN).unwrap_or(u32::MAX)
}

/// Length of a `/g/` link secret in the URL fragment: base64url without
/// padding, 16 characters. Any other fragment length is refused.
#[uniffi::export]
#[must_use]
pub fn group_invite_link_secret_url_len() -> u32 {
    u32::try_from(GROUP_INVITE_LINK_SECRET_B64URL_LEN).unwrap_or(u32::MAX)
}

/// Composed + verified group join-ticket capability (ADR-0028). Mirror of
/// [`lakko_contacts::GroupInviteCapability`].
/// The preview a `/g/` link shows before the person asks to join (ADR-0028
/// v2): the ISSUER's claim at mint time, sealed under a key derived from the
/// link secret that rides in the URL fragment. Names are optional; the count is a snapshot.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct GroupInvitePreview {
    pub group_name: Option<String>,
    pub inviter_name: Option<String>,
    pub member_count: u16,
}

/// What `compose_group_invite_capability_with_preview` mints: the signed
/// capability bytes (the slug) and the fresh link secret
/// ([`group_invite_link_secret_len`] = 12 bytes, 16 base64url characters) the
/// caller puts in the URL fragment and nowhere a server reads. The field keeps
/// its `link_key` name; it carries the secret the preview key is derived from.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct GroupInviteMint {
    pub wire: Vec<u8>,
    pub link_key: Vec<u8>,
}

/// Mint a `/g/` capability that carries a sealed preview (ADR-0028 v2). Names
/// are prepared like a profile / group name (trimmed; blank = none) and
/// refused past the display-name rule. `member_count` is what the issuer's
/// roster says now.
///
/// # Errors
///
/// * [`FfiError::Contacts`] for a bad seed / group id, or a name the rule
///   refuses.
#[uniffi::export]
pub fn compose_group_invite_capability_with_preview(
    issuer_signing_seed: Vec<u8>,
    group_id: Vec<u8>,
    expiry_ms: u64,
    group_name: Option<String>,
    inviter_name: Option<String>,
    member_count: u16,
) -> Result<GroupInviteMint, FfiError> {
    if issuer_signing_seed.len() != 32 {
        return Err(FfiError::Contacts(
            "issuerSigningSeed must be 32 bytes".to_string(),
        ));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&issuer_signing_seed);
    let signing = SigningKey::from_seed(&seed);
    if group_id.is_empty() || group_id.len() > MAX_GROUP_ID_LEN {
        return Err(FfiError::Contacts(format!(
            "groupId must be 1..={MAX_GROUP_ID_LEN} bytes",
        )));
    }
    let preview = RustGroupInvitePreview {
        group_name: rust_normalize_display_name(group_name.as_deref().unwrap_or(""))?,
        inviter_name: rust_normalize_display_name(inviter_name.as_deref().unwrap_or(""))?,
        member_count,
    };
    let link_key = rust_generate_link_key();
    let wire = rust_group_invite_compose_with_preview(
        &signing, &group_id, expiry_ms, &preview, &link_key,
    )?;
    Ok(GroupInviteMint {
        wire,
        link_key: link_key.to_vec(),
    })
}

#[derive(uniffi::Object)]
pub struct GroupInviteCapability {
    pub(crate) inner: RustGroupInviteCapability,
}

#[uniffi::export]
impl GroupInviteCapability {
    /// Joiner-side: decode wire bytes, verify the Ed25519 signature, return the
    /// validated capability. Does **not** check expiry — call
    /// [`Self::is_valid_now`] separately.
    ///
    /// # Errors
    ///
    /// [`FfiError::Contacts`] on a malformed / forged / truncated link.
    #[uniffi::constructor]
    pub fn decode_and_verify(wire: Vec<u8>) -> Result<Self, FfiError> {
        let inner = rust_group_invite_decode(&wire)?;
        Ok(Self { inner })
    }

    /// The target group's opaque MLS `GroupId` bytes — the joiner passes these
    /// to `Identity.createExternalJoinProposal`.
    pub fn group_id(&self) -> Vec<u8> {
        self.inner.group_id.clone()
    }

    /// The issuer's long-term Ed25519 verifying key (32 bytes).
    pub fn issuer_signature_pubkey(&self) -> Vec<u8> {
        self.inner.issuer_signature_pubkey.to_bytes().to_vec()
    }

    /// UNIX milliseconds when the link stops being valid. `0` = never expires.
    pub fn expiry_ms(&self) -> u64 {
        self.inner.expiry_ms
    }

    /// `true` iff the link has not expired at `now_ms`.
    pub fn is_valid_now(&self, now_ms: u64) -> bool {
        self.inner.is_valid_now(now_ms)
    }

    /// Signal-style anti-phishing fingerprint of the issuer's key.
    pub fn issuer_fingerprint(&self) -> String {
        self.inner.issuer_fingerprint()
    }

    /// Whether the link carries a sealed preview at all (a v2 link minted
    /// without one, or an older client's, shows the fingerprint only).
    pub fn has_preview(&self) -> bool {
        !self.inner.sealed_preview.is_empty()
    }

    /// Open the sealed preview with the link secret from the URL fragment
    /// (the 12 bytes the 16-character fragment decodes to). `None` when the
    /// link carries no preview. A wrong secret, a tampered preview, or a
    /// capability other than the one the secret was minted with (the derived
    /// key binds the group id and issuer) is an error; the caller shows the
    /// page without a preview, never a guess.
    ///
    /// # Errors
    ///
    /// * [`FfiError::Contacts`] when `link_key` is not
    ///   [`group_invite_link_secret_len`] (12) bytes - the pre-amendment
    ///   32-byte key included - or the preview does not open under it.
    pub fn open_preview(&self, link_key: Vec<u8>) -> Result<Option<GroupInvitePreview>, FfiError> {
        if link_key.len() != GROUP_INVITE_LINK_SECRET_LEN {
            return Err(FfiError::Contacts(format!(
                "linkKey must be {GROUP_INVITE_LINK_SECRET_LEN} bytes",
            )));
        }
        let mut key = [0u8; GROUP_INVITE_LINK_SECRET_LEN];
        key.copy_from_slice(&link_key);
        Ok(self.inner.open_preview(&key)?.map(|p| GroupInvitePreview {
            group_name: p.group_name,
            inviter_name: p.inviter_name,
            member_count: p.member_count,
        }))
    }
}

// ============================================================================
// GroupJoinAttribution — ADR-0028 OQ2 (issuer-in-roster surfacing, G-3)
// ============================================================================

/// Decoded join-attribution record (ADR-0028 OQ2): which member's `/g/` link a
/// join request arrived on. Produced by [`parse_group_join_attribution`] from a
/// group-journal envelope; the admin matches `joiner_signature_key` to a pending
/// external-join request and surfaces `issuer_fingerprint` + whether
/// `issuer_signature_pubkey` is in the group roster.
#[derive(uniffi::Record)]
pub struct GroupJoinAttribution {
    /// The joiner's MLS leaf signature key — the rendezvous tag matching the
    /// pending external-join request.
    pub joiner_signature_key: Vec<u8>,
    /// The target group's opaque MLS `GroupId` bytes — the admin checks this
    /// equals its own group before trusting the attribution.
    pub group_id: Vec<u8>,
    /// The issuer's Ed25519 verifying key (32 bytes) — the member who minted
    /// the link. The admin checks this is in the roster (OQ2).
    pub issuer_signature_pubkey: Vec<u8>,
    /// Signal-style anti-phishing fingerprint of the issuer's key.
    pub issuer_fingerprint: String,
    /// UNIX milliseconds when the link stops being valid. `0` = never expires.
    pub expiry_ms: u64,
}

/// Joiner-side: compose + self-sign the attribution envelope binding the
/// joiner's `GroupInviteCapability` to its MLS leaf signature key (ADR-0028
/// OQ2). `joiner_signing_seed` is the joiner's 32-byte Ed25519 identity seed
/// (`Identity.ed25519Seed`); the envelope embeds the matching verifying key and
/// is signed with it, so the admin can verify the attribution is authentic. The
/// joiner posts this to the group journal alongside its external-join proposal.
///
/// # Errors
///
/// [`FfiError::Contacts`] when the seed is not 32 bytes or the capability is
/// empty / exceeds its bound.
#[uniffi::export]
pub fn compose_group_join_attribution(
    joiner_signing_seed: Vec<u8>,
    capability_bytes: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    Ok(rust_group_join_attribution_compose(
        &joiner_signing_seed,
        &capability_bytes,
    )?)
}

/// Admin-side: decode + verify an attribution envelope from the group journal
/// (ADR-0028 OQ2). The embedded capability is re-verified (Ed25519); a malformed
/// or forged envelope is rejected.
///
/// # Errors
///
/// [`FfiError::Contacts`] on a malformed envelope or a capability whose
/// signature does not verify.
#[uniffi::export]
pub fn parse_group_join_attribution(bytes: Vec<u8>) -> Result<GroupJoinAttribution, FfiError> {
    let inner = rust_group_join_attribution_decode(&bytes)?;
    Ok(GroupJoinAttribution {
        joiner_signature_key: inner.joiner_signature_key,
        group_id: inner.capability.group_id.clone(),
        issuer_signature_pubkey: inner.capability.issuer_signature_pubkey.to_bytes().to_vec(),
        issuer_fingerprint: inner.capability.issuer_fingerprint(),
        expiry_ms: inner.capability.expiry_ms,
    })
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
            None,   // profile (ADR-0048)
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
        let opened = req
            .open_envelope(bob_priv.to_bytes().to_vec())
            .expect("open");
        assert_eq!(opened.profile, None);
        assert_eq!(opened.intro_message, intro);
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
            None,   // profile (ADR-0048)
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
            None,   // profile (ADR-0048)
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
            vec![0u8; 32], // wrong: the pre-amendment width; BLOB_ID_LEN is 6
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
            vec![0u8; 32], // wrong: the pre-amendment width; BLOB_ID_LEN is 6
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
            None,   // profile (ADR-0048)
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
            None, // profile (ADR-0048)
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

    // -------------------------------------------------------------------
    // Media-state envelope (camera-off-cleanup)
    // -------------------------------------------------------------------

    #[test]
    fn media_state_compose_then_decode_round_trips() {
        for camera_on in [true, false] {
            let wrapped = compose_media_state(camera_on);
            let decoded = decode_media_state(wrapped).expect("decode should succeed");
            assert_eq!(decoded, camera_on);
        }
    }

    #[test]
    fn decode_media_state_rejects_non_media_state_plaintext() {
        let intro = b"hello alice".to_vec();
        match decode_media_state(intro) {
            Err(FfiError::Contacts(_)) => {},
            other => panic!("expected Contacts error, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_routes_media_state_envelope() {
        for camera_on in [true, false] {
            let wrapped = compose_media_state(camera_on);
            let result = dispatch_incoming_envelope(wrapped).expect("dispatch should succeed");
            assert_eq!(result.kind, FfiIncomingEnvelopeKind::MediaState);
            assert_eq!(result.camera_on, Some(camera_on));
            assert!(result.payload.is_empty());
        }
    }

    #[test]
    fn dispatch_media_state_field_is_none_for_other_kinds() {
        let welcome = wrap_call_welcome(vec![0xABu8; 256]);
        let result = dispatch_incoming_envelope(welcome).expect("dispatch should succeed");
        assert_eq!(result.kind, FfiIncomingEnvelopeKind::CallWelcome);
        assert_eq!(result.camera_on, None);
    }

    // -------------------------------------------------------------------
    // Push-routing-secret + push_gid (ADR-0042)
    // -------------------------------------------------------------------

    #[test]
    fn push_routing_compose_then_decode_round_trips() {
        let secret = vec![0x42u8; 32];
        let wrapped =
            compose_push_routing_secret(7, secret.clone()).expect("compose should succeed");
        let decoded = decode_push_routing_secret(wrapped).expect("decode should succeed");
        assert_eq!(decoded.secret, secret);
        assert_eq!(decoded.mint_epoch, 7);
    }

    #[test]
    fn push_routing_round_trips_at_the_mint_epoch_extremes() {
        // The 8-byte big-endian field must survive the FFI
        // boundary un-truncated — a `u64` narrowed to a `u32`
        // anywhere on a platform binding would silently collapse
        // distant epochs onto each other and break the order.
        let secret = vec![0x42u8; 32];
        for epoch in [0u64, 1, u64::from(u32::MAX), u64::MAX - 1, u64::MAX] {
            let wrapped =
                compose_push_routing_secret(epoch, secret.clone()).expect("compose should succeed");
            let decoded = decode_push_routing_secret(wrapped).expect("decode should succeed");
            assert_eq!(decoded.mint_epoch, epoch);
            assert_eq!(decoded.secret, secret);
        }
    }

    #[test]
    fn push_routing_compose_rejects_wrong_length_secret() {
        match compose_push_routing_secret(7, vec![0u8; 31]) {
            Err(FfiError::Contacts(_)) => {},
            other => panic!("expected Contacts error, got {other:?}"),
        }
    }

    #[test]
    fn derive_push_gid_is_32_bytes_and_rejects_wrong_length() {
        let gid = derive_push_gid(vec![0x42u8; 32]).expect("derive should succeed");
        assert_eq!(gid.len(), 32);
        match derive_push_gid(vec![0u8; 16]) {
            Err(FfiError::Contacts(_)) => {},
            other => panic!("expected Contacts error, got {other:?}"),
        }
    }

    #[test]
    fn decode_push_routing_secret_rejects_non_matching_plaintext() {
        match decode_push_routing_secret(b"hello alice".to_vec()) {
            Err(FfiError::Contacts(_)) => {},
            other => panic!("expected Contacts error, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_routes_push_routing_secret_envelope() {
        let secret = vec![0x42u8; 32];
        let wrapped =
            compose_push_routing_secret(9, secret.clone()).expect("compose should succeed");
        let result = dispatch_incoming_envelope(wrapped).expect("dispatch should succeed");
        assert_eq!(result.kind, FfiIncomingEnvelopeKind::PushRoutingSecret);
        assert_eq!(result.payload, secret);
        assert_eq!(result.camera_on, None);
        assert_eq!(result.mint_epoch, Some(9));
    }

    #[test]
    fn dispatch_mint_epoch_field_is_none_for_other_kinds() {
        // Mirror of `dispatch_media_state_field_is_none_for_other_kinds`
        // for the second optional variant field: `mint_epoch` is
        // meaningful ONLY on `PushRoutingSecret`, so a platform
        // reading it on any other kind must see nothing rather
        // than a stale or defaulted stamp.
        let welcome = wrap_call_welcome(vec![0xABu8; 256]);
        let result = dispatch_incoming_envelope(welcome).expect("dispatch should succeed");
        assert_eq!(result.kind, FfiIncomingEnvelopeKind::CallWelcome);
        assert_eq!(result.mint_epoch, None);

        let media = compose_media_state(true);
        let result = dispatch_incoming_envelope(media).expect("dispatch should succeed");
        assert_eq!(result.kind, FfiIncomingEnvelopeKind::MediaState);
        assert_eq!(result.mint_epoch, None);

        let intro =
            dispatch_incoming_envelope(b"hello alice".to_vec()).expect("dispatch should succeed");
        assert_eq!(intro.kind, FfiIncomingEnvelopeKind::IntroMessage);
        assert_eq!(intro.mint_epoch, None);
    }

    #[test]
    fn decode_push_routing_secret_rejects_a_retired_v1_envelope() {
        // The ADR-0044 flag day, asserted at the FFI boundary: a
        // pre-v2 peer's 62-byte envelope carries no `mint_epoch`,
        // so it cannot be placed in the total order and must be
        // refused rather than accepted with a defaulted stamp.
        let mut v1 = b"lakko/push-routing-secret/v1\x00".to_vec();
        v1.push(0x01);
        v1.extend_from_slice(&[0x42u8; 32]);
        assert_eq!(v1.len(), 62);
        match decode_push_routing_secret(v1) {
            Err(FfiError::Contacts(_)) => {},
            other => panic!("expected Contacts error, got {other:?}"),
        }
    }

    // -------------------------------------------------------------------
    // The canonical total order (ADR-0044, G-36 residual 9)
    // -------------------------------------------------------------------

    #[test]
    fn is_newer_orders_on_the_epoch_then_the_secret_bytes() {
        let low = vec![0x11u8; 32];
        let high = vec![0x99u8; 32];
        // Epoch is the primary key and dominates the tiebreak.
        assert!(push_routing_secret_is_newer(
            9,
            low.clone(),
            8,
            high.clone()
        ));
        assert!(!push_routing_secret_is_newer(
            7,
            high.clone(),
            8,
            low.clone()
        ));
        // Exact epoch tie: the secret bytes decide, lexicographically.
        assert!(push_routing_secret_is_newer(
            5,
            high.clone(),
            5,
            low.clone()
        ));
        assert!(!push_routing_secret_is_newer(5, low, 5, high));
    }

    #[test]
    fn is_newer_says_an_identical_pair_is_not_newer() {
        // THE F-A property: a benign retry of the secret a device
        // already holds must not read as an update.
        let secret = vec![0x42u8; 32];
        assert!(!push_routing_secret_is_newer(5, secret.clone(), 5, secret));
    }

    #[test]
    fn is_newer_refuses_a_stale_rebroadcast_after_a_rotation() {
        // The end-to-end F-A scenario at the FFI boundary: a device
        // re-broadcasts the pre-rotation secret S0 (epoch 10) after
        // an admin rotated to S1 (epoch 11). Asserted for BOTH
        // tiebreak directions so the result cannot hinge on which
        // secret happens to sort higher.
        let low = vec![0x11u8; 32];
        let high = vec![0x99u8; 32];
        for (older, newer) in [(&low, &high), (&high, &low)] {
            assert!(!push_routing_secret_is_newer(
                10,
                older.clone(),
                11,
                newer.clone()
            ));
            assert!(push_routing_secret_is_newer(
                11,
                newer.clone(),
                10,
                older.clone()
            ));
        }
    }

    #[test]
    fn is_newer_is_fail_closed_on_a_wrong_length_secret() {
        // Neither side may panic or displace on a malformed value.
        let good = vec![0x42u8; 32];
        assert!(!push_routing_secret_is_newer(
            99,
            vec![0u8; 31],
            1,
            good.clone()
        ));
        assert!(!push_routing_secret_is_newer(
            99,
            vec![0u8; 33],
            1,
            good.clone()
        ));
        assert!(!push_routing_secret_is_newer(
            99,
            good.clone(),
            1,
            vec![0u8; 31]
        ));
        assert!(!push_routing_secret_is_newer(99, good, 1, Vec::new()));
    }

    #[test]
    fn is_newer_agrees_with_the_decoded_envelope_pair() {
        // Anti-drift: the order a platform computes over a decoded
        // envelope must match the order over the values that were
        // composed — no field is truncated or reordered across the
        // FFI boundary.
        let low = vec![0x11u8; 32];
        let high = vec![0x99u8; 32];
        for (ae, asec) in [(1u64, &low), (1, &high), (2, &low), (u64::MAX, &high)] {
            for (be, bsec) in [(1u64, &low), (1, &high), (2, &low), (u64::MAX, &high)] {
                let a = decode_push_routing_secret(
                    compose_push_routing_secret(ae, asec.clone()).expect("compose"),
                )
                .expect("decode");
                let b = decode_push_routing_secret(
                    compose_push_routing_secret(be, bsec.clone()).expect("compose"),
                )
                .expect("decode");
                assert_eq!(
                    push_routing_secret_is_newer(
                        a.mint_epoch,
                        a.secret.clone(),
                        b.mint_epoch,
                        b.secret.clone()
                    ),
                    push_routing_secret_is_newer(ae, asec.clone(), be, bsec.clone()),
                );
            }
        }
    }

    // ===================================================================
    // Committer liveness beacon — ADR-0045 decision 2, variant 1
    // ===================================================================

    #[test]
    fn beacon_composes_and_decodes_across_the_boundary() {
        let wire = compose_committer_beacon();
        assert_eq!(
            u32::try_from(wire.len()).unwrap(),
            committer_beacon_envelope_len(),
            "the advertised envelope length must be the length actually composed",
        );
        decode_committer_beacon(wire.clone()).expect("a composed beacon must decode");
        assert_eq!(
            wire,
            compose_committer_beacon(),
            "the beacon is field-less, so every beacon is byte-identical",
        );
    }

    #[test]
    fn beacon_decode_is_fail_closed() {
        // Wrong domain — a plain chat body must not read as a beacon.
        assert!(decode_committer_beacon(b"hello".to_vec()).is_err());
        // Right domain, truncated payload.
        let mut short = compose_committer_beacon();
        short.pop();
        assert!(decode_committer_beacon(short).is_err());
        // Right domain, unknown schema version.
        let mut bad_version = compose_committer_beacon();
        let last = bad_version.len() - 1;
        bad_version[last] = 0xFF;
        assert!(decode_committer_beacon(bad_version).is_err());
        // Right domain, trailing junk.
        let mut long = compose_committer_beacon();
        long.push(0x00);
        assert!(decode_committer_beacon(long).is_err());
    }

    #[test]
    fn a_beacon_classifies_as_a_beacon_and_never_as_chat() {
        let env = dispatch_incoming_envelope(compose_committer_beacon()).expect("dispatch");
        assert_eq!(env.kind, FfiIncomingEnvelopeKind::CommitterBeacon);
        assert!(env.payload.is_empty(), "a beacon carries no fields");
        assert!(env.camera_on.is_none());
        assert!(env.mint_epoch.is_none());
    }

    #[test]
    fn verdict_covers_its_three_outcomes() {
        let w = committer_beacon_freshness_window_ms();

        // 1 — a plain member is never a candidate, whatever it has heard.
        assert_eq!(
            acting_committer_verdict(
                None,
                vec![BeaconObservation {
                    rank: Some(0),
                    age_ms: 0,
                }],
                w,
            ),
            ActingCommitter::NotACandidate,
        );

        // 2 — the owner acts even with a fresh beacon from every admin.
        assert_eq!(
            acting_committer_verdict(
                Some(0),
                vec![
                    BeaconObservation {
                        rank: Some(1),
                        age_ms: 0,
                    },
                    BeaconObservation {
                        rank: Some(2),
                        age_ms: 0,
                    },
                ],
                w,
            ),
            ActingCommitter::Act,
        );

        // 3 — a strictly lower rank, fresh, defers this device and names whom.
        assert_eq!(
            acting_committer_verdict(
                Some(2),
                vec![
                    BeaconObservation {
                        rank: Some(1),
                        age_ms: w,
                    },
                    BeaconObservation {
                        rank: Some(3),
                        age_ms: 0,
                    },
                ],
                w,
            ),
            ActingCommitter::Defer { to_rank: 1 },
            "the window boundary is inclusive, and the LOWEST active rank is named",
        );

        // 4 — silence is Act, not a failure to elect.
        assert_eq!(
            acting_committer_verdict(Some(3), Vec::new(), w),
            ActingCommitter::Act,
        );
    }

    #[test]
    fn verdict_ignores_stale_non_candidate_and_equal_ranks() {
        let w = committer_beacon_freshness_window_ms();

        // Stale (one millisecond past the window) — ignored.
        assert_eq!(
            acting_committer_verdict(
                Some(2),
                vec![BeaconObservation {
                    rank: Some(0),
                    age_ms: w + 1,
                }],
                w,
            ),
            ActingCommitter::Act,
        );
        // A beacon whose sender the observer could not rank — ignored.
        assert_eq!(
            acting_committer_verdict(
                Some(2),
                vec![BeaconObservation {
                    rank: None,
                    age_ms: 0,
                }],
                w,
            ),
            ActingCommitter::Act,
        );
        // Equal rank never defers — this is what makes a device's own beacon,
        // echoed back off the journal, harmless.
        assert_eq!(
            acting_committer_verdict(
                Some(2),
                vec![BeaconObservation {
                    rank: Some(2),
                    age_ms: 0,
                }],
                w,
            ),
            ActingCommitter::Act,
        );
        // A HIGHER rank never defers either.
        assert_eq!(
            acting_committer_verdict(
                Some(2),
                vec![BeaconObservation {
                    rank: Some(5),
                    age_ms: 0,
                }],
                w,
            ),
            ActingCommitter::Act,
        );
    }

    #[test]
    fn emit_throttle_gates_on_candidacy_then_cadence() {
        let interval = committer_beacon_interval_ms();

        // A plain member never emits, however long it has been.
        assert!(!should_emit_beacon(None, None, 0));
        assert!(!should_emit_beacon(None, Some(0), u64::MAX));
        // The first beacon of a session is immediate.
        assert!(should_emit_beacon(Some(0), None, 0));
        assert!(should_emit_beacon(Some(7), None, 12_345));
        // Then one per interval — the boundary itself emits.
        assert!(!should_emit_beacon(
            Some(1),
            Some(1_000),
            1_000 + interval - 1
        ));
        assert!(should_emit_beacon(Some(1), Some(1_000), 1_000 + interval));
        // A backward clock jump suppresses rather than wrapping.
        assert!(!should_emit_beacon(Some(1), Some(u64::MAX), 0));
    }

    /// **An absent `own_rank` must never read as rank `0`.** `0` is the owner,
    /// who acts immediately; absence means "not in the committer set at all".
    /// Collapsing the two would make every plain member believe it is the owner
    /// and sweep. This is the fail-closed direction on both entry points, and it
    /// is also what an OMITTED argument reaches at the wasm ABI (the sibling
    /// surface encodes `None` as a sentinel, not as `0`), so a caller that
    /// forgets the argument never sweeps and never emits.
    #[test]
    fn an_absent_own_rank_is_never_rank_zero() {
        let w = committer_beacon_freshness_window_ms();
        assert_eq!(
            acting_committer_verdict(None, Vec::new(), w),
            ActingCommitter::NotACandidate,
        );
        assert_eq!(
            acting_committer_verdict(Some(0), Vec::new(), w),
            ActingCommitter::Act,
        );
        assert!(!should_emit_beacon(None, None, 0));
        assert!(should_emit_beacon(Some(0), None, 0));
    }

    #[test]
    fn the_exposed_constants_are_the_cores_constants() {
        // Identity with the core, so replacing any body with a hand-copied
        // literal fails the moment the core's number moves — the ADR-0044
        // drift shape this surface exists to prevent.
        assert_eq!(committer_beacon_interval_ms(), COMMITTER_BEACON_INTERVAL_MS);
        assert_eq!(
            committer_beacon_worst_case_poll_ms(),
            COMMITTER_BEACON_WORST_CASE_POLL_MS,
        );
        assert_eq!(
            committer_beacon_freshness_window_ms(),
            COMMITTER_BEACON_FRESHNESS_WINDOW_MS,
        );
        assert_eq!(
            committer_beacon_envelope_len(),
            u32::try_from(COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.len() + COMMITTER_BEACON_PAYLOAD_LEN)
                .unwrap(),
        );
        // And the window's construction, so a client cannot be handed an
        // interval and a window that no longer belong to one budget.
        assert_eq!(
            committer_beacon_freshness_window_ms(),
            2 * committer_beacon_interval_ms() + committer_beacon_worst_case_poll_ms(),
        );
    }

    // ========================================================
    // ADR-0048 — profile envelope + contact-request carrier
    // ========================================================

    fn typed_profile() -> FfiProfile {
        FfiProfile {
            display_name: Some("  Alice Petrova ".to_string()),
            username: Some("@Alice".to_string()),
            updated_at_ms: 1_700_000_000_000,
            account_id: None,
        }
    }

    fn canonical_profile() -> FfiProfile {
        FfiProfile {
            display_name: Some("Alice Petrova".to_string()),
            username: Some("alice".to_string()),
            updated_at_ms: 1_700_000_000_000,
            account_id: None,
        }
    }

    #[test]
    fn profile_envelope_prepares_then_round_trips() {
        let wire = compose_profile_envelope(typed_profile()).expect("compose");
        assert_eq!(
            decode_profile_envelope(wire.clone()).expect("decode"),
            canonical_profile()
        );
        let dispatched = dispatch_incoming_envelope(wire).expect("dispatch");
        assert_eq!(dispatched.kind, FfiIncomingEnvelopeKind::Profile);
        assert!(dispatched.payload.is_empty());
        assert_eq!(dispatched.profile, Some(canonical_profile()));
        // Blank fields mean "none".
        let blank = compose_profile_envelope(FfiProfile {
            display_name: Some("   ".to_string()),
            username: Some(String::new()),
            updated_at_ms: 1,
            account_id: None,
        })
        .expect("compose blank");
        let decoded = decode_profile_envelope(blank).expect("decode blank");
        assert_eq!(decoded.display_name, None);
        assert_eq!(decoded.username, None);
    }

    #[test]
    fn profile_envelope_refusals_are_typed() {
        let mut p = canonical_profile();
        p.username = Some("p\u{430}vel".to_string());
        assert!(matches!(
            compose_profile_envelope(p),
            Err(FfiError::UsernameInvalidCharacters(_))
        ));
        let mut p = canonical_profile();
        p.display_name = Some("Alice\u{202E}".to_string());
        assert!(matches!(
            compose_profile_envelope(p),
            Err(FfiError::DisplayNameControlOrFormatCharacter(_))
        ));
        assert!(matches!(
            normalize_display_name("a".repeat(65)),
            Err(FfiError::DisplayNameTooLong(_))
        ));
        assert!(matches!(
            normalize_display_name("Al\nice".to_string()),
            Err(FfiError::DisplayNameLineBreak(_))
        ));
        assert_eq!(
            normalize_display_name(" Bo ".to_string()).unwrap(),
            Some("Bo".to_string())
        );
        assert!(matches!(
            decode_profile_envelope(b"hi".to_vec()),
            Err(FfiError::Contacts(_))
        ));
        assert!(profile_is_newer(2, 1));
        assert!(!profile_is_newer(1, 1));
    }

    /// ADR-0051: the group-name envelope prepares (trims), round-trips, and
    /// dispatches as its own kind — never as an intro message.
    #[test]
    fn group_name_envelope_prepares_then_round_trips() {
        let wire = compose_group_name_envelope(FfiGroupName {
            name: "  Trio  ".to_string(),
            updated_at_ms: 42,
        })
        .expect("compose");
        let decoded = decode_group_name_envelope(wire.clone()).expect("decode");
        assert_eq!(
            decoded,
            FfiGroupName {
                name: "Trio".to_string(),
                updated_at_ms: 42,
            }
        );
        let dispatched = dispatch_incoming_envelope(wire).expect("dispatch");
        assert_eq!(dispatched.kind, FfiIncomingEnvelopeKind::GroupName);
        assert!(dispatched.payload.is_empty());
        assert_eq!(dispatched.profile, None);
        assert_eq!(dispatched.group_name, Some(decoded));

        // Blank clears the name, and the clear itself dispatches as a
        // group name (not as an empty intro message).
        let clear = compose_group_name_envelope(FfiGroupName {
            name: "   ".to_string(),
            updated_at_ms: 43,
        })
        .expect("compose clear");
        let decoded = decode_group_name_envelope(clear.clone()).expect("decode clear");
        assert_eq!(decoded.name, "");
        assert_eq!(decoded.updated_at_ms, 43);
        let dispatched = dispatch_incoming_envelope(clear).expect("dispatch clear");
        assert_eq!(dispatched.kind, FfiIncomingEnvelopeKind::GroupName);
        assert_eq!(dispatched.group_name.map(|g| g.name), Some(String::new()));
    }

    #[test]
    fn group_invite_preview_round_trips_over_the_ffi() {
        let seed = vec![7u8; 32];
        let mint = compose_group_invite_capability_with_preview(
            seed.clone(),
            vec![1, 2, 3, 4],
            0,
            Some("  Weekend plans ".to_string()),
            Some("Anna".to_string()),
            4,
        )
        .expect("mint");
        let cap = GroupInviteCapability::decode_and_verify(mint.wire.clone()).expect("decode");
        assert!(cap.has_preview());
        let preview = cap
            .open_preview(mint.link_key.clone())
            .expect("open")
            .expect("some");
        assert_eq!(preview.group_name.as_deref(), Some("Weekend plans"));
        assert_eq!(preview.inviter_name.as_deref(), Some("Anna"));
        assert_eq!(preview.member_count, 4);
        assert_eq!(mint.link_key.len(), 12);
        assert!(cap.open_preview(vec![0u8; 12]).is_err(), "wrong secret");
        assert!(cap.open_preview(vec![0u8; 32]).is_err(), "old key width");
        assert!(cap.open_preview(vec![0u8; 5]).is_err());
        let plain = compose_group_invite_capability(seed, vec![1, 2, 3, 4], 0).expect("plain");
        let cap = GroupInviteCapability::decode_and_verify(plain).expect("decode");
        assert!(!cap.has_preview());
        assert_eq!(cap.open_preview(mint.link_key).expect("open"), None);
    }

    #[test]
    fn short_link_lengths_are_exported() {
        assert_eq!(invite_blob_id_len(), 6);
        assert_eq!(invite_blob_id_url_len(), 8);
        assert_eq!(group_invite_link_secret_len(), 12);
        assert_eq!(group_invite_link_secret_url_len(), 16);
    }

    #[test]
    fn pin_envelope_round_trips_and_dispatches() {
        for pinned in [true, false] {
            let op = FfiPinOp {
                message_id: 41,
                pinned,
                updated_at_ms: 1_700_000_000_000,
            };
            let wire = compose_pin_envelope(op.clone());
            assert_eq!(decode_pin_envelope(wire.clone()).expect("decode"), op);
            let dispatched = dispatch_incoming_envelope(wire).expect("dispatch");
            assert_eq!(dispatched.kind, FfiIncomingEnvelopeKind::Pin);
            assert!(dispatched.payload.is_empty());
            assert_eq!(dispatched.pin, Some(op));
            assert_eq!(dispatched.group_name, None);
        }
        assert!(matches!(
            decode_pin_envelope(b"hi".to_vec()),
            Err(FfiError::Contacts(_))
        ));
        assert!(pin_is_newer(2, 1));
        assert!(!pin_is_newer(1, 1));
    }

    #[test]
    fn group_name_envelope_refusals_are_typed() {
        assert!(matches!(
            compose_group_name_envelope(FfiGroupName {
                name: "Trio\u{202E}".to_string(),
                updated_at_ms: 0,
            }),
            Err(FfiError::DisplayNameControlOrFormatCharacter(_))
        ));
        assert!(matches!(
            compose_group_name_envelope(FfiGroupName {
                name: "a".repeat(65),
                updated_at_ms: 0,
            }),
            Err(FfiError::DisplayNameTooLong(_))
        ));
        assert!(matches!(
            compose_group_name_envelope(FfiGroupName {
                name: "Tr\nio".to_string(),
                updated_at_ms: 0,
            }),
            Err(FfiError::DisplayNameLineBreak(_))
        ));
        assert!(matches!(
            decode_group_name_envelope(b"hi".to_vec()),
            Err(FfiError::Contacts(_))
        ));
        // A profile envelope is not a group name, and vice versa.
        let profile_wire = compose_profile_envelope(typed_profile()).expect("compose profile");
        assert!(matches!(
            decode_group_name_envelope(profile_wire),
            Err(FfiError::Contacts(_))
        ));
        assert!(group_name_is_newer(2, 1));
        assert!(!group_name_is_newer(1, 1));
        assert!(!group_name_is_newer(0, 1));
    }

    #[test]
    fn contact_request_carries_the_profile() {
        let alice_signing = SigningKey::from_seed(&[0xAAu8; 32]);
        let bob_priv = InnerX25519SecretKey::from_bytes([0xBBu8; 32]);
        let wire = compose_contact_request(
            alice_signing.to_seed().to_vec(),
            vec![0xA1u8; ACCOUNT_ID_LEN],
            bob_priv.public_key().to_bytes().to_vec(),
            vec![0x55u8; PSEUDONYM_LEN],
            0,
            vec![],
            Some(typed_profile()),
            b"hi bob".to_vec(),
        )
        .expect("compose");
        let req = ContactRequest::decode_and_verify(wire).expect("decode");
        let opened = req
            .open_envelope(bob_priv.to_bytes().to_vec())
            .expect("open");
        assert_eq!(opened.profile, Some(canonical_profile()));
        assert_eq!(opened.intro_message, b"hi bob".to_vec());

        let mut bad = canonical_profile();
        bad.username = Some("ab".to_string());
        let err = compose_contact_request(
            alice_signing.to_seed().to_vec(),
            vec![0xA1u8; ACCOUNT_ID_LEN],
            bob_priv.public_key().to_bytes().to_vec(),
            vec![0x55u8; PSEUDONYM_LEN],
            0,
            vec![],
            Some(bad),
            b"hi bob".to_vec(),
        )
        .expect_err("an out-of-bounds profile must refuse the compose");
        assert!(matches!(err, FfiError::UsernameTooShort(_)));
    }
}

// ============================================================================
// Live-call link values — docs/ux/call-links.md section 4
// ============================================================================

/// The expiry stamp for a link to a live call minted at `now_ms`.
///
/// The person is no longer asked how long their link should last, so the
/// answer is decided once, in `lakko_contacts::call_invite`, and reached from
/// here — three clients each carrying their own constant is how the window
/// came to differ between them in the first place.
///
/// Saturating, and never the never-expires value, which that field's own
/// documentation names a stalking risk.
#[uniffi::export]
#[must_use]
pub fn call_invite_expiry_ms(now_ms: u64) -> u64 {
    rust_call_invite_expiry_ms(now_ms)
}

/// How many people a link to a live call admits: no limit.
///
/// The host admits every arrival, so a count here would cap a meeting at the
/// door of a room the host was holding open.
#[uniffi::export]
#[must_use]
pub fn call_invite_max_uses() -> u32 {
    RUST_CALL_INVITE_MAX_USES
}
