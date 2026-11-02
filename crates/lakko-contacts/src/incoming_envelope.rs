// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Owner-side incoming-envelope dispatcher
//! (Phase 3c3-incoming-dispatcher).
//!
//! Single-call entry point the platform-side polling loop
//! uses after HPKE-opening a [`crate::request::ContactRequest`]
//! envelope. Runs the two call-side unwrap helpers in
//! **dispatch order** and surfaces a typed discriminated
//! result so the caller can branch on the envelope's kind
//! without re-implementing the order or risking a
//! cross-dispatch mistake.
//!
//! ## Dispatch order (load-bearing)
//!
//! 1. [`crate::call_welcome::try_unwrap_call_welcome`] — the
//!    first-joiner branch of a link call. Owner runs
//!    `Group::join_from_welcome` and enters the call group.
//! 2. [`crate::call_kp_forward::try_unwrap_call_kp_forward`]
//!    — the subsequent-joiner branch of a multi-party link
//!    call. Committer runs
//!    `Group::add_member_from_wire(kp_bytes)` + commits +
//!    ferries the resulting Welcome back via
//!    [`crate::call_welcome::wrap_call_welcome`].
//! 3. [`crate::media_state::try_unwrap_media_state`] — the
//!    camera on/off signal (camera-off-cleanup). Caller
//!    resets the sender's tile `camOn` state.
//! 4. [`crate::push_routing::try_unwrap_push_routing_secret`]
//!    — the in-group push-routing-secret distribution
//!    envelope (ADR-0042). Caller persists the 32-byte
//!    secret + builds the `push_gid → group_id` reverse
//!    index.
//! 5. [`crate::committer_beacon::try_unwrap_committer_beacon`]
//!    — the committer liveness beacon (ADR-0045 decision 2,
//!    variant 1). Caller folds it into the acting-committer
//!    decision via
//!    [`crate::committer_beacon::acting_committer_verdict`],
//!    pairing it with the MLS sender leaf and the DS row age.
//! 6. [`crate::profile::try_unwrap_profile`] — a member's
//!    profile (ADR-0048). Caller stores it under the sender's
//!    signature key resolved at receipt (C-68), keeps it only if
//!    [`crate::profile::profile_is_newer`] than what it holds, and
//!    verifies any claimed username through discovery before
//!    showing it.
//! 7. [`crate::group_name::try_unwrap_group_name`] — the group's
//!    shared name (ADR-0051 §1). Caller resolves the MLS sender
//!    leaf to the account key (C-68), runs the ADR-0051 §2 role
//!    gate (owner / admin, or any member when the group has no
//!    role table), and keeps it only if
//!    [`crate::group_name::group_name_is_newer`] than the stored
//!    stamp for that group. An empty name clears the name.
//! 8. [`crate::profile_request::try_unwrap_profile_request`],
//!    [`crate::pin::try_unwrap_pin`] and
//!    [`crate::registrar_seed::try_unwrap_registrar_seed`] (ADR-0048
//!    trigger 4, ADR-0052, ADR-0057): service envelopes the caller acts
//!    on and never renders. Then
//!    [`crate::shared_channels::try_unwrap_shared_channels`] (ADR-0059
//!    §6): the listed channels one member shares with the group, and
//!    [`crate::committer_delegation::try_unwrap_committer_delegation`]
//!    (ADR-0060 §6): a proposer's delegations for the committer that
//!    sweeps its `Add`. Then
//!    [`crate::group_closed::try_unwrap_group_closed`] (ADR-0073 phase 2):
//!    an admin closed the group and names the recreated one. Never
//!    rendered.
//! 9. [`crate::introduction::try_unwrap_introduction_offer`] and
//!    [`crate::introduction::try_unwrap_introductions`] — a contact's
//!    introduction record to relay, and a contact's bundle of records to
//!    search (ADR-0060 §4).
//! 10. [`crate::visibility::try_unwrap_visibility`] and
//!     [`crate::visibility::try_unwrap_reach_state`] — the owner's own
//!     setting and reach-key versions, from another of their devices
//!     (ADR-0060 §7).
//!     (The device card of ADR-0080,
//!     [`crate::device_card::try_unwrap_device_card`], and the link-request
//!     claim of ADR-0079, [`crate::link_claim::try_unwrap_link_claim`], are
//!     checked earlier, right after the key-rotation envelope; neither
//!     separator prefixes another, so the position changes nothing.)
//! 11. **Intro-message fallthrough** — plain free-text
//!     `ContactRequest` (ADR-0005 scenario A; "add me as a
//!     contact"). Caller renders the bytes as a UTF-8 intro
//!     message.
//!
//! The order matters: the two call-side envelopes are
//! mutually exclusive (see C-32 and C-33 in
//! [`docs/security/calls.md`](../../docs/security/calls.md))
//! so swapping the order would not in practice misroute a
//! payload, but pinning the order in **one** place — this
//! dispatcher — keeps the platform polling loops on web /
//! iOS / Android in lock-step. A future addition of a new
//! envelope kind (e.g. group-invite Welcome) lands as a
//! new variant on [`IncomingEnvelope`] + a new step in
//! [`dispatch_incoming_envelope`], not as a parallel edit
//! on three platforms.
//!
//! ## What this module does NOT do
//!
//! - The HPKE-open of the outer
//!   [`crate::request::ContactRequest`] envelope. That's
//!   [`crate::request::open_envelope`] and runs **before**
//!   the dispatcher.
//! - The MLS operations (`join_from_welcome`,
//!   `add_member_from_wire`). Those live in `lakko-mls` and
//!   run **after** the dispatcher.
//! - Identifying the joiner. The outer envelope's Ed25519
//!   signature pins which identity sent the payload; this
//!   module sees only the decrypted plaintext.

use thiserror::Error;

