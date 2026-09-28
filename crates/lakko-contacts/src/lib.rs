// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Contact-request envelopes and pseudonym lifecycle for Lakko.
//!
//! Phase 1.9a covers ADR-0005 scenario A — "I know `@username`,
//! add them as a contact" — minus the username discovery step
//! (handled by `services/discovery` + the OPRF round-trip in
//! `lakko-transport`).
//!
//! ## What's here (1.9a)
//!
//! - [`pseudonym`] — three pseudonym classes per ADR-0005 §13.7,
//!   each with a distinct derivation domain so leaks don't cross
//!   classes. `BLAKE3-keyed` for client-side variants;
//!   `BLAKE3(domain || pubkey)` for the public identity pseudonym.
//! - [`envelope`] — a sealed-box AEAD envelope (HPKE-Base-equivalent
//!   over X25519 + HKDF-SHA-256 + ChaCha20-Poly1305). Used to
//!   encrypt the optional intro message inside a ContactRequest to
//!   the recipient's identity public key.
//! - [`request`] — the [`ContactRequest`] wire type plus
//!   [`compose`] / [`decode_and_verify`] / [`open_envelope`].
//!   Manual length-prefixed binary codec, frozen at v1.
//!   Ed25519-signed by the sender's identity key.
//!   The sealed plaintext is `profile_len ‖ profile ‖ intro`
//!   (ADR-0048 §2).
//! - [`profile`] — the ADR-0048 profile envelope (display name,
//!   claimed username, `updated_at_ms`): bounded, fail-closed,
//!   classified by [`incoming_envelope`].
//! - [`username`] — the ADR-0048 §4 canonical-username rule
//!   (`[a-z0-9_]`, 3 to 32), shared with `lakko-discovery-client`.
//! - [`group_name`] — the ADR-0051 group-name envelope (the shared
//!   name of a group, `updated_at_ms`, empty = clear): the profile
//!   template reused, bounded, fail-closed, classified by
//!   [`incoming_envelope`]; the sender's authority is the receiver's
//!   role-table check, not this crate's.
//! - [`pin`] — the ADR-0052 shared-pin envelope (pin / unpin one message
//!   for everyone, `updated_at_ms`): fixed-length, fail-closed, classified
//!   by [`incoming_envelope`]; the order is [`pin::pin_is_newer`], the
//!   authority is the receiver's role-table check.
//! - [`reach`] — ADR-0058 reach keys: the per-class, per-scope Ed25519 keys
//!   an identity mailbox accepts a write under, derived from the account
//!   seed, and the [`ReachGrant`] that hands one to someone.
//! - [`introduction`] — ADR-0058 contacts of contacts: introduction records
//!   sealed under OPRF outputs, the offer that asks a contact to relay one,
//!   and the padded bundle a contact relays.
//! - [`visibility`] — ADR-0058 "who can find me" setting and the reach-key
//!   version state, as envelopes the owner's devices exchange.
//!
//! ## What's NOT here (1.9b and later)
//!
//! - QR safety numbers (scenario D) — Signal-style fingerprint
//!   verification.
//! - Group-invite envelope (scenario E).
//! - PoW + Privacy Pass tokens carried inside the request — the
//!   server side already exists in 1.1e; the wire-format extension
//!   waits for a real client integration sprint.
//! - `ContactAck` reply path (recipient → sender) — easy follow-up
//!   to add once we have UI to drive it.
//!
//! ## Threat-model notes (cross-link to ADR-0005)
//!
//! - Sealed envelopes provide **confidentiality** but not
//!   sender-authentication. The Ed25519 signature on the request
//!   itself is what proves "this came from `from_pubkey`".
//! - The `from_pseudonym` is fresh per recipient so the server
//!   cannot link two contact-requests Alice sends to different
//!   people. KT-log inclusion proofs over `from_pubkey` (handled
//!   in `lakko-transparency`) anchor the public key to a
//!   verifiable identity record.
//! - Server sees only `(stranger_inbox_pseudonym → wire bytes)`;
//!   it learns nothing about who Alice is or what Bob does with
//!   the request.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ack;
pub mod call_invite;
pub mod call_kp_forward;
pub mod call_welcome;
pub mod committer_beacon;
pub mod envelope;
pub mod error;
pub mod group_invite;
pub mod group_join_attribution;
pub mod group_name;
pub mod incoming_envelope;
pub mod introduction;
pub mod invite;
pub mod media_state;
pub mod pin;
pub mod profile;
pub mod profile_request;
pub mod pseudonym;
pub mod push_routing;
pub mod reach;
pub mod request;
pub mod safety;
pub mod username;
pub mod visibility;

