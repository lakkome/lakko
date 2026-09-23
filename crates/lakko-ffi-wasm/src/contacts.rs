// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Mirror of `lakko-contacts`: pseudonym lifecycle and the
//! [`ContactRequest`] envelope that drives ADR-0005 scenario A
//! ("I know `@username`, add them"). Phase 1.9a only.

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
    compose as rust_group_invite_compose, decode_and_verify as rust_group_invite_decode,
    group_sign_revocation as rust_group_sign_revocation,
    GroupInviteCapability as RustGroupInviteCapability, MAX_GROUP_ID_LEN,
};
use lakko_contacts::group_join_attribution::{
    compose_attribution as rust_group_join_attribution_compose,
    decode_attribution as rust_group_join_attribution_decode,
    GroupJoinAttribution as RustGroupJoinAttribution,
};
use lakko_contacts::group_name::{
    compose_group_name as rust_compose_group_name, decode_group_name as rust_decode_group_name,
    group_name_is_newer as rust_group_name_is_newer,
    normalize_group_name as rust_normalize_group_name, GroupName as RustGroupName, GroupNameError,
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
use lakko_contacts::profile::{
    compose_profile as rust_compose_profile, decode_profile as rust_decode_profile,
    normalize_display_name as rust_normalize_display_name,
    profile_is_newer as rust_profile_is_newer, DisplayNameError, Profile as RustProfile,
    ProfileError,
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

/// A real `Error` carrying one stable reason code as an own property —
/// the `mls.rs` `refusalReason` pattern. The message keeps the core's
/// sentence (never the rejected input); a client branches on the property.
fn coded_error(message: &str, property: &str, code: &str) -> JsValue {
    let err = js_sys::Error::new(message);
    // Best-effort: a failed property write must not replace the error.
    let _ = js_sys::Reflect::set(
        err.as_ref(),
        &JsValue::from_str(property),
        &JsValue::from_str(code),
    );
    err.into()
}

/// Thrown for a profile refused by its bounds or its wire shape: an `Error`
/// with a `profileError` code — `"displayName.tooLong"`,
/// `"username.invalidCharacters"`, `"truncated"`, …
fn profile_error_value(e: ProfileError) -> JsValue {
    coded_error(&e.to_string(), "profileError", &e.code())
}

/// Thrown for a group name refused by its bounds or its wire shape: an
/// `Error` with a `groupNameError` code — `"name.tooLong"`,
/// `"name.controlOrFormatCharacter"`, `"truncated"`, …
fn group_name_error_value(e: GroupNameError) -> JsValue {
    coded_error(&e.to_string(), "groupNameError", &e.code())
}

/// Turn caller-supplied fields into the canonical core profile: trim the
/// display name (blank = no name), normalise + check a non-empty username
/// (`None` or empty = no username).
fn prepare_profile(
    display_name: Option<String>,
    username: Option<String>,
    updated_at_ms: u64,
    account_id: Option<Vec<u8>>,
) -> Result<RustProfile, JsValue> {
    let account_id = match account_id {
        None => None,
        Some(bytes) => Some(<[u8; 16]>::try_from(bytes.as_slice()).map_err(|_| {
            JsValue::from_str(&format!(
                "profile account id must be 16 bytes, got {}",
                bytes.len()
            ))
        })?),
    };
    let display_name = match display_name {
        Some(name) => rust_normalize_display_name(&name)
            .map_err(|e| profile_error_value(ProfileError::DisplayName(e)))?,
        None => None,
    };
    let username = match username {
        Some(u) if !u.is_empty() => Some(
            lakko_discovery_client::validate_username(&u)
                .map_err(|e| profile_error_value(ProfileError::Username(e)))?,
        ),
        _ => None,
    };
    Ok(RustProfile {
        display_name,
        username,
        updated_at_ms,
        account_id,
    })
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
    /// `profileEnvelope` is the sender's profile (ADR-0048) as produced by
    /// `composeProfileEnvelope`, or an EMPTY array for no profile. It is
    /// decoded and bounds-checked before sealing, so a recipient never
    /// receives a profile it would drop.
    ///
    /// # Errors
    ///
    /// Throws on any input-shape or AEAD failure; throws an `Error` with a
    /// `profileError` code when `profileEnvelope` is non-empty and invalid.
    #[wasm_bindgen]
    pub fn compose(
        sender_signing_seed: &[u8],
        sender_account_id: &[u8],
        recipient_x25519_pub: &[u8],
        from_pseudonym: &[u8],
        valid_after_ms: u64,
        privacy_pass_token: &[u8],
        profile_envelope: &[u8],
        intro_message: &[u8],
    ) -> Result<Vec<u8>, JsValue> {
        let profile = if profile_envelope.is_empty() {
            None
        } else {
            Some(rust_decode_profile(profile_envelope).map_err(profile_error_value)?)
        };
        if sender_signing_seed.len() != 32 {
            return Err(JsError::new("senderSigningSeed must be 32 bytes").into());
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(sender_signing_seed);
        let signing = SigningKey::from_seed(&seed);

        if sender_account_id.len() != lakko_contacts::ACCOUNT_ID_LEN {
            return Err(JsError::new(&format!(
                "senderAccountId must be {} bytes",
                lakko_contacts::ACCOUNT_ID_LEN,
            ))
            .into());
        }
        let mut acct = [0u8; lakko_contacts::ACCOUNT_ID_LEN];
        acct.copy_from_slice(sender_account_id);

        if recipient_x25519_pub.len() != PUBLIC_KEY_LEN {
            return Err(JsError::new(&format!(
                "recipientX25519Pub must be {PUBLIC_KEY_LEN} bytes",
            ))
            .into());
        }
        let mut x25519_pub_arr = [0u8; PUBLIC_KEY_LEN];
        x25519_pub_arr.copy_from_slice(recipient_x25519_pub);
        let recipient_pub = X25519PublicKey::from_bytes(&x25519_pub_arr).map_err(crypto_err)?;

        let pseudonym = decode_pseudonym_bytes(from_pseudonym)?;

        Ok(rust_compose(
            &signing,
            &acct,
            &recipient_pub,
            pseudonym,
            valid_after_ms,
            privacy_pass_token,
            profile.as_ref(),
            intro_message,
        )
        .map_err(contact_err)?)
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

    /// Decrypt the sealed envelope using the recipient's 32-byte X25519
    /// private key. Returns an `OpenedContactRequest` carrying the
    /// sender's profile (or `undefined`) and the intro message.
    ///
    /// The profile is the sender's **claim**, bound to `fromPubkey` (which
    /// the request signature authenticates): store it under that key and
    /// show its username only after verifying it through discovery
    /// (ADR-0048 §3). A profile that fails its bounds arrives as
    /// `undefined` with the intro intact.
    #[wasm_bindgen(js_name = openEnvelope)]
    pub fn open_envelope(
        &self,
        recipient_x25519_priv: &[u8],
    ) -> Result<OpenedContactRequest, JsError> {
        if recipient_x25519_priv.len() != 32 {
            return Err(JsError::new("recipientX25519Priv must be 32 bytes"));
        }
        let mut sk = [0u8; 32];
        sk.copy_from_slice(recipient_x25519_priv);
        let priv_key = X25519SecretKey::from_bytes(sk);
        let (profile, intro_message) = rust_open(&self.inner, &priv_key).map_err(contact_err)?;
        Ok(OpenedContactRequest {
            profile: profile.map(WasmProfile::from),
            intro_message,
        })
    }
}

/// Result of `ContactRequest.openEnvelope` (ADR-0048 §2).
#[wasm_bindgen]
pub struct OpenedContactRequest {
    profile: Option<WasmProfile>,
    intro_message: Vec<u8>,
}

#[wasm_bindgen]
impl OpenedContactRequest {
    /// The sender's profile, or `undefined` when the request carried none
    /// or carried one that failed its bounds.
    #[wasm_bindgen(getter)]
    pub fn profile(&self) -> Option<WasmProfile> {
        self.profile.clone()
    }

    /// The intro message bytes, possibly empty. May itself be a call
    /// envelope — classify with `dispatchIncomingEnvelope` as before.
    #[wasm_bindgen(getter, js_name = introMessage)]
    pub fn intro_message(&self) -> Vec<u8> {
        self.intro_message.clone()
    }
}

// ============================================================================
// Profile envelope (ADR-0048)
// ============================================================================

/// A person's self-stated profile. Mirror of
/// [`lakko_contacts::profile::Profile`].
///
/// **Every field received from someone else is that person's claim.** Store
/// it under the sender's signature key resolved at receipt (C-68), keep it
/// only when `profileIsNewer`, and show `username` only once discovery
/// resolves it to that same key.
#[wasm_bindgen(js_name = Profile)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmProfile {
    display_name: Option<String>,
    username: Option<String>,
    updated_at_ms: u64,
    account_id: Option<Vec<u8>>,
}

impl From<RustProfile> for WasmProfile {
    fn from(p: RustProfile) -> Self {
        Self {
            display_name: p.display_name,
            username: p.username,
            updated_at_ms: p.updated_at_ms,
            account_id: p.account_id.map(|id| id.to_vec()),
        }
    }
}

#[wasm_bindgen(js_class = Profile)]
impl WasmProfile {
    /// Display name, or `undefined` for no name.
    #[wasm_bindgen(getter, js_name = displayName)]
    pub fn display_name(&self) -> Option<String> {
        self.display_name.clone()
    }

    /// Claimed username in canonical form, or `undefined` for none.
    #[wasm_bindgen(getter)]
    pub fn username(&self) -> Option<String> {
        self.username.clone()
    }

    /// Sender-clock UNIX milliseconds, as a `bigint`.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    pub fn updated_at_ms(&self) -> u64 {
        self.updated_at_ms
    }

    /// The sender's 16-byte auth account id, or `undefined` (ADR-0048 v2):
    /// what the call roster derives the ADR-0021 peer tag from.
    #[wasm_bindgen(getter, js_name = accountId)]
    pub fn account_id(&self) -> Option<Vec<u8>> {
        self.account_id.clone()
    }
}

/// Compose a profile envelope (ADR-0048) — the inner plaintext to publish as
/// an MLS application message, or to pass to `ContactRequest.compose`.
///
/// The display name is trimmed (blank = no name); a non-empty username is
/// normalised (`@Alice` -> `alice`) and must pass the ADR-0048 §4 rule. The
/// envelope produced is always one a receiver accepts.
///
/// # Errors
///
/// Throws an `Error` with a `profileError` code such as
/// `"displayName.controlOrFormatCharacter"` or `"username.tooShort"`.
#[wasm_bindgen(js_name = composeProfileEnvelope)]
pub fn compose_profile_envelope(
    display_name: Option<String>,
    username: Option<String>,
    updated_at_ms: u64,
    account_id: Option<Vec<u8>>,
) -> Result<Vec<u8>, JsValue> {
    let profile = prepare_profile(display_name, username, updated_at_ms, account_id)?;
    rust_compose_profile(&profile).map_err(profile_error_value)
}

/// Strictly decode a profile envelope (ADR-0048). Fail-closed: anything
/// malformed or out of bounds is refused as a whole.
///
/// Use `dispatchIncomingEnvelope` when the plaintext might be any kind.
///
/// # Errors
///
/// Throws an `Error` with a `profileError` code (`"wrongDomain"`,
/// `"truncated"`, `"displayName.tooLong"`, …).
#[wasm_bindgen(js_name = decodeProfileEnvelope)]
pub fn decode_profile_envelope(plaintext: &[u8]) -> Result<WasmProfile, JsValue> {
    rust_decode_profile(plaintext)
        .map(WasmProfile::from)
        .map_err(profile_error_value)
}

/// `true` when a profile stamped `incomingUpdatedAtMs` must replace the stored
/// one **from the same signature key**. Strictly greater wins; equal is not
/// newer.
#[wasm_bindgen(js_name = profileIsNewer)]
#[must_use]
pub fn profile_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_profile_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

// ============================================================================
// Group-name envelope (ADR-0051 §1)
// ============================================================================

/// A group's shared name. Mirror of
/// [`lakko_contacts::group_name::GroupName`].
///
/// **A name received from someone else is that person's assertion about the
/// whole group, and the sender's authority is the RECEIVER's check** (ADR-0051
/// §2): resolve the MLS sender leaf to the account key at receipt (C-68), read
/// the live role table, accept only from the owner or an admin (or from any
/// member when the group carries no role table), then keep it only when
/// `groupNameIsNewer` than the stored stamp for that group. Persist the stamp
/// and re-emit it VERBATIM on a growth re-send (the ADR-0044 lesson).
#[wasm_bindgen(js_name = GroupName)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmGroupName {
    name: String,
    updated_at_ms: u64,
}

impl From<RustGroupName> for WasmGroupName {
    fn from(g: RustGroupName) -> Self {
        Self {
            name: g.name,
            updated_at_ms: g.updated_at_ms,
        }
    }
}

#[wasm_bindgen(js_class = GroupName)]
impl WasmGroupName {
    /// The name; **empty means "clear the name"** (fall back to the
    /// member-derived title).
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// Setter-clock UNIX milliseconds, as a `bigint`.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    pub fn updated_at_ms(&self) -> u64 {
        self.updated_at_ms
    }
}

