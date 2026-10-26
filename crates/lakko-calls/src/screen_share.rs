// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Screen sharing: the signed share state and the one-share-per-call rule
//! (ADR-0083).
//!
//! # Why
//!
//! A call shows at most one screen share. A new share stops the one before
//! it, and two people pressing Share within the relay delay must leave
//! exactly one share on every device. Each device hears the starts in its
//! own order, so "the start I heard last wins" leaves two shares alive, or
//! none, depending on who looks. The rule here depends only on the SET of
//! states a device received, never on their order (C-87).
//!
//! # The share state (wire)
//!
//! ```text
//! "lakko/call/screen-share/v1\0" (27) || version(1) = 0x01 ||
//! call_id(16) || peer_id_len(1) || peer_id(1..=64, visible ASCII) ||
//! seq(8, BE u64) || status(1: 0x01 sharing, 0x02 stopped) ||
//! flags(1: bit 0 = carries sound, other bits zero) ||
//! signer_key(32) || signature(64)
//! ```
//!
//! The signature is Ed25519 by `signer_key` (the sharer's MLS leaf key)
//! over `"lakko/call/screen-share/sig/v1\0" || call_id || peer_id_len ||
//! peer_id || seq || status || flags`. In a group call the state rides the
//! sealed SFU relay ([`crate::relay`]) and `peer_id` is the sharer's SFU
//! peer id; in a one-to-one call it rides the call signalling
//! ([`crate::signaling::SignalingBody::ScreenShare`]) and `peer_id` is
//! [`ONE_TO_ONE_PEER_ID`].
//!
//! It is signed, unlike the camera flag, because it makes another device
//! stop its capture: the SFU stamps the relay sender and could otherwise
//! re-attribute one participant's sealed state to another.
//!
//! # The order
//!
//! A share is named by its [`ShareId`] `(seq, signer_key)`, ordered by
//! `seq` then by the key bytewise. A start takes
//! [`ScreenShareState::next_start_seq`] (one above the highest `seq` seen);
//! a stop carries the `seq` of the share it stops. [`ScreenShareState`]
//! keeps the greatest id it has seen and whether that one was stopped; a
//! state with a lower id is ignored, so a superseded share is over for
//! good. That is a pure function of the set of states received, so every
//! device that received the same states names the same current share.

use std::cmp::Ordering;

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use lakko_mls::{Group, Identity};

use crate::relay::{RelayError, CALL_ID_LEN, MAX_PEER_ID_LEN};

/// Domain prefix of a share state.
pub const SCREEN_SHARE_DOMAIN: &[u8] = b"lakko/call/screen-share/v1\x00";

/// Version byte of a share state.
pub const SCREEN_SHARE_VERSION: u8 = 0x01;

/// Domain of the signed bytes; distinct from the wire domain and from the
/// peer announcement's, so no signature crosses between them.
const SCREEN_SHARE_SIG_DOMAIN: &[u8] = b"lakko/call/screen-share/sig/v1\x00";

/// The peer id a one-to-one call's share state carries: there is no SFU
/// peer id on a peer-to-peer call.
pub const ONE_TO_ONE_PEER_ID: &str = "p2p";

/// SFrame stream index of a share's picture (ADR-0035, reserved since then).
pub const SFRAME_STREAM_SCREEN: u8 = 2;

/// SFrame stream index of a share's sound (ADR-0083).
pub const SFRAME_STREAM_SCREEN_AUDIO: u8 = 5;

/// Suffix of the stream id a share's tracks travel under: `<peer>~screen`
/// at the SFU, `<anything>~screen` on a one-to-one connection.
pub const SCREEN_STREAM_SUFFIX: &str = "~screen";

const FLAG_SOUND: u8 = 0x01;

/// Length of a share state carrying a `peer_id_len`-byte peer id.
#[must_use]
pub const fn screen_share_len(peer_id_len: usize) -> usize {
    SCREEN_SHARE_DOMAIN.len()
        + 1
        + CALL_ID_LEN
        + 1
        + peer_id_len
        + 8
        + 1
        + 1
        + PUBLIC_KEY_LEN
        + SIGNATURE_LEN
}

/// Whether a share is running or over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShareStatus {
    /// The share is running.
    Sharing,
    /// The share is over.
    Stopped,
}

impl ShareStatus {
    const fn to_byte(self) -> u8 {
        match self {
            Self::Sharing => 0x01,
            Self::Stopped => 0x02,
        }
    }

