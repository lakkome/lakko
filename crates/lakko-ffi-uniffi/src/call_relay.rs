// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the sealed call relay and the peer announcement
//! ([ADR-0078] section 2, `lakko_calls::relay`). Mirror of
//! `crates/lakko-ffi-wasm/src/call_relay.rs`.
//!
//! - [`sfu_room_for_call`] - the room a call lives in on the SFU, the value
//!   a client mints its admission token for and probes with.
//! - [`CallRelayKey`] - seals and opens every `signal-relay` payload of an
//!   SFU call under the key of the current call epoch.
//! - [`classify_call_relay_plaintext`] - what an opened payload is: a peer
//!   announcement or the camera on/off media state.
//! - [`compose_call_peer_announcement`] - this device's SFU peer id, signed
//!   with its MLS leaf key.
//! - [`verify_call_peer_announcement`] - what a receiver runs before it
//!   names a tile: the announcement must verify against a leaf of the
//!   call's group and claim the peer id the SFU stamped as the sender.
//! - [`compose_call_screen_share`], [`verify_call_screen_share`] and
//!   [`CallScreenShareState`] - the signed screen-share state and the
//!   one-share-per-call fold of ADR-0083 (`lakko_calls::screen_share`).
//! - [`compose_call_name`], [`verify_call_name`], [`call_name_is_newer`] -
//!   the name a participant gives themself for one call
//!   (`lakko_calls::call_name`, `docs/ux/calls.md` section 4.1).
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lakko_calls::call_name as names;
use lakko_calls::relay::{self as core, RelayError, RelayPayload, CALL_ID_LEN};
use lakko_calls::screen_share::{self as share, ShareId, ShareStatus};

use crate::{FfiError, Group, Identity};

fn relay_err(e: RelayError) -> FfiError {
    FfiError::CallRelay(format!("{}: {e}", e.code()))
}

fn call_id16(call_id: &[u8]) -> Result<[u8; CALL_ID_LEN], FfiError> {
    <[u8; CALL_ID_LEN]>::try_from(call_id).map_err(|_| {
        FfiError::CallRelay(format!(
            "callIdLength: a call id is 16 bytes, got {}",
            call_id.len()
        ))
    })
}

/// The SFU room of a call (ADR-0078 section 2): 64 lower-case hex
/// characters, `BLAKE3_derive_key("lakko/sfu/room/v1", call_id)`. The
/// admission token is minted for it and `GET /v1/rooms/{room}` probes it.
///
/// # Errors
///
/// [`FfiError::CallRelay`] `callIdLength` when `call_id` is not 16 bytes.
#[uniffi::export]
pub fn sfu_room_for_call(call_id: Vec<u8>) -> Result<String, FfiError> {
    Ok(lakko_sfu_proto::room_hex(&call_id16(&call_id)?))
}

/// The key the relay channel of one call epoch is sealed under.
#[derive(uniffi::Object)]
pub struct CallRelayKey {
    inner: core::RelayKey,
}

#[uniffi::export]
impl CallRelayKey {
    /// Derive the key from the 32-byte per-call seed of the current epoch
    /// (`group.exportSecret(identity, "lakko/call/v1", callId, 32)`).
    ///
    /// # Errors
    ///
    /// [`FfiError::CallRelay`] `seedLength` when the seed is not 32 bytes.
    #[uniffi::constructor]
    pub fn from_call_seed(seed: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        let seed = <[u8; 32]>::try_from(seed.as_slice()).map_err(|_| {
            FfiError::CallRelay(format!(
                "seedLength: a call seed is 32 bytes, got {}",
                seed.len()
            ))
        })?;
        Ok(Arc::new(Self {
            inner: core::RelayKey::from_call_seed(&seed),
        }))
    }

    /// Seal a relay payload.
    ///
    /// # Errors
    ///
    /// [`FfiError::CallRelay`] `badLength` for a plaintext over the bound.
    pub fn seal(&self, plaintext: Vec<u8>) -> Result<Vec<u8>, FfiError> {
        self.inner.seal(&plaintext).map_err(relay_err)
    }