/// Compose a group-name envelope (ADR-0051 §1) — the inner plaintext to
/// publish as an MLS application message on the group's journal.
///
/// The name is trimmed (blank = clear the name) and must pass the profile
/// display-name rule. The envelope produced is always one a receiver
/// accepts.
///
/// # Errors
///
/// Throws an `Error` with a `groupNameError` code such as
/// `"name.controlOrFormatCharacter"` or `"name.tooLong"`.
#[wasm_bindgen(js_name = composeGroupNameEnvelope)]
pub fn compose_group_name_envelope(name: &str, updated_at_ms: u64) -> Result<Vec<u8>, JsValue> {
    let group_name = RustGroupName {
        name: rust_normalize_group_name(name)
            .map_err(|e| group_name_error_value(GroupNameError::Name(e)))?,
        updated_at_ms,
    };
    rust_compose_group_name(&group_name).map_err(group_name_error_value)
}

/// Strictly decode a group-name envelope (ADR-0051 §1). Fail-closed:
/// anything malformed or out of bounds is refused as a whole.
///
/// Use `dispatchIncomingEnvelope` when the plaintext might be any kind.
///
/// # Errors
///
/// Throws an `Error` with a `groupNameError` code (`"wrongDomain"`,
/// `"truncated"`, `"name.tooLong"`, …).
#[wasm_bindgen(js_name = decodeGroupNameEnvelope)]
pub fn decode_group_name_envelope(plaintext: &[u8]) -> Result<WasmGroupName, JsValue> {
    rust_decode_group_name(plaintext)
        .map(WasmGroupName::from)
        .map_err(group_name_error_value)
}