    const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(Self::Sharing),
            0x02 => Some(Self::Stopped),
            _ => None,
        }
    }
}

/// The name of one share: its sequence number and its signer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShareId {
    /// The Lamport sequence number the share started with.
    pub seq: u64,
    /// The sharer's MLS leaf signature key.
    pub signer_key: [u8; PUBLIC_KEY_LEN],
}

impl Ord for ShareId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.seq
            .cmp(&other.seq)
            .then_with(|| self.signer_key.cmp(&other.signer_key))
    }
}

impl PartialOrd for ShareId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A decoded (not yet verified) share state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareAnnouncement {
    /// The call it names.
    pub call_id: [u8; CALL_ID_LEN],
    /// The sharer's SFU peer id, or [`ONE_TO_ONE_PEER_ID`].
    pub peer_id: String,
    /// The share's sequence number.
    pub seq: u64,
    /// Running or over.
    pub status: ShareStatus,
    /// Whether the share carries sound.
    pub with_sound: bool,
    /// The leaf key that signed it.
    pub signer_key: [u8; PUBLIC_KEY_LEN],
    /// The signature.
    pub signature: [u8; SIGNATURE_LEN],
}

impl ShareAnnouncement {
    /// The share this state names.
    #[must_use]
    pub const fn id(&self) -> ShareId {
        ShareId {
            seq: self.seq,
            signer_key: self.signer_key,
        }
    }
}

/// A share state that verified against the call's roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedShare {
    /// The share it names.
    pub id: ShareId,
    /// The sharer's peer id.
    pub peer_id: String,
    /// Running or over.
    pub status: ShareStatus,
    /// Whether the share carries sound.
    pub with_sound: bool,
    /// The account key the signer's leaf resolves to (ADR-0050): the person.
    pub account_key: Vec<u8>,
}

fn check_peer_id(peer_id: &[u8]) -> Result<(), RelayError> {
    if peer_id.is_empty() || peer_id.len() > MAX_PEER_ID_LEN {
        return Err(RelayError::Malformed("peer id length"));
    }
    if !peer_id.iter().all(|b| (0x21..=0x7e).contains(b)) {
        return Err(RelayError::Malformed("peer id characters"));
    }
    Ok(())
}

