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
    decode_and_verify as rust_call_invite_decode, open_link as rust_call_invite_open_link,
    seal_link as rust_call_invite_seal_link, CallInviteCapability as RustCallInviteCapability,
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
use lakko_contacts::group_closed::{
    compose_group_closed as rust_compose_group_closed,
    decode_group_closed as rust_decode_group_closed,
    group_closed_is_newer as rust_group_closed_is_newer, GroupClosed as RustGroupClosed,
    GroupClosedError, GROUP_CLOSED_ENVELOPE_MAX_LEN, GROUP_CLOSED_ENVELOPE_MIN_LEN,
    SUCCESSOR_GROUP_ID_LEN,
};
use lakko_contacts::group_invite::{
    compose as rust_group_invite_compose,
    compose_with_preview as rust_group_invite_compose_with_preview,
    decode_and_verify as rust_group_invite_decode, generate_link_key as rust_generate_link_key,
    group_sign_revocation as rust_group_sign_revocation, open_link as rust_group_invite_open_link,
    seal_link as rust_group_invite_seal_link, GroupInviteCapability as RustGroupInviteCapability,
    GroupInvitePreview as RustGroupInvitePreview,
    LINK_SECRET_B64URL_LEN as GROUP_INVITE_LINK_SECRET_B64URL_LEN,
    LINK_SECRET_LEN as GROUP_INVITE_LINK_SECRET_LEN, MAX_GROUP_ID_LEN,
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
    decode_and_verify as rust_invite_decode, open_link as rust_invite_open_link,
    revocation_public_key as rust_revocation_public_key, seal_link as rust_invite_seal_link,
    sign_revocation as rust_sign_revocation, BLOB_ID_B64URL_LEN, BLOB_ID_LEN,
};
use lakko_contacts::link_seal::{
    generate_link_secret as rust_generate_link_secret,
    link_secret_from_slice as rust_link_secret_from_slice,
    LINK_SECRET_B64URL_LEN as INVITE_LINK_SECRET_B64URL_LEN,
    LINK_SECRET_LEN as INVITE_LINK_SECRET_LEN,
};
use lakko_contacts::media_state::{
    compose_media_state as rust_compose_media_state, decode_media_state as rust_decode_media_state,
};
use lakko_contacts::pin::{
    compose_pin as rust_compose_pin, decode_pin as rust_decode_pin,
    pin_is_newer as rust_pin_is_newer, PinAction as RustPinAction, PinError, PinOp as RustPinOp,
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
use lakko_contacts::shared_channels::{
    compose_shared_channels as rust_compose_shared_channels,
    decode_shared_channels as rust_decode_shared_channels,
    shared_channels_is_newer as rust_shared_channels_is_newer, SharedChannel as RustSharedChannel,
    SharedChannels as RustSharedChannels, SharedChannelsError, SHARED_CHANNEL_ID_LEN,
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

use crate::scoped_discovery::{
    CommitterDelegationEnvelope, IntroductionsBundle, ReachGrant, ReachState, VisibilitySetting,
};

fn contact_err(e: ContactError) -> JsError {
    JsError::new(&format!("{e}"))
}

/// A fresh random 16-byte link id (ADR-0060 §6): the scope of an `/i/` or
/// `/c/` link's `link` reach grant. Mint one per link.
#[wasm_bindgen(js_name = generateLinkId)]
#[must_use]
pub fn generate_link_id() -> Vec<u8> {
    lakko_contacts::invite::generate_link_id().to_vec()
}

/// Width in bytes of a link id: 16.
#[wasm_bindgen(js_name = linkIdLen)]
#[must_use]
pub fn link_id_len() -> usize {
    lakko_contacts::invite::LINK_ID_LEN
}

fn link_id16(bytes: &[u8]) -> Result<[u8; lakko_contacts::invite::LINK_ID_LEN], JsError> {
    <[u8; lakko_contacts::invite::LINK_ID_LEN]>::try_from(bytes).map_err(|_| {
        JsError::new(&format!(
            "linkId must be {} bytes, got {}",
            lakko_contacts::invite::LINK_ID_LEN,
            bytes.len()
        ))
    })
}

fn crypto_err(e: lakko_crypto::error::Error) -> JsError {
    JsError::new(&format!("{e}"))
}

// ============================================================================
// Sealed invite links — the 2026-09-28 security pass (ADR-0028, ADR-0009)
// ============================================================================

/// Thrown by every `openLink`: an `Error` whose `inviteLinkError` property is
/// `"damaged"` for a missing or wrong fragment secret, a blob of another kind,
/// a truncated, tampered or substituted blob, or a capability that does not
/// verify. Any other failure (none is reachable today) keeps its message and
/// carries no code.
fn invite_link_error(e: ContactError) -> JsValue {
    match e {
        ContactError::LinkDamaged => coded_error(&e.to_string(), "inviteLinkError", "damaged"),
        other => JsError::new(&format!("{other}")).into(),
    }
}

/// What every invite compose returns (`/i/`, `/c/`, `/g/`): the sealed blob
/// to upload (or base64url-encode into the inline `/<k>/<sealed>#<secret>`
/// link), the fresh 12-byte link secret for the URL fragment only, and the
/// issuer's 32-byte Ed25519 key the upload carries in clear for the revoke.
#[wasm_bindgen(js_name = InviteLinkMint)]
pub struct WasmInviteLinkMint {
    sealed: Vec<u8>,
    link_secret: Vec<u8>,
    issuer_pubkey: Vec<u8>,
}

#[wasm_bindgen(js_class = InviteLinkMint)]
impl WasmInviteLinkMint {
    /// The sealed link blob.
    #[wasm_bindgen(getter)]
    pub fn sealed(&self) -> Vec<u8> {
        self.sealed.clone()
    }

    /// The link secret (12 bytes) for the URL fragment.
    #[wasm_bindgen(getter, js_name = linkSecret)]
    pub fn link_secret(&self) -> Vec<u8> {
        self.link_secret.clone()
    }

    /// The issuer's Ed25519 verifying key (32 bytes), for the upload body.
    #[wasm_bindgen(getter, js_name = issuerPubkey)]
    pub fn issuer_pubkey(&self) -> Vec<u8> {
        self.issuer_pubkey.clone()
    }
}

fn seal_mint(
    signing: &SigningKey,
    seal: impl FnOnce(&[u8; INVITE_LINK_SECRET_LEN]) -> Result<Vec<u8>, ContactError>,
    secret: [u8; INVITE_LINK_SECRET_LEN],
) -> Result<WasmInviteLinkMint, JsError> {
    let sealed = seal(&secret).map_err(contact_err)?;
    Ok(WasmInviteLinkMint {
        sealed,
        link_secret: secret.to_vec(),
        issuer_pubkey: signing.verifying_key().to_bytes().to_vec(),
    })
}

/// Width in bytes of an invite link secret (the URL fragment), every kind: 12.
#[wasm_bindgen(js_name = inviteLinkSecretLen)]
#[must_use]
pub fn invite_link_secret_len() -> u32 {
    u32::try_from(INVITE_LINK_SECRET_LEN).unwrap_or(u32::MAX)
}

/// Length of an invite link secret in the URL fragment: base64url without
/// padding, 16 characters. Any other fragment length (or none) is a damaged
/// link.
#[wasm_bindgen(js_name = inviteLinkSecretUrlLen)]
#[must_use]
pub fn invite_link_secret_url_len() -> u32 {
    u32::try_from(INVITE_LINK_SECRET_B64URL_LEN).unwrap_or(u32::MAX)
}

/// A real `Error` carrying one stable reason code as an own property —
/// the `mls.rs` `refusalReason` pattern. The message keeps the core's
/// sentence (never the rejected input); a client branches on the property.
pub(crate) fn coded_error(message: &str, property: &str, code: &str) -> JsValue {
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

/// Thrown for a pin envelope refused by its wire shape: an `Error` with a
/// `pinError` code — `"wrongDomain"`, `"badLength"`, `"unknownVersion"`,
/// `"unknownAction"`.
fn pin_error_value(e: PinError) -> JsValue {
    coded_error(&e.to_string(), "pinError", e.code())
}

/// Thrown for a group-closed envelope refused by its wire shape: an `Error`
/// with a `groupClosedError` code — `"wrongDomain"`, `"truncated"`,
/// `"trailingBytes"`, `"unknownVersion"`, `"badSuccessorLen"`.
fn group_closed_error_value(e: GroupClosedError) -> JsValue {
    coded_error(&e.to_string(), "groupClosedError", e.code())
}

/// Thrown for a shared-channels envelope refused by its bounds or its wire
/// shape: an `Error` with a `sharedChannelsError` code — `"handle.tooShort"`,
/// `"name.empty"`, `"duplicateChannel"`, `"tooManyChannels"`, `"truncated"`, …
fn shared_channels_error_value(e: SharedChannelsError) -> JsValue {
    coded_error(&e.to_string(), "sharedChannelsError", &e.code())
}

/// Turn caller-supplied fields into the canonical core profile: trim the
/// display name (blank = no name), normalise + check a non-empty username
/// (`None` or empty = no username).
fn prepare_profile(
    display_name: Option<String>,
    username: Option<String>,
    updated_at_ms: u64,
    account_id: Option<Vec<u8>>,
    username_certificate: Option<Vec<u8>>,
    grants: Option<Vec<ReachGrant>>,
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
        username_certificate,
        grants: grants
            .unwrap_or_default()
            .iter()
            .map(|g| g.rust().clone())
            .collect(),
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
    /// `replyGrant` (ADR-0060 §6) is the sender's `reply` reach grant, sealed
    /// beside the profile so the recipient can write the accept and the ack
    /// back to the sender's closed mailbox: `deriveReachGrant(accountSeed,
    /// ReachClass.Reply, replyId, 0)` over a fresh random 16-byte `replyId`,
    /// its public key registered with a 7-day expiry BEFORE sending and
    /// removed once the request is answered. `undefined` only when the
    /// sender's mailbox is open to everyone. A cold-start call join (a
    /// `wrapCallKeyPackage` / `wrapCallWelcome` intro) carries it too. The
    /// `ReachGrant` handle is consumed: read `verifyingKey()` first.
    ///
    /// `linkAnswerId` and `linkAnswerSecret` (ADR-0079 §2) name the `/i/`
    /// link the request is sent from: the capability's `linkId` (16 bytes)
    /// and the link secret from the URL fragment (12 bytes). Pass two EMPTY
    /// arrays for a request that does not come from an `/i/` link.
    ///
    /// # Errors
    ///
    /// Throws on any input-shape or AEAD failure, a reply grant of any
    /// class but `Reply`, or a link answer of the wrong width; throws an
    /// `Error` with a `profileError` code when `profileEnvelope` is
    /// non-empty and invalid.
    #[wasm_bindgen]
    #[allow(clippy::too_many_arguments)]
    pub fn compose(
        sender_signing_seed: &[u8],
        sender_account_id: &[u8],
        recipient_x25519_pub: &[u8],
        from_pseudonym: &[u8],
        valid_after_ms: u64,
        privacy_pass_token: &[u8],
        profile_envelope: &[u8],
        reply_grant: Option<ReachGrant>,
        link_answer_id: &[u8],
        link_answer_secret: &[u8],
        intro_message: &[u8],
    ) -> Result<Vec<u8>, JsValue> {
        let link_answer = if link_answer_id.is_empty() && link_answer_secret.is_empty() {
            None
        } else {
            Some(lakko_contacts::LinkAnswer {
                link_id: <[u8; 16]>::try_from(link_answer_id)
                    .map_err(|_| JsError::new("linkAnswerId must be 16 bytes"))?,
                link_secret: <[u8; 12]>::try_from(link_answer_secret)
                    .map_err(|_| JsError::new("linkAnswerSecret must be 12 bytes"))?,
            })
        };
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
            reply_grant.as_ref().map(ReachGrant::rust),
            link_answer.as_ref(),
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
        let opened = rust_open(&self.inner, &priv_key).map_err(contact_err)?;
        Ok(OpenedContactRequest {
            profile: opened.profile.map(WasmProfile::from),
            reply_grant: opened.reply_grant.map(ReachGrant::from_rust),
            link_answer: opened.link_answer,
            intro_message: opened.intro,
        })
    }
}

/// Result of `ContactRequest.openEnvelope` (ADR-0048 §2).
#[wasm_bindgen]
pub struct OpenedContactRequest {
    profile: Option<WasmProfile>,
    reply_grant: Option<ReachGrant>,
    link_answer: Option<lakko_contacts::LinkAnswer>,
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

    /// The requester's `reply` reach grant (ADR-0060 §6), or `undefined`
    /// when it sent none. SECRET — store encrypted, never log. Sign the
    /// accept (the one-to-one Welcome, or a call Welcome) and the ack back to
    /// the requester's mailbox with `DsProofSigner.forReach(replyGrant.seed)`.
    /// Always class `Reply`; a grant of any other class fails the open.
    #[wasm_bindgen(getter, js_name = replyGrant)]
    pub fn reply_grant(&self) -> Option<ReachGrant> {
        self.reply_grant.clone()
    }

    /// The 16-byte id of the `/i/` link the request answers (ADR-0079 §2),
    /// or `undefined`. Ask `SyncedState.linkAnswerVerdict` with it, the
    /// secret and the request's `fromPubkey`.
    #[wasm_bindgen(getter, js_name = linkAnswerId)]
    pub fn link_answer_id(&self) -> Option<Vec<u8>> {
        self.link_answer.as_ref().map(|a| a.link_id.to_vec())
    }

    /// The 12-byte secret of that link, or `undefined`. SECRET: never log.
    #[wasm_bindgen(getter, js_name = linkAnswerSecret)]
    pub fn link_answer_secret(&self) -> Option<Vec<u8>> {
        self.link_answer.as_ref().map(|a| a.link_secret.to_vec())
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
    username_certificate: Option<Vec<u8>>,
    grants: Vec<ReachGrant>,
}

impl From<RustProfile> for WasmProfile {
    fn from(p: RustProfile) -> Self {
        Self {
            display_name: p.display_name,
            username: p.username,
            updated_at_ms: p.updated_at_ms,
            account_id: p.account_id.map(|id| id.to_vec()),
            username_certificate: p.username_certificate,
            grants: p.grants.into_iter().map(ReachGrant::from_rust).collect(),
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

    /// The 153-byte discovery-signed username certificate (ADR-0060 §5), or
    /// `undefined`. Show the username only after `verifyUsernameBinding`
    /// accepts it for the OPRF of the claimed username and this account id.
    #[wasm_bindgen(getter, js_name = usernameCertificate)]
    pub fn username_certificate(&self) -> Option<Vec<u8>> {
        self.username_certificate.clone()
    }

    /// The reach grants handed to the receivers (ADR-0060 §3, §6), at most
    /// two (`Contact` / `Group` / `Member` / `Channel`, one per class).
    /// SECRET: store encrypted, never log. Keep a `Contact` grant ONLY from
    /// the other participant of a one-to-one conversation.
    #[wasm_bindgen(getter)]
    pub fn grants(&self) -> Vec<ReachGrant> {
        self.grants.clone()
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
/// `"displayName.controlOrFormatCharacter"`, `"username.tooShort"`, or the
/// ADR-0060 bounds `"certificate.length"`, `"certificate.withoutUsername"`,
/// `"grants.tooMany"`, `"grants.duplicateClass"`, `"grants.classNotAllowed"`.
///
/// `usernameCertificate` (153 bytes) and `grants` (at most two, `Contact` /
/// `Group` / `Member` / `Channel`, one per class) are optional: `undefined`
/// means none. A group's profile carries `Group` (only where the setting
/// allows that group) + `Member`; a one-to-one conversation's profile
/// carries `Contact` + `Member`; a channel card carries `Channel`.
#[wasm_bindgen(js_name = composeProfileEnvelope)]
pub fn compose_profile_envelope(
    display_name: Option<String>,
    username: Option<String>,
    updated_at_ms: u64,
    account_id: Option<Vec<u8>>,
    username_certificate: Option<Vec<u8>>,
    grants: Option<Vec<ReachGrant>>,
) -> Result<Vec<u8>, JsValue> {
    let profile = prepare_profile(
        display_name,
        username,
        updated_at_ms,
        account_id,
        username_certificate,
        grants,
    )?;
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

// ============================================================================
// Shared-pin envelope — ADR-0052 decision 3
// ============================================================================

/// One shared-pin operation on one message. Mirror of
/// [`lakko_contacts::pin::PinOp`].
///
/// **A pin received from someone else is that person's assertion about the
/// whole conversation, and the sender's authority is the RECEIVER's check**:
/// resolve the MLS sender leaf to the account key at receipt (C-68), read the
/// live role table, accept only from the owner or an admin (any member when
/// the group carries no role table; either participant of a one-to-one), then
/// keep it only when `pinIsNewer` than the stored stamp **for that message**,
/// the whole read-decide-write under the per-group lock. Persist the stamp
/// and re-emit it VERBATIM on a re-send.
#[wasm_bindgen(js_name = PinOp)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmPinOp {
    message_id: i64,
    pinned: bool,
    updated_at_ms: u64,
}

impl From<RustPinOp> for WasmPinOp {
    fn from(op: RustPinOp) -> Self {
        Self {
            message_id: op.message_id,
            pinned: matches!(op.action, RustPinAction::Pin),
            updated_at_ms: op.updated_at_ms,
        }
    }
}

#[wasm_bindgen(js_class = PinOp)]
impl WasmPinOp {
    /// DS `messageId` of the message, as a `bigint`.
    #[wasm_bindgen(getter, js_name = messageId)]
    pub fn message_id(&self) -> i64 {
        self.message_id
    }

    /// `true` = pinned for everyone; `false` = no longer pinned.
    #[wasm_bindgen(getter)]
    pub fn pinned(&self) -> bool {
        self.pinned
    }

    /// Sender-clock UNIX milliseconds, as a `bigint`.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    pub fn updated_at_ms(&self) -> u64 {
        self.updated_at_ms
    }
}

/// Compose a shared-pin envelope (ADR-0052 decision 3) — the inner plaintext
/// to publish as an MLS application message on the conversation's journal.
/// Infallible: the envelope has no variable field.
#[wasm_bindgen(js_name = composePinEnvelope)]
#[must_use]
pub fn compose_pin_envelope(message_id: i64, pinned: bool, updated_at_ms: u64) -> Vec<u8> {
    rust_compose_pin(&RustPinOp {
        message_id,
        action: if pinned {
            RustPinAction::Pin
        } else {
            RustPinAction::Unpin
        },
        updated_at_ms,
    })
}

/// Strictly decode a shared-pin envelope (ADR-0052 decision 3). Fail-closed:
/// anything malformed is refused as a whole.
///
/// Use `dispatchIncomingEnvelope` when the plaintext might be any kind.
///
/// # Errors
///
/// Throws an `Error` with a `pinError` code (`"wrongDomain"`, `"badLength"`,
/// `"unknownVersion"`, `"unknownAction"`).
#[wasm_bindgen(js_name = decodePinEnvelope)]
pub fn decode_pin_envelope(plaintext: &[u8]) -> Result<WasmPinOp, JsValue> {
    rust_decode_pin(plaintext)
        .map(WasmPinOp::from)
        .map_err(pin_error_value)
}

/// `true` when a pin action stamped `incomingUpdatedAtMs` must replace the
/// stored action **for the same message**. Strictly greater wins; equal is
/// not newer, so a re-send of the stored action is not an update.
#[wasm_bindgen(js_name = pinIsNewer)]
#[must_use]
pub fn pin_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_pin_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

// ============================================================================
// Group-closed envelope — ADR-0073 phase 2
// ============================================================================

/// A group's closed notice (ADR-0073 phase 2, `docs/ux/identity-recovery.md`
/// §16.5). Mirror of [`lakko_contacts::group_closed::GroupClosed`].
///
/// **A notice received from someone else is that person's assertion about
/// the whole group, and the sender's authority is the RECEIVER's check**:
/// resolve the MLS sender leaf to the account key at receipt (C-68), read the
/// live role table, accept only from the owner or an admin (a plain member's
/// notice is dropped), then keep it only when `groupClosedIsNewer` than the
/// stored stamp **for that group**, the whole read-decide-write under the
/// per-group lock. Persist the stamp and re-emit it VERBATIM on a growth
/// re-send. The successor id is the admin's claim: offer `Open the new
/// group` only once this device has joined it through the ordinary
/// invitation path.
#[wasm_bindgen(js_name = GroupClosed)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmGroupClosed {
    closed_at_ms: u64,
    successor_group_id: Option<[u8; SUCCESSOR_GROUP_ID_LEN]>,
}

impl From<RustGroupClosed> for WasmGroupClosed {
    fn from(c: RustGroupClosed) -> Self {
        Self {
            closed_at_ms: c.closed_at_ms,
            successor_group_id: c.successor_group_id,
        }
    }
}

#[wasm_bindgen(js_class = GroupClosed)]
impl WasmGroupClosed {
    /// Closer-clock UNIX milliseconds, as a `bigint`.
    #[wasm_bindgen(getter, js_name = closedAtMs)]
    pub fn closed_at_ms(&self) -> u64 {
        self.closed_at_ms
    }

    /// The recreated group's 32-byte id, or `undefined` when the group was
    /// closed without a successor.
    #[wasm_bindgen(getter, js_name = successorGroupId)]
    pub fn successor_group_id(&self) -> Option<Vec<u8>> {
        self.successor_group_id.map(|id| id.to_vec())
    }
}

/// Compose a group-closed envelope (ADR-0073 phase 2) — the inner plaintext
/// to publish as an MLS application message on the OLD group's journal.
/// `successorGroupId` is the recreated group's 32-byte id, or `undefined`
/// when the group is closed without one.
///
/// # Errors
///
/// Throws an `Error` with a `groupClosedError` code `"badSuccessorLen"`
/// when the successor is present but not 32 bytes — refused before any
/// bytes exist, since a receiver would refuse the envelope as a whole.
#[wasm_bindgen(js_name = composeGroupClosedEnvelope)]
pub fn compose_group_closed_envelope(
    closed_at_ms: u64,
    successor_group_id: Option<Vec<u8>>,
) -> Result<Vec<u8>, JsValue> {
    let successor_group_id = successor_group_id
        .map(|id| {
            <[u8; SUCCESSOR_GROUP_ID_LEN]>::try_from(id.as_slice()).map_err(|_| {
                group_closed_error_value(GroupClosedError::BadSuccessorLen(
                    u8::try_from(id.len()).unwrap_or(u8::MAX),
                ))
            })
        })
        .transpose()?;
    Ok(rust_compose_group_closed(&RustGroupClosed {
        closed_at_ms,
        successor_group_id,
    }))
}

/// Strictly decode a group-closed envelope (ADR-0073 phase 2). Fail-closed:
/// anything malformed is refused as a whole.
///
/// Use `dispatchIncomingEnvelope` when the plaintext might be any kind.
///
/// # Errors
///
/// Throws an `Error` with a `groupClosedError` code (`"wrongDomain"`,
/// `"truncated"`, `"trailingBytes"`, `"unknownVersion"`,
/// `"badSuccessorLen"`).
#[wasm_bindgen(js_name = decodeGroupClosedEnvelope)]
pub fn decode_group_closed_envelope(plaintext: &[u8]) -> Result<WasmGroupClosed, JsValue> {
    rust_decode_group_closed(plaintext)
        .map(WasmGroupClosed::from)
        .map_err(group_closed_error_value)
}

/// `true` when a closed notice stamped `incomingClosedAtMs` must replace the
/// stored one **for the same group**. Strictly greater wins; equal is not
/// newer, so a growth re-send of the stored notice is not an update.
#[wasm_bindgen(js_name = groupClosedIsNewer)]
#[must_use]
pub fn group_closed_is_newer(incoming_closed_at_ms: u64, stored_closed_at_ms: u64) -> bool {
    rust_group_closed_is_newer(incoming_closed_at_ms, stored_closed_at_ms)
}

/// Shortest well-formed group-closed envelope in bytes (closed without a
/// successor).
#[wasm_bindgen(js_name = groupClosedEnvelopeMinLen)]
#[must_use]
pub fn group_closed_envelope_min_len() -> u32 {
    u32::try_from(GROUP_CLOSED_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// Longest well-formed group-closed envelope in bytes (closed with a
/// successor). There is no length in between.
#[wasm_bindgen(js_name = groupClosedEnvelopeMaxLen)]
#[must_use]
pub fn group_closed_envelope_max_len() -> u32 {
    u32::try_from(GROUP_CLOSED_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

// ============================================================================
// Shared-channels envelope — ADR-0059 §6
// ============================================================================

/// The listed channels one member shares with one group (ADR-0059 §6).
/// Entry `i` is `channelIds[16*i .. 16*i+16]`, `handles[i]`, `names[i]`.
///
/// **Speaks only for its sender.** Store it per member, keyed on the sender's
/// account key resolved from the MLS sender leaf at receipt (C-68), only when
/// `sharedChannelsIsNewer` than the list stored for that member, under the
/// per-group lock. Every handle is the sender's claim: resolve it through the
/// OPRF lookup to the same channel id before subscribing. An empty list means
/// the sender stopped sharing.
#[wasm_bindgen(js_name = SharedChannels)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmSharedChannels {
    updated_at_ms: u64,
    channel_ids: Vec<u8>,
    handles: Vec<String>,
    names: Vec<String>,
}

impl From<RustSharedChannels> for WasmSharedChannels {
    fn from(l: RustSharedChannels) -> Self {
        let mut channel_ids = Vec::with_capacity(l.channels.len() * SHARED_CHANNEL_ID_LEN);
        let mut handles = Vec::with_capacity(l.channels.len());
        let mut names = Vec::with_capacity(l.channels.len());
        for c in l.channels {
            channel_ids.extend_from_slice(&c.channel_id);
            handles.push(c.handle);
            names.push(c.name);
        }
        Self {
            updated_at_ms: l.updated_at_ms,
            channel_ids,
            handles,
            names,
        }
    }
}

#[wasm_bindgen(js_class = SharedChannels)]
impl WasmSharedChannels {
    /// Sender-clock UNIX milliseconds, as a `bigint`.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    pub fn updated_at_ms(&self) -> u64 {
        self.updated_at_ms
    }

    /// How many channels the list carries.
    #[wasm_bindgen(getter)]
    pub fn count(&self) -> usize {
        self.handles.len()
    }

    /// The channel ids, 16 bytes each, concatenated in list order.
    #[wasm_bindgen(getter, js_name = channelIds)]
    pub fn channel_ids(&self) -> Vec<u8> {
        self.channel_ids.clone()
    }

    /// The handles, in list order.
    #[wasm_bindgen(getter)]
    pub fn handles(&self) -> Vec<String> {
        self.handles.clone()
    }

    /// The names, in list order.
    #[wasm_bindgen(getter)]
    pub fn names(&self) -> Vec<String> {
        self.names.clone()
    }
}

/// Compose a shared-channels envelope (ADR-0059 §6) — the inner plaintext to
/// publish as an MLS application message on the group's journal.
/// `channelIds` is the 16-byte ids concatenated; `handles` and `names` are
/// parallel to it.
///
/// # Errors
///
/// Throws an `Error` with a `sharedChannelsError` code, or a plain `Error`
/// when the three lists do not line up.
#[wasm_bindgen(js_name = composeSharedChannelsEnvelope)]
pub fn compose_shared_channels_envelope(
    updated_at_ms: u64,
    channel_ids: &[u8],
    handles: Vec<String>,
    names: Vec<String>,
) -> Result<Vec<u8>, JsValue> {
    if channel_ids.len() != handles.len() * SHARED_CHANNEL_ID_LEN || handles.len() != names.len() {
        return Err(JsValue::from_str(
            "channelIds must be 16 bytes per handle, and handles and names must be parallel",
        ));
    }
    let channels = channel_ids
        .chunks_exact(SHARED_CHANNEL_ID_LEN)
        .zip(handles.into_iter().zip(names))
        .map(|(id, (handle, name))| {
            let mut channel_id = [0u8; SHARED_CHANNEL_ID_LEN];
            channel_id.copy_from_slice(id);
            RustSharedChannel {
                channel_id,
                handle,
                name,
            }
        })
        .collect();
    rust_compose_shared_channels(&RustSharedChannels {
        updated_at_ms,
        channels,
    })
    .map_err(shared_channels_error_value)
}

/// Strictly decode a shared-channels envelope. Fail-closed.
///
/// # Errors
///
/// Throws an `Error` with a `sharedChannelsError` code.
#[wasm_bindgen(js_name = decodeSharedChannelsEnvelope)]
pub fn decode_shared_channels_envelope(plaintext: &[u8]) -> Result<WasmSharedChannels, JsValue> {
    rust_decode_shared_channels(plaintext)
        .map(WasmSharedChannels::from)
        .map_err(shared_channels_error_value)
}

/// `true` when a list stamped `incomingUpdatedAtMs` must replace the list
/// stored **for the same member**. Strictly greater; equal is not newer.
#[wasm_bindgen(js_name = sharedChannelsIsNewer)]
#[must_use]
pub fn shared_channels_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_shared_channels_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
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
    wire: Vec<u8>,
}

#[wasm_bindgen]
impl InviteCapability {
    /// Compose a signed invite capability and seal it for an `/i/` link under
    /// a fresh link secret. Returns an `InviteLinkMint`.
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
    /// `linkId` (16 random bytes, `generateLinkId()`) and `linkGrant`
    /// (ADR-0060 §6) scope the link's reach key: derive the grant as
    /// `deriveReachGrant(accountSeed, ReachClass.Link, linkId, version)`,
    /// register its public key with the Delivery Service BEFORE sharing the
    /// link, and remove it when the link is revoked or expires.
    ///
    /// `inviterDisplayName` and `inviterUsername` (ADR-0079 §1) name the owner
    /// on the invite page: the owner's display name (trimmed; blank or
    /// `undefined` = none) and username (normalised; blank or `undefined` =
    /// none), sealed with the rest of the capability.
    ///
    /// # Errors
    ///
    /// Throws on any input-shape failure, or a grant of any class but
    /// `Link`; an `Error` with a `profileError` code for a preview that
    /// breaks the ADR-0048 rules.
    #[wasm_bindgen]
    #[allow(clippy::too_many_arguments)]
    pub fn compose(
        owner_signing_seed: &[u8],
        owner_account_id: &[u8],
        owner_x25519_pubkey: &[u8],
        expiry_ms: u64,
        max_uses: u32,
        bootstrap_referral: bool,
        label: &str,
        link_id: &[u8],
        link_grant: &ReachGrant,
        inviter_display_name: Option<String>,
        inviter_username: Option<String>,
    ) -> Result<WasmInviteLinkMint, JsValue> {
        let link_id = link_id16(link_id)?;
        let preview = lakko_contacts::InvitePreview {
            display_name: match inviter_display_name {
                Some(name) => rust_normalize_display_name(&name)
                    .map_err(|e| profile_error_value(ProfileError::DisplayName(e)))?
                    .unwrap_or_default(),
                None => String::new(),
            },
            username: match inviter_username.as_deref().map(str::trim) {
                None | Some("") => String::new(),
                Some(raw) => lakko_discovery_client::validate_username(raw)
                    .map_err(|e| profile_error_value(ProfileError::Username(e)))?,
            },
        };
        if owner_signing_seed.len() != 32 {
            return Err(JsValue::from(JsError::new(
                "ownerSigningSeed must be 32 bytes",
            )));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(owner_signing_seed);
        let signing = SigningKey::from_seed(&seed);

        if owner_account_id.len() != lakko_contacts::ACCOUNT_ID_LEN {
            return Err(JsValue::from(JsError::new(&format!(
                "ownerAccountId must be {} bytes",
                lakko_contacts::ACCOUNT_ID_LEN,
            ))));
        }
        let mut acct = [0u8; lakko_contacts::ACCOUNT_ID_LEN];
        acct.copy_from_slice(owner_account_id);

        if owner_x25519_pubkey.len() != PUBLIC_KEY_LEN {
            return Err(JsValue::from(JsError::new(&format!(
                "ownerX25519Pubkey must be {PUBLIC_KEY_LEN} bytes",
            ))));
        }
        let mut x_arr = [0u8; PUBLIC_KEY_LEN];
        x_arr.copy_from_slice(owner_x25519_pubkey);
        let x25519_pub = X25519PublicKey::from_bytes(&x_arr).map_err(crypto_err)?;

        if label.len() > MAX_LABEL_LEN {
            return Err(JsValue::from(JsError::new(&format!(
                "label is {} bytes, max is {MAX_LABEL_LEN}",
                label.len(),
            ))));
        }

        let wire = rust_invite_compose(
            &signing,
            &acct,
            &x25519_pub,
            expiry_ms,
            max_uses,
            bootstrap_referral,
            label,
            &link_id,
            link_grant.rust(),
            &preview,
        )
        .map_err(contact_err)?;
        Ok(seal_mint(
            &signing,
            |secret| rust_invite_seal_link(secret, &wire),
            rust_generate_link_secret(),
        )?)
    }

    /// Decode PLAINTEXT capability wire bytes and verify the Ed25519
    /// signature. A link never carries plaintext since the 2026-09-28
    /// security pass — a joiner uses [`InviteCapability::openLink`]. Does
    /// NOT check expiry — call [`InviteCapability::isValidNow`].
    #[wasm_bindgen(js_name = decodeAndVerify)]
    pub fn decode_and_verify(wire: &[u8]) -> Result<InviteCapability, JsError> {
        let inner = rust_invite_decode(wire).map_err(contact_err)?;
        Ok(Self {
            inner,
            wire: wire.to_vec(),
        })
    }

    /// Joiner-side: open an `/i/` link's sealed blob (fetched by id, or the
    /// inline path segment) with the 12-byte secret from the URL fragment and
    /// verify the capability inside. Does NOT check expiry.
    ///
    /// # Errors
    ///
    /// Throws an `Error` with `inviteLinkError: "damaged"` on any failure.
    #[wasm_bindgen(js_name = openLink)]
    pub fn open_link(sealed: &[u8], link_secret: &[u8]) -> Result<InviteCapability, JsValue> {
        let secret = rust_link_secret_from_slice(link_secret).map_err(invite_link_error)?;
        let (inner, wire) = rust_invite_open_link(&secret, sealed).map_err(invite_link_error)?;
        Ok(Self { inner, wire })
    }

    /// The plaintext capability wire — what `computeJoinerInviteTag` takes.
    #[wasm_bindgen(getter)]
    pub fn wire(&self) -> Vec<u8> {
        self.wire.clone()
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

    /// The owner's display name as the link states it (ADR-0079 §1), or
    /// `undefined`. The owner's own claim: shown on the invite page, never
    /// used for identity.
    #[wasm_bindgen(getter, js_name = inviterDisplayName)]
    pub fn inviter_display_name(&self) -> Option<String> {
        Some(self.inner.preview.display_name.clone()).filter(|n| !n.is_empty())
    }

    /// The owner's username as the link states it (ADR-0079 §1), canonical,
    /// or `undefined`.
    #[wasm_bindgen(getter, js_name = inviterUsername)]
    pub fn inviter_username(&self) -> Option<String> {
        Some(self.inner.preview.username.clone()).filter(|u| !u.is_empty())
    }

    /// The link's 16-byte reach scope id (ADR-0060 §6).
    #[wasm_bindgen(getter, js_name = linkId)]
    pub fn link_id(&self) -> Vec<u8> {
        self.inner.link_id.to_vec()
    }

    /// The link's `link` reach grant (ADR-0060 §6). SECRET — the joiner
    /// writes its `ContactRequest` to the owner's mailbox with
    /// `DsProofSigner.forReach(linkGrant.seed)`, then drops it.
    #[wasm_bindgen(getter, js_name = linkGrant)]
    pub fn link_grant(&self) -> ReachGrant {
        ReachGrant::from_rust(self.inner.link_grant.clone())
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
/// 6-byte blob_id required).
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
/// 6-byte blob_id required).
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

/// The Ed25519 public key of a per-link revocation seed (ADR-0078
/// section 10): what an invite upload (`/i/`, `/c/`, `/g/`) stores in the
/// ledger in place of an owner or issuer. The client draws 32 random bytes
/// for each link it mints, keeps them beside the link secret, uploads this
/// key, and later signs a revocation with the seed.
///
/// # Errors
///
/// `JsError` when `seed` is not 32 bytes.
#[wasm_bindgen(js_name = linkRevocationPublicKey)]
pub fn link_revocation_public_key(seed: &[u8]) -> Result<Vec<u8>, JsError> {
    let seed: [u8; 32] = seed
        .try_into()
        .map_err(|_| JsError::new("seed must be 32 bytes"))?;
    Ok(rust_revocation_public_key(&seed).to_vec())
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

/// Compose + sign a call-invite capability (Phase 3c3) and seal it for a
/// `/c/` link under a fresh link secret (the 2026-09-28 security pass).
/// Returns an `InviteLinkMint`; the link is `/c/<id>#<secret>` after the
/// upload, or `/c/<base64url sealed>#<secret>` inline.
///
/// Same shape as `composeInviteCapability` but for a
/// link-based group-call invite. The owner embeds a
/// one-time `KeyPackage` minted via `Identity.createKeyPackage`;
/// the joiner uses it to bootstrap a fresh MLS group for
/// the call. `linkId` + `linkGrant` scope the link's reach key exactly as
/// `InviteCapability.compose` does (ADR-0060 §6).
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
    link_id: &[u8],
    link_grant: &ReachGrant,
) -> Result<WasmInviteLinkMint, JsError> {
    let link_id = link_id16(link_id)?;
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
        &link_id,
        link_grant.rust(),
    )
    .map_err(|e| JsError::new(&format!("{e}")))?;
    seal_mint(
        &signing,
        |secret| rust_call_invite_seal_link(secret, &wire),
        rust_generate_link_secret(),
    )
}

/// Composed and verified call-invite capability (Phase
/// 3c3). Mirror of [`lakko_contacts::CallInviteCapability`].
#[wasm_bindgen]
pub struct CallInviteCapability {
    pub(crate) inner: RustCallInviteCapability,
    wire: Vec<u8>,
}

#[wasm_bindgen]
impl CallInviteCapability {
    /// Decode PLAINTEXT capability wire bytes and verify the Ed25519
    /// signature. A link never carries plaintext — a joiner uses `openLink`.
    #[wasm_bindgen(js_name = decodeAndVerify)]
    pub fn decode_and_verify(wire: &[u8]) -> Result<CallInviteCapability, JsError> {
        let inner = rust_call_invite_decode(wire).map_err(|e| JsError::new(&format!("{e}")))?;
        Ok(Self {
            inner,
            wire: wire.to_vec(),
        })
    }

    /// Joiner-side: open a `/c/` link's sealed blob with the 12-byte secret
    /// from the URL fragment and verify the capability inside. Without the
    /// secret a fetched blob is not a seat in the call. Does NOT check expiry.
    ///
    /// # Errors
    ///
    /// Throws an `Error` with `inviteLinkError: "damaged"` on any failure.
    #[wasm_bindgen(js_name = openLink)]
    pub fn open_link(sealed: &[u8], link_secret: &[u8]) -> Result<CallInviteCapability, JsValue> {
        let secret = rust_link_secret_from_slice(link_secret).map_err(invite_link_error)?;
        let (inner, wire) =
            rust_call_invite_open_link(&secret, sealed).map_err(invite_link_error)?;
        Ok(Self { inner, wire })
    }

    /// The plaintext capability wire — what `computeJoinerInviteTag` takes.
    #[wasm_bindgen(getter)]
    pub fn wire(&self) -> Vec<u8> {
        self.wire.clone()
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

    /// The link's 16-byte reach scope id (ADR-0060 §6).
    #[wasm_bindgen(getter, js_name = linkId)]
    pub fn link_id(&self) -> Vec<u8> {
        self.inner.link_id.to_vec()
    }

    /// The link's `link` reach grant (ADR-0060 §6). SECRET — the joiner
    /// writes its `ContactRequest` to the owner's mailbox with
    /// `DsProofSigner.forReach(linkGrant.seed)`, then drops it.
    #[wasm_bindgen(getter, js_name = linkGrant)]
    pub fn link_grant(&self) -> ReachGrant {
        ReachGrant::from_rust(self.inner.link_grant.clone())
    }

    /// The owner's mailbox key the link grant names, verified against the
    /// owner's ACCOUNT key (the one-time KeyPackage's device binding, or the
    /// leaf key of an unbound leaf) — ADR-0078 phase 1. A joiner writes under
    /// the link grant only to this key; a throw means the link is damaged.
    ///
    /// # Errors
    ///
    /// Throws when the KeyPackage or either binding does not verify.
    #[wasm_bindgen(js_name = verifiedLinkGrantMailboxPk)]
    pub fn verified_link_grant_mailbox_pk(&self) -> Result<Vec<u8>, JsError> {
        lakko_calls::link_invite::verified_link_grant_mailbox_pk(&self.inner)
            .map(|pk| pk.to_vec())
            .map_err(|e| JsError::new(&format!("call link grant: {e}")))
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
    /// A shared pin or unpin of one message (ADR-0052 decision 3). The
    /// decoded operation rides the `pin` getter of [`WasmIncomingEnvelope`];
    /// `payload` is empty. Never render it as a chat bubble; the sender's
    /// authority is the receiver's role-table check, then `pinIsNewer` for
    /// that message.
    Pin = 9,
    /// A journal root hands one member its Delivery Service registrar seed
    /// (ADR-0057). `payload` is the whole envelope, for
    /// `decodeRegistrarSeedEnvelope` / `openRegistrarSeedEnvelope`. Never
    /// render it; keep it only when addressed to the own account key and
    /// the sender resolves (C-68) to the group's owner.
    RegistrarSeed = 10,
    /// A contact's introduction record to relay (ADR-0060 §4). The record
    /// wire rides the `introductionRecord` getter; `payload` is empty. Never
    /// render it; accept it only from the other participant of a 1:1
    /// conversation and keep the newest per contact.
    IntroductionOffer = 11,
    /// A contact's bundle of introduction records (ADR-0060 §4), on the
    /// `introductions` getter. Keep the newest per contact via
    /// `bundleIsNewer`, search it locally, never re-relay it.
    Introductions = 12,
    /// The owner's "who can find me" setting from another of their devices
    /// (ADR-0060 §7), on the `visibility` getter. Accept only on the owner's
    /// DeviceGroup, keep via `visibilityIsNewer`.
    Visibility = 13,
    /// The owner's reach-key version counters (ADR-0060 §7), on the
    /// `reachState` getter. Same rules, ordered by `reachStateIsNewer`.
    ReachState = 14,
    /// A proposer's committer delegations (ADR-0060 §6), on the
    /// `committerDelegation` getter; `payload` is empty. Never render it.
    /// Keep the newest per `(MLS sender resolved at receipt,
    /// addedAccountKey)`; when this device commits a sweep that added that
    /// person, get its delegation with `committerDelegationFor` and deliver
    /// the Welcome under it.
    CommitterDelegation = 15,
    /// The listed channels one member shares with the group (ADR-0059 §6).
    /// The decoded list rides the `sharedChannels` getter; `payload` is
    /// empty. Never render it as a chat bubble; store it per member only
    /// when `sharedChannelsIsNewer`.
    SharedChannels = 16,
    /// An admin closed this group and names the recreated one (ADR-0073
    /// phase 2). The decoded notice rides the `groupClosed` getter; `payload`
    /// is empty. Never render it as a chat bubble; the sender's authority is
    /// the receiver's role-table check (owner / admin only), then
    /// `groupClosedIsNewer` for that group.
    GroupClosed = 17,
    /// Another device of this account is changing the account keys
    /// (ADR-0074 section 3.3). The decoded envelope rides the `keyRotation`
    /// getter; `payload` is empty. Never render it. Meaningful ONLY in the
    /// account's own DeviceGroup: met anywhere else, drop it. There, pass
    /// the original plaintext to `verifyKeyRotationEnvelope` with the
    /// account key this device holds, store the result as pending, and
    /// adopt it only once the server-side change is visible. It carries the
    /// new account's entropy: never log it.
    KeyRotation = 18,
    /// What another device of this account says about itself (ADR-0080).
    /// The decoded card rides the `deviceCard` getter; `payload` is empty.
    /// Never render it. Meaningful ONLY in the account's own DeviceGroup:
    /// store it under the sender's leaf signature key resolved at receipt,
    /// keep it only when `deviceCardIsNewer`, drop it when the leaf leaves.
    DeviceCard = 19,
    /// Another device of this account claims a contact request sent through
    /// one of the account's invite links (ADR-0079, reworked 2026-10-17).
    /// The decoded claim rides the `linkClaim` getter; `payload` is empty.
    /// Never render it. Meaningful ONLY in the account's own DeviceGroup.
    LinkClaim = 20,
    /// The device that won an invite-link request names the one-to-one it
    /// founded, with the founding Welcome (ADR-0079, ordering of 2026-10-17).
    /// The decoded note rides the `linkFounded` getter; `payload` is empty.
    /// Never render it. Meaningful ONLY in the account's own DeviceGroup.
    LinkFounded = 21,
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
    pin: Option<WasmPinOp>,
    shared_channels: Option<WasmSharedChannels>,
    introduction_record: Option<Vec<u8>>,
    introductions: Option<IntroductionsBundle>,
    visibility: Option<VisibilitySetting>,
    reach_state: Option<ReachState>,
    committer_delegation: Option<CommitterDelegationEnvelope>,
    group_closed: Option<WasmGroupClosed>,
    key_rotation: Option<crate::key_rotation::WasmKeyRotation>,
    device_card: Option<crate::device_card::WasmDeviceCard>,
    link_claim: Option<crate::link_claim::WasmLinkClaim>,
    link_founded: Option<crate::link_founded::WasmLinkFounded>,
}

impl WasmIncomingEnvelope {
    /// A result of `kind` with every kind-specific field empty; each arm of
    /// the dispatcher fills in its own.
    const fn of(kind: WasmIncomingEnvelopeKind) -> Self {
        Self {
            kind,
            payload: Vec::new(),
            camera_on: None,
            mint_epoch: None,
            profile: None,
            group_name: None,
            pin: None,
            introduction_record: None,
            introductions: None,
            visibility: None,
            reach_state: None,
            committer_delegation: None,
            shared_channels: None,
            group_closed: None,
            key_rotation: None,
            device_card: None,
            link_claim: None,
            link_founded: None,
        }
    }
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

    /// Set only for `Pin` (ADR-0052): the decoded pin operation. `undefined`
    /// for every other kind.
    #[wasm_bindgen(getter)]
    pub fn pin(&self) -> Option<WasmPinOp> {
        self.pin.clone()
    }

    /// Set only for `SharedChannels` (ADR-0059): the decoded list.
    /// `undefined` for every other kind.
    #[wasm_bindgen(getter, js_name = sharedChannels)]
    pub fn shared_channels(&self) -> Option<WasmSharedChannels> {
        self.shared_channels.clone()
    }

    /// Set only for `IntroductionOffer` (ADR-0060): the record wire,
    /// structurally checked and still sealed. `undefined` otherwise.
    #[wasm_bindgen(getter, js_name = introductionRecord)]
    pub fn introduction_record(&self) -> Option<Vec<u8>> {
        self.introduction_record.clone()
    }

    /// Set only for `Introductions` (ADR-0060): the decoded bundle.
    /// `undefined` otherwise.
    #[wasm_bindgen(getter)]
    pub fn introductions(&self) -> Option<IntroductionsBundle> {
        self.introductions.clone()
    }

    /// Set only for `Visibility` (ADR-0060): the decoded setting. `undefined`
    /// otherwise.
    #[wasm_bindgen(getter)]
    pub fn visibility(&self) -> Option<VisibilitySetting> {
        self.visibility.clone()
    }

    /// Set only for `ReachState` (ADR-0060): the decoded reach state.
    /// `undefined` otherwise.
    #[wasm_bindgen(getter, js_name = reachState)]
    pub fn reach_state(&self) -> Option<ReachState> {
        self.reach_state.clone()
    }

    /// Set only for `CommitterDelegation` (ADR-0060 §6): the envelope,
    /// structurally checked. `undefined` for every other kind.
    #[wasm_bindgen(getter, js_name = committerDelegation)]
    pub fn committer_delegation(&self) -> Option<CommitterDelegationEnvelope> {
        self.committer_delegation.clone()
    }

    /// Set only for `GroupClosed` (ADR-0073): the decoded notice. `undefined`
    /// for every other kind.
    #[wasm_bindgen(getter, js_name = groupClosed)]
    pub fn group_closed(&self) -> Option<WasmGroupClosed> {
        self.group_closed.clone()
    }

    /// Set only for `KeyRotation` (ADR-0074): the decoded envelope, shape
    /// only. `undefined` for every other kind.
    #[wasm_bindgen(getter, js_name = keyRotation)]
    pub fn key_rotation(&self) -> Option<crate::key_rotation::WasmKeyRotation> {
        self.key_rotation.clone()
    }

    /// Set only for `DeviceCard` (ADR-0080): the decoded card. `undefined`
    /// for every other kind.
    #[wasm_bindgen(getter, js_name = deviceCard)]
    pub fn device_card(&self) -> Option<crate::device_card::WasmDeviceCard> {
        self.device_card.clone()
    }

    /// Set only for `LinkClaim` (ADR-0079): the decoded claim. `undefined`
    /// for every other kind.
    #[wasm_bindgen(getter, js_name = linkClaim)]
    pub fn link_claim(&self) -> Option<crate::link_claim::WasmLinkClaim> {
        self.link_claim
    }

    /// Set only for `LinkFounded` (ADR-0079): the decoded note. `undefined`
    /// for every other kind.
    #[wasm_bindgen(getter, js_name = linkFounded)]
    pub fn link_founded(&self) -> Option<crate::link_founded::WasmLinkFounded> {
        self.link_founded.clone()
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
// One flat arm per envelope kind; splitting the match would only move it.
#[allow(clippy::too_many_lines)]
#[wasm_bindgen(js_name = dispatchIncomingEnvelope)]
pub fn dispatch_incoming_envelope(plaintext: &[u8]) -> Result<WasmIncomingEnvelope, JsError> {
    dispatch_incoming_envelope_inner(plaintext).map_err(|msg| JsError::new(&msg))
}

/// The committer-delegation field of a dispatched plaintext, for host tests
/// of the ADR-0060 §6 surface in `scoped_discovery.rs`.
#[cfg(test)]
pub(crate) fn dispatch_incoming_envelope_for_tests(
    plaintext: &[u8],
) -> Option<CommitterDelegationEnvelope> {
    let dispatched = dispatch_incoming_envelope_inner(plaintext).ok()?;
    assert_eq!(
        dispatched.kind,
        WasmIncomingEnvelopeKind::CommitterDelegation
    );
    assert!(dispatched.payload.is_empty());
    dispatched.committer_delegation
}

/// The `JsError`-free core of [`dispatch_incoming_envelope`], so the refusal
/// is exercisable on the host.
fn dispatch_incoming_envelope_inner(plaintext: &[u8]) -> Result<WasmIncomingEnvelope, String> {
    use WasmIncomingEnvelopeKind as K;
    let parsed = rust_dispatch_incoming_envelope(plaintext).map_err(|err| err.to_string())?;
    Ok(match parsed {
        IncomingEnvelope::CallWelcome(p) => WasmIncomingEnvelope {
            payload: p.to_vec(),
            ..WasmIncomingEnvelope::of(K::CallWelcome)
        },
        IncomingEnvelope::CallKeyPackage(p) => WasmIncomingEnvelope {
            payload: p.to_vec(),
            ..WasmIncomingEnvelope::of(K::CallKeyPackage)
        },
        IncomingEnvelope::MediaState { camera_on } => WasmIncomingEnvelope {
            camera_on: Some(camera_on),
            ..WasmIncomingEnvelope::of(K::MediaState)
        },
        IncomingEnvelope::PushRoutingSecret { mint_epoch, secret } => WasmIncomingEnvelope {
            payload: secret.to_vec(),
            mint_epoch: Some(mint_epoch),
            ..WasmIncomingEnvelope::of(K::PushRoutingSecret)
        },
        IncomingEnvelope::CommitterBeacon => WasmIncomingEnvelope::of(K::CommitterBeacon),
        IncomingEnvelope::Profile(p) => WasmIncomingEnvelope {
            profile: Some(p.into()),
            ..WasmIncomingEnvelope::of(K::Profile)
        },
        IncomingEnvelope::GroupName(g) => WasmIncomingEnvelope {
            group_name: Some(g.into()),
            ..WasmIncomingEnvelope::of(K::GroupName)
        },
        IncomingEnvelope::ProfileRequest => WasmIncomingEnvelope::of(K::ProfileRequest),
        IncomingEnvelope::Pin(op) => WasmIncomingEnvelope {
            pin: Some(op.into()),
            ..WasmIncomingEnvelope::of(K::Pin)
        },
        IncomingEnvelope::IntroductionOffer(record) => WasmIncomingEnvelope {
            introduction_record: Some(record.encode().map_err(|e| e.to_string())?),
            ..WasmIncomingEnvelope::of(K::IntroductionOffer)
        },
        IncomingEnvelope::Introductions(bundle) => WasmIncomingEnvelope {
            introductions: Some(IntroductionsBundle::from_rust(&bundle).map_err(|e| e.message)?),
            ..WasmIncomingEnvelope::of(K::Introductions)
        },
        IncomingEnvelope::Visibility(setting) => WasmIncomingEnvelope {
            visibility: Some(VisibilitySetting::from_rust(setting)),
            ..WasmIncomingEnvelope::of(K::Visibility)
        },
        IncomingEnvelope::ReachState(state) => WasmIncomingEnvelope {
            reach_state: Some(ReachState::from_rust(state)),
            ..WasmIncomingEnvelope::of(K::ReachState)
        },
        IncomingEnvelope::RegistrarSeed(_) => WasmIncomingEnvelope {
            payload: plaintext.to_vec(),
            ..WasmIncomingEnvelope::of(K::RegistrarSeed)
        },
        IncomingEnvelope::CommitterDelegation(envelope) => WasmIncomingEnvelope {
            committer_delegation: Some(CommitterDelegationEnvelope::from_rust(envelope)),
            ..WasmIncomingEnvelope::of(K::CommitterDelegation)
        },
        IncomingEnvelope::SharedChannels(list) => WasmIncomingEnvelope {
            shared_channels: Some(list.into()),
            ..WasmIncomingEnvelope::of(K::SharedChannels)
        },
        IncomingEnvelope::GroupClosed {
            closed_at_ms,
            successor_group_id,
        } => WasmIncomingEnvelope {
            group_closed: Some(WasmGroupClosed {
                closed_at_ms,
                successor_group_id,
            }),
            ..WasmIncomingEnvelope::of(K::GroupClosed)
        },
        IncomingEnvelope::KeyRotation(rotation) => WasmIncomingEnvelope {
            key_rotation: Some(rotation.into()),
            ..WasmIncomingEnvelope::of(K::KeyRotation)
        },
        IncomingEnvelope::DeviceCard(card) => WasmIncomingEnvelope {
            device_card: Some(card.into()),
            ..WasmIncomingEnvelope::of(K::DeviceCard)
        },
        IncomingEnvelope::LinkFounded(note) => WasmIncomingEnvelope {
            link_founded: Some(note.into()),
            ..WasmIncomingEnvelope::of(K::LinkFounded)
        },
        IncomingEnvelope::LinkClaim(claim) => WasmIncomingEnvelope {
            link_claim: Some(claim.into()),
            ..WasmIncomingEnvelope::of(K::LinkClaim)
        },
        IncomingEnvelope::IntroMessage(p) => WasmIncomingEnvelope {
            payload: p.to_vec(),
            ..WasmIncomingEnvelope::of(K::IntroMessage)
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

/// One observed committer beacon, reduced to the three things
/// [`acting_committer_verdict`] reads. wasm-bindgen mirror of UniFFI's
/// `BeaconObservation` / [`lakko_contacts::committer_beacon::BeaconObservation`].
///
/// **Every field is the observer's own re-derivation, never anything the
/// beacon claimed.** The payload is field-less: the sender comes from the MLS
/// sender leaf and the age from the DS row's server-assigned `created_at`, so a
/// sender can neither name itself nor assert its own freshness.
///
/// # Why a constructible class rather than parallel arrays
///
/// The fields belong to one observation, so they are carried as one object
/// — following [`crate::mls::ProposalRecord`], the house shape for a small
/// record crossing this boundary. Two parallel arrays (`ranks[]`, `ages[]`)
/// plus a sentinel for the absent rank would let the lengths diverge silently
/// and would need a magic number that `undefined` already expresses.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BeaconObservation {
    rank: Option<u32>,
    age_ms: u64,
    signature_key: [u8; 32],
}

#[wasm_bindgen]
impl BeaconObservation {
    /// Build one observation from a beacon the caller has already decrypted and
    /// classified.
    ///
    /// `rank` is the committer rank the observer **re-derived** for this
    /// beacon's MLS-authenticated sender — `Group.committerRank(signatureKey)`
    /// — or `undefined` when that sender is not a candidate at all (a leaf
    /// that no longer resolves). Since ADR-0050 section 2.5a a plain member's
    /// leaves ARE ranked, after the admins, because they beacon while they hold
    /// a device admission; and since item 6 of that section every leaf of a
    /// ranked account holds the account's rank.
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
    ///
    /// `signatureKey` is the sender's 32-byte leaf signature key — the key the
    /// MLS sender leaf resolved to at receipt, the same one ranked into
    /// `rank`. It breaks the tie between devices of one account (ADR-0050
    /// section 2.5a item 6): the account's lowest-keyed device whose beacon is
    /// fresh acts.
    ///
    /// # Errors
    ///
    /// `JsError` when `signatureKey` is not exactly 32 bytes.
    #[wasm_bindgen(constructor)]
    pub fn new(rank: Option<u32>, age_ms: u64, signature_key: &[u8]) -> Result<Self, JsError> {
        Self::new_core(rank, age_ms, signature_key).map_err(|m| JsError::new(&m))
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

    /// The sender's 32-byte leaf signature key.
    #[wasm_bindgen(getter, js_name = signatureKey)]
    #[must_use]
    pub fn signature_key(&self) -> Vec<u8> {
        self.signature_key.to_vec()
    }
}

impl BeaconObservation {
    /// Core of [`Self::new`], testable off-wasm.
    fn new_core(rank: Option<u32>, age_ms: u64, signature_key: &[u8]) -> Result<Self, String> {
        Ok(Self {
            rank,
            age_ms,
            signature_key: beacon_signature_key(signature_key, "observation signature key")?,
        })
    }
}

/// A 32-byte signature key for the fold, or a message naming `what`.
fn beacon_signature_key(key: &[u8], what: &str) -> Result<[u8; 32], String> {
    <[u8; 32]>::try_from(key).map_err(|_| format!("{what} must be 32 bytes, got {}", key.len()))
}

impl From<BeaconObservation> for RustBeaconObservation {
    fn from(o: BeaconObservation) -> Self {
        Self {
            rank: o.rank,
            age_ms: o.age_ms,
            signature_key: o.signature_key,
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
    defer_to_signature_key: Option<[u8; 32]>,
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
    /// * `"defer"` — a better-placed candidate (a lower rank, or the same rank
    ///   and a lower signature key — another device of the same account,
    ///   ADR-0050 section 2.5a item 6) beaconed inside the freshness window;
    ///   stand down and re-evaluate on the next poll. [`Self::defer_to_rank`]
    ///   and [`Self::defer_to_signature_key`] name it.
    /// * `"not-a-candidate"` — this device is not an owner or admin and can
    ///   never sweep. Distinct from `"defer"`: there is nothing to re-evaluate,
    ///   and no beacon this device should be emitting.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kind(&self) -> String {
        self.kind.to_string()
    }

    /// The rank of the best-placed fresh observation, present **iff** `kind
    /// === "defer"` and `undefined` for every other kind. Equal to this
    /// device's own rank when it defers to a lower-keyed device of the same
    /// account.
    ///
    /// Purely informational — the decision is already made — but it is what lets
    /// a log line answer *deferring to whom*.
    #[wasm_bindgen(getter, js_name = deferToRank)]
    #[must_use]
    pub fn defer_to_rank(&self) -> Option<u32> {
        self.defer_to_rank
    }

    /// That observation's 32-byte signature key, present **iff** `kind ===
    /// "defer"`. Never this device's own key.
    #[wasm_bindgen(getter, js_name = deferToSignatureKey)]
    #[must_use]
    pub fn defer_to_signature_key(&self) -> Option<Vec<u8>> {
        self.defer_to_signature_key.map(|k| k.to_vec())
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
                defer_to_signature_key: None,
            },
            RustActingCommitter::Defer {
                to_rank,
                to_signature_key,
            } => Self {
                kind: "defer",
                defer_to_rank: Some(to_rank),
                defer_to_signature_key: Some(to_signature_key),
            },
            RustActingCommitter::NotACandidate => Self {
                kind: "not-a-candidate",
                defer_to_rank: None,
                defer_to_signature_key: None,
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
/// 2. An observation whose `rank` is `undefined` is ignored (its sender is not
///    a candidate).
/// 3. An observation older than `freshnessWindowMs` is ignored.
/// 4. An observation under `ownSignatureKey` is ignored whatever its rank,
///    which is what makes a device's own echoed-back beacon harmless.
/// 5. Only a better-placed observation defers this device: a **strictly**
///    lower rank, or the **same** rank and a **strictly** lower signature key,
///    bytewise (ADR-0050 section 2.5a item 6 — every device of a ranked
///    account holds the account's rank, and the account's acting device is its
///    lowest-keyed one whose beacon is fresh, so a lost device that stays in
///    the roster stops holding the duty once its beacon ages out). Rank `0`
///    never waits for a higher rank, but does defer to a fresh lower-keyed
///    device of the owner's own account.
/// 6. Surviving all of that with nothing left is `"act"`: hearing nothing is
///    the correct answer when nobody better placed is running, not a failure to
///    elect.
///
/// **This does not close the race.** Each candidate folds over its own
/// poll-timed view and a beacon is stale by up to one
/// [`committer_beacon_interval_ms`] even when its sender is perfectly healthy,
/// so two candidates — two devices of one account included — can both reach
/// `"act"`. ADR-0045's stand-down journal
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
///
/// # Errors
///
/// `JsError` when `ownSignatureKey` is not exactly 32 bytes (each
/// [`BeaconObservation`] checked its own key when it was built).
#[wasm_bindgen(js_name = actingCommitterVerdict)]
pub fn acting_committer_verdict(
    own_rank: Option<u32>,
    own_signature_key: &[u8],
    observed: Vec<BeaconObservation>,
    freshness_window_ms: u64,
) -> Result<ActingCommitter, JsError> {
    acting_committer_verdict_core(own_rank, own_signature_key, observed, freshness_window_ms)
        .map_err(|m| JsError::new(&m))
}

/// Core of [`acting_committer_verdict`], testable off-wasm.
fn acting_committer_verdict_core(
    own_rank: Option<u32>,
    own_signature_key: &[u8],
    observed: Vec<BeaconObservation>,
    freshness_window_ms: u64,
) -> Result<ActingCommitter, String> {
    let own_key = beacon_signature_key(own_signature_key, "own signature key")?;
    let rust_observed: Vec<RustBeaconObservation> = observed.into_iter().map(Into::into).collect();
    Ok(
        rust_acting_committer_verdict(own_rank, &own_key, &rust_observed, freshness_window_ms)
            .into(),
    )
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

/// Owner-side: compose + sign a group join-ticket capability without a
/// preview (ADR-0028) and seal it for a `/g/` link under a fresh link secret.
/// `issuerSigningSeed` is the 32-byte Ed25519 seed; `groupId` is the target
/// group's opaque MLS GroupId bytes; `expiryMs` is UNIX millis (`0` = never).
/// Returns an `InviteLinkMint`.
#[wasm_bindgen(js_name = composeGroupInviteCapability)]
pub fn compose_group_invite_capability(
    issuer_signing_seed: &[u8],
    group_id: &[u8],
    expiry_ms: u64,
) -> Result<WasmInviteLinkMint, JsError> {
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
    let wire = rust_group_invite_compose(&signing, group_id, expiry_ms)
        .map_err(|e| JsError::new(&format!("{e}")))?;
    seal_mint(
        &signing,
        |secret| rust_group_invite_seal_link(secret, &wire),
        rust_generate_link_key(),
    )
}

/// Owner-side: sign a group-invite revocation (ADR-0028 revoke). Returns the
/// 64-byte Ed25519 signature over the group-invite revoke canonical bytes (the
/// `DELETE /discovery/group-invite/{blob_id}` body). `issuerSigningSeed` is the
/// 32-byte Ed25519 seed that minted the link; `blobId` is the 6-byte id the
/// discovery upload returned.
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

/// Width in bytes of a discovery-ledger id (`/i/`, `/c/`, `/g/` pointer
/// links): 6. The server mints it; a client only carries it (ADR-0028
/// short-link amendment, 2026-09-27).
#[wasm_bindgen(js_name = inviteBlobIdLen)]
#[must_use]
pub fn invite_blob_id_len() -> u32 {
    u32::try_from(BLOB_ID_LEN).unwrap_or(u32::MAX)
}

/// Length of a ledger id in a link: base64url without padding, 8 characters.
/// A pointer path segment of any other length is not a pointer id.
#[wasm_bindgen(js_name = inviteBlobIdUrlLen)]
#[must_use]
pub fn invite_blob_id_url_len() -> u32 {
    u32::try_from(BLOB_ID_B64URL_LEN).unwrap_or(u32::MAX)
}

/// Width in bytes of a `/g/` link secret (the URL fragment): 12.
#[wasm_bindgen(js_name = groupInviteLinkSecretLen)]
#[must_use]
pub fn group_invite_link_secret_len() -> u32 {
    u32::try_from(GROUP_INVITE_LINK_SECRET_LEN).unwrap_or(u32::MAX)
}

/// Length of a `/g/` link secret in the URL fragment: base64url without
/// padding, 16 characters. Any other fragment length is refused.
#[wasm_bindgen(js_name = groupInviteLinkSecretUrlLen)]
#[must_use]
pub fn group_invite_link_secret_url_len() -> u32 {
    u32::try_from(GROUP_INVITE_LINK_SECRET_B64URL_LEN).unwrap_or(u32::MAX)
}

/// Composed + verified group join-ticket capability (ADR-0028). Mirror of
/// [`lakko_contacts::GroupInviteCapability`].
/// The preview a `/g/` link shows before the person asks to join (ADR-0028
/// v2): the ISSUER's claim at mint time, sealed under a key derived from the
/// link secret that rides in the URL fragment.
#[wasm_bindgen(js_name = GroupInvitePreview)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmGroupInvitePreview {
    group_name: Option<String>,
    inviter_name: Option<String>,
    member_count: u16,
    channel_id: Option<Vec<u8>>,
}

#[wasm_bindgen(js_class = GroupInvitePreview)]
impl WasmGroupInvitePreview {
    /// The group's name, or `undefined`.
    #[wasm_bindgen(getter, js_name = groupName)]
    pub fn group_name(&self) -> Option<String> {
        self.group_name.clone()
    }

    /// The inviter's display name, or `undefined`.
    #[wasm_bindgen(getter, js_name = inviterName)]
    pub fn inviter_name(&self) -> Option<String> {
        self.inviter_name.clone()
    }

    /// How many people were in the group when the link was minted.
    #[wasm_bindgen(getter, js_name = memberCount)]
    pub fn member_count(&self) -> u16 {
        self.member_count
    }

    /// ADR-0078 section 6: the 16-byte channel id of a co-publisher link
    /// (then `groupName` is the channel's name), or `undefined` for a group
    /// link.
    #[wasm_bindgen(getter, js_name = channelId)]
    pub fn channel_id(&self) -> Option<Vec<u8>> {
        self.channel_id.clone()
    }
}

/// Mint a `/g/` capability that carries a sealed preview (ADR-0028 v2) and
/// seal the whole capability for the link under the same fresh link secret
/// (the 2026-09-28 security pass). Names are prepared like a profile / group
/// name (trimmed; blank = none) and refused past the display-name rule.
/// Returns an `InviteLinkMint`.
///
/// # Errors
///
/// Throws on a bad seed / group id or a name the rule refuses.
#[wasm_bindgen(js_name = composeGroupInviteCapabilityWithPreview)]
pub fn compose_group_invite_capability_with_preview(
    issuer_signing_seed: &[u8],
    group_id: &[u8],
    expiry_ms: u64,
    group_name: Option<String>,
    inviter_name: Option<String>,
    member_count: u16,
) -> Result<WasmInviteLinkMint, JsError> {
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
    let name_error = |e: DisplayNameError| JsError::new(&format!("{e}"));
    let preview = RustGroupInvitePreview {
        group_name: rust_normalize_display_name(group_name.as_deref().unwrap_or(""))
            .map_err(name_error)?,
        inviter_name: rust_normalize_display_name(inviter_name.as_deref().unwrap_or(""))
            .map_err(name_error)?,
        member_count,
        channel_id: None,
    };
    let link_key = rust_generate_link_key();
    let wire =
        rust_group_invite_compose_with_preview(&signing, group_id, expiry_ms, &preview, &link_key)
            .map_err(|e| JsError::new(&format!("{e}")))?;
    seal_mint(
        &signing,
        |secret| rust_group_invite_seal_link(secret, &wire),
        link_key,
    )
}

/// ADR-0028 OQ3 / ADR-0078 section 6 — mint a co-publisher `/g/` link over a
/// channel's PublisherGroup `groupId`, its sealed preview carrying the
/// 16-byte `channelId` and the channel's name (optional). Since ADR-0078 the
/// PublisherGroup id is derived from the owner's seed, so nothing inverts it,
/// and the plaintext capability must not name the channel. Returns an
/// `InviteLinkMint`.
///
/// # Errors
///
/// Throws on a bad seed, group id or channel id, or a name the rule refuses.
#[wasm_bindgen(js_name = composeCoPublisherInviteCapability)]
pub fn compose_co_publisher_invite_capability(
    issuer_signing_seed: &[u8],
    group_id: &[u8],
    expiry_ms: u64,
    channel_id: &[u8],
    channel_name: Option<String>,
) -> Result<WasmInviteLinkMint, JsError> {
    let seed: [u8; 32] = issuer_signing_seed
        .try_into()
        .map_err(|_| JsError::new("issuerSigningSeed must be 32 bytes"))?;
    let signing = SigningKey::from_seed(&seed);
    if group_id.is_empty() || group_id.len() > MAX_GROUP_ID_LEN {
        return Err(JsError::new(&format!(
            "groupId must be 1..={MAX_GROUP_ID_LEN} bytes"
        )));
    }
    let channel: [u8; 16] = channel_id
        .try_into()
        .map_err(|_| JsError::new("channelId must be 16 bytes"))?;
    let preview = RustGroupInvitePreview {
        group_name: rust_normalize_display_name(channel_name.as_deref().unwrap_or(""))
            .map_err(|e: DisplayNameError| JsError::new(&format!("{e}")))?,
        inviter_name: None,
        member_count: 0,
        channel_id: Some(channel),
    };
    let link_key = rust_generate_link_key();
    let wire =
        rust_group_invite_compose_with_preview(&signing, group_id, expiry_ms, &preview, &link_key)
            .map_err(|e| JsError::new(&format!("{e}")))?;
    seal_mint(
        &signing,
        |secret| rust_group_invite_seal_link(secret, &wire),
        link_key,
    )
}

/// Decoded, signature-verified group join-ticket (ADR-0028) — the wasm face
/// of [`lakko_contacts::GroupInviteCapability`].
#[wasm_bindgen]
pub struct GroupInviteCapability {
    pub(crate) inner: RustGroupInviteCapability,
    wire: Vec<u8>,
}

#[wasm_bindgen]
impl GroupInviteCapability {
    /// Decode PLAINTEXT capability wire bytes and verify the Ed25519 signature
    /// (the join attribution carries plaintext; a link never does since the
    /// 2026-09-28 security pass — use `openLink`). Does **not** check expiry —
    /// call `isValidNow`.
    #[wasm_bindgen(js_name = decodeAndVerify)]
    pub fn decode_and_verify(wire: &[u8]) -> Result<GroupInviteCapability, JsError> {
        let inner = rust_group_invite_decode(wire).map_err(|e| JsError::new(&format!("{e}")))?;
        Ok(Self {
            inner,
            wire: wire.to_vec(),
        })
    }

    /// Joiner-side: open a `/g/` link's sealed blob with the 12-byte secret
    /// from the URL fragment and verify the capability inside; the same
    /// secret then opens the preview (`openPreview`). A `/g/` link without its
    /// fragment has nothing to open this with. Does NOT check expiry.
    ///
    /// # Errors
    ///
    /// Throws an `Error` with `inviteLinkError: "damaged"` on any failure.
    #[wasm_bindgen(js_name = openLink)]
    pub fn open_link(sealed: &[u8], link_secret: &[u8]) -> Result<GroupInviteCapability, JsValue> {
        let secret = rust_link_secret_from_slice(link_secret).map_err(invite_link_error)?;
        let (inner, wire) =
            rust_group_invite_open_link(&secret, sealed).map_err(invite_link_error)?;
        Ok(Self { inner, wire })
    }

    /// The plaintext capability wire — what `computeJoinerInviteTag` and
    /// `composeGroupJoinAttribution` take.
    #[wasm_bindgen(getter)]
    pub fn wire(&self) -> Vec<u8> {
        self.wire.clone()
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

    /// Whether the link carries a sealed preview at all.
    #[wasm_bindgen(getter, js_name = hasPreview)]
    pub fn has_preview(&self) -> bool {
        !self.inner.sealed_preview.is_empty()
    }

    /// Open the sealed preview with the link secret from the URL fragment
    /// (the 12 bytes the 16-character fragment decodes to). `undefined` when
    /// the link carries no preview. A wrong secret, a tampered preview, or a
    /// capability other than the one the secret was minted with (the derived
    /// key binds the group id and issuer) throws; the caller shows the page
    /// without a preview, never a guess.
    ///
    /// # Errors
    ///
    /// Throws when `linkKey` is not 12 bytes (the pre-amendment 32-byte key
    /// included) or the preview does not open.
    #[wasm_bindgen(js_name = openPreview)]
    pub fn open_preview(&self, link_key: &[u8]) -> Result<Option<WasmGroupInvitePreview>, JsError> {
        if link_key.len() != GROUP_INVITE_LINK_SECRET_LEN {
            return Err(JsError::new(&format!(
                "linkKey must be {GROUP_INVITE_LINK_SECRET_LEN} bytes"
            )));
        }
        let mut key = [0u8; GROUP_INVITE_LINK_SECRET_LEN];
        key.copy_from_slice(link_key);
        let opened = self
            .inner
            .open_preview(&key)
            .map_err(|e| JsError::new(&format!("{e}")))?;
        Ok(opened.map(|p| WasmGroupInvitePreview {
            group_name: p.group_name,
            inviter_name: p.inviter_name,
            member_count: p.member_count,
            channel_id: p.channel_id.map(|c| c.to_vec()),
        }))
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
///
/// `replyGrant` (ADR-0060 §6, C-69) is the joiner's `reply` reach grant:
/// `deriveReachGrant(accountSeed, ReachClass.Reply, replyId, 0)` over a fresh
/// random 16-byte `replyId`, its public key registered with a 7-day expiry
/// BEFORE posting and removed once joined. Every member of the group can read
/// it (the ADR's stated residual). A grant of any other class throws.
#[wasm_bindgen(js_name = composeGroupJoinAttribution)]
pub fn compose_group_join_attribution(
    joiner_signing_seed: &[u8],
    capability_bytes: &[u8],
    reply_grant: &ReachGrant,
) -> Result<Vec<u8>, JsError> {
    rust_group_join_attribution_compose(joiner_signing_seed, capability_bytes, reply_grant.rust())
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

    /// The joiner's `reply` reach grant (ADR-0060 §6), inside the signed
    /// part. SECRET and readable by every member: the acting committer that
    /// sweeps the joiner's approved `Add` delivers the Welcome to the
    /// joiner's mailbox with `DsProofSigner.forReach(replyGrant.seed)`.
    #[wasm_bindgen(getter, js_name = replyGrant)]
    pub fn reply_grant(&self) -> ReachGrant {
        ReachGrant::from_rust(self.inner.reply_grant.clone())
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
    #[test]
    fn shared_channels_round_trip_and_dispatch_wasm_host() {
        let mut ids = vec![1u8; 16];
        ids.extend_from_slice(&[2u8; 16]);
        let wire = compose_shared_channels_envelope(
            7,
            &ids,
            vec!["lakko_news".to_string(), "cooking".to_string()],
            vec!["Lakko News".to_string(), "Cooking".to_string()],
        )
        .unwrap();
        let list = decode_shared_channels_envelope(&wire).unwrap();
        assert_eq!(list.count(), 2);
        assert_eq!(list.channel_ids(), ids);
        assert_eq!(list.handles()[1], "cooking");
        assert_eq!(list.updated_at_ms(), 7);
        let env = dispatch_incoming_envelope(&wire).unwrap();
        assert_eq!(env.kind(), WasmIncomingEnvelopeKind::SharedChannels);
        assert_eq!(env.shared_channels(), Some(list));
        assert!(env.payload().is_empty());
        assert!(shared_channels_is_newer(8, 7));
        assert!(!shared_channels_is_newer(7, 7));
    }

    use super::*;

    const TEST_LINK_ID: [u8; 16] = [0x1C; 16];

    /// Derived from the tests' capability owner seed (`[9; 32]`): its mailbox
    /// binding must verify under the owner's key (ADR-0078).
    fn test_link_grant() -> ReachGrant {
        ReachGrant::from_rust(
            lakko_contacts::reach::ReachGrant::derive(
                &[9; 32],
                lakko_contacts::reach::ReachClass::Link,
                &TEST_LINK_ID,
                0,
            )
            .unwrap(),
        )
    }

    /// Derived from the tests' request sender seed (`[7; 32]`).
    fn test_reply_grant() -> ReachGrant {
        ReachGrant::from_rust(
            lakko_contacts::reach::ReachGrant::derive(
                &[7; 32],
                lakko_contacts::reach::ReachClass::Reply,
                &[0x9A; 16],
                0,
            )
            .unwrap(),
        )
    }
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
            Some(test_reply_grant()),
            &[],
            &[],
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
        assert_eq!(opened.reply_grant(), Some(test_reply_grant()));
        assert_eq!(opened.intro_message(), b"hi bob");
    }

    /// ADR-0060 §6: the joiner's reply grant rides the self-signed `/g/`
    /// attribution and comes out whole (success path only, see above).
    #[test]
    fn group_join_attribution_carries_the_reply_grant() {
        let mint = compose_group_invite_capability(&[0x33; 32], &[7u8; 32], 0).unwrap();
        let cap = GroupInviteCapability::open_link(&mint.sealed(), &mint.link_secret())
            .unwrap_or_else(|_| panic!("the minted link opens"));
        let env =
            compose_group_join_attribution(&[0x44; 32], &cap.wire(), &test_reply_grant()).unwrap();
        let parsed = GroupJoinAttribution::parse(&env).unwrap();
        assert_eq!(parsed.reply_grant(), test_reply_grant());
        assert_eq!(parsed.group_id(), vec![7u8; 32]);
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
            None,
            None,
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
            None,
            &[],
            &[],
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

    /// ADR-0060: profile v3 carries the certificate and the grants across
    /// the boundary, both ways. Success paths only (a throw panics off
    /// wasm32); the refusal matrix is the core's.
    #[test]
    fn profile_v3_carries_certificate_and_grants() {
        use crate::scoped_discovery::{derive_reach_grant, ReachClass};
        let grants = vec![
            derive_reach_grant(&[0x42; 32], ReachClass::Group, &[], 2).unwrap(),
            derive_reach_grant(&[0x42; 32], ReachClass::Member, &[], 1).unwrap(),
        ];
        let envelope = compose_profile_envelope(
            Some("Alice".to_string()),
            Some("alice".to_string()),
            7,
            None,
            Some(vec![9; 153]),
            Some(grants.clone()),
        )
        .unwrap();
        let decoded = decode_profile_envelope(&envelope).unwrap();
        assert_eq!(decoded.username_certificate(), Some(vec![9; 153]));
        assert_eq!(decoded.grants(), grants);
        let dispatched = dispatch_incoming_envelope(&envelope).unwrap();
        assert_eq!(dispatched.profile(), Some(decoded));
        // The ADR-0060 bounds refuse before any bytes exist.
        let mut too_many = grants.clone();
        too_many.push(derive_reach_grant(&[0x42; 32], ReachClass::Channel, &[], 1).unwrap());
        let p = prepare_profile(None, None, 1, None, None, Some(too_many)).unwrap();
        assert!(matches!(
            rust_compose_profile(&p),
            Err(ProfileError::TooManyGrants { count: 3 })
        ));
    }

    /// ADR-0060: the four envelopes S1 refused dispatch as their own kinds.
    #[test]
    fn dispatch_surfaces_the_adr_0060_envelopes() {
        use crate::scoped_discovery::{
            compose_reach_state_envelope, compose_visibility_envelope, default_visibility_setting,
            ReachClass, ReachStateEntry,
        };
        use lakko_contacts::introduction::{
            compose_bundle, compose_introduction, compose_introduction_offer, IntroductionRecord,
        };
        let rust_payload = lakko_contacts::introduction::IntroductionPayload {
            identity_pk: Ed25519::from_seed(&[3; 32]).verifying_key().to_bytes(),
            account_id: [2; 16],
            display_name: Some("Anna".into()),
            username: None,
            username_certificate: None,
            mailbox_binding: lakko_contacts::mailbox::MailboxBinding::for_account_seed(&[3; 32]),
            fof: Some(
                lakko_contacts::reach::ReachGrant::derive(
                    &[3; 32],
                    lakko_contacts::reach::ReachClass::Fof,
                    b"",
                    1,
                )
                .unwrap(),
            ),
        };
        let record = compose_introduction(&rust_payload, &[[5; 64]], 1).unwrap();

        let offer =
            dispatch_incoming_envelope(&compose_introduction_offer(&record).unwrap()).unwrap();
        assert_eq!(offer.kind(), WasmIncomingEnvelopeKind::IntroductionOffer);
        assert!(offer.payload().is_empty());
        assert_eq!(offer.introduction_record(), Some(record.clone()));

        let bundle = compose_bundle(&[IntroductionRecord::decode(&record).unwrap()], 3).unwrap();
        let b = dispatch_incoming_envelope(&bundle).unwrap();
        assert_eq!(b.kind(), WasmIncomingEnvelopeKind::Introductions);
        let bundle = b.introductions().unwrap();
        assert_eq!((bundle.updated_at_ms(), bundle.record_count()), (3, 1));

        let setting = default_visibility_setting();
        let v =
            dispatch_incoming_envelope(&compose_visibility_envelope(&setting).unwrap()).unwrap();
        assert_eq!(v.kind(), WasmIncomingEnvelopeKind::Visibility);
        assert_eq!(v.visibility(), Some(setting));

        let state = crate::scoped_discovery::ReachState::new(
            vec![ReachStateEntry::new(ReachClass::Fof, vec![], 4)],
            5,
        );
        let r = dispatch_incoming_envelope(&compose_reach_state_envelope(&state).unwrap()).unwrap();
        assert_eq!(r.kind(), WasmIncomingEnvelopeKind::ReachState);
        assert_eq!(r.reach_state(), Some(state));
        assert_eq!(r.visibility(), None);

        // A malformed ADR-0060 envelope is refused, never an intro message.
        let mut broken = compose_visibility_envelope(&default_visibility_setting()).unwrap();
        broken.push(0);
        assert!(dispatch_incoming_envelope_inner(&broken).is_err());
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

    /// Happy path only, for the same reason as the group name above.
    #[test]
    fn group_closed_envelope_round_trips_and_dispatches() {
        for successor in [Some(vec![0x5A; 32]), None] {
            let envelope = compose_group_closed_envelope(42, successor.clone()).unwrap();
            let decoded = decode_group_closed_envelope(&envelope).unwrap();
            assert_eq!(decoded.closed_at_ms(), 42);
            assert_eq!(decoded.successor_group_id(), successor);

            let dispatched = dispatch_incoming_envelope(&envelope).unwrap();
            assert_eq!(dispatched.kind(), WasmIncomingEnvelopeKind::GroupClosed);
            assert!(dispatched.payload().is_empty());
            assert_eq!(dispatched.group_name(), None);
            assert_eq!(dispatched.pin(), None);
            assert_eq!(dispatched.group_closed(), Some(decoded));
        }
        assert!(group_closed_is_newer(43, 42) && !group_closed_is_newer(42, 42));
        assert_eq!(group_closed_envelope_min_len(), 32);
        assert_eq!(group_closed_envelope_max_len(), 64);
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
            &TEST_LINK_ID,
            &test_link_grant(),
            None,
            None,
        )
        .unwrap();

        assert_eq!(wire.link_secret().len(), 12);
        let cap = InviteCapability::open_link(&wire.sealed(), &wire.link_secret())
            .unwrap_or_else(|_| panic!("the minted link opens"));
        assert_eq!(cap.owner_signature_pubkey(), wire.issuer_pubkey());
        assert!(InviteCapability::decode_and_verify(&cap.wire()).is_ok());
        assert_eq!(cap.owner_account_id(), owner_account_id.to_vec());
        assert_eq!(cap.owner_x25519_pubkey(), owner_x25519_pub.to_vec());
        assert_eq!(cap.expiry_ms(), 1_700_000_000_000);
        assert_eq!(cap.max_uses(), 5);
        assert!(!cap.bootstrap_referral());
        assert_eq!(cap.label(), "alice@home");
        assert_eq!(cap.link_id(), TEST_LINK_ID.to_vec());
        assert_eq!(cap.link_grant(), test_link_grant());
        assert_eq!(generate_link_id().len(), link_id_len());
        // Ed25519 pubkey is derivable from the seed; assert non-empty.
        assert_eq!(cap.owner_signature_pubkey().len(), 32);
        // is_valid_now boundary check.
        assert!(cap.is_valid_now(0));
        assert!(!cap.is_valid_now(1_700_000_000_000));
    }

    #[test]
    fn short_link_lengths_are_exported() {
        assert_eq!(invite_blob_id_len(), 6);
        assert_eq!(invite_blob_id_url_len(), 8);
        assert_eq!(group_invite_link_secret_len(), 12);
        assert_eq!(group_invite_link_secret_url_len(), 16);
    }

    #[test]
    fn sign_invite_revocation_round_trip() {
        use lakko_contacts::invite::revoke_canonical_bytes;
        use lakko_crypto::signing::{Signature, VerifyingKey};

        let seed = [0xAAu8; 32];
        let blob_id = [0xBBu8; BLOB_ID_LEN];
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
        let blob_id = [0xDDu8; BLOB_ID_LEN];
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
        let blob_id = [0xFFu8; BLOB_ID_LEN];
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
        let owner_seed = [9u8; 32];
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
            &TEST_LINK_ID,
            &test_link_grant(),
            None,
            None,
        )
        .unwrap();

        assert_eq!(wire.link_secret().len(), 12);
        let cap = InviteCapability::open_link(&wire.sealed(), &wire.link_secret())
            .unwrap_or_else(|_| panic!("the minted link opens"));
        assert_eq!(cap.owner_signature_pubkey(), wire.issuer_pubkey());
        assert!(InviteCapability::decode_and_verify(&cap.wire()).is_ok());
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

    const OWN_KEY: [u8; 32] = [0x80; 32];
    const HI_KEY: [u8; 32] = [0xEE; 32];
    const LO_KEY: [u8; 32] = [0x10; 32];

    fn beacon(rank: Option<u32>, age_ms: u64, key: [u8; 32]) -> BeaconObservation {
        BeaconObservation::new_core(rank, age_ms, &key).expect("32-byte key")
    }

    fn verdict(own_rank: Option<u32>, observed: Vec<BeaconObservation>, w: u64) -> ActingCommitter {
        acting_committer_verdict_core(own_rank, &OWN_KEY, observed, w).expect("32-byte key")
    }

    #[test]
    fn verdict_covers_its_three_outcomes_with_a_per_variant_payload() {
        let w = committer_beacon_freshness_window_ms();

        // 1 — a plain member is never a candidate, whatever it has heard. An
        // OMITTED `ownRank` argument reaches the ABI as absent and lands here
        // too, which is the fail-closed direction: it never sweeps.
        let not_candidate = verdict(None, vec![beacon(Some(0), 0, HI_KEY)], w);
        assert_eq!(not_candidate.kind(), "not-a-candidate");
        assert!(!not_candidate.is_act());
        assert!(
            not_candidate.defer_to_rank().is_none()
                && not_candidate.defer_to_signature_key().is_none(),
            "the defer payload must be absent outside its own variant",
        );

        // 2 — the owner acts even with a fresh beacon from every admin.
        let owner = verdict(
            Some(0),
            vec![beacon(Some(1), 0, LO_KEY), beacon(Some(2), 0, LO_KEY)],
            w,
        );
        assert_eq!(owner.kind(), "act");
        assert!(owner.is_act());
        assert!(owner.defer_to_rank().is_none());
        assert!(owner.defer_to_signature_key().is_none());

        // 3 — a strictly lower rank, fresh, defers this device and names whom.
        let defer = verdict(
            Some(2),
            vec![beacon(Some(1), w, HI_KEY), beacon(Some(3), 0, LO_KEY)],
            w,
        );
        assert_eq!(defer.kind(), "defer");
        assert!(!defer.is_act());
        assert_eq!(
            defer.defer_to_rank(),
            Some(1),
            "the window boundary is inclusive, and the LOWEST active rank is named",
        );
        assert_eq!(defer.defer_to_signature_key(), Some(HI_KEY.to_vec()));

        // 4 — silence is Act, not a failure to elect.
        let silence = verdict(Some(3), Vec::new(), w);
        assert_eq!(silence.kind(), "act");
        assert!(silence.is_act());
    }

    #[test]
    fn verdict_ignores_stale_non_candidate_own_and_worse_placed() {
        let w = committer_beacon_freshness_window_ms();
        for (label, obs) in [
            ("stale by one millisecond", beacon(Some(0), w + 1, LO_KEY)),
            (
                "sender the observer could not rank",
                beacon(None, 0, LO_KEY),
            ),
            (
                "this device's own echoed beacon",
                beacon(Some(0), 0, OWN_KEY),
            ),
            ("equal rank, higher key", beacon(Some(2), 0, HI_KEY)),
            ("a higher rank", beacon(Some(5), 0, LO_KEY)),
        ] {
            let v = verdict(Some(2), vec![obs], w);
            assert_eq!(v.kind(), "act", "{label} must not defer this device");
            assert!(v.defer_to_rank().is_none());
        }
    }

    #[test]
    fn verdict_breaks_a_same_rank_tie_by_signature_key() {
        let w = committer_beacon_freshness_window_ms();
        let v = verdict(Some(0), vec![beacon(Some(0), 0, LO_KEY)], w);
        assert_eq!(v.kind(), "defer");
        assert_eq!(v.defer_to_rank(), Some(0));
        assert_eq!(v.defer_to_signature_key(), Some(LO_KEY.to_vec()));
        // The lower-keyed device fell silent (a lost device): this one acts.
        assert!(verdict(Some(0), vec![beacon(Some(0), w + 1, LO_KEY)], w).is_act());
    }

    #[test]
    fn beacon_observation_carries_its_fields_verbatim_and_refuses_a_bad_key() {
        // The fields travel as ONE object, so the rank, the age and the key
        // cannot be paired up wrongly the way parallel arrays could.
        let obs = beacon(Some(4), 1_234, LO_KEY);
        assert_eq!(obs.rank(), Some(4));
        assert_eq!(obs.age_ms(), 1_234);
        assert_eq!(obs.signature_key(), LO_KEY.to_vec());
        assert_eq!(beacon(None, 0, LO_KEY).rank(), None);
        assert!(BeaconObservation::new_core(Some(1), 0, &[0; 31]).is_err());
        assert!(acting_committer_verdict_core(Some(1), &[0; 33], Vec::new(), 0).is_err());
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
        assert_eq!(verdict(None, Vec::new(), w).kind(), "not-a-candidate");
        assert_eq!(verdict(Some(0), Vec::new(), w).kind(), "act");
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