    /// Open a relay payload.
    ///
    /// # Errors
    ///
    /// [`FfiError::CallRelay`] `badLength`, `unknownVersion` or `notOpened`.
    pub fn open(&self, sealed: Vec<u8>) -> Result<Vec<u8>, FfiError> {
        self.inner.open(&sealed).map_err(relay_err)
    }
}

/// What an opened relay plaintext is.
#[derive(uniffi::Enum, Debug, Clone, PartialEq, Eq)]
pub enum CallRelayPayload {
    /// A peer announcement; verify it with
    /// [`verify_call_peer_announcement`].
    PeerAnnouncement {
        /// The whole announcement.
        announcement: Vec<u8>,
    },
    /// The sender's camera on/off state.
    MediaState {
        /// Whether the camera is on.
        camera_on: bool,
    },
    /// Anything else: drop it.
    Unknown,
    /// A screen-share state (ADR-0083); verify it with
    /// [`verify_call_screen_share`].
    ScreenShare {
        /// The whole share state.
        state: Vec<u8>,
    },
    /// A participant's name for this call; verify it with
    /// [`verify_call_name`]. Appended last: uniffi lowers a variant by its
    /// ordinal.
    CallName {
        /// The whole call name.
        name: Vec<u8>,
    },
}

/// Classify an opened relay plaintext.
#[uniffi::export]
#[must_use]
pub fn classify_call_relay_plaintext(plaintext: Vec<u8>) -> CallRelayPayload {
    match core::classify_relay_plaintext(&plaintext) {
        RelayPayload::PeerAnnouncement(announcement) => {
            CallRelayPayload::PeerAnnouncement { announcement }
        },
        RelayPayload::MediaState { camera_on } => CallRelayPayload::MediaState { camera_on },
        RelayPayload::ScreenShare(state) => CallRelayPayload::ScreenShare { state },
        RelayPayload::CallName(name) => CallRelayPayload::CallName { name },
        RelayPayload::Unknown => CallRelayPayload::Unknown,
    }
}

/// Compose this device's peer announcement: `peer_id` (the SFU's Welcome
/// `peer_id`) in call `call_id`, signed with `identity`'s leaf key. Seal it
/// with [`CallRelayKey::seal`] before relaying it.
///
/// # Errors
///
/// [`FfiError::CallRelay`] `callIdLength`, `malformed` (a bad peer id) or
/// `identityKeyMismatch`.
#[uniffi::export]
pub fn compose_call_peer_announcement(
    identity: Arc<Identity>,
    call_id: Vec<u8>,
    peer_id: String,
) -> Result<Vec<u8>, FfiError> {
    core::compose_peer_announcement(&identity.inner, &call_id16(&call_id)?, &peer_id)
        .map_err(relay_err)
}

/// A peer announcement that verified against the call's roster.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCallPeer {
    /// The SFU peer id.
    pub peer_id: String,
    /// The signer's leaf index at this epoch.
    pub leaf_index: u32,
    /// The signer's 32-byte leaf signature key.
    pub signature_key: Vec<u8>,
    /// The account key the leaf resolves to (ADR-0050): the person.
    pub account_key: Vec<u8>,
}

/// Verify a peer announcement against `group` (the call's MLS group: the
/// chat group of a group call, the call group of a link call).
/// `relay_from` is the `from` the SFU stamped on the relay frame.
///
/// # Errors
///
/// [`FfiError::CallRelay`] with `callIdLength`, a decode code, `wrongCall`,
/// `wrongPeer`, `notAMember`, `noAccount` or `badSignature`: drop the
/// announcement.
#[uniffi::export]
pub fn verify_call_peer_announcement(
    group: Arc<Group>,
    call_id: Vec<u8>,
    relay_from: String,
    announcement: Vec<u8>,
) -> Result<VerifiedCallPeer, FfiError> {
    let call_id = call_id16(&call_id)?;
    let g = group.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
    let verified = core::verify_peer_announcement(&g, &call_id, &relay_from, &announcement)
        .map_err(relay_err)?;
    Ok(VerifiedCallPeer {
        peer_id: verified.peer_id,
        leaf_index: verified.leaf_index,
        signature_key: verified.signature_key.to_vec(),
        account_key: verified.account_key,
    })
}

