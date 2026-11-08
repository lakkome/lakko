// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The sealed `signal-relay` channel of an SFU call and the peer
//! announcement it carries (ADR-0078 section 2).
//!
//! # Why
//!
//! The SFU names a participant by a random `peer_id` drawn into its
//! admission token and holds no account id, so it can no longer tell the
//! other participants who a peer is (the ADR-0021 peer tag over the account
//! id is gone). Each participant says it itself: it announces its own SFU
//! peer id to the call, signed with its MLS leaf signature key, and every
//! receiver checks the signature against the leaf in the call's MLS roster.
//! The roster leaf is account-bound (ADR-0050), so the binding
//! `peer id -> person` stays spoof-proof, and the SFU never sees it: the
//! announcement, and every other relay payload the clients agree on (today
//! the camera and microphone media state, the ADR-0083 screen-share state and the
//! name a participant gives themself for the call, [`crate::call_name`]), is
//! sealed under a key only members hold.
//!
//! # The relay key
//!
//! `relay_key = BLAKE3_derive_key("lakko/call/relay/v1", call_seed)`, where
//! `call_seed` is the per-call MLS exporter output every member derives
//! (`group.export_secret("lakko/call/v1", call_id, 32)`, audit-pack C-1). It
//! changes with the MLS epoch exactly as the SFrame key does (ADR-0029); a
//! receiver keeps the previous epoch's key for the same overlap.
//!
//! # Wire
//!
//! A sealed relay payload is `version(1) = 0x01 || nonce(12) ||
//! ChaCha20-Poly1305(relay_key, nonce, aad, plaintext)` with
//! `aad = "lakko/call/relay/v1\0" || version`. The nonce is random per
//! payload; a key carries a handful of payloads per epoch, far inside the
//! random-nonce bound.
//!
//! A peer announcement (the plaintext) is
//!
//! ```text
//! "lakko/call/peer-announce/v1\0" (28) || version(1) = 0x01 ||
//! call_id(16) || peer_id_len(1) || peer_id(1..=64, visible ASCII) ||
//! signer_key(32) || signature(64)
//! ```
//!
//! The signature is Ed25519 by the leaf key named in `signer_key`, over
//! `"lakko/call/peer-announce/sig/v1\0" || call_id || peer_id_len ||
//! peer_id`. A receiver accepts it only when the call id is its own call's,
//! the peer id is the one the SFU stamped as the relay's sender, the signer
//! key is a leaf of the call's group, and the signature verifies.

use lakko_crypto::aead::{self, AeadKey, Algorithm, NONCE_LEN, TAG_LEN};
use lakko_crypto::hash::blake3_derive_key_from_material;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use lakko_crypto::SecretBytes;
use lakko_mls::{Group, Identity};
use thiserror::Error;

/// Domain context of the relay key derivation.
pub const RELAY_KEY_CONTEXT: &str = "lakko/call/relay/v1";

/// Version byte of a sealed relay payload.
pub const RELAY_VERSION: u8 = 0x01;

/// AAD prefix of a sealed relay payload (followed by the version byte).
const RELAY_AAD_DOMAIN: &[u8] = b"lakko/call/relay/v1\x00";

/// Largest plaintext a relay payload may carry. An announcement is at most
/// 206 bytes, a media state 28 and a call name 461; the bound keeps a hostile payload from
/// costing more than a small allocation.
pub const MAX_RELAY_PLAINTEXT: usize = 1024;

/// Smallest sealed payload: version, nonce and a tag over an empty
/// plaintext.
pub const MIN_SEALED_RELAY_LEN: usize = 1 + NONCE_LEN + TAG_LEN;

/// Largest sealed payload.
pub const MAX_SEALED_RELAY_LEN: usize = MIN_SEALED_RELAY_LEN + MAX_RELAY_PLAINTEXT;

/// Domain prefix of a peer announcement plaintext. Frozen.
pub const PEER_ANNOUNCEMENT_DOMAIN: &[u8] = b"lakko/call/peer-announce/v1\x00";

/// Version byte of a peer announcement.
pub const PEER_ANNOUNCEMENT_VERSION: u8 = 0x01;