use crate::admission_seed::{try_unwrap_admission_seed, AdmissionSeedEnvelope, AdmissionSeedError};
use crate::call_kp_forward::try_unwrap_call_kp_forward;
use crate::call_welcome::{try_unwrap_call_welcome, CallWelcomeError};
use crate::committer_beacon::{try_unwrap_committer_beacon, CommitterBeaconError};
use crate::committer_delegation::{
    try_unwrap_committer_delegation, CommitterDelegationEnvelope, CommitterDelegationError,
};
use crate::conversation_handle::{
    try_unwrap_conversation_handle, ConversationHandle, ConversationHandleError,
};
use crate::device_card::{try_unwrap_device_card, DeviceCard, DeviceCardError};
use crate::group_closed::{try_unwrap_group_closed, GroupClosedError, SUCCESSOR_GROUP_ID_LEN};
use crate::group_name::{try_unwrap_group_name, GroupName, GroupNameError};
use crate::introduction::{
    try_unwrap_introduction_offer, try_unwrap_introductions, IntroductionError, IntroductionRecord,
    IntroductionsBundle,
};
use crate::key_rotation::{try_unwrap_key_rotation, KeyRotation, KeyRotationError};
use crate::knock_seed::{try_unwrap_knock_seed, KnockSeedEnvelope, KnockSeedError};
use crate::link_claim::{try_unwrap_link_claim, LinkClaim, LinkClaimError};
use crate::link_founded::{try_unwrap_link_founded, LinkFounded, LinkFoundedError};
use crate::media_state::{try_unwrap_media_state, MediaStateError};
use crate::pin::{try_unwrap_pin, PinError, PinOp};
use crate::profile::{try_unwrap_profile, Profile, ProfileError};
use crate::profile_request::{try_unwrap_profile_request, ProfileRequestError};
use crate::push_routing::{try_unwrap_push_routing_secret, PushRoutingSecretError};
use crate::registrar_seed::{try_unwrap_registrar_seed, RegistrarSeedEnvelope, RegistrarSeedError};
use crate::shared_channels::{try_unwrap_shared_channels, SharedChannels, SharedChannelsError};
use crate::visibility::{
    try_unwrap_reach_state, try_unwrap_visibility, ReachState, VisibilityError, VisibilitySetting,
};