fn signed_bytes(
    call_id: &[u8; CALL_ID_LEN],
    peer_id: &[u8],
    seq: u64,
    status: ShareStatus,
    flags: u8,
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(SCREEN_SHARE_SIG_DOMAIN.len() + CALL_ID_LEN + 1 + peer_id.len() + 8 + 2);
    out.extend_from_slice(SCREEN_SHARE_SIG_DOMAIN);
    out.extend_from_slice(call_id);
    // `peer_id.len() <= MAX_PEER_ID_LEN` is checked by every caller.
    out.push(u8::try_from(peer_id.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(peer_id);
    out.extend_from_slice(&seq.to_be_bytes());
    out.push(status.to_byte());
    out.push(flags);
    out
}

/// Compose this device's share state: share `seq` in call `call_id` under
/// `peer_id`, signed with `identity`'s leaf signature key.
///
/// # Errors
///
/// [`RelayError::Malformed`] for a bad peer id or a `seq` of 0 (no share is
/// numbered 0); [`RelayError::IdentityKeyMismatch`] when the identity's
/// signing seed does not produce its published signature key.
pub fn compose_screen_share(
    identity: &Identity,
    call_id: &[u8; CALL_ID_LEN],
    peer_id: &str,
    seq: u64,
    status: ShareStatus,
    with_sound: bool,
) -> Result<Vec<u8>, RelayError> {
    let peer = peer_id.as_bytes();
    check_peer_id(peer)?;
    if seq == 0 {
        return Err(RelayError::Malformed("seq"));
    }
    let signing = SigningKey::from_seed(&identity.ed25519_seed_bytes());
    let signer_key = signing.verifying_key().to_bytes();
    if identity.signature_public_key() != signer_key.as_slice() {
        return Err(RelayError::IdentityKeyMismatch);
    }
    let flags = if with_sound { FLAG_SOUND } else { 0 };
    let signature = signing
        .sign(&signed_bytes(call_id, peer, seq, status, flags))
        .to_bytes();
    let mut out = Vec::with_capacity(screen_share_len(peer.len()));
    out.extend_from_slice(SCREEN_SHARE_DOMAIN);
    out.push(SCREEN_SHARE_VERSION);
    out.extend_from_slice(call_id);
    out.push(u8::try_from(peer.len()).map_err(|_| RelayError::Malformed("peer id length"))?);
    out.extend_from_slice(peer);
    out.extend_from_slice(&seq.to_be_bytes());
    out.push(status.to_byte());
    out.push(flags);
    out.extend_from_slice(&signer_key);
    out.extend_from_slice(&signature);
    Ok(out)
}

/// Decode a share state without verifying it. Bounded and exact: every
/// length is checked before a slice is taken.
///
/// # Errors
///
/// [`RelayError::WrongDomain`], [`RelayError::UnknownVersion`],
/// [`RelayError::BadLength`] or [`RelayError::Malformed`].
pub fn decode_screen_share(bytes: &[u8]) -> Result<ShareAnnouncement, RelayError> {
    let rest = bytes
        .strip_prefix(SCREEN_SHARE_DOMAIN)
        .ok_or(RelayError::WrongDomain)?;
    let (&version, rest) = rest.split_first().ok_or(RelayError::BadLength)?;
    if version != SCREEN_SHARE_VERSION {
        return Err(RelayError::UnknownVersion);
    }
    if rest.len() < CALL_ID_LEN + 1 {
        return Err(RelayError::BadLength);
    }
    let (call_id_bytes, rest) = rest.split_at(CALL_ID_LEN);
    let (&peer_len, rest) = rest.split_first().ok_or(RelayError::BadLength)?;
    let peer_len = usize::from(peer_len);
    if rest.len() != peer_len + 8 + 1 + 1 + PUBLIC_KEY_LEN + SIGNATURE_LEN {
        return Err(RelayError::BadLength);
    }
    let (peer, rest) = rest.split_at(peer_len);
    check_peer_id(peer)?;
    let (seq_bytes, rest) = rest.split_at(8);
    let (&status_byte, rest) = rest.split_first().ok_or(RelayError::BadLength)?;
    let (&flags, rest) = rest.split_first().ok_or(RelayError::BadLength)?;
    let (key_bytes, sig_bytes) = rest.split_at(PUBLIC_KEY_LEN);

    let mut seq_be = [0u8; 8];
    seq_be.copy_from_slice(seq_bytes);
    let seq = u64::from_be_bytes(seq_be);
    if seq == 0 {
        return Err(RelayError::Malformed("seq"));
    }
    let status = ShareStatus::from_byte(status_byte).ok_or(RelayError::Malformed("status"))?;
    if flags & !FLAG_SOUND != 0 {
        return Err(RelayError::Malformed("flags"));
    }
    let mut call_id = [0u8; CALL_ID_LEN];
    call_id.copy_from_slice(call_id_bytes);
    let mut signer_key = [0u8; PUBLIC_KEY_LEN];
    signer_key.copy_from_slice(key_bytes);
    let mut signature = [0u8; SIGNATURE_LEN];
    signature.copy_from_slice(sig_bytes);
    // `check_peer_id` admitted visible ASCII only, so this is UTF-8.
    let peer_id = String::from_utf8(peer.to_vec()).map_err(|_| RelayError::Malformed("utf-8"))?;
    Ok(ShareAnnouncement {
        call_id,
        peer_id,
        seq,
        status,
        with_sound: flags & FLAG_SOUND != 0,
        signer_key,
        signature,
    })
}

/// Verify a share state: it names `call_id`, it names `peer_id`, it was
/// signed by `peer_signer_key`, that key is a leaf of `group`, and the
/// signature verifies.
///
/// `peer_id` and `peer_signer_key` are what the caller has already bound to
/// the sender: in a group call the relay's stamped sender and the key that
/// sender's verified peer announcement named; in a one-to-one call
/// [`ONE_TO_ONE_PEER_ID`] and the MLS sender's leaf key.
///
/// # Errors
///
/// Any decode error, then [`RelayError::WrongCall`],
/// [`RelayError::WrongPeer`], [`RelayError::NotAMember`],
/// [`RelayError::NoAccount`] or [`RelayError::BadSignature`].
pub fn verify_screen_share(
    group: &Group,
    call_id: &[u8; CALL_ID_LEN],
    peer_id: &str,
    peer_signer_key: &[u8],
    bytes: &[u8],
) -> Result<VerifiedShare, RelayError> {
    let decoded = decode_screen_share(bytes)?;
    if &decoded.call_id != call_id {
        return Err(RelayError::WrongCall);
    }
    if decoded.peer_id != peer_id {
        return Err(RelayError::WrongPeer);
    }
    if decoded.signer_key.as_slice() != peer_signer_key {
        return Err(RelayError::WrongPeer);
    }
    let member = group
        .members()
        .into_iter()
        .find(|m| m.signature_public_key.as_slice() == decoded.signer_key.as_slice())
        .ok_or(RelayError::NotAMember)?;
    let verifying =
        VerifyingKey::from_bytes(&decoded.signer_key).map_err(|_| RelayError::BadSignature)?;
    let signature =
        Signature::from_bytes(&decoded.signature).map_err(|_| RelayError::BadSignature)?;
    let flags = if decoded.with_sound { FLAG_SOUND } else { 0 };
    verifying
        .verify(
            &signed_bytes(
                &decoded.call_id,
                decoded.peer_id.as_bytes(),
                decoded.seq,
                decoded.status,
                flags,
            ),
            &signature,
        )
        .map_err(|_| RelayError::BadSignature)?;
    if member.account_key.is_empty() {
        return Err(RelayError::NoAccount);
    }
    Ok(VerifiedShare {
        id: decoded.id(),
        peer_id: decoded.peer_id,
        status: decoded.status,
        with_sound: decoded.with_sound,
        account_key: member.account_key,
    })
}

/// The share a call is showing now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentShare {
    /// Its name.
    pub id: ShareId,
    /// The sharer's peer id.
    pub peer_id: String,
    /// Whether it carries sound.
    pub with_sound: bool,
    /// The sharer's account key (empty for this device's own share).
    pub account_key: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Top {
    id: ShareId,
    stopped: bool,
    /// What the `sharing` state said; `None` while only a stop was seen.
    shown: Option<(String, bool, Vec<u8>)>,
}

/// What [`ScreenShareState::observe`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareChange {
    /// The current share is not what it was.
    Changed,
    /// Nothing visible changed.
    Unchanged,
}

/// The one-share-per-call fold (ADR-0083 section 2, C-87).
///
/// Feed it every verified share state (and this device's own starts and
/// stops); read [`Self::current`] for what to show and
/// [`Self::own_superseded`] for whether this device must stop its capture.
/// The answer depends only on the set of states fed, never on their order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenShareState {
    own_key: [u8; PUBLIC_KEY_LEN],
    max_seq: u64,
    top: Option<Top>,
    own: Option<ShareId>,
}