fn key32(bytes: &[u8]) -> Result<[u8; 32], FfiError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        FfiError::CallRelay(format!(
            "keyLength: a leaf key is 32 bytes, got {}",
            bytes.len()
        ))
    })
}

/// The peer id a one-to-one call's screen-share state carries (ADR-0083).
#[uniffi::export]
#[must_use]
pub fn call_screen_share_one_to_one_peer_id() -> String {
    share::ONE_TO_ONE_PEER_ID.to_owned()
}

/// SFrame stream index of a screen share's picture (ADR-0083).
#[uniffi::export]
#[must_use]
pub const fn sframe_stream_screen() -> u8 {
    share::SFRAME_STREAM_SCREEN
}

/// SFrame stream index of a screen share's sound (ADR-0083).
#[uniffi::export]
#[must_use]
pub const fn sframe_stream_screen_audio() -> u8 {
    share::SFRAME_STREAM_SCREEN_AUDIO
}

/// The stream-id suffix a share's tracks travel under (`~screen`).
#[uniffi::export]
#[must_use]
pub fn screen_stream_suffix() -> String {
    share::SCREEN_STREAM_SUFFIX.to_owned()
}

/// Compose this device's screen-share state (ADR-0083): share `seq` in call
/// `call_id` under `peer_id` (the SFU peer id, or
/// [`call_screen_share_one_to_one_peer_id`]), sharing or stopped, signed
/// with `identity`'s leaf key. In a group call seal it with
/// [`CallRelayKey::seal`]; in a one-to-one call send it as the
/// `ScreenShare` call-signalling body.
///
/// # Errors
///
/// [`FfiError::CallRelay`] `callIdLength`, `malformed` (a bad peer id or a
/// seq of 0) or `identityKeyMismatch`.
#[uniffi::export]
pub fn compose_call_screen_share(
    identity: Arc<Identity>,
    call_id: Vec<u8>,
    peer_id: String,
    seq: u64,
    sharing: bool,
    with_sound: bool,
) -> Result<Vec<u8>, FfiError> {
    let status = if sharing {
        ShareStatus::Sharing
    } else {
        ShareStatus::Stopped
    };
    share::compose_screen_share(
        &identity.inner,
        &call_id16(&call_id)?,
        &peer_id,
        seq,
        status,
        with_sound,
    )
    .map_err(relay_err)
}

/// A screen-share state that verified against the call's roster.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCallScreenShare {
    /// The share's sequence number.
    pub seq: u64,
    /// The sharer's 32-byte leaf signature key.
    pub signer_key: Vec<u8>,
    /// The sharer's peer id.
    pub peer_id: String,
    /// `true` while sharing, `false` for a stop.
    pub sharing: bool,
    /// Whether the share carries sound.
    pub with_sound: bool,
    /// The account key the signer's leaf resolves to: the person.
    pub account_key: Vec<u8>,
}