/// `true` when a group name stamped `incomingUpdatedAtMs` must replace the
/// stored one **for the same group**. Strictly greater wins; equal is not
/// newer, so a growth re-send of the stored name is not an update.
#[wasm_bindgen(js_name = groupNameIsNewer)]
#[must_use]
pub fn group_name_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_group_name_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// Prepare what a person typed as their display name: trim, `undefined` for
/// blank ("no name"), and throw the reason for anything out of bounds.
///
/// # Errors
///
/// Throws an `Error` with a `displayNameError` code: `"lineBreak"`,
/// `"controlOrFormatCharacter"`, `"tooLong"` or `"tooManyBytes"`.
#[wasm_bindgen(js_name = normalizeDisplayName)]
pub fn normalize_display_name(input: &str) -> Result<Option<String>, JsValue> {
    rust_normalize_display_name(input)
        .map_err(|e: DisplayNameError| coded_error(&e.to_string(), "displayNameError", e.code()))
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
    /// A member's profile (ADR-0048). The decoded, bounds-checked profile
    /// rides the `profile` getter of [`WasmIncomingEnvelope`]; `payload` is
    /// empty, `cameraOn` and `mintEpoch` are `undefined`. Appended as `6` so
    /// the pre-existing tags stay fixed. Never render it as a chat bubble;
    /// store it under the sender's signature key resolved at receipt
    /// (C-68), keep it only when `profileIsNewer`, and verify the username
    /// through discovery before showing it.
    Profile = 6,
    /// The group's shared name (ADR-0051 §1). The decoded, bounds-checked
    /// name rides the `groupName` getter of [`WasmIncomingEnvelope`];
    /// `payload` is empty, `cameraOn`, `mintEpoch` and `profile` are
    /// `undefined`. Appended as `7` so the pre-existing tags stay fixed.
    /// Never render it as a chat bubble; the sender's authority is the
    /// receiver's role-table check (ADR-0051 §2), then `groupNameIsNewer`.
    /// An empty name clears the name.
    GroupName = 7,
    /// A member asks the group to introduce itself (ADR-0048 §2 trigger 4).
    /// `payload` is empty; answer with the own profile through the throttled
    /// growth path; never render it.
    ProfileRequest = 8,
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
    profile: Option<WasmProfile>,
    group_name: Option<WasmGroupName>,
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

    /// Set only for `Profile` (ADR-0048): the decoded profile. `undefined`
    /// for every other kind.
    #[wasm_bindgen(getter)]
    pub fn profile(&self) -> Option<WasmProfile> {
        self.profile.clone()
    }

    /// Set only for `GroupName` (ADR-0051): the decoded group name.
    /// `undefined` for every other kind.
    #[wasm_bindgen(getter, js_name = groupName)]
    pub fn group_name(&self) -> Option<WasmGroupName> {
        self.group_name.clone()
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
            profile: None,
            group_name: None,
        },
        IncomingEnvelope::CallKeyPackage(p) => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::CallKeyPackage,
            payload: p.to_vec(),
            camera_on: None,
            mint_epoch: None,
            profile: None,
            group_name: None,
        },
        IncomingEnvelope::MediaState { camera_on } => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::MediaState,
            payload: Vec::new(),
            camera_on: Some(camera_on),
            mint_epoch: None,
            profile: None,
            group_name: None,
        },
        IncomingEnvelope::PushRoutingSecret { mint_epoch, secret } => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::PushRoutingSecret,
            payload: secret.to_vec(),
            camera_on: None,
            mint_epoch: Some(mint_epoch),
            profile: None,
            group_name: None,
        },
        IncomingEnvelope::CommitterBeacon => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::CommitterBeacon,
            payload: Vec::new(),
            camera_on: None,
            mint_epoch: None,
            profile: None,
            group_name: None,
        },
        IncomingEnvelope::Profile(p) => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::Profile,
            payload: Vec::new(),
            camera_on: None,
            mint_epoch: None,
            profile: Some(p.into()),
            group_name: None,
        },
        IncomingEnvelope::GroupName(g) => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::GroupName,
            payload: Vec::new(),
            camera_on: None,
            mint_epoch: None,
            profile: None,
            group_name: Some(g.into()),
        },
        IncomingEnvelope::ProfileRequest => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::ProfileRequest,
            payload: Vec::new(),
            camera_on: None,
            mint_epoch: None,
            profile: None,
            group_name: None,
        },
        IncomingEnvelope::IntroMessage(p) => WasmIncomingEnvelope {
            kind: WasmIncomingEnvelopeKind::IntroMessage,
            payload: p.to_vec(),
            camera_on: None,
            mint_epoch: None,
            profile: None,
            group_name: None,
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
// Committer liveness beacon — ADR-0045 decision 2, variant 1
// ============================================================================

/// One observed committer beacon, reduced to the only two things
/// [`acting_committer_verdict`] reads. wasm-bindgen mirror of UniFFI's
/// `BeaconObservation` / [`lakko_contacts::committer_beacon::BeaconObservation`].
///
/// **Both fields are the observer's own re-derivation, never anything the
/// beacon claimed.** The payload is field-less: the sender comes from the MLS
/// sender leaf and the age from the DS row's server-assigned `created_at`, so a
/// sender can neither name itself nor assert its own freshness.
///
/// # Why a constructible class rather than parallel arrays
///
/// The two fields belong to one observation, so they are carried as one object
/// — following [`crate::mls::ProposalRecord`], the house shape for a small
/// record crossing this boundary. Two parallel arrays (`ranks[]`, `ages[]`)
/// plus a sentinel for the absent rank would let the lengths diverge silently
/// and would need a magic number that `undefined` already expresses.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BeaconObservation {
    rank: Option<u32>,
    age_ms: u64,
}