/// Domain of the bytes a peer announcement signs (distinct from the wire
/// domain, so the signature can never be lifted onto another message).
const PEER_ANNOUNCEMENT_SIG_DOMAIN: &[u8] = b"lakko/call/peer-announce/sig/v1\x00";

/// Length of a call id.
pub const CALL_ID_LEN: usize = 16;

/// Longest peer id an announcement carries.
pub const MAX_PEER_ID_LEN: usize = 64;

/// Length of an announcement with a peer id of `peer_id_len` bytes.
#[must_use]
pub const fn peer_announcement_len(peer_id_len: usize) -> usize {
    PEER_ANNOUNCEMENT_DOMAIN.len()
        + 1
        + CALL_ID_LEN
        + 1
        + peer_id_len
        + PUBLIC_KEY_LEN
        + SIGNATURE_LEN
}

/// Why a relay payload or an announcement was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RelayError {
    /// The sealed payload or the plaintext is shorter or longer than its
    /// layout allows.
    #[error("relay payload length out of bounds")]
    BadLength,
    /// The version byte is not the one this build speaks.
    #[error("unknown relay payload version")]
    UnknownVersion,
    /// The payload did not open under this key (wrong key, wrong epoch,
    /// tampered, or not a sealed relay payload at all).
    #[error("relay payload did not open")]
    NotOpened,
    /// The plaintext is not a peer announcement (wrong domain).
    #[error("not a peer announcement")]
    WrongDomain,
    /// A field of the announcement is malformed.
    #[error("malformed peer announcement: {0}")]
    Malformed(&'static str),
    /// The announcement names another call.
    #[error("peer announcement names another call")]
    WrongCall,
    /// The announced peer id is not the relay's sender.
    #[error("announced peer id is not the relay sender")]
    WrongPeer,
    /// The signer key is not a leaf of the call's group.
    #[error("announcement signer is not a member of the call")]
    NotAMember,
    /// The signer's leaf resolves to no account.
    #[error("announcement signer resolves to no account")]
    NoAccount,
    /// The signature does not verify.
    #[error("peer announcement signature does not verify")]
    BadSignature,
    /// The identity's signing seed does not produce its signature key.
    #[error("identity signing key mismatch")]
    IdentityKeyMismatch,
    /// AEAD sealing failed (unreachable for valid inputs).
    #[error("relay payload sealing failed")]
    Seal,
}

impl RelayError {
    /// A frozen machine-readable code, carried by both FFI surfaces as the
    /// `"<code>: <reason>"` prefix.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadLength => "badLength",
            Self::UnknownVersion => "unknownVersion",
            Self::NotOpened => "notOpened",
            Self::WrongDomain => "wrongDomain",
            Self::Malformed(_) => "malformed",
            Self::WrongCall => "wrongCall",
            Self::WrongPeer => "wrongPeer",
            Self::NotAMember => "notAMember",
            Self::NoAccount => "noAccount",
            Self::BadSignature => "badSignature",
            Self::IdentityKeyMismatch => "identityKeyMismatch",
            Self::Seal => "seal",
        }
    }
}

/// The key the relay channel of one call epoch is sealed under.
#[derive(Clone)]
pub struct RelayKey(SecretBytes<32>);

impl RelayKey {
    /// Derive the relay key from the per-call seed of the current epoch.
    #[must_use]
    pub fn from_call_seed(call_seed: &[u8; 32]) -> Self {
        Self(SecretBytes::new(blake3_derive_key_from_material(
            RELAY_KEY_CONTEXT,
            call_seed,
        )))
    }

    /// Seal `plaintext` for the relay.
    ///
    /// # Errors
    ///
    /// [`RelayError::BadLength`] when the plaintext exceeds
    /// [`MAX_RELAY_PLAINTEXT`]; [`RelayError::Seal`] on an AEAD failure.
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, RelayError> {
        if plaintext.len() > MAX_RELAY_PLAINTEXT {
            return Err(RelayError::BadLength);
        }
        let mut nonce = [0u8; NONCE_LEN];
        lakko_crypto::random::fill(&mut nonce);
        let key = AeadKey::new(*self.0.as_bytes());
        let ciphertext = aead::seal(
            Algorithm::ChaCha20Poly1305,
            &key,
            &nonce,
            &relay_aad(RELAY_VERSION),
            plaintext,
        )
        .map_err(|_| RelayError::Seal)?;
        let mut wire = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
        wire.push(RELAY_VERSION);
        wire.extend_from_slice(&nonce);
        wire.extend_from_slice(&ciphertext);
        Ok(wire)
    }

