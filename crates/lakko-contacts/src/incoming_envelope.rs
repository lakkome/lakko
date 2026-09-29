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
//!    Then [`crate::profile_request`] (ADR-0048), [`crate::pin`]
//!    (ADR-0052), [`crate::registrar_seed`] (ADR-0057) and
//!    [`crate::shared_channels`] (ADR-0059): service envelopes the
//!    caller acts on and never renders.
//! 8. **Intro-message fallthrough** — plain free-text
//!    `ContactRequest` (ADR-0005 scenario A; "add me as a
//!    contact"). Caller renders the bytes as a UTF-8 intro
//!    message.
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

use crate::call_kp_forward::try_unwrap_call_kp_forward;
use crate::call_welcome::{try_unwrap_call_welcome, CallWelcomeError};
use crate::committer_beacon::{try_unwrap_committer_beacon, CommitterBeaconError};
use crate::group_name::{try_unwrap_group_name, GroupName, GroupNameError};
use crate::media_state::{try_unwrap_media_state, MediaStateError};
use crate::pin::{try_unwrap_pin, PinError, PinOp};
use crate::profile::{try_unwrap_profile, Profile, ProfileError};
use crate::profile_request::{try_unwrap_profile_request, ProfileRequestError};
use crate::push_routing::{try_unwrap_push_routing_secret, PushRoutingSecretError};
use crate::registrar_seed::{try_unwrap_registrar_seed, RegistrarSeedEnvelope, RegistrarSeedError};
use crate::shared_channels::{try_unwrap_shared_channels, SharedChannels, SharedChannelsError};

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
    /// A journal root hands one member its Delivery Service registrar seed
    /// (ADR-0057), sealed to that member's account X25519 key. Never a chat
    /// bubble. **The caller keeps it only when addressed to its own account
    /// key** ([`RegistrarSeedEnvelope::is_addressed_to`]) and the MLS sender
    /// resolves at receipt (C-68) to the group's owner; it opens the seed
    /// with its account X25519 secret and drops the envelope on a failed
    /// open. Every other member drops it unopened.
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
    /// The registrar-seed domain (ADR-0057) with a malformed payload.
    #[error(transparent)]
    RegistrarSeed(#[from] RegistrarSeedError),
    /// The shared-channels domain (ADR-0059) with a malformed payload.
    #[error(transparent)]
    SharedChannels(#[from] SharedChannelsError),
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
    // 8. Plain intro message — ADR-0005 scenario A.
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
        use crate::profile::{DisplayNameError, PROFILE_ENVELOPE_DOMAIN_V1, PROFILE_VERSION_V2};
        let name = "Alice\u{202E}".as_bytes();
        let mut wrapped = PROFILE_ENVELOPE_DOMAIN_V1.to_vec();
        wrapped.push(PROFILE_VERSION_V2);
        wrapped.extend_from_slice(&0u64.to_be_bytes());
        wrapped.push(u8::try_from(name.len()).unwrap());
        wrapped.extend_from_slice(name);
        wrapped.push(0);
        wrapped.push(0);
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