#[wasm_bindgen]
impl BeaconObservation {
    /// Build one observation from a beacon the caller has already decrypted and
    /// classified.
    ///
    /// `rank` is the committer rank the observer **re-derived** for this
    /// beacon's MLS-authenticated sender — `Group.committerRank(signatureKey)`
    /// — or `undefined` when that sender is not a candidate at all (a plain
    /// member, or a leaf that no longer resolves).
    ///
    /// **Derive it at the moment the verdict is taken, never at the moment the
    /// beacon arrived.** A rank is a *position* in the role table's canonical
    /// order (`0` = owner, `k + 1` = the `k`-th admin), so promoting, demoting
    /// or removing any admin renumbers every admin sorting after it; a rank
    /// captured under one table cannot be compared against this device's own
    /// rank, which is necessarily current. Persist the sender's **32-byte
    /// signature key** with the row timestamp and rank it at fold time — never
    /// a leaf index, which OpenMLS reuses for the next member added.
    ///
    /// `ageMs` is `now - created_at` in milliseconds on the DS row's
    /// **server-assigned** timestamp, which every member reads identically; the
    /// sender's own clock never enters it. Clamp a row that appears to come
    /// from the future to `0` rather than wrapping — that is also the safe
    /// direction, since an age that reads too fresh makes this device defer,
    /// while one that reads too stale makes it act and risks a double sweep.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(rank: Option<u32>, age_ms: u64) -> Self {
        Self { rank, age_ms }
    }

    /// The observer's re-derived committer rank for this beacon's sender, or
    /// `undefined` when the sender is not a candidate.
    ///
    /// `undefined` and `0` are **not** the same thing: `0` is the owner. Because
    /// `0` is falsy in JavaScript, `if (obs.rank)` is wrong in both directions.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn rank(&self) -> Option<u32> {
        self.rank
    }

    /// Age of the beacon's DS row in milliseconds (BigInt on the JS side).
    #[wasm_bindgen(getter, js_name = ageMs)]
    #[must_use]
    pub fn age_ms(&self) -> u64 {
        self.age_ms
    }
}