    /// Open a sealed relay payload.
    ///
    /// # Errors
    ///
    /// [`RelayError::BadLength`], [`RelayError::UnknownVersion`] or
    /// [`RelayError::NotOpened`]; never panics on any input.
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, RelayError> {
        if sealed.len() < MIN_SEALED_RELAY_LEN || sealed.len() > MAX_SEALED_RELAY_LEN {
            return Err(RelayError::BadLength);
        }
        let (&version, rest) = sealed.split_first().ok_or(RelayError::BadLength)?;
        if version != RELAY_VERSION {
            return Err(RelayError::UnknownVersion);
        }
        let (nonce_bytes, ciphertext) = rest.split_at(NONCE_LEN);
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(nonce_bytes);
        let key = AeadKey::new(*self.0.as_bytes());
        aead::open(
            Algorithm::ChaCha20Poly1305,
            &key,
            &nonce,
            &relay_aad(version),
            ciphertext,
        )
        .map_err(|_| RelayError::NotOpened)
    }
}

impl std::fmt::Debug for RelayKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RelayKey(..)")
    }
}

fn relay_aad(version: u8) -> Vec<u8> {
    let mut aad = Vec::with_capacity(RELAY_AAD_DOMAIN.len() + 1);
    aad.extend_from_slice(RELAY_AAD_DOMAIN);
    aad.push(version);
    aad
}

/// What an opened relay plaintext is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayPayload {
    /// A peer announcement (the whole plaintext; verify it with
    /// [`verify_peer_announcement`]).
    PeerAnnouncement(Vec<u8>),
    /// The camera and microphone media state
    /// (`lakko_contacts::media_state`).
    MediaState {
        /// Whether the sender's camera is on.
        camera_on: bool,
        /// Whether the sender's microphone is on (`false` = muted).
        microphone_on: bool,
    },
    /// A screen-share state (the whole plaintext; verify it with
    /// [`crate::screen_share::verify_screen_share`], ADR-0083).
    ScreenShare(Vec<u8>),
    /// A participant's name for this call (the whole plaintext; verify it
    /// with [`crate::call_name::verify_call_name`]).
    CallName(Vec<u8>),
    /// Anything else: dropped by the caller.
    Unknown,
}

/// Classify an opened relay plaintext by its domain prefix. A plaintext
/// that carries a known domain but is malformed is [`RelayPayload::Unknown`]
/// (dropped, never guessed at).
#[must_use]
pub fn classify_relay_plaintext(plaintext: &[u8]) -> RelayPayload {
    if plaintext.starts_with(PEER_ANNOUNCEMENT_DOMAIN) {
        return RelayPayload::PeerAnnouncement(plaintext.to_vec());
    }
    if plaintext.starts_with(crate::screen_share::SCREEN_SHARE_DOMAIN) {
        return RelayPayload::ScreenShare(plaintext.to_vec());
    }
    if plaintext.starts_with(crate::call_name::CALL_NAME_DOMAIN) {
        return RelayPayload::CallName(plaintext.to_vec());
    }
    match lakko_contacts::media_state::try_unwrap_media_state(plaintext) {
        Ok(Some(state)) => RelayPayload::MediaState {
            camera_on: state.camera_on,
            microphone_on: state.microphone_on,
        },
        Ok(None) | Err(_) => RelayPayload::Unknown,
    }
}

/// A decoded (not yet verified) peer announcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAnnouncement {
    /// The call it names.
    pub call_id: [u8; CALL_ID_LEN],
    /// The SFU peer id it claims.
    pub peer_id: String,
    /// The leaf signature key that signed it.
    pub signer_key: [u8; PUBLIC_KEY_LEN],
    /// The signature.
    pub signature: [u8; SIGNATURE_LEN],
}