impl ScreenShareState {
    /// A fold for a device whose leaf signature key is `own_key`.
    #[must_use]
    pub const fn new(own_key: [u8; PUBLIC_KEY_LEN]) -> Self {
        Self {
            own_key,
            max_seq: 0,
            top: None,
            own: None,
        }
    }

    /// The `seq` a share this device starts now must carry.
    #[must_use]
    pub const fn next_start_seq(&self) -> u64 {
        self.max_seq.saturating_add(1)
    }

    fn apply(
        &mut self,
        id: ShareId,
        status: ShareStatus,
        shown: Option<(String, bool, Vec<u8>)>,
    ) -> ShareChange {
        let before = self.current();
        self.max_seq = self.max_seq.max(id.seq);
        match &mut self.top {
            Some(top) if id < top.id => {},
            Some(top) if id == top.id => match status {
                ShareStatus::Stopped => top.stopped = true,
                ShareStatus::Sharing => {
                    if top.shown.is_none() {
                        top.shown = shown;
                    }
                },
            },
            _ => {
                self.top = Some(Top {
                    id,
                    stopped: status == ShareStatus::Stopped,
                    shown: if status == ShareStatus::Sharing {
                        shown
                    } else {
                        None
                    },
                });
            },
        }
        if self.current() == before {
            ShareChange::Unchanged
        } else {
            ShareChange::Changed
        }
    }

    /// Fold in a verified share state from another participant.
    pub fn observe(&mut self, share: &VerifiedShare) -> ShareChange {
        self.apply(
            share.id,
            share.status,
            Some((
                share.peer_id.clone(),
                share.with_sound,
                share.account_key.clone(),
            )),
        )
    }