/// Verify a screen-share state. `peer_id` and `peer_signer_key` are what
/// the caller has bound to the sender: in a group call the relay `from` and
/// the signature key of that peer's verified announcement; in a one-to-one
/// call [`call_screen_share_one_to_one_peer_id`] and the MLS sender's leaf
/// key.
///
/// # Errors
///
/// [`FfiError::CallRelay`] with `callIdLength`, a decode code, `wrongCall`,
/// `wrongPeer`, `notAMember`, `noAccount` or `badSignature`: drop it.
#[uniffi::export]
pub fn verify_call_screen_share(
    group: Arc<Group>,
    call_id: Vec<u8>,
    peer_id: String,
    peer_signer_key: Vec<u8>,
    state: Vec<u8>,
) -> Result<VerifiedCallScreenShare, FfiError> {
    let call_id = call_id16(&call_id)?;
    let g = group.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
    let v = share::verify_screen_share(&g, &call_id, &peer_id, &peer_signer_key, &state)
        .map_err(relay_err)?;
    Ok(VerifiedCallScreenShare {
        seq: v.id.seq,
        signer_key: v.id.signer_key.to_vec(),
        peer_id: v.peer_id,
        sharing: v.status == ShareStatus::Sharing,
        with_sound: v.with_sound,
        account_key: v.account_key,
    })
}

/// Compose this device's name for the call: `name` (empty to go back to
/// the default naming) numbered `seq` (1, 2, 3, ... for the life of
/// `peer_id`) for call `call_id` under SFU `peer_id`, signed with
/// `identity`'s leaf key. `name` must already be normalised
/// (`normalizeDisplayName`): an untrimmed or otherwise inadmissible name is
/// refused. Seal it with [`CallRelayKey::seal`].
///
/// # Errors
///
/// [`FfiError::CallRelay`] `callIdLength`, `malformed` (a bad peer id, a
/// seq of 0, or a bad name) or `identityKeyMismatch`.
#[uniffi::export]
pub fn compose_call_name(
    identity: Arc<Identity>,
    call_id: Vec<u8>,
    peer_id: String,
    seq: u64,
    name: String,
) -> Result<Vec<u8>, FfiError> {
    names::compose_call_name(&identity.inner, &call_id16(&call_id)?, &peer_id, seq, &name)
        .map_err(relay_err)
}

/// A call name that verified against the call's roster.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCallName {
    /// The sender's peer id.
    pub peer_id: String,
    /// The name's sequence number; keep the greatest per peer
    /// ([`call_name_is_newer`]).
    pub seq: u64,
    /// The name, `None` when the sender went back to its default naming.
    pub name: Option<String>,
    /// The account key the signer's leaf resolves to: the person.
    pub account_key: Vec<u8>,
}

/// Verify a call name. `peer_id` is the relay `from`, `peer_signer_key` the
/// signature key of that peer's verified announcement.
///
/// # Errors
///
/// [`FfiError::CallRelay`] with `callIdLength`, a decode code, `wrongCall`,
/// `wrongPeer`, `notAMember`, `noAccount` or `badSignature`: drop it.
#[uniffi::export]
pub fn verify_call_name(
    group: Arc<Group>,
    call_id: Vec<u8>,
    peer_id: String,
    peer_signer_key: Vec<u8>,
    name: Vec<u8>,
) -> Result<VerifiedCallName, FfiError> {
    let call_id = call_id16(&call_id)?;
    let g = group.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
    let v = names::verify_call_name(&g, &call_id, &peer_id, &peer_signer_key, &name)
        .map_err(relay_err)?;
    Ok(VerifiedCallName {
        peer_id: v.peer_id,
        seq: v.seq,
        name: v.name,
        account_key: v.account_key,
    })
}

/// Whether a call name numbered `incoming_seq` replaces the one held for the
/// same peer (`stored_seq`, `None` when none is held): strictly greater.
#[uniffi::export]
#[must_use]
pub const fn call_name_is_newer(incoming_seq: u64, stored_seq: Option<u64>) -> bool {
    names::call_name_is_newer(incoming_seq, stored_seq)
}

/// The share a call is showing now.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct CallCurrentScreenShare {
    /// The share's sequence number.
    pub seq: u64,
    /// The sharer's leaf signature key.
    pub signer_key: Vec<u8>,
    /// The sharer's peer id.
    pub peer_id: String,
    /// Whether it carries sound.
    pub with_sound: bool,
    /// The sharer's account key (empty for this device's own share).
    pub account_key: Vec<u8>,
    /// Whether it is this device's own share.
    pub is_own: bool,
}