/// A peer announcement that verified against the call's roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPeer {
    /// The SFU peer id.
    pub peer_id: String,
    /// The signer's leaf index in the call's group at this epoch.
    pub leaf_index: u32,
    /// The signer's leaf signature key.
    pub signature_key: [u8; PUBLIC_KEY_LEN],
    /// The account key the leaf resolves to (ADR-0050): the person.
    pub account_key: Vec<u8>,
}

fn signed_bytes(call_id: &[u8; CALL_ID_LEN], peer_id: &[u8]) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(PEER_ANNOUNCEMENT_SIG_DOMAIN.len() + CALL_ID_LEN + 1 + peer_id.len());
    out.extend_from_slice(PEER_ANNOUNCEMENT_SIG_DOMAIN);
    out.extend_from_slice(call_id);
    // `peer_id.len() <= MAX_PEER_ID_LEN` is checked by every caller.
    out.push(u8::try_from(peer_id.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(peer_id);
    out
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

/// Compose this device's peer announcement: its SFU `peer_id` in call
/// `call_id`, signed with `identity`'s leaf signature key.
///
/// # Errors
///
/// [`RelayError::Malformed`] for an empty, over-long or non-visible-ASCII
/// peer id; [`RelayError::IdentityKeyMismatch`] when the identity's signing
/// seed does not produce its published signature key (never for an
/// Ed25519 identity).
pub fn compose_peer_announcement(
    identity: &Identity,
    call_id: &[u8; CALL_ID_LEN],
    peer_id: &str,
) -> Result<Vec<u8>, RelayError> {
    let peer = peer_id.as_bytes();
    check_peer_id(peer)?;
    let signing = SigningKey::from_seed(&identity.ed25519_seed_bytes());
    let signer_key = signing.verifying_key().to_bytes();
    if identity.signature_public_key() != signer_key.as_slice() {
        return Err(RelayError::IdentityKeyMismatch);
    }
    let signature = signing.sign(&signed_bytes(call_id, peer)).to_bytes();
    let mut out = Vec::with_capacity(peer_announcement_len(peer.len()));
    out.extend_from_slice(PEER_ANNOUNCEMENT_DOMAIN);
    out.push(PEER_ANNOUNCEMENT_VERSION);
    out.extend_from_slice(call_id);
    out.push(u8::try_from(peer.len()).map_err(|_| RelayError::Malformed("peer id length"))?);
    out.extend_from_slice(peer);
    out.extend_from_slice(&signer_key);
    out.extend_from_slice(&signature);
    Ok(out)
}

/// Decode a peer announcement without verifying it. Bounded and exact:
/// every length is checked before a slice is taken.
///
/// # Errors
///
/// [`RelayError::WrongDomain`], [`RelayError::UnknownVersion`],
/// [`RelayError::BadLength`] or [`RelayError::Malformed`].
pub fn decode_peer_announcement(bytes: &[u8]) -> Result<PeerAnnouncement, RelayError> {
    let rest = bytes
        .strip_prefix(PEER_ANNOUNCEMENT_DOMAIN)
        .ok_or(RelayError::WrongDomain)?;
    let (&version, rest) = rest.split_first().ok_or(RelayError::BadLength)?;
    if version != PEER_ANNOUNCEMENT_VERSION {
        return Err(RelayError::UnknownVersion);
    }
    if rest.len() < CALL_ID_LEN + 1 {
        return Err(RelayError::BadLength);
    }
    let (call_id_bytes, rest) = rest.split_at(CALL_ID_LEN);
    let (&peer_len, rest) = rest.split_first().ok_or(RelayError::BadLength)?;
    let peer_len = usize::from(peer_len);
    if rest.len() != peer_len + PUBLIC_KEY_LEN + SIGNATURE_LEN {
        return Err(RelayError::BadLength);
    }
    let (peer, rest) = rest.split_at(peer_len);
    check_peer_id(peer)?;
    let (key_bytes, sig_bytes) = rest.split_at(PUBLIC_KEY_LEN);
    let mut call_id = [0u8; CALL_ID_LEN];
    call_id.copy_from_slice(call_id_bytes);
    let mut signer_key = [0u8; PUBLIC_KEY_LEN];
    signer_key.copy_from_slice(key_bytes);
    let mut signature = [0u8; SIGNATURE_LEN];
    signature.copy_from_slice(sig_bytes);
    // `check_peer_id` admitted visible ASCII only, so this is UTF-8.
    let peer_id = String::from_utf8(peer.to_vec()).map_err(|_| RelayError::Malformed("utf-8"))?;
    Ok(PeerAnnouncement {
        call_id,
        peer_id,
        signer_key,
        signature,
    })
}

/// Verify an announcement against the call's roster: it names `call_id`,
/// it claims the peer id the SFU stamped as the relay sender
/// (`relay_from`), its signer key is a leaf of `group`, and the signature
/// verifies under that key.
///
/// # Errors
///
/// Any decode error, then [`RelayError::WrongCall`],
/// [`RelayError::WrongPeer`], [`RelayError::NotAMember`],
/// [`RelayError::NoAccount`] or [`RelayError::BadSignature`].
pub fn verify_peer_announcement(
    group: &Group,
    call_id: &[u8; CALL_ID_LEN],
    relay_from: &str,
    bytes: &[u8],
) -> Result<VerifiedPeer, RelayError> {
    let decoded = decode_peer_announcement(bytes)?;
    if &decoded.call_id != call_id {
        return Err(RelayError::WrongCall);
    }
    if decoded.peer_id != relay_from {
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
    verifying
        .verify(
            &signed_bytes(&decoded.call_id, decoded.peer_id.as_bytes()),
            &signature,
        )
        .map_err(|_| RelayError::BadSignature)?;
    if member.account_key.is_empty() {
        return Err(RelayError::NoAccount);
    }
    Ok(VerifiedPeer {
        peer_id: decoded.peer_id,
        leaf_index: member.leaf_index,
        signature_key: decoded.signer_key,
        account_key: member.account_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_mls::GroupKind;

    const CALL: [u8; CALL_ID_LEN] = [0x11; CALL_ID_LEN];

    fn identity(seed: u8) -> Identity {
        Identity::from_seed(&[seed; 32]).expect("identity")
    }

    #[test]
    fn a_sealed_payload_opens_under_the_same_seed_only() {
        let key = RelayKey::from_call_seed(&[0x42; 32]);
        let sealed = key.seal(b"hello").unwrap();
        assert_eq!(sealed.len(), MIN_SEALED_RELAY_LEN + 5);
        assert_eq!(key.open(&sealed).unwrap(), b"hello");
        let other = RelayKey::from_call_seed(&[0x43; 32]);
        assert_eq!(other.open(&sealed), Err(RelayError::NotOpened));
    }

    #[test]
    fn a_tampered_or_short_payload_is_refused() {
        let key = RelayKey::from_call_seed(&[0x42; 32]);
        let sealed = key.seal(b"abc").unwrap();
        for i in 0..sealed.len() {
            let mut bad = sealed.clone();
            bad[i] ^= 0x01;
            assert!(key.open(&bad).is_err(), "flip at {i} must be refused");
        }
        assert_eq!(
            key.open(&sealed[..MIN_SEALED_RELAY_LEN - 1]),
            Err(RelayError::BadLength)
        );
        assert_eq!(key.open(&[]), Err(RelayError::BadLength));
        assert_eq!(
            key.seal(&vec![0u8; MAX_RELAY_PLAINTEXT + 1]),
            Err(RelayError::BadLength)
        );
    }

    #[test]
    fn the_relay_key_is_not_the_seed_and_is_per_seed() {
        let a = RelayKey::from_call_seed(&[1; 32]);
        let b = RelayKey::from_call_seed(&[2; 32]);
        assert_ne!(a.0.as_bytes(), b.0.as_bytes());
        assert_ne!(a.0.as_bytes(), &[1; 32]);
    }

    #[test]
    fn classify_names_announcements_and_media_state() {
        let alice = identity(1);
        let ann = compose_peer_announcement(&alice, &CALL, "00ff").unwrap();
        assert_eq!(
            classify_relay_plaintext(&ann),
            RelayPayload::PeerAnnouncement(ann.clone())
        );
        let on = lakko_contacts::media_state::compose_media_state(true, false);
        assert_eq!(
            classify_relay_plaintext(&on),
            RelayPayload::MediaState {
                camera_on: true,
                microphone_on: false
            }
        );
        let share = crate::screen_share::compose_screen_share(
            &alice,
            &CALL,
            "00ff",
            1,
            crate::screen_share::ShareStatus::Sharing,
            false,
        )
        .unwrap();
        assert_eq!(
            classify_relay_plaintext(&share),
            RelayPayload::ScreenShare(share.clone())
        );
        assert_eq!(classify_relay_plaintext(b"junk"), RelayPayload::Unknown);
    }

    #[test]
    fn an_announcement_round_trips_through_decode() {
        let alice = identity(1);
        let ann = compose_peer_announcement(&alice, &CALL, "abcdef0123").unwrap();
        assert_eq!(ann.len(), peer_announcement_len(10));
        let decoded = decode_peer_announcement(&ann).unwrap();
        assert_eq!(decoded.call_id, CALL);
        assert_eq!(decoded.peer_id, "abcdef0123");
        assert_eq!(decoded.signer_key.as_slice(), alice.signature_public_key());
    }

    #[test]
    fn a_bad_peer_id_is_refused_at_compose() {
        let alice = identity(1);
        assert!(compose_peer_announcement(&alice, &CALL, "").is_err());
        assert!(compose_peer_announcement(&alice, &CALL, &"a".repeat(65)).is_err());
        assert!(compose_peer_announcement(&alice, &CALL, "a b").is_err());
    }

    #[test]
    fn every_truncation_and_extension_is_refused_at_decode() {
        let ann = compose_peer_announcement(&identity(1), &CALL, "peer").unwrap();
        for n in 0..ann.len() {
            assert!(decode_peer_announcement(&ann[..n]).is_err(), "prefix {n}");
        }
        let mut long = ann.clone();
        long.push(0);
        assert_eq!(decode_peer_announcement(&long), Err(RelayError::BadLength));
    }

    #[test]
    fn verify_binds_the_announcement_to_the_roster_leaf() {
        let alice = identity(1);
        let bob = identity(2);
        let mut group = Group::create(&alice, GroupKind::Group).expect("group");
        let kp = bob.create_key_package().unwrap();
        group.add_member(&alice, kp).expect("add bob");

        let ann = compose_peer_announcement(&bob, &CALL, "bobpeer").unwrap();
        let verified = verify_peer_announcement(&group, &CALL, "bobpeer", &ann).unwrap();
        assert_eq!(verified.peer_id, "bobpeer");
        assert_eq!(
            verified.signature_key.as_slice(),
            bob.signature_public_key()
        );
        assert_eq!(verified.account_key.as_slice(), bob.signature_public_key());

        // Another call, another relay sender, a stranger, a forged signature.
        assert_eq!(
            verify_peer_announcement(&group, &[0x22; CALL_ID_LEN], "bobpeer", &ann),
            Err(RelayError::WrongCall)
        );
        assert_eq!(
            verify_peer_announcement(&group, &CALL, "alicepeer", &ann),
            Err(RelayError::WrongPeer)
        );
        let carol = identity(3);
        let stranger = compose_peer_announcement(&carol, &CALL, "carol").unwrap();
        assert_eq!(
            verify_peer_announcement(&group, &CALL, "carol", &stranger),
            Err(RelayError::NotAMember)
        );
        let mut forged = ann.clone();
        let last = forged.len() - 1;
        forged[last] ^= 0x01;
        assert_eq!(
            verify_peer_announcement(&group, &CALL, "bobpeer", &forged),
            Err(RelayError::BadSignature)
        );
        // Bob's key with Alice's signature over a claim to be Bob's peer.
        let mut swapped = compose_peer_announcement(&alice, &CALL, "bobpeer").unwrap();
        let key_at = swapped.len() - SIGNATURE_LEN - PUBLIC_KEY_LEN;
        swapped[key_at..key_at + PUBLIC_KEY_LEN].copy_from_slice(bob.signature_public_key());
        assert_eq!(
            verify_peer_announcement(&group, &CALL, "bobpeer", &swapped),
            Err(RelayError::BadSignature)
        );
    }
}