    /// Record that this device started share `seq` (the value it took from
    /// [`Self::next_start_seq`] and composed into its state).
    pub fn start_own(&mut self, seq: u64, peer_id: &str, with_sound: bool) -> ShareChange {
        let id = ShareId {
            seq,
            signer_key: self.own_key,
        };
        self.own = Some(id);
        self.apply(
            id,
            ShareStatus::Sharing,
            Some((peer_id.to_owned(), with_sound, Vec::new())),
        )
    }

    /// Record that this device stopped its share. Returns the `seq` its
    /// `stopped` state must carry, or `None` when it had no share.
    pub fn stop_own(&mut self) -> Option<u64> {
        let id = self.own.take()?;
        self.apply(id, ShareStatus::Stopped, None);
        Some(id.seq)
    }

    /// The `seq` of this device's own share when another share has
    /// superseded it: the caller stops the capture and calls
    /// [`Self::stop_own`]. `None` while its share is current or it has none.
    #[must_use]
    pub fn own_superseded(&self) -> Option<u64> {
        let own = self.own?;
        match &self.top {
            Some(top) if top.id > own => Some(own.seq),
            _ => None,
        }
    }

    /// A participant whose leaf key is `signer_key` left the call: its share
    /// is over as if it had sent `stopped`.
    pub fn participant_left(&mut self, signer_key: &[u8]) -> ShareChange {
        let before = self.current();
        if let Some(top) = &mut self.top {
            if top.id.signer_key.as_slice() == signer_key {
                top.stopped = true;
            }
        }
        if self.current() == before {
            ShareChange::Unchanged
        } else {
            ShareChange::Changed
        }
    }

    /// The share to show, or `None` when no share is running.
    #[must_use]
    pub fn current(&self) -> Option<CurrentShare> {
        let top = self.top.as_ref()?;
        if top.stopped {
            return None;
        }
        let (peer_id, with_sound, account_key) = top.shown.clone()?;
        Some(CurrentShare {
            id: top.id,
            peer_id,
            with_sound,
            account_key,
        })
    }