/// The one-share-per-call fold of ADR-0083 (C-87), one per call.
#[derive(uniffi::Object)]
pub struct CallScreenShareState {
    inner: Mutex<share::ScreenShareState>,
}

impl CallScreenShareState {
    /// The state is plain values and no method can panic while holding the
    /// lock, so a poisoned lock still guards a consistent state.
    fn state(&self) -> MutexGuard<'_, share::ScreenShareState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[uniffi::export]
impl CallScreenShareState {
    /// A fold for a device whose leaf signature key is `own_key`.
    ///
    /// # Errors
    ///
    /// [`FfiError::CallRelay`] `keyLength` when the key is not 32 bytes.
    #[uniffi::constructor]
    pub fn new(own_key: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        Ok(Arc::new(Self {
            inner: Mutex::new(share::ScreenShareState::new(key32(&own_key)?)),
        }))
    }

    /// The seq a share this device starts now must carry.
    pub fn next_start_seq(&self) -> u64 {
        self.state().next_start_seq()
    }

    /// Fold in a verified state from another participant. Returns whether
    /// the current share changed.
    ///
    /// # Errors
    ///
    /// [`FfiError::CallRelay`] `keyLength` for a signer key not 32 bytes.
    pub fn observe(&self, share_state: VerifiedCallScreenShare) -> Result<bool, FfiError> {
        let verified = share::VerifiedShare {
            id: ShareId {
                seq: share_state.seq,
                signer_key: key32(&share_state.signer_key)?,
            },
            peer_id: share_state.peer_id,
            status: if share_state.sharing {
                ShareStatus::Sharing
            } else {
                ShareStatus::Stopped
            },
            with_sound: share_state.with_sound,
            account_key: share_state.account_key,
        };
        Ok(self.state().observe(&verified) == share::ShareChange::Changed)
    }

    /// Record that this device started share `seq`. Returns whether the
    /// current share changed.
    pub fn start_own(&self, seq: u64, peer_id: String, with_sound: bool) -> bool {
        self.state().start_own(seq, &peer_id, with_sound) == share::ShareChange::Changed
    }

    /// Record that this device stopped its share: the seq its stopped state
    /// must carry, or `None` when it had none.
    pub fn stop_own(&self) -> Option<u64> {
        self.state().stop_own()
    }

    /// The seq of this device's share when another share superseded it:
    /// stop the capture, then [`Self::stop_own`].
    pub fn own_superseded(&self) -> Option<u64> {
        self.state().own_superseded()
    }

    /// The participant whose leaf key is `signer_key` left the call.
    /// Returns whether the current share changed.
    pub fn participant_left(&self, signer_key: Vec<u8>) -> bool {
        self.state().participant_left(&signer_key) == share::ShareChange::Changed
    }