/// Discriminated dispatch result. Borrow-flavoured slice so
/// the dispatcher itself is zero-copy; the caller decides
/// whether to clone bytes when it persists / forwards them
/// (e.g. handing a Welcome to `Group::join_from_welcome`
/// which takes a slice).
///
/// Not `Copy`: the [`IncomingEnvelope::Profile`] variant owns its
/// decoded strings, because a profile is validated field by field
/// (UTF-8, bounds) and handing back the raw slices would invite a
/// caller to re-read unvalidated bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncomingEnvelope<'a> {
    /// First-joiner Welcome for a link-based call. Caller
    /// drives `Group::join_from_welcome(payload)`.
    CallWelcome(&'a [u8]),
    /// Subsequent-joiner KeyPackage for a multi-party
    /// link-based call. Caller drives
    /// `Group::add_member_from_wire(payload)` + ferries the
    /// resulting Welcome back via
    /// `wrap_call_welcome(welcome)`.
    CallKeyPackage(&'a [u8]),
    /// App-level camera on/off media-state signal
    /// (camera-off-cleanup). Rides the same opaque relay
    /// transport as the call envelopes but carries a fully
    /// decoded flag rather than a byte slice. Caller resets
    /// the sender's tile `camOn` state to `camera_on`.
    MediaState {
        /// Whether the sender's camera is currently on.
        camera_on: bool,
    },
    /// In-group push-routing-secret distribution envelope
    /// (ADR-0042, v2). Rides an MLS application message on the
    /// group journal; carries the 32-byte per-group
    /// `push_routing_secret` a member/joiner persists to derive the
    /// opaque `push_gid`. Caller stores the secret + builds the
    /// `push_gid → (account, group_id)` reverse index — never
    /// rendered as a chat bubble.
    ///
    /// **The caller must not store this unconditionally.** Run the
    /// role gate, then
    /// [`crate::push_routing::push_routing_secret_is_newer`]
    /// against whatever is already stored — a stale re-broadcast
    /// carries a valid secret from an authorised sender and is
    /// refused only by the order.
    PushRoutingSecret {
        /// The minter's recency stamp; the primary key of the
        /// canonical total order.
        mint_epoch: u64,
        /// The decoded 32-byte per-group routing secret.
        secret: [u8; crate::push_routing::PUSH_ROUTING_SECRET_LEN],
    },
    /// Committer liveness beacon (ADR-0045 decision 2, variant 1).
    /// Rides an MLS application message on the group journal, and
    /// carries **no fields** — everything the receiver needs comes
    /// from outside the payload, which is why this is a unit
    /// variant and not a struct one. Never rendered as a chat
    /// bubble.
    ///
    /// The caller pairs it with two values the dispatcher does not
    /// see: the **MLS sender leaf** (resolved through
    /// `Group::members()` to a signature key, then to a rank via
    /// `lakko_mls::group_roles::committer_rank`) and the DS row's
    /// server-assigned `created_at` (giving the age). Those two
    /// become a
    /// [`crate::committer_beacon::BeaconObservation`], and the set
    /// of them feeds
    /// [`crate::committer_beacon::acting_committer_verdict`].
    ///
    /// **A beacon licenses "a candidate is active", never "this
    /// person is online"** — no UI copy may imply the stronger
    /// claim.
    CommitterBeacon,
    /// A member's profile (ADR-0048): display name, claimed username,
    /// sender-clock stamp — already validated against every bound.
    ///
    /// **Every field is the sender's claim.** The caller stores it
    /// under the sender's signature key resolved at receipt (C-68,
    /// never a leaf index, never a key the payload might name), keeps
    /// it only when [`crate::profile::profile_is_newer`] than the
    /// stored one, and shows `username` only after verifying it
    /// through discovery (ADR-0048 §3). Never rendered as a chat
    /// bubble.
    Profile(Profile),
    /// The group's shared name (ADR-0051 §1): the name (empty = clear)
    /// and the setter-clock stamp — already validated against every
    /// bound. Never rendered as a chat bubble.
    ///
    /// **The caller must not store this unconditionally.** The sender's
    /// authority is the receiver's check (ADR-0051 §2): resolve the MLS
    /// sender leaf to the account key at receipt (C-68), read the live
    /// role table, accept only from the owner or an admin (or from any
    /// member when the group has no role table), then keep it only when
    /// [`crate::group_name::group_name_is_newer`] than the stored stamp
    /// for that group — the whole read-decide-write under the per-group
    /// lock (C-62 sub-invariant 5).
    GroupName(GroupName),
    /// A member asks the group to introduce itself (ADR-0048 §2,
    /// trigger 4). Carries nothing; the receiver answers with its own
    /// profile through the throttled growth path. Never a chat bubble.
    ProfileRequest,
    /// A shared pin or unpin of one message (ADR-0052 decision 3). Never a
    /// chat bubble. **The caller must not store this unconditionally:**
    /// resolve the MLS sender to the account key at receipt (C-68), accept
    /// only from the owner or an admin of the live role table (any member
    /// of a table-less group; either participant of a one-to-one), then
    /// keep it only when [`crate::pin::pin_is_newer`] than the stored stamp
    /// for that message — the whole read-decide-write under the per-group
    /// lock (C-62 sub-invariant 5).
    Pin(PinOp),
    /// A contact's introduction record to relay (ADR-0060 §4), structurally
    /// checked and still sealed. Never a chat bubble. **Accept it only from
    /// the other participant of a 1:1 conversation** and keep the newest per
    /// contact; it goes into the bundles this device sends its contacts.
    IntroductionOffer(IntroductionRecord),
    /// A contact's bundle of introduction records (ADR-0060 §4), padded to a
    /// size bucket. Never a chat bubble. Keep the newest per contact via
    /// [`crate::introduction::bundle_is_newer`]; search it locally with OPRF
    /// outputs. Never re-relayed (contacts of contacts, not further).
    Introductions(IntroductionsBundle),
    /// The owner's "who can find me" setting, from another of their devices
    /// (ADR-0060 §7). Never a chat bubble. **Accept only on the owner's
    /// DeviceGroup**, keep via [`crate::visibility::visibility_is_newer`].
    Visibility(VisibilitySetting),
    /// The owner's reach-key version counters, from another of their
    /// devices (ADR-0060 §7). Same rules as [`Self::Visibility`], ordered by
    /// [`crate::visibility::reach_state_is_newer`].
    ReachState(ReachState),
    /// A journal root hands one member its Delivery Service registrar seed
    /// (ADR-0057), sealed to that member's account X25519 key. Never a chat
    /// bubble. **The caller keeps it only when addressed to its own account
    /// key** ([`RegistrarSeedEnvelope::is_addressed_to`]) and the MLS sender
    /// resolves at receipt (C-68) to the group's owner; it opens the seed
    /// with its account X25519 secret and drops the envelope on a failed
    /// open (or a seed that does not derive the clear registrar public key).
    /// Every other member keeps only `member_account_key ->
    /// registrar_public_key` (from an owner-sent envelope), the mapping it
    /// revokes from when it commits that account's removal; it never opens
    /// the seed.
    RegistrarSeed(RegistrarSeedEnvelope),
    /// The listed channels one member shares with the group (ADR-0059 §6),
    /// already validated against every bound. Never a chat bubble. **The
    /// caller stores it per member**, keyed on the sender's account key
    /// resolved at receipt (C-68), keeps it only when
    /// [`crate::shared_channels::shared_channels_is_newer`] than the list
    /// stored for that member, under the per-group lock (C-62 sub-invariant
    /// 5). Any member may send one; it speaks only for its sender. An empty
    /// list means the sender stopped sharing.
    SharedChannels(SharedChannels),
    /// A proposer's committer delegations (ADR-0060 §6), structurally
    /// checked only. Never a chat bubble. **The caller parses the blobs with
    /// `lakko_ds_proof::committer`** (each must name the added person's
    /// mailbox; no two the same delegate), keeps the newest envelope per
    /// `(MLS sender resolved at receipt, added_account_key)`, and uses a
    /// delegation only when it names its own account key, only to deliver
    /// the Welcome of a sweep that added that person.
    CommitterDelegation(CommitterDelegationEnvelope),
    /// An admin closed this group and, when it was recreated, names the
    /// successor (ADR-0073 phase 2, `docs/ux/identity-recovery.md` §16.5).
    /// Never a chat bubble. **The caller must not store this
    /// unconditionally:** resolve the MLS sender to the account key at
    /// receipt (C-68), accept only from the owner or an admin of the live
    /// role table (a plain member's notice is dropped), then keep it only
    /// when [`crate::group_closed::group_closed_is_newer`] than the stored
    /// stamp for that group — the whole read-decide-write under the
    /// per-group lock (C-62 sub-invariant 5). The successor id is the
    /// admin's claim: offer `Open the new group` only once this device has
    /// joined it through the ordinary invitation path.
    GroupClosed {
        /// Closer-clock UNIX milliseconds at which the group was closed.
        closed_at_ms: u64,
        /// The recreated group's id, or `None` when closed without one.
        successor_group_id: Option<[u8; SUCCESSOR_GROUP_ID_LEN]>,
    },
    /// The account's keys are being changed by another of its devices, and
    /// this is the new recovery-phrase entropy with the proof that the old
    /// account key authorised it (ADR-0074 section 3.3). Never a chat
    /// bubble, and meaningful ONLY in the account's own DeviceGroup: a
    /// client that meets one in any other group drops it. **The caller must
    /// not adopt this on receipt:** check the proof against the account key
    /// it holds and the entropy against the proof's new key (one call on
    /// both FFI surfaces), store it as pending, and adopt only once the
    /// server-side change is visible. See [`crate::key_rotation`].
    KeyRotation(KeyRotation),
    /// What another device of this account says about itself (ADR-0080):
    /// platform, model, versions, city, last activity. Never a chat bubble,
    /// and meaningful ONLY in the account's own DeviceGroup: a client that
    /// meets one in any other group drops it. The caller stores it under the
    /// sender's DeviceGroup leaf signature key resolved at receipt (C-68),
    /// keeps it only when [`crate::device_card::device_card_is_newer`] than
    /// the stored card for that key, and drops it when the leaf leaves.
    DeviceCard(DeviceCard),
    /// Another device of this account claims a contact request sent through
    /// one of the account's invite links (ADR-0079, as reworked 2026-10-17).
    /// Never a chat bubble, and meaningful ONLY in the account's own
    /// DeviceGroup: a client that meets one in any other group drops it. The
    /// caller honours it only from a resolvable DeviceGroup member (C-68),
    /// keeps the earliest DS message id per request
    /// ([`LinkClaim::request_key`]) and decides its own claim with
    /// [`crate::link_claim::link_claim_outcome`] under its per-group lock
    /// (C-62 sub-invariant 5).
    LinkClaim(LinkClaim),
    /// The device that won a link request founded its one-to-one and names it
    /// here, with the founding Welcome, before delivering that Welcome to
    /// anyone (ADR-0079, ordering of 2026-10-17). Never a chat bubble, and
    /// meaningful ONLY in the account's own DeviceGroup. The caller keeps the
    /// earliest note per request from a resolvable DeviceGroup member (C-68);
    /// a device that holds one never founds for that request, it writes the
    /// note's Welcome to the requester.
    LinkFounded(LinkFounded),
    /// A group's `@name` and its public or private flag (ADR-0087 section
    /// 11). Never a chat bubble. The caller verifies the certificate with one
    /// OPRF evaluation over the name, then the signature under the
    /// certificate's handle key for this group
    /// ([`ConversationHandle::verify_signature`]), and keeps it only when
    /// [`crate::conversation_handle::conversation_handle_is_newer`] than the
    /// stored stamp. No role check: any member may relay it.
    ConversationHandle(ConversationHandle),
    /// A knock-key seed sealed to one admin (ADR-0087 section 5). Never a
    /// chat bubble. The receiver keeps it only under the rule in
    /// [`crate::knock_seed`]: addressed to its own account, for this group,
    /// sent by the owner or an admin resolved at receipt, received by an
    /// owner or admin, and opening to a seed that derives the clear public
    /// key.
    KnockSeed(KnockSeedEnvelope),
    /// A private channel's admission-key seed sealed to one co-publisher
    /// (ADR-0087 section 7). Never a chat bubble. The receiver keeps it only
    /// under the rule in [`crate::admission_seed`]: on the channel's
    /// PublisherGroup journal, addressed to its own account, sent by the
    /// channel's owner resolved at receipt, and opening to a seed that
    /// derives the clear public key.
    AdmissionSeed(AdmissionSeedEnvelope),
    /// Plain free-text intro message (ADR-0005 scenario A).
    /// Caller renders as a UTF-8 ContactRequest.
    IntroMessage(&'a [u8]),
}

/// Error surfaced by [`dispatch_incoming_envelope`] when a
/// domain separator matched but the payload behind it was
/// malformed. One variant per envelope kind that can fail —
/// fail-closed: a syntactically-marked but semantically-invalid
/// payload is logged + dropped by the caller, never silently
/// mis-rendered as an intro message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DispatchError {
    /// A call-side envelope (Welcome / KP-forward) matched its
    /// domain separator but its payload was truncated below the
    /// kind's minimum floor.
    #[error(transparent)]
    Call(#[from] CallWelcomeError),
    /// A media-state envelope matched its domain separator but
    /// its payload was malformed (wrong length / unknown
    /// version / out-of-range flag byte).
    #[error(transparent)]
    MediaState(#[from] MediaStateError),
    /// A push-routing-secret envelope (ADR-0042) matched its
    /// domain separator but its payload was malformed (wrong
    /// length / unknown version).
    #[error(transparent)]
    PushRoutingSecret(#[from] PushRoutingSecretError),
    /// A committer-beacon envelope (ADR-0045) matched its domain
    /// separator but its payload was malformed (wrong length /
    /// unknown version).
    #[error(transparent)]
    CommitterBeacon(#[from] CommitterBeaconError),
    /// A profile envelope (ADR-0048) matched its domain separator
    /// but was malformed or out of bounds. The whole envelope is
    /// refused — a profile has no remainder worth keeping.
    #[error(transparent)]
    Profile(#[from] ProfileError),
    /// A group-name envelope (ADR-0051) matched its domain separator
    /// but was malformed or out of bounds. The whole envelope is
    /// refused — a name has no remainder worth keeping.
    #[error(transparent)]
    GroupName(#[from] GroupNameError),
    /// The profile-request domain with a malformed payload.
    #[error(transparent)]
    ProfileRequest(#[from] ProfileRequestError),
    /// The pin domain (ADR-0052) with a malformed payload.
    #[error(transparent)]
    Pin(#[from] PinError),
    /// An introduction offer or bundle matched its domain but was
    /// malformed.
    #[error(transparent)]
    Introduction(#[from] IntroductionError),
    /// A setting or reach-state envelope matched its domain but was
    /// malformed.
    #[error(transparent)]
    Visibility(#[from] VisibilityError),
    /// The registrar-seed domain (ADR-0057) with a malformed payload.
    #[error(transparent)]
    RegistrarSeed(#[from] RegistrarSeedError),
    /// The shared-channels domain (ADR-0059) with a malformed payload.
    #[error(transparent)]
    SharedChannels(#[from] SharedChannelsError),
    /// The committer-delegation domain (ADR-0060 §6) with a malformed
    /// payload.
    #[error(transparent)]
    CommitterDelegation(#[from] CommitterDelegationError),
    /// The group-closed domain (ADR-0073 phase 2) with a malformed payload.
    #[error(transparent)]
    GroupClosed(#[from] GroupClosedError),
    /// The key-rotation domain (ADR-0074 section 3.3) with a malformed
    /// payload.
    #[error(transparent)]
    KeyRotation(#[from] KeyRotationError),
    /// The device-card domain (ADR-0080) with a malformed payload.
    #[error(transparent)]
    DeviceCard(#[from] DeviceCardError),
    /// The link-claim domain (ADR-0079) with a malformed payload.
    #[error(transparent)]
    LinkClaim(#[from] LinkClaimError),
    /// The link-founded domain (ADR-0079) with a malformed payload.
    #[error(transparent)]
    LinkFounded(#[from] LinkFoundedError),
    /// The conversation-handle domain (ADR-0087) with a malformed payload.
    #[error(transparent)]
    ConversationHandle(#[from] ConversationHandleError),
    /// The knock-seed domain (ADR-0087) with a malformed payload.
    #[error(transparent)]
    KnockSeed(#[from] KnockSeedError),
    /// The admission-seed domain (ADR-0087) with a malformed payload.
    #[error(transparent)]
    AdmissionSeed(#[from] AdmissionSeedError),
}

/// Single-call dispatcher. Runs the two unwrap helpers in
/// dispatch order and returns the typed variant.
///
/// # Errors
///
/// Propagates the **first** malformed-payload error
/// encountered — a domain separator matched but the payload
/// behind it was invalid. Distinct kinds carry different
/// validity rules (32-byte floor for Welcome, 256-byte floor
/// for KP-forward, exact 2-byte payload for media-state); a
/// malformed envelope of any kind surfaces here as an
/// `Err(DispatchError)` — caller logs + drops.
///
/// **Never** returns an error when the plaintext is a plain
/// intro message — empty plaintexts surface as
/// `Ok(IntroMessage(&[]))`, which is a valid (degenerate)
/// ContactRequest payload the UI can render as an empty
/// message body.
pub fn dispatch_incoming_envelope(plaintext: &[u8]) -> Result<IncomingEnvelope<'_>, DispatchError> {
    // 1. Call-Welcome — first joiner.
    if let Some(welcome) = try_unwrap_call_welcome(plaintext)? {
        return Ok(IncomingEnvelope::CallWelcome(welcome));
    }
    // 2. Call-KP-Forward — subsequent joiner.
    if let Some(kp) = try_unwrap_call_kp_forward(plaintext)? {
        return Ok(IncomingEnvelope::CallKeyPackage(kp));
    }
    // 3. Media-state — camera on/off signal.
    if let Some(state) = try_unwrap_media_state(plaintext)? {
        return Ok(IncomingEnvelope::MediaState {
            camera_on: state.camera_on,
        });
    }
    // 4. Push-routing-secret — in-group secret distribution (ADR-0042).
    if let Some(envelope) = try_unwrap_push_routing_secret(plaintext)? {
        return Ok(IncomingEnvelope::PushRoutingSecret {
            mint_epoch: envelope.mint_epoch,
            secret: envelope.secret,
        });
    }
    // 5. Committer liveness beacon — ADR-0045 decision 2.
    if try_unwrap_committer_beacon(plaintext)?.is_some() {
        return Ok(IncomingEnvelope::CommitterBeacon);
    }
    // 6. Profile — ADR-0048.
    if let Some(profile) = try_unwrap_profile(plaintext)? {
        return Ok(IncomingEnvelope::Profile(profile));
    }
    // 7. Group name — ADR-0051.
    if let Some(group_name) = try_unwrap_group_name(plaintext)? {
        return Ok(IncomingEnvelope::GroupName(group_name));
    }
    // 7b. Profile request — ADR-0048 §2 trigger 4.
    if try_unwrap_profile_request(plaintext)?.is_some() {
        return Ok(IncomingEnvelope::ProfileRequest);
    }
    // 7c. Shared pin — ADR-0052 decision 3.
    if let Some(op) = try_unwrap_pin(plaintext)? {
        return Ok(IncomingEnvelope::Pin(op));
    }
    // 7d. Registrar seed — ADR-0057.
    if let Some(envelope) = try_unwrap_registrar_seed(plaintext)? {
        return Ok(IncomingEnvelope::RegistrarSeed(envelope));
    }
    // 7e. Shared channels — ADR-0059 §6.
    if let Some(list) = try_unwrap_shared_channels(plaintext)? {
        return Ok(IncomingEnvelope::SharedChannels(list));
    }
    // 7f. Committer delegation — ADR-0060 §6.
    if let Some(envelope) = try_unwrap_committer_delegation(plaintext)? {
        return Ok(IncomingEnvelope::CommitterDelegation(envelope));
    }
    // 7g. Group closed — ADR-0073 phase 2.
    if let Some(closed) = try_unwrap_group_closed(plaintext)? {
        return Ok(IncomingEnvelope::GroupClosed {
            closed_at_ms: closed.closed_at_ms,
            successor_group_id: closed.successor_group_id,
        });
    }
    // 7h. Key rotation — ADR-0074 section 3.3.
    if let Some(rotation) = try_unwrap_key_rotation(plaintext)? {
        return Ok(IncomingEnvelope::KeyRotation(rotation));
    }
    // 7i. Device card — ADR-0080.
    if let Some(card) = try_unwrap_device_card(plaintext)? {
        return Ok(IncomingEnvelope::DeviceCard(card));
    }
    // 7j. Link-request claim — ADR-0079 (reworked 2026-10-17).
    if let Some(claim) = try_unwrap_link_claim(plaintext)? {
        return Ok(IncomingEnvelope::LinkClaim(claim));
    }
    // 7k. Link-founded note — ADR-0079 (ordering of 2026-10-17).
    if let Some(note) = try_unwrap_link_founded(plaintext)? {
        return Ok(IncomingEnvelope::LinkFounded(note));
    }
    // 7l. Conversation handle — ADR-0087 section 11.
    if let Some(handle) = try_unwrap_conversation_handle(plaintext)? {
        return Ok(IncomingEnvelope::ConversationHandle(handle));
    }
    // 7m. Knock seed — ADR-0087 section 5.
    if let Some(seed) = try_unwrap_knock_seed(plaintext)? {
        return Ok(IncomingEnvelope::KnockSeed(seed));
    }
    // 7n. Channel admission seed — ADR-0087 section 7.
    if let Some(seed) = try_unwrap_admission_seed(plaintext)? {
        return Ok(IncomingEnvelope::AdmissionSeed(seed));
    }
    // 8. Plain intro message — ADR-0005 scenario A.
    if let Some(record) = try_unwrap_introduction_offer(plaintext)? {
        return Ok(IncomingEnvelope::IntroductionOffer(record));
    }
    if let Some(bundle) = try_unwrap_introductions(plaintext)? {
        return Ok(IncomingEnvelope::Introductions(bundle));
    }
    if let Some(setting) = try_unwrap_visibility(plaintext)? {
        return Ok(IncomingEnvelope::Visibility(setting));
    }
    if let Some(state) = try_unwrap_reach_state(plaintext)? {
        return Ok(IncomingEnvelope::ReachState(state));
    }
    Ok(IncomingEnvelope::IntroMessage(plaintext))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_kp_forward::{wrap_call_kp_forward, CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1};
    use crate::call_welcome::{wrap_call_welcome, CALL_WELCOME_ENVELOPE_DOMAIN_V1};

    #[test]
    fn dispatches_a_registrar_seed_and_never_renders_it() {
        use crate::registrar_seed::{compose_registrar_seed, RegistrarSeedRank};
        use lakko_crypto::agreement::SecretKey;
        let secret = SecretKey::from_bytes([0x21; 32]);
        let wire = compose_registrar_seed(
            &[1; 32],
            &[2; 32],
            &secret.public_key(),
            RegistrarSeedRank::Admin,
            &[3; 32],
        )
        .unwrap();
        match dispatch_incoming_envelope(&wire).unwrap() {
            IncomingEnvelope::RegistrarSeed(env) => {
                assert_eq!(env.rank, RegistrarSeedRank::Admin);
                assert_eq!(
                    env.registrar_public_key,
                    crate::registrar_seed::registrar_public_key(&[3; 32])
                );
                assert_eq!(env.open(&secret).unwrap(), [3; 32]);
            },
            other => panic!("expected a registrar seed, got {other:?}"),
        }
        // A truncated one is a dispatch error, not an intro message.
        assert!(matches!(
            dispatch_incoming_envelope(&wire[..wire.len() - 1]),
            Err(DispatchError::RegistrarSeed(_))
        ));
    }

    #[test]
    fn dispatches_shared_channels_and_never_renders_them() {
        use crate::shared_channels::{compose_shared_channels, SharedChannel, SharedChannels};
        let list = SharedChannels {
            updated_at_ms: 7,
            channels: vec![SharedChannel {
                channel_id: [3; 16],
                handle: "lakko_news".to_string(),
                name: "Lakko News".to_string(),
            }],
        };
        let wire = compose_shared_channels(&list).unwrap();
        assert_eq!(
            dispatch_incoming_envelope(&wire).unwrap(),
            IncomingEnvelope::SharedChannels(list)
        );
        assert!(matches!(
            dispatch_incoming_envelope(&wire[..wire.len() - 1]),
            Err(DispatchError::SharedChannels(_))
        ));
    }

    #[test]
    fn dispatches_a_committer_delegation_and_never_renders_it() {
        use crate::committer_delegation::{
            compose_committer_delegation, COMMITTER_DELEGATION_BLOB_LEN,
            COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1,
        };
        let env = CommitterDelegationEnvelope {
            added_account_key: [4; 32],
            added_mailbox_binding: crate::mailbox::MailboxBinding::for_account_seed(&[4; 32]),
            delegations: vec![[1; COMMITTER_DELEGATION_BLOB_LEN]; 2],
        };
        let wire = compose_committer_delegation(&env).unwrap();
        assert_eq!(
            dispatch_incoming_envelope(&wire).unwrap(),
            IncomingEnvelope::CommitterDelegation(env),
        );
        let mut bad = COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1.to_vec();
        bad.push(0x7F);
        assert_eq!(
            dispatch_incoming_envelope(&bad),
            Err(DispatchError::CommitterDelegation(
                CommitterDelegationError::UnknownVersion(0x7F)
            )),
        );
        assert!(matches!(
            dispatch_incoming_envelope(&wire[..wire.len() - 1]),
            Err(DispatchError::CommitterDelegation(_))
        ));
    }

    #[test]
    fn dispatches_a_link_claim_and_never_renders_it() {
        use crate::link_claim::{compose_link_claim, LINK_CLAIM_ENVELOPE_DOMAIN_V1};
        let claim = LinkClaim {
            link_id: [5; 16],
            requester_account_key: [6; 32],
        };
        let wire = compose_link_claim(&claim);
        assert_eq!(
            dispatch_incoming_envelope(&wire).unwrap(),
            IncomingEnvelope::LinkClaim(claim)
        );
        assert!(matches!(
            dispatch_incoming_envelope(&wire[..wire.len() - 1]),
            Err(DispatchError::LinkClaim(_))
        ));
        let mut bad = LINK_CLAIM_ENVELOPE_DOMAIN_V1.to_vec();
        bad.push(0x7F);
        assert_eq!(
            dispatch_incoming_envelope(&bad),
            Err(DispatchError::LinkClaim(LinkClaimError::UnknownVersion(
                0x7F
            )))
        );
    }

    #[test]
    fn dispatches_a_link_founded_note_and_never_renders_it() {
        use crate::link_founded::{compose_link_founded, LINK_FOUNDED_ENVELOPE_DOMAIN_V1};
        let note = LinkFounded {
            link_id: [5; 16],
            requester_account_key: [6; 32],
            group_id: [7; 32],
            welcome: vec![8; 40],
        };
        let wire = compose_link_founded(&note).unwrap();
        assert_eq!(
            dispatch_incoming_envelope(&wire).unwrap(),
            IncomingEnvelope::LinkFounded(note)
        );
        assert!(matches!(
            dispatch_incoming_envelope(&wire[..wire.len() - 1]),
            Err(DispatchError::LinkFounded(_))
        ));
        let mut bad = LINK_FOUNDED_ENVELOPE_DOMAIN_V1.to_vec();
        bad.push(0x7F);
        assert_eq!(
            dispatch_incoming_envelope(&bad),
            Err(DispatchError::LinkFounded(
                LinkFoundedError::UnknownVersion(0x7F)
            ))
        );
    }

    #[test]
    fn dispatches_call_welcome_envelope() {
        let welcome = vec![0xABu8; 256];
        let wrapped = wrap_call_welcome(&welcome);
        let result = dispatch_incoming_envelope(&wrapped).unwrap();
        assert_eq!(result, IncomingEnvelope::CallWelcome(welcome.as_slice()));
    }

    #[test]
    fn dispatches_call_kp_forward_envelope() {
        let kp = vec![0xCDu8; 512];
        let wrapped = wrap_call_kp_forward(&kp);
        let result = dispatch_incoming_envelope(&wrapped).unwrap();
        assert_eq!(result, IncomingEnvelope::CallKeyPackage(kp.as_slice()));
    }

    #[test]
    fn dispatches_committer_beacon_envelope() {
        use crate::committer_beacon::compose_committer_beacon;
        let wrapped = compose_committer_beacon();
        assert_eq!(
            dispatch_incoming_envelope(&wrapped).unwrap(),
            IncomingEnvelope::CommitterBeacon,
        );
    }

    #[test]
    fn a_malformed_beacon_is_an_error_not_an_intro_message() {
        // Fail-closed: the domain matched, so the payload behind it is
        // a broken beacon and must NOT fall through to be rendered as
        // chat text. This is the discriminating case for the dispatch
        // order — an implementation that swallowed the beacon error
        // would surface these bytes as a bubble containing a NUL and
        // a stray version byte.
        use crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1;
        let mut wrapped = COMMITTER_BEACON_ENVELOPE_DOMAIN_V1.to_vec();
        wrapped.push(0x02); // unknown version
        assert!(matches!(
            dispatch_incoming_envelope(&wrapped),
            Err(DispatchError::CommitterBeacon(_)),
        ));
    }

    #[test]
    fn dispatches_profile_envelope() {
        use crate::profile::compose_profile;
        let profile = Profile {
            display_name: Some("Alice Petrova".to_string()),
            username: Some("alice".to_string()),
            updated_at_ms: 42,
            account_id: None,
            username_certificate: None,
            grants: Vec::new(),
            avatar: None,
        };
        let wrapped = compose_profile(&profile).unwrap();
        assert_eq!(
            dispatch_incoming_envelope(&wrapped).unwrap(),
            IncomingEnvelope::Profile(profile),
        );
    }

    #[test]
    fn an_out_of_bounds_profile_is_an_error_not_an_intro_message() {
        // Fail-closed: a profile carrying a right-to-left override in
        // its name must be dropped, never rendered as chat text.
        use crate::profile::{DisplayNameError, PROFILE_ENVELOPE_DOMAIN_V1, PROFILE_VERSION_V4};
        let name = "Alice\u{202E}".as_bytes();
        let mut wrapped = PROFILE_ENVELOPE_DOMAIN_V1.to_vec();
        wrapped.push(PROFILE_VERSION_V4);
        wrapped.extend_from_slice(&0u64.to_be_bytes());
        wrapped.push(u8::try_from(name.len()).unwrap());
        wrapped.extend_from_slice(name);
        // No username, account id, certificate, grants or avatar.
        wrapped.extend_from_slice(&[0, 0, 0, 0, 0]);
        assert_eq!(
            dispatch_incoming_envelope(&wrapped),
            Err(DispatchError::Profile(ProfileError::DisplayName(
                DisplayNameError::ControlOrFormatCharacter
            ))),
        );
    }

    #[test]
    fn dispatches_group_name_envelope() {
        use crate::group_name::compose_group_name;
        for name in ["Trio", ""] {
            let group_name = GroupName {
                name: name.to_string(),
                updated_at_ms: 42,
            };
            let wrapped = compose_group_name(&group_name).unwrap();
            let result = dispatch_incoming_envelope(&wrapped).unwrap();
            assert_eq!(result, IncomingEnvelope::GroupName(group_name));
            assert!(!matches!(result, IncomingEnvelope::IntroMessage(_)));
        }
    }

    #[test]
    fn an_out_of_bounds_group_name_is_an_error_not_an_intro_message() {
        // Fail-closed: a name carrying a right-to-left override must be
        // dropped, never rendered as chat text.
        use crate::group_name::{GROUP_NAME_ENVELOPE_DOMAIN_V1, GROUP_NAME_VERSION_V1};
        use crate::profile::DisplayNameError;
        let name = "Trio\u{202E}".as_bytes();
        let mut wrapped = GROUP_NAME_ENVELOPE_DOMAIN_V1.to_vec();
        wrapped.push(GROUP_NAME_VERSION_V1);
        wrapped.extend_from_slice(&0u64.to_be_bytes());
        wrapped.push(u8::try_from(name.len()).unwrap());
        wrapped.extend_from_slice(name);
        assert_eq!(
            dispatch_incoming_envelope(&wrapped),
            Err(DispatchError::GroupName(GroupNameError::Name(
                DisplayNameError::ControlOrFormatCharacter
            ))),
        );
        // A truncated one too: the domain matched, so it is a broken
        // group name, not a bubble reading `lakko/group-name/v1`.
        let mut truncated = GROUP_NAME_ENVELOPE_DOMAIN_V1.to_vec();
        truncated.push(GROUP_NAME_VERSION_V1);
        assert_eq!(
            dispatch_incoming_envelope(&truncated),
            Err(DispatchError::GroupName(GroupNameError::Truncated)),
        );
    }

    #[test]
    fn dispatches_a_group_closed_notice_and_never_renders_it() {
        use crate::group_closed::{
            compose_group_closed, GroupClosed, GROUP_CLOSED_ENVELOPE_DOMAIN_V1,
            GROUP_CLOSED_VERSION_V1,
        };
        for successor in [Some([0x5A; SUCCESSOR_GROUP_ID_LEN]), None] {
            let wire = compose_group_closed(&GroupClosed {
                closed_at_ms: 42,
                successor_group_id: successor,
            });
            let result = dispatch_incoming_envelope(&wire).unwrap();
            assert_eq!(
                result,
                IncomingEnvelope::GroupClosed {
                    closed_at_ms: 42,
                    successor_group_id: successor,
                }
            );
            assert!(!matches!(result, IncomingEnvelope::IntroMessage(_)));
            // A truncated one is a dispatch error, not an intro message
            // reading `lakko/group-closed/v1`.
            assert!(matches!(
                dispatch_incoming_envelope(&wire[..wire.len() - 1]),
                Err(DispatchError::GroupClosed(GroupClosedError::Truncated))
            ));
        }
        let mut bad = GROUP_CLOSED_ENVELOPE_DOMAIN_V1.to_vec();
        bad.push(0x7F);
        assert_eq!(
            dispatch_incoming_envelope(&bad),
            Err(DispatchError::GroupClosed(
                GroupClosedError::UnknownVersion(0x7F)
            )),
        );
        // A successor of the wrong length is refused as a whole.
        let mut odd = GROUP_CLOSED_ENVELOPE_DOMAIN_V1.to_vec();
        odd.push(GROUP_CLOSED_VERSION_V1);
        odd.extend_from_slice(&0u64.to_be_bytes());
        odd.push(16);
        odd.extend_from_slice(&[0; 16]);
        assert_eq!(
            dispatch_incoming_envelope(&odd),
            Err(DispatchError::GroupClosed(
                GroupClosedError::BadSuccessorLen(16)
            )),
        );
    }

    #[test]
    fn dispatches_the_adr_0060_envelopes() {
        use crate::introduction::{
            compose_bundle, compose_introduction, compose_introduction_offer, IntroductionPayload,
        };
        use crate::reach::{ReachClass, ReachGrant};
        use crate::visibility::{compose_reach_state, compose_visibility, ReachStateEntry};

        let payload = IntroductionPayload {
            identity_pk: lakko_crypto::signing::SigningKey::from_seed(&[9; 32])
                .verifying_key()
                .to_bytes(),
            account_id: [2; 16],
            display_name: Some("Anna".to_string()),
            username: None,
            username_certificate: None,
            mailbox_binding: crate::mailbox::MailboxBinding::for_account_seed(&[9; 32]),
            fof: Some(ReachGrant::derive(&[9; 32], ReachClass::Fof, b"", 1).unwrap()),
        };
        let record = compose_introduction(&payload, &[[5; 64]], 1).unwrap();
        let parsed = IntroductionRecord::decode(&record).unwrap();
        assert_eq!(
            dispatch_incoming_envelope(&compose_introduction_offer(&record).unwrap()).unwrap(),
            IncomingEnvelope::IntroductionOffer(parsed.clone()),
        );
        let bundle = compose_bundle(&[parsed], 3).unwrap();
        assert!(matches!(
            dispatch_incoming_envelope(&bundle).unwrap(),
            IncomingEnvelope::Introductions(b) if b.records.len() == 1 && b.updated_at_ms == 3
        ));
        let setting = VisibilitySetting::default_for_new_account();
        assert_eq!(
            dispatch_incoming_envelope(&compose_visibility(&setting).unwrap()).unwrap(),
            IncomingEnvelope::Visibility(setting),
        );
        let state = ReachState {
            entries: vec![ReachStateEntry {
                class: ReachClass::Contact,
                scope_id: Vec::new(),
                version: 4,
            }],
            updated_at_ms: 5,
        };
        assert_eq!(
            dispatch_incoming_envelope(&compose_reach_state(&state).unwrap()).unwrap(),
            IncomingEnvelope::ReachState(state),
        );
    }

    #[test]
    fn a_malformed_adr_0060_envelope_is_an_error_not_an_intro_message() {
        use crate::introduction::{
            INTRODUCTIONS_ENVELOPE_DOMAIN_V1, INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
        };
        use crate::visibility::{REACH_STATE_ENVELOPE_DOMAIN_V1, VISIBILITY_ENVELOPE_DOMAIN_V1};
        for domain in [
            INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
            INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
        ] {
            let mut bad = domain.to_vec();
            bad.push(0x7F);
            assert_eq!(
                dispatch_incoming_envelope(&bad),
                Err(DispatchError::Introduction(
                    IntroductionError::UnknownVersion(0x7F)
                )),
            );
        }
        for domain in [
            VISIBILITY_ENVELOPE_DOMAIN_V1,
            REACH_STATE_ENVELOPE_DOMAIN_V1,
        ] {
            let mut bad = domain.to_vec();
            bad.push(0x7F);
            assert_eq!(
                dispatch_incoming_envelope(&bad),
                Err(DispatchError::Visibility(VisibilityError::UnknownVersion(
                    0x7F
                ))),
            );
        }
    }

    #[test]
    fn dispatches_intro_message_for_plain_text() {
        let intro = b"hi alice it is bob";
        let result = dispatch_incoming_envelope(intro).unwrap();
        assert_eq!(result, IncomingEnvelope::IntroMessage(intro));
    }

    #[test]
    fn dispatches_intro_message_for_empty_plaintext() {
        // Degenerate but legal — empty ContactRequest plaintext
        // is a valid intro message with empty body, NOT a
        // truncated call envelope.
        let result = dispatch_incoming_envelope(&[]).unwrap();
        assert_eq!(result, IncomingEnvelope::IntroMessage(&[]));
    }

    #[test]
    fn truncated_welcome_envelope_surfaces_as_error() {
        // Domain separator matches but payload < 32-byte
        // welcome floor.
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(CALL_WELCOME_ENVELOPE_DOMAIN_V1);
        wrapped.extend_from_slice(&[0u8; 4]);
        let err = dispatch_incoming_envelope(&wrapped).unwrap_err();
        assert_eq!(
            err,
            DispatchError::Call(CallWelcomeError::TruncatedPayload { min: 32, got: 4 }),
        );
    }

    #[test]
    fn truncated_kp_forward_envelope_surfaces_as_error() {
        // Domain separator matches but payload < 256-byte
        // KP-forward floor.
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1);
        wrapped.extend_from_slice(&[0u8; 100]);
        let err = dispatch_incoming_envelope(&wrapped).unwrap_err();
        assert_eq!(
            err,
            DispatchError::Call(CallWelcomeError::TruncatedPayload { min: 256, got: 100 }),
        );
    }

    #[test]
    fn dispatches_media_state_camera_on() {
        let wrapped = crate::media_state::compose_media_state(true);
        let result = dispatch_incoming_envelope(&wrapped).unwrap();
        assert_eq!(result, IncomingEnvelope::MediaState { camera_on: true });
    }

    #[test]
    fn dispatches_media_state_camera_off() {
        let wrapped = crate::media_state::compose_media_state(false);
        let result = dispatch_incoming_envelope(&wrapped).unwrap();
        assert_eq!(result, IncomingEnvelope::MediaState { camera_on: false });
    }

    #[test]
    fn malformed_media_state_surfaces_as_error() {
        // Domain separator matches but the flag byte is out of
        // range — fail-closed, not an intro-message fallthrough.
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1);
        wrapped.extend_from_slice(&[crate::media_state::MEDIA_STATE_VERSION_V1, 0x02]);
        let err = dispatch_incoming_envelope(&wrapped).unwrap_err();
        assert_eq!(
            err,
            DispatchError::MediaState(MediaStateError::BadFlag(0x02))
        );
    }

    #[test]
    fn dispatches_push_routing_secret_envelope() {
        let secret = [0x42u8; crate::push_routing::PUSH_ROUTING_SECRET_LEN];
        let wrapped = crate::push_routing::compose_push_routing_secret(0xDEAD_BEEF, &secret);
        let result = dispatch_incoming_envelope(&wrapped).unwrap();
        assert_eq!(
            result,
            IncomingEnvelope::PushRoutingSecret {
                mint_epoch: 0xDEAD_BEEF,
                secret,
            },
        );
    }

    #[test]
    fn malformed_push_routing_secret_surfaces_as_error() {
        // Domain separator matches but the version byte is unknown
        // — fail-closed, not an intro-message fallthrough.
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1);
        wrapped.push(0x7F);
        wrapped.extend_from_slice(&[0u8; crate::push_routing::PUSH_ROUTING_SECRET_LEN]);
        let err = dispatch_incoming_envelope(&wrapped).unwrap_err();
        assert_eq!(
            err,
            DispatchError::PushRoutingSecret(PushRoutingSecretError::UnknownVersion(0x7F)),
        );
    }

    #[test]
    fn retired_v1_push_routing_secret_never_reaches_the_chat_parser() {
        // The v1 -> v2 flag day at the dispatch layer: a pre-v2
        // peer's envelope must surface as a fail-closed error the
        // caller drops, NOT fall through to `IntroMessage` and get
        // rendered as 62 bytes of garbage text in the timeline.
        let mut wrapped = Vec::new();
        wrapped.extend_from_slice(crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1);
        wrapped.push(0x01);
        wrapped.extend_from_slice(&[0x42u8; crate::push_routing::PUSH_ROUTING_SECRET_LEN]);
        let err = dispatch_incoming_envelope(&wrapped).unwrap_err();
        assert_eq!(
            err,
            DispatchError::PushRoutingSecret(PushRoutingSecretError::UnknownVersion(0x01)),
        );
    }

    #[test]
    fn dispatch_order_welcome_before_kp_forward() {
        // The Welcome and KP-forward envelopes are byte-
        // distinct by C-32+C-33 invariants, so swapping the
        // order would not in practice misroute a payload.
        // What this test pins is that **even if** the
        // dispatcher were fed a payload that looked like a
        // welcome to one helper and like a KP-forward to
        // another (impossible today, but a future-refactor
        // safety net), the call-welcome dispatch wins. Set
        // up by feeding a real welcome envelope: BOTH
        // unwraps see the same byte prefix dispatch — only
        // call-welcome matches, but the assertion is
        // semantic: we get `CallWelcome` back, never
        // `CallKeyPackage`.
        let welcome = vec![0xEEu8; 256];
        let wrapped = wrap_call_welcome(&welcome);
        let result = dispatch_incoming_envelope(&wrapped).unwrap();
        assert!(matches!(result, IncomingEnvelope::CallWelcome(_)));
        assert!(!matches!(result, IncomingEnvelope::CallKeyPackage(_)));
    }

    #[test]
    fn dispatch_is_zero_copy_on_intro_path() {
        // Surface here if a future refactor accidentally
        // copies the intro plaintext when it should be a
        // borrow. The exact pointer equality is checked: the
        // returned slice points into the input.
        let intro: &[u8] = b"hello world";
        let result = dispatch_incoming_envelope(intro).unwrap();
        if let IncomingEnvelope::IntroMessage(slice) = result {
            assert_eq!(slice.as_ptr(), intro.as_ptr());
            assert_eq!(slice.len(), intro.len());
        } else {
            panic!("expected IntroMessage, got {result:?}");
        }
    }

    #[test]
    fn dispatch_is_zero_copy_on_call_welcome_path() {
        // Same zero-copy contract for the welcome branch —
        // the returned slice points into the input bytes
        // (just past the 22-byte separator). Crate-wide
        // `#![forbid(unsafe_code)]` rules out pointer
        // arithmetic, so we compare the safe-Rust borrow
        // form: the dispatcher's returned slice must equal
        // `&wrapped[prefix_len..]` byte-for-byte.
        let welcome = vec![0x77u8; 256];
        let wrapped = wrap_call_welcome(&welcome);
        let result = dispatch_incoming_envelope(&wrapped).unwrap();
        if let IncomingEnvelope::CallWelcome(slice) = result {
            let expected: &[u8] = &wrapped[CALL_WELCOME_ENVELOPE_DOMAIN_V1.len()..];
            assert_eq!(slice.len(), welcome.len());
            // Pointer equality through the safe slice path —
            // `as_ptr()` on two slices that point to the same
            // backing storage at the same offset yields the
            // same address.
            assert_eq!(slice.as_ptr(), expected.as_ptr());
        } else {
            panic!("expected CallWelcome, got {result:?}");
        }
    }
}