impl From<BeaconObservation> for RustBeaconObservation {
    fn from(o: BeaconObservation) -> Self {
        Self {
            rank: o.rank,
            age_ms: o.age_ms,
        }
    }
}

/// The fold's answer to *am I the acting committer right now?* wasm-bindgen
/// mirror of UniFFI's `ActingCommitter` /
/// [`lakko_contacts::committer_beacon::ActingCommitter`].
///
/// # Why a flat tagged class rather than a sum type
///
/// wasm-bindgen has no idiomatic story for a Rust enum carrying data, so this
/// follows [`crate::mls::ProposalRecord`]: a string [`Self::kind`]
/// discriminator plus a per-variant getter. The payload getter is named for its
/// own variant — [`Self::defer_to_rank`], `undefined` outside `"defer"` — and
/// deliberately **not** a shared slot in the `ParsedBody` style, because only
/// one variant has a payload and a shared name would invite reading it on the
/// others.
///
/// A string tag also sidesteps the numeric-enum hazard this crate pins
/// elsewhere (`outgoing_owner_variants_are_never_zero`): there is no
/// discriminant for a missing value to coerce to.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActingCommitter {
    kind: &'static str,
    defer_to_rank: Option<u32>,
}

#[wasm_bindgen]
impl ActingCommitter {
    /// Discriminator: `"act" | "defer" | "not-a-candidate"`. Useful for
    /// `switch` / `case` in TypeScript.
    ///
    /// * `"act"` — this device is the acting committer: it is a candidate and
    ///   has heard no fresh beacon from any lower-ranked candidate, **including
    ///   when it has heard nothing at all**, which means nobody better placed is
    ///   running. Still only a licence to *attempt* the sweep: ADR-0045's
    ///   stand-down journal re-read and the staged-commit rollback
    ///   (`stagePendingProposals` / `mergeStagedSweep` / `abandonStagedSweep`)
    ///   both remain required, because two candidates can still both reach it.
    /// * `"defer"` — a lower-ranked candidate beaconed inside the freshness
    ///   window; stand down and re-evaluate on the next poll.
    ///   [`Self::defer_to_rank`] names it.
    /// * `"not-a-candidate"` — this device is not an owner or admin and can
    ///   never sweep. Distinct from `"defer"`: there is nothing to re-evaluate,
    ///   and no beacon this device should be emitting.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kind(&self) -> String {
        self.kind.to_string()
    }

    /// The lowest rank observed active, present **iff** `kind === "defer"` and
    /// `undefined` for every other kind.
    ///
    /// Purely informational — the decision is already made — but it is what lets
    /// a log line answer *deferring to whom*.
    #[wasm_bindgen(getter, js_name = deferToRank)]
    #[must_use]
    pub fn defer_to_rank(&self) -> Option<u32> {
        self.defer_to_rank
    }

    /// `true` only for `kind === "act"`. Convenience for the common call site;
    /// the kind is still the thing to log.
    #[wasm_bindgen(getter, js_name = isAct)]
    #[must_use]
    pub fn is_act(&self) -> bool {
        self.kind == "act"
    }
}