    /// The share to show, or `None`.
    pub fn current(&self) -> Option<CallCurrentScreenShare> {
        let st = self.state();
        let is_own = st.current_is_own();
        st.current().map(|c| CallCurrentScreenShare {
            seq: c.id.seq,
            signer_key: c.id.signer_key.to_vec(),
            peer_id: c.peer_id,
            with_sound: c.with_sound,
            account_key: c.account_key,
            is_own,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_is_the_core_derivation_and_refuses_a_short_id() {
        let room = sfu_room_for_call(vec![0x01; 16]).expect("room");
        assert_eq!(room, lakko_sfu_proto::room_hex(&[0x01; 16]));
        assert!(sfu_room_for_call(vec![0x01; 15]).is_err());
    }

    #[test]
    fn a_relay_payload_round_trips_and_classifies() {
        let key = CallRelayKey::from_call_seed(vec![7; 32]).expect("key");
        let state = lakko_contacts::media_state::compose_media_state(false);
        let sealed = key.seal(state).expect("seal");
        let opened = key.open(sealed).expect("open");
        assert_eq!(
            classify_call_relay_plaintext(opened),
            CallRelayPayload::MediaState { camera_on: false }
        );
        assert!(CallRelayKey::from_call_seed(vec![7; 31]).is_err());
    }

    #[test]
    fn a_call_name_seals_classifies_and_verifies() {
        use lakko_mls::GroupKind;
        let alice = lakko_mls::Identity::from_seed(&[1; 32]).expect("alice");
        let bob = lakko_mls::Identity::from_seed(&[2; 32]).expect("bob");
        let mut g = lakko_mls::Group::create(&alice, GroupKind::Group).expect("group");
        g.add_member(&alice, bob.create_key_package().expect("kp"))
            .expect("add");
        let group = Arc::new(Group {
            inner: Mutex::new(g),
        });
        let bob_key = bob.signature_public_key().to_vec();
        let bob = Arc::new(Identity { inner: bob });
        let wire = compose_call_name(bob.clone(), vec![9; 16], "bob".into(), 1, "Bobby".into())
            .expect("compose");
        let key = CallRelayKey::from_call_seed(vec![7; 32]).expect("key");
        let opened = key
            .open(key.seal(wire.clone()).expect("seal"))
            .expect("open");
        assert_eq!(
            classify_call_relay_plaintext(opened),
            CallRelayPayload::CallName { name: wire.clone() }
        );
        let v = verify_call_name(
            group.clone(),
            vec![9; 16],
            "bob".into(),
            bob_key.clone(),
            wire,
        )
        .expect("verify");
        assert_eq!(v.name.as_deref(), Some("Bobby"));
        assert_eq!(v.seq, 1);
        let cleared = compose_call_name(bob.clone(), vec![9; 16], "bob".into(), 2, String::new())
            .expect("compose empty");
        let v2 = verify_call_name(group, vec![9; 16], "bob".into(), bob_key, cleared)
            .expect("verify empty");
        assert_eq!(v2.name, None);
        assert!(call_name_is_newer(2, Some(1)) && !call_name_is_newer(1, Some(1)));
        assert!(compose_call_name(bob, vec![9; 16], "bob".into(), 1, " x".into()).is_err());
    }

    #[test]
    fn a_screen_share_state_seals_classifies_verifies_and_folds() {
        use lakko_mls::GroupKind;
        let alice = lakko_mls::Identity::from_seed(&[1; 32]).expect("alice");
        let bob = lakko_mls::Identity::from_seed(&[2; 32]).expect("bob");
        let mut g = lakko_mls::Group::create(&alice, GroupKind::Group).expect("group");
        g.add_member(&alice, bob.create_key_package().expect("kp"))
            .expect("add");
        let group = Arc::new(Group {
            inner: Mutex::new(g),
        });
        let bob_key = bob.signature_public_key().to_vec();
        let bob = Arc::new(Identity { inner: bob });

        let fold = CallScreenShareState::new(alice.signature_public_key().to_vec()).expect("fold");
        let seq = fold.next_start_seq();
        let state = compose_call_screen_share(bob, vec![9; 16], "bob".into(), seq, true, true)
            .expect("compose");
        let key = CallRelayKey::from_call_seed(vec![7; 32]).expect("key");
        let opened = key
            .open(key.seal(state.clone()).expect("seal"))
            .expect("open");
        assert_eq!(
            classify_call_relay_plaintext(opened),
            CallRelayPayload::ScreenShare {
                state: state.clone()
            }
        );
        let verified = verify_call_screen_share(
            group.clone(),
            vec![9; 16],
            "bob".into(),
            bob_key.clone(),
            state.clone(),
        )
        .expect("verify");
        assert!(fold.observe(verified).expect("observe"));
        let current = fold.current().expect("current");
        assert_eq!(current.peer_id, "bob");
        assert!(current.with_sound && !current.is_own);
        assert!(fold.participant_left(bob_key.clone()));
        assert!(fold.current().is_none());
        assert!(
            verify_call_screen_share(group, vec![9; 16], "eve".into(), bob_key, state).is_err()
        );
    }
}