pub use ack::{compose_ack, decode_and_verify_ack, ContactAck};
pub use call_invite::{
    call_revoke_canonical_bytes, call_sign_revocation, compose as compose_call_invite,
    decode_and_verify as decode_and_verify_call_invite, CallInviteCapability, CallInviteMedia,
    CALL_ID_LEN, CALL_INVITE_REVOKE_DOMAIN_V1, CALL_NEVER_EXPIRES, CALL_REVOKE_REPLAY_WINDOW_MS,
    CALL_SINGLE_USE, MAX_KEY_PACKAGE_LEN,
};
pub use call_kp_forward::{
    try_unwrap_call_kp_forward, wrap_call_kp_forward, CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
    CALL_KP_FORWARD_MIN_PAYLOAD_LEN,
};
pub use call_welcome::{
    try_unwrap_call_welcome, wrap_call_welcome, CallWelcomeError, CALL_WELCOME_ENVELOPE_DOMAIN_V1,
    CALL_WELCOME_MIN_PAYLOAD_LEN,
};
pub use committer_beacon::{
    acting_committer_verdict, compose_committer_beacon, decode_committer_beacon,
    should_emit_beacon, try_unwrap_committer_beacon, ActingCommitter, BeaconObservation,
    CommitterBeacon, CommitterBeaconError, COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
    COMMITTER_BEACON_FRESHNESS_WINDOW_MS, COMMITTER_BEACON_INTERVAL_MS,
    COMMITTER_BEACON_PAYLOAD_LEN, COMMITTER_BEACON_VERSION_V1, COMMITTER_BEACON_WORST_CASE_POLL_MS,
};
pub use error::{ContactError, Result};
pub use group_invite::{
    compose as compose_group_invite, decode_and_verify as decode_and_verify_group_invite,
    group_revoke_canonical_bytes, group_sign_revocation, GroupInviteCapability,
    GROUP_INVITE_NEVER_EXPIRES, GROUP_INVITE_REVOKE_DOMAIN_V1,
    GROUP_INVITE_REVOKE_REPLAY_WINDOW_MS, MAX_GROUP_ID_LEN,
};
pub use group_join_attribution::{
    compose_attribution as compose_group_join_attribution,
    decode_attribution as decode_group_join_attribution, GroupJoinAttribution,
};
pub use group_name::{
    compose_group_name, decode_group_name, group_name_is_newer, normalize_group_name,
    try_unwrap_group_name, GroupName, GroupNameError, GROUP_NAME_ENVELOPE_DOMAIN_V1,
    GROUP_NAME_ENVELOPE_MAX_LEN, GROUP_NAME_ENVELOPE_MIN_LEN, GROUP_NAME_VERSION_V1,
};
pub use incoming_envelope::{dispatch_incoming_envelope, DispatchError, IncomingEnvelope};
pub use introduction::{
    bundle_is_newer, compose_bundle, compose_introduction, compose_introduction_offer,
    decode_introduction_offer, decode_introductions, fof_tag, introduction_tags, open_introduction,
    try_unwrap_introduction_offer, try_unwrap_introductions, IntroductionEntry, IntroductionError,
    IntroductionPayload, IntroductionRecord, IntroductionsBundle, BUNDLE_SIZE_BUCKETS,
    FOF_KEY_DOMAIN, FOF_TAG_DOMAIN, FOF_TAG_LEN, INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
    INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1, INTRODUCTION_RECORD_MAX_LEN,
    INTRODUCTION_RECORD_MIN_LEN, MAX_BUNDLE_RECORDS, MAX_INTRODUCTION_TERMS, OPRF_OUTPUT_LEN,
    WRAPPED_KEY_LEN,
};
pub use invite::{
    compute_joiner_tag, generate_blob_id, owner_list_canonical_bytes, revoke_canonical_bytes,
    sign_owner_list, sign_revocation, InviteCapability, BLOB_ID_B64URL_LEN, BLOB_ID_LEN,
    JOINER_TAG_LEN, JOINER_TAG_SALT_DOMAIN_V1, MAX_LABEL_LEN, NEVER_EXPIRES, OWNER_LIST_DOMAIN_V1,
    REVOKE_DOMAIN_V1, REVOKE_REPLAY_WINDOW_MS, SINGLE_USE,
};
pub use media_state::{
    compose_media_state, decode_media_state, try_unwrap_media_state, MediaState, MediaStateError,
    MEDIA_STATE_ENVELOPE_DOMAIN_V1, MEDIA_STATE_PAYLOAD_LEN, MEDIA_STATE_VERSION_V1,
};
pub use pin::{
    compose_pin, decode_pin, pin_is_newer, try_unwrap_pin, PinAction, PinError, PinOp,
    PIN_ENVELOPE_DOMAIN_V1, PIN_ENVELOPE_LEN, PIN_VERSION_V1,
};
pub use profile::{
    check_display_name, compose_profile, decode_profile, is_format_char, is_profile_grant_class,
    normalize_display_name, profile_is_newer, try_unwrap_profile, DisplayNameError, Profile,
    ProfileError, DISPLAY_NAME_MAX_BYTES, DISPLAY_NAME_MAX_CHARS, FORMAT_CHAR_RANGES,
    MAX_PROFILE_GRANTS, PROFILE_ENVELOPE_DOMAIN_V1, PROFILE_ENVELOPE_MAX_LEN,
    PROFILE_ENVELOPE_MIN_LEN, PROFILE_VERSION_V1, PROFILE_VERSION_V2, PROFILE_VERSION_V3,
    USERNAME_CERT_LEN,
};
pub use profile_request::{
    compose_profile_request, decode_profile_request, should_request_profiles,
    try_unwrap_profile_request, ProfileRequest, ProfileRequestError,
    PROFILE_REQUEST_ENVELOPE_DOMAIN_V1, PROFILE_REQUEST_ENVELOPE_LEN,
    PROFILE_REQUEST_MIN_INTERVAL_MS, PROFILE_REQUEST_PAYLOAD_LEN, PROFILE_REQUEST_VERSION_V1,
};
pub use pseudonym::{Direction, IdentityPseudonym, PerRelationship, StrangerInbox, PSEUDONYM_LEN};
pub use push_routing::{
    compose_push_routing_secret, decode_push_routing_secret, derive_push_gid,
    push_routing_secret_is_newer, try_unwrap_push_routing_secret, PushRoutingSecretEnvelope,
    PushRoutingSecretError, PUSH_GID_DERIVE_DOMAIN_V1, PUSH_GID_LEN,
    PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1, PUSH_ROUTING_SECRET_LEN,
    PUSH_ROUTING_SECRET_MINT_EPOCH_LEN, PUSH_ROUTING_SECRET_PAYLOAD_LEN,
    PUSH_ROUTING_SECRET_VERSION_V2,
};
pub use reach::{
    reach_seed, reach_verifying_key, ReachClass, ReachError, ReachGrant, MAX_REACH_SCOPE_ID_LEN,
    REACH_GRANT_LEN, REACH_SEED_DOMAIN, REACH_SEED_LEN,
};
pub use request::{compose, decode_and_verify, open_envelope, ContactRequest, ACCOUNT_ID_LEN};
pub use safety::{
    safety_number, safety_number_bytes, SAFETY_NUMBER_DIGITS, SAFETY_NUMBER_DIGITS_PER_GROUP,
    SAFETY_NUMBER_GROUPS,
};
pub use username::{
    check_canonical_username, login_handle, UsernameError, LOGIN_HANDLE_DOMAIN, LOGIN_HANDLE_LEN,
    USERNAME_MAX_LEN, USERNAME_MIN_LEN,
};
pub use visibility::{
    compose_reach_state, compose_visibility, decode_reach_state, decode_visibility,
    reach_state_is_newer, try_unwrap_reach_state, try_unwrap_visibility, visibility_is_newer,
    GroupsForm, ReachState, ReachStateEntry, ScopeId, ScopeKind, VisibilityError, VisibilityMode,
    VisibilitySetting, MAX_CHOSEN_SCOPES, MAX_REACH_STATE_ENTRIES, MAX_SCOPE_ID_LEN,
    REACH_STATE_ENVELOPE_DOMAIN_V1, VISIBILITY_ENVELOPE_DOMAIN_V1,
};

/// Entry points for the fuzz harnesses in `crates/lakko-contacts/fuzz`.
///
/// Not part of the supported API: it reaches parsers that sit behind a
/// cryptographic check a fuzzer cannot pass on random input.
#[doc(hidden)]
pub mod fuzz_api {
    /// Decode a group-invite preview **plaintext** (the bytes inside the
    /// AEAD of a v2 `/g/` capability). `None` when the decoder refuses,
    /// `Some(true)` when the accepted preview validates and re-encodes to the
    /// same bytes, `Some(false)` on a decoder / encoder disagreement (a bug).
    #[must_use]
    pub fn group_invite_preview_plaintext(plain: &[u8]) -> Option<bool> {
        crate::group_invite::fuzz_preview_plaintext(plain)
    }
}