impl From<RustActingCommitter> for ActingCommitter {
    fn from(v: RustActingCommitter) -> Self {
        match v {
            RustActingCommitter::Act => Self {
                kind: "act",
                defer_to_rank: None,
            },
            RustActingCommitter::Defer { to_rank } => Self {
                kind: "defer",
                defer_to_rank: Some(to_rank),
            },
            RustActingCommitter::NotACandidate => Self {
                kind: "not-a-candidate",
                defer_to_rank: None,
            },
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
/// service-worker background decrypt structurally cannot commit (ADR-0041,
/// C-59), so a beacon emitted from the background is a lie that costs the group
/// a takeover.
#[wasm_bindgen(js_name = composeCommitterBeacon)]
#[must_use]
pub fn compose_committer_beacon() -> Vec<u8> {
    rust_compose_committer_beacon()
}

/// Strictly decode a committer-liveness beacon (ADR-0045). Fail-closed:
/// throws for a plaintext that does not carry the beacon domain separator, or
/// whose payload is the wrong length or an unknown schema version.
///
/// **Returning without throwing is the entire result.** There is deliberately
/// no payload to hand back — everything a receiver needs (who sent it, how old
/// it is) comes from the MLS sender leaf and the DS row, not from the bytes.
///
/// Use [`dispatch_incoming_envelope`] instead when the plaintext might be any
/// relay-payload / in-group envelope kind; it classifies a beacon as
/// [`WasmIncomingEnvelopeKind::CommitterBeacon`] and falls through to the other
/// kinds rather than throwing.
///
/// # Errors
///
/// `JsError` when `plaintext` is not a valid committer beacon (wrong domain /
/// bad length / unknown version).
#[wasm_bindgen(js_name = decodeCommitterBeacon)]
pub fn decode_committer_beacon(plaintext: &[u8]) -> Result<(), JsError> {
    decode_committer_beacon_core(plaintext).map_err(|m| JsError::new(&m))
}

/// Core of [`decode_committer_beacon`], returning a plain `String` so host
/// tests can drive the failure path (`JsError::new` panics off-wasm).
fn decode_committer_beacon_core(plaintext: &[u8]) -> Result<(), String> {
    rust_decode_committer_beacon(plaintext)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// ADR-0048 §2 trigger 4 — the newcomer's "introduce yourselves" envelope.
/// Publish it as an MLS application message right after joining a group,
/// and on opening a group with an unnamed member when
/// `shouldRequestProfiles` says so. Field-less; byte-identical every call.
#[wasm_bindgen(js_name = composeProfileRequest)]
#[must_use]
pub fn compose_profile_request() -> Vec<u8> {
    rust_compose_profile_request()
}

/// Recognise a profile request: resolves for a well-formed one, throws for
/// a foreign domain or a malformed payload.
#[wasm_bindgen(js_name = decodeProfileRequest)]
pub fn decode_profile_request(plaintext: &[u8]) -> Result<(), JsError> {
    rust_decode_profile_request(plaintext)
        .map(|_| ())
        .map_err(|err| JsError::new(&err.to_string()))
}

/// Length of the profile-request wire, for a cheap pre-filter.
#[wasm_bindgen(js_name = profileRequestEnvelopeLen)]
#[must_use]
pub fn profile_request_envelope_len() -> u32 {
    u32::try_from(PROFILE_REQUEST_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

/// The daily floor between two requests to one group outside a join.
#[wasm_bindgen(js_name = profileRequestMinIntervalMs)]
#[must_use]
pub fn profile_request_min_interval_ms() -> u64 {
    PROFILE_REQUEST_MIN_INTERVAL_MS
}

/// Should this device ask the group to introduce itself now? Always on a
/// join; otherwise only with an unnamed member and no sooner than
/// `profileRequestMinIntervalMs` after the last request (`undefined` =
/// never asked).
#[wasm_bindgen(js_name = shouldRequestProfiles)]
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
/// 1. `ownRank === undefined` is `"not-a-candidate"`, decided without reading a
///    single beacon. An **omitted** argument reaches the ABI as absent and
///    lands here too, which is the fail-closed direction: a caller that forgets
///    the argument never sweeps.
/// 2. Rank `0` — the owner — is `"act"` immediately; no rank is lower, so no
///    beacon could outrank it and none is consulted.
/// 3. An observation whose `rank` is `undefined` is ignored (its sender is not
///    a candidate).
/// 4. An observation older than `freshnessWindowMs` is ignored.
/// 5. Only a **strictly** lower rank defers this device — equal rank never
///    does, which is what makes a device's own echoed-back beacon harmless.
/// 6. Surviving all of that with nothing left is `"act"`: hearing nothing is
///    the correct answer when nobody better placed is running, not a failure to
///    elect.
///
/// **This does not close the race.** Each candidate folds over its own
/// poll-timed view and a beacon is stale by up to one
/// [`committer_beacon_interval_ms`] even when its sender is perfectly healthy,
/// so two candidates can both reach `"act"`. ADR-0045's stand-down journal
/// re-read and its staged-commit rollback both remain required; deleting either
/// on the strength of this verdict is a mis-reading.
///
/// Pass [`committer_beacon_freshness_window_ms`] for `freshnessWindowMs` unless
/// there is a measured reason not to — the interval and the window are two
/// halves of one budget.
///
/// # The observations are CONSUMED
///
/// wasm-bindgen moves each element of `observed` into wasm, so every
/// [`BeaconObservation`] in the array is freed by this call and must not be
/// used again. Build the array fresh, on each evaluation, from the stored
/// signature-key + DS-`created_at` rows — which is what
/// [`BeaconObservation::rank`] requires anyway, since the rank has to be
/// re-derived against the current role table each time.
#[wasm_bindgen(js_name = actingCommitterVerdict)]
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
/// 1. **Only candidates emit** — `ownRank === undefined` is `false`. A plain
///    member's beacon can never make it the acting committer (the fold drops
///    it), so emitting one is pure journal litter and a pure privacy leak.
/// 2. **The first beacon of a session is immediate** — `lastEmittedAtMs ===
///    undefined` is `true`, so a candidate that has just opened the app
///    announces itself at once rather than staying invisible for a whole
///    interval, which is exactly when a stale view is likeliest to hand the
///    duty elsewhere.
/// 3. Otherwise, one per [`committer_beacon_interval_ms`].
///
/// Both timestamps come from this device's **own local** clock, which is
/// legitimate here and nowhere else in the beacon design: it governs only this
/// device's send cadence and feeds no judgement about anybody else. A backward
/// clock jump suppresses one beacon and self-heals on the next tick.
///
/// **Foregroundedness is a caller obligation this function cannot check.** See
/// [`compose_committer_beacon`].
#[wasm_bindgen(js_name = shouldEmitBeacon)]
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
#[wasm_bindgen(js_name = committerBeaconIntervalMs)]
#[must_use]
pub fn committer_beacon_interval_ms() -> u64 {
    COMMITTER_BEACON_INTERVAL_MS
}

/// The slowest group-poll cadence in the client tree, in milliseconds — a term
/// in the freshness window's construction (ADR-0045).
///
/// Exposed because it is a **constraint on the caller**, not decoration: a
/// client that polls its group journal slower than this invalidates the
/// window's derivation, so the cadence has to be checkable against one shared
/// number instead of against a comment.
#[wasm_bindgen(js_name = committerBeaconWorstCasePollMs)]
#[must_use]
pub fn committer_beacon_worst_case_poll_ms() -> u64 {
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
#[wasm_bindgen(js_name = committerBeaconFreshnessWindowMs)]
#[must_use]
pub fn committer_beacon_freshness_window_ms() -> u64 {
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
#[wasm_bindgen(js_name = committerBeaconEnvelopeLen)]
#[must_use]
pub fn committer_beacon_envelope_len() -> u32 {
    u32::try_from(COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.len() + COMMITTER_BEACON_PAYLOAD_LEN)
        .unwrap_or(u32::MAX)
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
        let opened = req.open_envelope(&priv_bytes).unwrap();
        assert_eq!(opened.profile(), None);
        assert_eq!(opened.intro_message(), b"hi bob");
    }

    /// ADR-0048: the profile envelope composed here rides a contact request
    /// and comes back out of `openEnvelope`, prepared (trimmed, normalised).
    /// Success paths only — a throw builds a `js_sys::Error`, which panics
    /// off wasm32.
    #[test]
    fn profile_envelope_rides_a_contact_request() {
        let envelope = compose_profile_envelope(
            Some("  Alice Petrova ".to_string()),
            Some("@Alice".to_string()),
            42,
            Some(vec![9; 16]),
        )
        .unwrap();
        let decoded = decode_profile_envelope(&envelope).unwrap();
        assert_eq!(decoded.display_name(), Some("Alice Petrova".to_string()));
        assert_eq!(decoded.username(), Some("alice".to_string()));
        assert_eq!(decoded.updated_at_ms(), 42);
        assert_eq!(decoded.account_id(), Some(vec![9; 16]));
        // The wrong-length refusal is a `JsValue` error, which cannot be
        // constructed off wasm32; the core pins it (`profile.rs`).
        assert!(profile_is_newer(43, 42) && !profile_is_newer(42, 42));
        assert_eq!(
            normalize_display_name("  Bo ").unwrap(),
            Some("Bo".to_string())
        );

        let dispatched = dispatch_incoming_envelope(&envelope).unwrap();
        assert_eq!(dispatched.kind(), WasmIncomingEnvelopeKind::Profile);
        assert_eq!(dispatched.profile(), Some(decoded.clone()));

        let recipient = X25519SecretKey::from_bytes([0xCD; 32]);
        let wire = ContactRequest::compose(
            &[7u8; 32],
            &[0x77u8; 16],
            &recipient.public_key().to_bytes(),
            &[0xAA; 32],
            0,
            b"",
            &envelope,
            b"hi bob",
        )
        .unwrap();
        let opened = ContactRequest::decode_and_verify(&wire)
            .unwrap()
            .open_envelope(&recipient.to_bytes())
            .unwrap();
        assert_eq!(opened.profile(), Some(decoded));
        assert_eq!(opened.intro_message(), b"hi bob");
    }

    /// ADR-0051: the group-name envelope is trimmed on compose, round-trips,
    /// and dispatches as its own kind — never as an intro message. Success
    /// paths only — a throw builds a `js_sys::Error`, which panics off
    /// wasm32; the refusal matrix lives in `lakko-contacts`.
    #[test]
    fn group_name_envelope_round_trips_and_dispatches() {
        let envelope = compose_group_name_envelope("  Trio ", 42).unwrap();
        let decoded = decode_group_name_envelope(&envelope).unwrap();
        assert_eq!(decoded.name(), "Trio");
        assert_eq!(decoded.updated_at_ms(), 42);
        assert!(group_name_is_newer(43, 42) && !group_name_is_newer(42, 42));

        let dispatched = dispatch_incoming_envelope(&envelope).unwrap();
        assert_eq!(dispatched.kind(), WasmIncomingEnvelopeKind::GroupName);
        assert!(dispatched.payload().is_empty());
        assert_eq!(dispatched.profile(), None);
        assert_eq!(dispatched.group_name(), Some(decoded));

        // Blank clears the name and still dispatches as a group name.
        let clear = compose_group_name_envelope("   ", 43).unwrap();
        let decoded = decode_group_name_envelope(&clear).unwrap();
        assert_eq!(decoded.name(), "");
        let dispatched = dispatch_incoming_envelope(&clear).unwrap();
        assert_eq!(dispatched.kind(), WasmIncomingEnvelopeKind::GroupName);
        assert_eq!(
            dispatched.group_name().map(|g| g.name()),
            Some(String::new())
        );
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
        decode_committer_beacon_core(&wire).expect("a composed beacon must decode");
        assert_eq!(
            wire,
            compose_committer_beacon(),
            "the beacon is field-less, so every beacon is byte-identical",
        );
    }

    #[test]
    fn beacon_decode_is_fail_closed() {
        // Driven through the plain-Rust core: `JsError::new` panics off-wasm,
        // so the failure path is unreachable through the exported wrapper.
        assert!(decode_committer_beacon_core(b"hello").is_err());
        let mut short = compose_committer_beacon();
        short.pop();
        assert!(decode_committer_beacon_core(&short).is_err());
        let mut bad_version = compose_committer_beacon();
        let last = bad_version.len() - 1;
        bad_version[last] = 0xFF;
        assert!(decode_committer_beacon_core(&bad_version).is_err());
        let mut long = compose_committer_beacon();
        long.push(0x00);
        assert!(decode_committer_beacon_core(&long).is_err());
    }

    #[test]
    fn a_beacon_classifies_as_a_beacon_and_never_as_chat() {
        let env = dispatch_incoming_envelope(&compose_committer_beacon()).expect("dispatch");
        assert_eq!(env.kind(), WasmIncomingEnvelopeKind::CommitterBeacon);
        assert!(env.payload().is_empty(), "a beacon carries no fields");
        assert!(env.camera_on().is_none());
        assert!(env.mint_epoch().is_none());
    }

    #[test]
    fn verdict_covers_its_three_outcomes_with_a_per_variant_payload() {
        let w = committer_beacon_freshness_window_ms();

        // 1 — a plain member is never a candidate, whatever it has heard. An
        // OMITTED `ownRank` argument reaches the ABI as absent and lands here
        // too, which is the fail-closed direction: it never sweeps.
        let not_candidate =
            acting_committer_verdict(None, vec![BeaconObservation::new(Some(0), 0)], w);
        assert_eq!(not_candidate.kind(), "not-a-candidate");
        assert!(!not_candidate.is_act());
        assert!(
            not_candidate.defer_to_rank().is_none(),
            "the defer payload must be absent outside its own variant",
        );

        // 2 — the owner acts even with a fresh beacon from every admin.
        let owner = acting_committer_verdict(
            Some(0),
            vec![
                BeaconObservation::new(Some(1), 0),
                BeaconObservation::new(Some(2), 0),
            ],
            w,
        );
        assert_eq!(owner.kind(), "act");
        assert!(owner.is_act());
        assert!(owner.defer_to_rank().is_none());

        // 3 — a strictly lower rank, fresh, defers this device and names whom.
        let defer = acting_committer_verdict(
            Some(2),
            vec![
                BeaconObservation::new(Some(1), w),
                BeaconObservation::new(Some(3), 0),
            ],
            w,
        );
        assert_eq!(defer.kind(), "defer");
        assert!(!defer.is_act());
        assert_eq!(
            defer.defer_to_rank(),
            Some(1),
            "the window boundary is inclusive, and the LOWEST active rank is named",
        );

        // 4 — silence is Act, not a failure to elect.
        let silence = acting_committer_verdict(Some(3), Vec::new(), w);
        assert_eq!(silence.kind(), "act");
        assert!(silence.is_act());
    }

    #[test]
    fn verdict_ignores_stale_non_candidate_and_equal_ranks() {
        let w = committer_beacon_freshness_window_ms();
        for (label, obs) in [
            (
                "stale by one millisecond",
                BeaconObservation::new(Some(0), w + 1),
            ),
            (
                "sender the observer could not rank",
                BeaconObservation::new(None, 0),
            ),
            (
                "equal rank — a device's own echoed beacon",
                BeaconObservation::new(Some(2), 0),
            ),
            ("a higher rank", BeaconObservation::new(Some(5), 0)),
        ] {
            let v = acting_committer_verdict(Some(2), vec![obs], w);
            assert_eq!(v.kind(), "act", "{label} must not defer this device");
            assert!(v.defer_to_rank().is_none());
        }
    }

    #[test]
    fn beacon_observation_carries_its_two_fields_verbatim() {
        // The pair travels as ONE object, so the rank and the age cannot be
        // paired up wrongly the way two parallel arrays could.
        let obs = BeaconObservation::new(Some(4), 1_234);
        assert_eq!(obs.rank(), Some(4));
        assert_eq!(obs.age_ms(), 1_234);
        assert_eq!(BeaconObservation::new(None, 0).rank(), None);
    }

    #[test]
    fn emit_throttle_gates_on_candidacy_then_cadence() {
        let interval = committer_beacon_interval_ms();

        assert!(!should_emit_beacon(None, None, 0));
        assert!(!should_emit_beacon(None, Some(0), u64::MAX));
        assert!(should_emit_beacon(Some(0), None, 0));
        assert!(should_emit_beacon(Some(7), None, 12_345));
        assert!(!should_emit_beacon(
            Some(1),
            Some(1_000),
            1_000 + interval - 1
        ));
        assert!(should_emit_beacon(Some(1), Some(1_000), 1_000 + interval));
        assert!(!should_emit_beacon(Some(1), Some(u64::MAX), 0));
    }

    /// **An absent `ownRank` must never read as rank `0`.** `0` is the owner,
    /// who acts immediately; absence means "not in the committer set at all".
    /// Collapsing the two would make every plain member believe it is the owner
    /// and sweep.
    ///
    /// This is the string-tagged analogue of
    /// `outgoing_owner_variants_are_never_zero`: an OMITTED JS argument reaches
    /// the ABI as absent, wasm-bindgen encodes `None` for a `u32` as a sentinel
    /// rather than as `0`, and the verdict it lands on is the one that never
    /// sweeps and never emits.
    #[test]
    fn an_absent_own_rank_is_never_rank_zero() {
        let w = committer_beacon_freshness_window_ms();
        assert_eq!(
            acting_committer_verdict(None, Vec::new(), w).kind(),
            "not-a-candidate",
        );
        assert_eq!(
            acting_committer_verdict(Some(0), Vec::new(), w).kind(),
            "act"
        );
        assert!(!should_emit_beacon(None, None, 0));
        assert!(should_emit_beacon(Some(0), None, 0));
    }

    #[test]
    fn the_exposed_constants_are_the_cores_constants() {
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
        assert_eq!(
            committer_beacon_freshness_window_ms(),
            2 * committer_beacon_interval_ms() + committer_beacon_worst_case_poll_ms(),
        );
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
/// documentation names a stalking risk. A `bigint` on the JS side.
#[wasm_bindgen(js_name = callInviteExpiryMs)]
#[must_use]
pub fn call_invite_expiry_ms(now_ms: u64) -> u64 {
    rust_call_invite_expiry_ms(now_ms)
}

/// How many people a link to a live call admits: no limit.
///
/// The host admits every arrival, so a count here would cap a meeting at the
/// door of a room the host was holding open.
#[wasm_bindgen(js_name = callInviteMaxUses)]
#[must_use]
pub fn call_invite_max_uses() -> u32 {
    RUST_CALL_INVITE_MAX_USES
}