    /// Whether the current share is this device's own.
    #[must_use]
    pub fn current_is_own(&self) -> bool {
        self.current()
            .is_some_and(|c| c.id.signer_key == self.own_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_mls::GroupKind;
    use proptest::prelude::*;

    const CALL: [u8; CALL_ID_LEN] = [0x11; CALL_ID_LEN];

    fn identity(seed: u8) -> Identity {
        Identity::from_seed(&[seed; 32]).expect("identity")
    }

    fn key(identity: &Identity) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(identity.signature_public_key());
        out
    }

    fn verified(signer: u8, seq: u64, status: ShareStatus) -> VerifiedShare {
        VerifiedShare {
            id: ShareId {
                seq,
                signer_key: [signer; 32],
            },
            peer_id: format!("peer{signer}"),
            status,
            with_sound: false,
            account_key: vec![signer; 32],
        }
    }

    #[test]
    fn the_domains_have_the_documented_lengths() {
        assert_eq!(SCREEN_SHARE_DOMAIN.len(), 27);
        assert_ne!(SCREEN_SHARE_DOMAIN, SCREEN_SHARE_SIG_DOMAIN);
        assert!(!SCREEN_SHARE_DOMAIN.starts_with(crate::relay::PEER_ANNOUNCEMENT_DOMAIN));
    }

    #[test]
    fn a_share_state_round_trips_through_decode() {
        let alice = identity(1);
        let wire =
            compose_screen_share(&alice, &CALL, "alice", 7, ShareStatus::Sharing, true).unwrap();
        assert_eq!(wire.len(), screen_share_len(5));
        let decoded = decode_screen_share(&wire).unwrap();
        assert_eq!(decoded.call_id, CALL);
        assert_eq!(decoded.peer_id, "alice");
        assert_eq!(decoded.seq, 7);
        assert_eq!(decoded.status, ShareStatus::Sharing);
        assert!(decoded.with_sound);
        assert_eq!(decoded.signer_key, key(&alice));
    }

    #[test]
    fn compose_refuses_a_bad_peer_id_and_seq_zero() {
        let alice = identity(1);
        let compose = |peer: &str, seq| {
            compose_screen_share(&alice, &CALL, peer, seq, ShareStatus::Sharing, false)
        };
        assert!(compose("", 1).is_err());
        assert!(compose(&"a".repeat(65), 1).is_err());
        assert!(compose("a b", 1).is_err());
        assert_eq!(compose("a", 0), Err(RelayError::Malformed("seq")));
    }

    #[test]
    fn every_truncation_extension_and_bad_field_is_refused() {
        let wire =
            compose_screen_share(&identity(1), &CALL, "p", 3, ShareStatus::Stopped, false).unwrap();
        for n in 0..wire.len() {
            assert!(decode_screen_share(&wire[..n]).is_err(), "prefix {n}");
        }
        let mut long = wire.clone();
        long.push(0);
        assert_eq!(decode_screen_share(&long), Err(RelayError::BadLength));

        let status_at = SCREEN_SHARE_DOMAIN.len() + 1 + CALL_ID_LEN + 1 + 1 + 8;
        let mut bad_status = wire.clone();
        bad_status[status_at] = 0x03;
        assert_eq!(
            decode_screen_share(&bad_status),
            Err(RelayError::Malformed("status"))
        );
        let mut bad_flags = wire.clone();
        bad_flags[status_at + 1] = 0x02;
        assert_eq!(
            decode_screen_share(&bad_flags),
            Err(RelayError::Malformed("flags"))
        );
        let mut zero_seq = wire.clone();
        zero_seq[status_at - 8..status_at].fill(0);
        assert_eq!(
            decode_screen_share(&zero_seq),
            Err(RelayError::Malformed("seq"))
        );
        let mut version = wire;
        version[SCREEN_SHARE_DOMAIN.len()] = 0x02;
        assert_eq!(
            decode_screen_share(&version),
            Err(RelayError::UnknownVersion)
        );
    }

    #[test]
    fn a_peer_announcement_is_not_a_share_state() {
        let ann = crate::relay::compose_peer_announcement(&identity(1), &CALL, "p").unwrap();
        assert_eq!(decode_screen_share(&ann), Err(RelayError::WrongDomain));
    }

    #[test]
    fn verify_binds_the_state_to_the_peer_and_the_roster() {
        let alice = identity(1);
        let bob = identity(2);
        let mut group = Group::create(&alice, GroupKind::Group).expect("group");
        let kp = bob.create_key_package().unwrap();
        group.add_member(&alice, kp).expect("add bob");
        let bob_key = key(&bob);

        let wire =
            compose_screen_share(&bob, &CALL, "bobpeer", 2, ShareStatus::Sharing, true).unwrap();
        let ok = verify_screen_share(&group, &CALL, "bobpeer", &bob_key, &wire).unwrap();
        assert_eq!(ok.id.seq, 2);
        assert_eq!(ok.id.signer_key, bob_key);
        assert!(ok.with_sound);
        assert_eq!(ok.account_key.as_slice(), bob.signature_public_key());

        assert_eq!(
            verify_screen_share(&group, &[0x22; CALL_ID_LEN], "bobpeer", &bob_key, &wire),
            Err(RelayError::WrongCall)
        );
        // The SFU stamped another sender on Bob's sealed state.
        assert_eq!(
            verify_screen_share(&group, &CALL, "alicepeer", &bob_key, &wire),
            Err(RelayError::WrongPeer)
        );
        // The peer id matches but that peer announced another key.
        assert_eq!(
            verify_screen_share(&group, &CALL, "bobpeer", &key(&alice), &wire),
            Err(RelayError::WrongPeer)
        );
        let carol = identity(3);
        let stranger =
            compose_screen_share(&carol, &CALL, "carol", 1, ShareStatus::Sharing, false).unwrap();
        assert_eq!(
            verify_screen_share(&group, &CALL, "carol", &key(&carol), &stranger),
            Err(RelayError::NotAMember)
        );
        // Any signed field flipped breaks the signature.
        let seq_at = SCREEN_SHARE_DOMAIN.len() + 1 + CALL_ID_LEN + 1 + 7;
        for at in [seq_at, seq_at + 7, seq_at + 8, wire.len() - 1] {
            let mut forged = wire.clone();
            forged[at] ^= 0x01;
            let result = verify_screen_share(&group, &CALL, "bobpeer", &bob_key, &forged);
            assert!(result.is_err(), "flip at {at} must be refused");
        }
    }

    #[test]
    fn a_later_start_supersedes_the_earlier_one() {
        let mut fold = ScreenShareState::new([1; 32]);
        assert_eq!(fold.next_start_seq(), 1);
        fold.start_own(1, "me", false);
        assert!(fold.current_is_own());
        // Bob heard our start and started after it.
        assert_eq!(
            fold.observe(&verified(2, 2, ShareStatus::Sharing)),
            ShareChange::Changed
        );
        assert_eq!(fold.own_superseded(), Some(1));
        assert_eq!(fold.stop_own(), Some(1));
        assert_eq!(fold.own_superseded(), None);
        assert_eq!(fold.current().unwrap().peer_id, "peer2");
        assert_eq!(fold.next_start_seq(), 3);
        // Bob stops: nothing is shown, and our superseded share stays over.
        fold.observe(&verified(2, 2, ShareStatus::Stopped));
        assert_eq!(fold.current(), None);
    }

    #[test]
    fn a_tie_is_broken_by_the_key_alike_on_both_sides() {
        let mut low = ScreenShareState::new([1; 32]);
        let mut high = ScreenShareState::new([2; 32]);
        low.start_own(1, "peer1", false);
        high.start_own(1, "peer2", false);
        low.observe(&verified(2, 1, ShareStatus::Sharing));
        high.observe(&verified(1, 1, ShareStatus::Sharing));
        assert_eq!(low.own_superseded(), Some(1));
        assert_eq!(high.own_superseded(), None);
        assert_eq!(low.current().unwrap().id, high.current().unwrap().id);
    }

    #[test]
    fn a_stop_that_arrives_first_wins_over_its_late_start() {
        let mut fold = ScreenShareState::new([9; 32]);
        fold.observe(&verified(2, 4, ShareStatus::Stopped));
        assert_eq!(
            fold.observe(&verified(2, 4, ShareStatus::Sharing)),
            ShareChange::Unchanged
        );
        assert_eq!(fold.current(), None);
        // A replayed older start does not bring anything back.
        fold.observe(&verified(3, 3, ShareStatus::Sharing));
        assert_eq!(fold.current(), None);
    }

    #[test]
    fn a_participant_leaving_ends_its_share() {
        let mut fold = ScreenShareState::new([9; 32]);
        fold.observe(&verified(2, 1, ShareStatus::Sharing));
        assert_eq!(fold.participant_left(&[3; 32]), ShareChange::Unchanged);
        assert_eq!(fold.participant_left(&[2; 32]), ShareChange::Changed);
        assert_eq!(fold.current(), None);
    }

    fn arb_state() -> impl Strategy<Value = VerifiedShare> {
        (1u8..5, 1u64..6, any::<bool>()).prop_map(|(signer, seq, stop)| {
            verified(
                signer,
                seq,
                if stop {
                    ShareStatus::Stopped
                } else {
                    ShareStatus::Sharing
                },
            )
        })
    }

    proptest! {
        /// C-87: two devices that received the same share states, in any
        /// order, name the same current share.
        #[test]
        fn the_current_share_does_not_depend_on_arrival_order(
            states in proptest::collection::vec(arb_state(), 0..24),
            perm in any::<proptest::sample::Index>(),
            seed in any::<u64>(),
        ) {
            let mut shuffled = states.clone();
            // A deterministic shuffle driven by the generated seed.
            let mut s = seed | 1;
            for i in (1..shuffled.len()).rev() {
                s ^= s << 13; s ^= s >> 7; s ^= s << 17;
                let j = usize::try_from(s % (i as u64 + 1)).unwrap();
                shuffled.swap(i, j);
            }
            if !shuffled.is_empty() {
                let k = perm.index(shuffled.len());
                shuffled.rotate_left(k);
            }
            let mut a = ScreenShareState::new([0xee; 32]);
            let mut b = ScreenShareState::new([0xee; 32]);
            for st in &states { a.observe(st); }
            for st in &shuffled { b.observe(st); }
            prop_assert_eq!(a.current().map(|c| c.id), b.current().map(|c| c.id));
            prop_assert_eq!(a.next_start_seq(), b.next_start_seq());
        }

        /// Whatever arrived, a start this device makes now becomes current.
        #[test]
        fn a_new_start_always_becomes_current(
            states in proptest::collection::vec(arb_state(), 0..24),
        ) {
            let mut fold = ScreenShareState::new([0; 32]);
            for st in &states { fold.observe(st); }
            let seq = fold.next_start_seq();
            fold.start_own(seq, "me", false);
            prop_assert!(fold.current_is_own());
            prop_assert_eq!(fold.own_superseded(), None);
        }
    }
}
