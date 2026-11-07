// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The name a participant gives themself for one call (2026-11-07,
//! `docs/ux/calls.md` section 4.1).
//!
//! # Why
//!
//! Any participant of a call, a guest or an account holder, may change the
//! name the other participants see on their tile, for this call only. The
//! profile name does not change and no server learns the name. The name
//! rides the sealed SFU relay of [`crate::relay`] (ADR-0078 section 2),
//! signed with the participant's MLS leaf key exactly like the peer
//! announcement and the screen-share state, so a receiver attributes it to
//! the leaf that signed it and never to whatever the SFU stamped as the
//! relay sender.
//!
//! # Wire
//!
//! ```text
//! "lakko/call/name/v1\0" (19) || version(1) = 0x01 ||
//! call_id(16) || peer_id_len(1) || peer_id(1..=64, visible ASCII) ||
//! seq(8, BE u64, >= 1) || name_len(1) || name(0..=255 bytes UTF-8) ||
//! signer_key(32) || signature(64)
//! ```
//!
//! The signature is Ed25519 by `signer_key` (the sender's MLS leaf key) over
//! `"lakko/call/name/sig/v1\0" || call_id || peer_id_len || peer_id || seq
//! || name_len || name`. An empty name withdraws an earlier one (the
//! receiver goes back to its default naming); a non-empty name passes the
//! display-name rule of `lakko_contacts::profile::check_display_name` as it
//! stands (no trimming: the sender normalises before composing).
//!
//! # Order
//!
//! A sender numbers its names `1, 2, 3, ...` for the life of its peer id
//! and re-sends the current one, unchanged, whenever it re-announces itself.
//! A receiver keeps, per peer, the name with the greatest `seq`
//! ([`call_name_is_newer`]): equal is not newer, so a re-send is not an
//! update and a late copy of an older name never replaces a newer one.

use lakko_contacts::profile::check_display_name;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use lakko_mls::{Group, Identity};

use crate::relay::{RelayError, CALL_ID_LEN, MAX_PEER_ID_LEN};

/// Domain prefix of a call name. Frozen.
pub const CALL_NAME_DOMAIN: &[u8] = b"lakko/call/name/v1\x00";

/// Version byte of a call name.
pub const CALL_NAME_VERSION: u8 = 0x01;

/// Domain of the signed bytes; distinct from the wire domain and from the
/// peer announcement's and the screen-share state's, so no signature
/// crosses between them.
const CALL_NAME_SIG_DOMAIN: &[u8] = b"lakko/call/name/sig/v1\x00";

/// Longest name, in bytes (the display-name rule's byte bound).
pub const MAX_CALL_NAME_BYTES: usize = lakko_contacts::profile::DISPLAY_NAME_MAX_BYTES;

/// Length of a call name carrying a `peer_id_len`-byte peer id and a
/// `name_len`-byte name.
#[must_use]
pub const fn call_name_len(peer_id_len: usize, name_len: usize) -> usize {
    CALL_NAME_DOMAIN.len()
        + 1
        + CALL_ID_LEN
        + 1
        + peer_id_len
        + 8
        + 1
        + name_len
        + PUBLIC_KEY_LEN
        + SIGNATURE_LEN
}

/// `true` when a name numbered `incoming_seq` must replace the one a
/// receiver holds for the same peer, numbered `stored_seq` (`None` when it
/// holds none). Strictly greater wins; equal is a re-send, not an update.
#[must_use]
pub const fn call_name_is_newer(incoming_seq: u64, stored_seq: Option<u64>) -> bool {
    match stored_seq {
        None => true,
        Some(stored) => incoming_seq > stored,
    }
}

/// A decoded (not yet verified) call name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallNameAnnouncement {
    /// The call it names.
    pub call_id: [u8; CALL_ID_LEN],
    /// The sender's SFU peer id.
    pub peer_id: String,
    /// The sender's sequence number for this name (>= 1).
    pub seq: u64,
    /// The name; empty withdraws an earlier one.
    pub name: String,
    /// The leaf key that signed it.
    pub signer_key: [u8; PUBLIC_KEY_LEN],
    /// The signature.
    pub signature: [u8; SIGNATURE_LEN],
}

/// A call name that verified against the call's roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCallName {
    /// The sender's peer id.
    pub peer_id: String,
    /// The sequence number.
    pub seq: u64,
    /// The name, `None` when the sender withdrew its call name.
    pub name: Option<String>,
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

/// Empty, or a display name as it stands.
fn check_name(name: &str) -> Result<(), RelayError> {
    if name.is_empty() {
        return Ok(());
    }
    check_display_name(name).map_err(|_| RelayError::Malformed("name"))
}

fn signed_bytes(call_id: &[u8; CALL_ID_LEN], peer_id: &[u8], seq: u64, name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        CALL_NAME_SIG_DOMAIN.len() + CALL_ID_LEN + 1 + peer_id.len() + 8 + 1 + name.len(),
    );
    out.extend_from_slice(CALL_NAME_SIG_DOMAIN);
    out.extend_from_slice(call_id);
    // Both lengths are bounded below `u8::MAX` by every caller.
    out.push(u8::try_from(peer_id.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(peer_id);
    out.extend_from_slice(&seq.to_be_bytes());
    out.push(u8::try_from(name.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(name);
    out
}

/// Compose this device's call name: `name` (empty to withdraw) numbered
/// `seq` for call `call_id` under SFU `peer_id`, signed with `identity`'s
/// leaf signature key.
///
/// # Errors
///
/// [`RelayError::Malformed`] for a bad peer id, a `seq` of 0, or a name
/// that is neither empty nor an admissible display name as it stands (an
/// untrimmed name is refused: normalise first);
/// [`RelayError::IdentityKeyMismatch`] when the identity's signing seed does
/// not produce its published signature key.
pub fn compose_call_name(
    identity: &Identity,
    call_id: &[u8; CALL_ID_LEN],
    peer_id: &str,
    seq: u64,
    name: &str,
) -> Result<Vec<u8>, RelayError> {
    let peer = peer_id.as_bytes();
    check_peer_id(peer)?;
    if seq == 0 {
        return Err(RelayError::Malformed("seq"));
    }
    check_name(name)?;
    let name_bytes = name.as_bytes();
    let signing = SigningKey::from_seed(&identity.ed25519_seed_bytes());
    let signer_key = signing.verifying_key().to_bytes();
    if identity.signature_public_key() != signer_key.as_slice() {
        return Err(RelayError::IdentityKeyMismatch);
    }
    let signature = signing
        .sign(&signed_bytes(call_id, peer, seq, name_bytes))
        .to_bytes();
    let mut out = Vec::with_capacity(call_name_len(peer.len(), name_bytes.len()));
    out.extend_from_slice(CALL_NAME_DOMAIN);
    out.push(CALL_NAME_VERSION);
    out.extend_from_slice(call_id);
    out.push(u8::try_from(peer.len()).map_err(|_| RelayError::Malformed("peer id length"))?);
    out.extend_from_slice(peer);
    out.extend_from_slice(&seq.to_be_bytes());
    out.push(u8::try_from(name_bytes.len()).map_err(|_| RelayError::Malformed("name"))?);
    out.extend_from_slice(name_bytes);
    out.extend_from_slice(&signer_key);
    out.extend_from_slice(&signature);
    Ok(out)
}

/// Decode a call name without verifying it. Bounded and exact: every length
/// is checked before a slice is taken, and the name passes the same rule
/// [`compose_call_name`] enforces.
///
/// # Errors
///
/// [`RelayError::WrongDomain`], [`RelayError::UnknownVersion`],
/// [`RelayError::BadLength`] or [`RelayError::Malformed`].
pub fn decode_call_name(bytes: &[u8]) -> Result<CallNameAnnouncement, RelayError> {
    let rest = bytes
        .strip_prefix(CALL_NAME_DOMAIN)
        .ok_or(RelayError::WrongDomain)?;
    let (&version, rest) = rest.split_first().ok_or(RelayError::BadLength)?;
    if version != CALL_NAME_VERSION {
        return Err(RelayError::UnknownVersion);
    }
    if rest.len() < CALL_ID_LEN + 1 {
        return Err(RelayError::BadLength);
    }
    let (call_id_bytes, rest) = rest.split_at(CALL_ID_LEN);
    let (&peer_len, rest) = rest.split_first().ok_or(RelayError::BadLength)?;
    let peer_len = usize::from(peer_len);
    if rest.len() < peer_len + 8 + 1 {
        return Err(RelayError::BadLength);
    }
    let (peer, rest) = rest.split_at(peer_len);
    check_peer_id(peer)?;
    let (seq_bytes, rest) = rest.split_at(8);
    let (&name_len, rest) = rest.split_first().ok_or(RelayError::BadLength)?;
    let name_len = usize::from(name_len);
    if rest.len() != name_len + PUBLIC_KEY_LEN + SIGNATURE_LEN {
        return Err(RelayError::BadLength);
    }
    let (name_bytes, rest) = rest.split_at(name_len);
    let (key_bytes, sig_bytes) = rest.split_at(PUBLIC_KEY_LEN);

    let mut seq_be = [0u8; 8];
    seq_be.copy_from_slice(seq_bytes);
    let seq = u64::from_be_bytes(seq_be);
    if seq == 0 {
        return Err(RelayError::Malformed("seq"));
    }
    let name =
        String::from_utf8(name_bytes.to_vec()).map_err(|_| RelayError::Malformed("utf-8"))?;
    check_name(&name)?;
    let mut call_id = [0u8; CALL_ID_LEN];
    call_id.copy_from_slice(call_id_bytes);
    let mut signer_key = [0u8; PUBLIC_KEY_LEN];
    signer_key.copy_from_slice(key_bytes);
    let mut signature = [0u8; SIGNATURE_LEN];
    signature.copy_from_slice(sig_bytes);
    // `check_peer_id` admitted visible ASCII only, so this is UTF-8.
    let peer_id = String::from_utf8(peer.to_vec()).map_err(|_| RelayError::Malformed("utf-8"))?;
    Ok(CallNameAnnouncement {
        call_id,
        peer_id,
        seq,
        name,
        signer_key,
        signature,
    })
}

/// Verify a call name: it names `call_id`, it names `peer_id`, it was signed
/// by `peer_signer_key`, that key is a leaf of `group`, and the signature
/// verifies.
///
/// `peer_id` and `peer_signer_key` are what the caller has already bound to
/// the sender: the relay's stamped sender and the key that sender's
/// verified peer announcement named. Requiring both is what keeps an SFU
/// that colludes with one member from putting that member's name on another
/// member's tile.
///
/// # Errors
///
/// Any decode error, then [`RelayError::WrongCall`],
/// [`RelayError::WrongPeer`], [`RelayError::NotAMember`],
/// [`RelayError::NoAccount`] or [`RelayError::BadSignature`].
pub fn verify_call_name(
    group: &Group,
    call_id: &[u8; CALL_ID_LEN],
    peer_id: &str,
    peer_signer_key: &[u8],
    bytes: &[u8],
) -> Result<VerifiedCallName, RelayError> {
    let decoded = decode_call_name(bytes)?;
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
    verifying
        .verify(
            &signed_bytes(
                &decoded.call_id,
                decoded.peer_id.as_bytes(),
                decoded.seq,
                decoded.name.as_bytes(),
            ),
            &signature,
        )
        .map_err(|_| RelayError::BadSignature)?;
    if member.account_key.is_empty() {
        return Err(RelayError::NoAccount);
    }
    Ok(VerifiedCallName {
        peer_id: decoded.peer_id,
        seq: decoded.seq,
        name: if decoded.name.is_empty() {
            None
        } else {
            Some(decoded.name)
        },
        account_key: member.account_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::{classify_relay_plaintext, compose_peer_announcement, RelayPayload};
    use lakko_mls::GroupKind;
    use proptest::prelude::*;

    const CALL: [u8; CALL_ID_LEN] = [0x11; CALL_ID_LEN];

    fn identity(seed: u8) -> Identity {
        Identity::from_seed(&[seed; 32]).expect("identity")
    }

    fn two_member_group() -> (Identity, Identity, Group) {
        let alice = identity(1);
        let bob = identity(2);
        let mut group = Group::create(&alice, GroupKind::Group).expect("group");
        let kp = bob.create_key_package().unwrap();
        group.add_member(&alice, kp).expect("add bob");
        (alice, bob, group)
    }

    #[test]
    fn a_name_round_trips_and_classifies() {
        let bob = identity(2);
        let wire = compose_call_name(&bob, &CALL, "bobpeer", 3, "Anna K").unwrap();
        assert_eq!(wire.len(), call_name_len(7, 6));
        let decoded = decode_call_name(&wire).unwrap();
        assert_eq!(decoded.call_id, CALL);
        assert_eq!(decoded.peer_id, "bobpeer");
        assert_eq!(decoded.seq, 3);
        assert_eq!(decoded.name, "Anna K");
        assert_eq!(decoded.signer_key.as_slice(), bob.signature_public_key());
        assert_eq!(
            classify_relay_plaintext(&wire),
            RelayPayload::CallName(wire.clone())
        );
    }

    #[test]
    fn an_empty_name_withdraws() {
        let (_alice, bob, group) = two_member_group();
        let wire = compose_call_name(&bob, &CALL, "bobpeer", 2, "").unwrap();
        let v =
            verify_call_name(&group, &CALL, "bobpeer", bob.signature_public_key(), &wire).unwrap();
        assert_eq!(v.name, None);
        assert_eq!(v.seq, 2);
    }

    #[test]
    fn compose_refuses_what_decode_refuses() {
        let bob = identity(2);
        for bad in [" Anna", "Anna ", "a\nb", "a\u{200B}b", &"x".repeat(65)] {
            assert_eq!(
                compose_call_name(&bob, &CALL, "p", 1, bad),
                Err(RelayError::Malformed("name")),
                "{bad:?}"
            );
        }
        assert_eq!(
            compose_call_name(&bob, &CALL, "p", 0, "Anna"),
            Err(RelayError::Malformed("seq"))
        );
        assert!(compose_call_name(&bob, &CALL, "", 1, "Anna").is_err());
        assert!(compose_call_name(&bob, &CALL, "a b", 1, "Anna").is_err());
    }

    #[test]
    fn a_name_that_breaks_the_rule_is_refused_at_decode() {
        // Build a wire by hand with an untrimmed name and a valid layout.
        let bob = identity(2);
        let good = compose_call_name(&bob, &CALL, "p", 1, "Anna").unwrap();
        let name_at = CALL_NAME_DOMAIN.len() + 1 + CALL_ID_LEN + 1 + 1 + 8 + 1;
        let mut bad = good;
        bad[name_at] = b' ';
        assert_eq!(decode_call_name(&bad), Err(RelayError::Malformed("name")));
    }

    #[test]
    fn verify_binds_the_name_to_the_announced_leaf() {
        let (alice, bob, group) = two_member_group();
        let wire = compose_call_name(&bob, &CALL, "bobpeer", 1, "Bobby").unwrap();
        let ok =
            verify_call_name(&group, &CALL, "bobpeer", bob.signature_public_key(), &wire).unwrap();
        assert_eq!(ok.name.as_deref(), Some("Bobby"));
        assert_eq!(ok.account_key.as_slice(), bob.signature_public_key());

        assert_eq!(
            verify_call_name(
                &group,
                &[0x22; CALL_ID_LEN],
                "bobpeer",
                bob.signature_public_key(),
                &wire
            ),
            Err(RelayError::WrongCall)
        );
        // Re-attributed to another peer by the SFU.
        assert_eq!(
            verify_call_name(
                &group,
                &CALL,
                "alicepeer",
                bob.signature_public_key(),
                &wire
            ),
            Err(RelayError::WrongPeer)
        );
        // Bob signs a name claiming Alice's peer id; the SFU stamps it as
        // Alice's: the key Alice's announcement named is not Bob's.
        let claim = compose_call_name(&bob, &CALL, "alicepeer", 9, "Alice?").unwrap();
        assert_eq!(
            verify_call_name(
                &group,
                &CALL,
                "alicepeer",
                alice.signature_public_key(),
                &claim
            ),
            Err(RelayError::WrongPeer)
        );
        let carol = identity(3);
        let stranger = compose_call_name(&carol, &CALL, "carol", 1, "Carol").unwrap();
        assert_eq!(
            verify_call_name(
                &group,
                &CALL,
                "carol",
                carol.signature_public_key(),
                &stranger
            ),
            Err(RelayError::NotAMember)
        );
        let mut forged = wire.clone();
        let last = forged.len() - 1;
        forged[last] ^= 0x01;
        assert_eq!(
            verify_call_name(
                &group,
                &CALL,
                "bobpeer",
                bob.signature_public_key(),
                &forged
            ),
            Err(RelayError::BadSignature)
        );
    }

    #[test]
    fn signatures_do_not_cross_with_the_announcement() {
        // A peer announcement's signature is over another domain: splicing it
        // into a name of the same peer never verifies.
        let (_alice, bob, group) = two_member_group();
        let ann = compose_peer_announcement(&bob, &CALL, "bobpeer").unwrap();
        let ann_sig = &ann[ann.len() - SIGNATURE_LEN..];
        let mut wire = compose_call_name(&bob, &CALL, "bobpeer", 1, "Bob").unwrap();
        let at = wire.len() - SIGNATURE_LEN;
        wire[at..].copy_from_slice(ann_sig);
        assert_eq!(
            verify_call_name(&group, &CALL, "bobpeer", bob.signature_public_key(), &wire),
            Err(RelayError::BadSignature)
        );
    }

    #[test]
    fn newer_is_strictly_greater() {
        assert!(call_name_is_newer(1, None));
        assert!(call_name_is_newer(2, Some(1)));
        assert!(!call_name_is_newer(1, Some(1)));
        assert!(!call_name_is_newer(1, Some(2)));
    }

    #[test]
    fn the_largest_name_fits_a_relay_payload() {
        let longest = call_name_len(MAX_PEER_ID_LEN, MAX_CALL_NAME_BYTES);
        assert!(longest <= crate::relay::MAX_RELAY_PLAINTEXT);
    }

    fn valid_name() -> impl Strategy<Value = String> {
        prop_oneof![
            Just(String::new()),
            "[A-Za-z0-9]([A-Za-z0-9 ._'-]{0,40}[A-Za-z0-9])?",
            "[\u{0400}-\u{04FF}\u{4E00}-\u{4E80}]{1,30}",
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn compose_then_decode_is_the_identity(
            peer in "[!-~]{1,64}",
            seq in 1u64..=u64::MAX,
            name in valid_name(),
            call_id in proptest::array::uniform16(any::<u8>()),
        ) {
            let bob = identity(2);
            let wire = compose_call_name(&bob, &call_id, &peer, seq, &name).unwrap();
            prop_assert_eq!(wire.len(), call_name_len(peer.len(), name.len()));
            let d = decode_call_name(&wire).unwrap();
            prop_assert_eq!(d.call_id, call_id);
            prop_assert_eq!(d.peer_id, peer);
            prop_assert_eq!(d.seq, seq);
            prop_assert_eq!(d.name, name);
        }

        #[test]
        fn decode_never_panics_and_refuses_every_damaged_wire(
            bytes in proptest::collection::vec(any::<u8>(), 0..600),
            cut in 0usize..200,
        ) {
            let _ = decode_call_name(&bytes);
            let mut prefixed = CALL_NAME_DOMAIN.to_vec();
            prefixed.extend_from_slice(&bytes);
            let _ = decode_call_name(&prefixed);
            let bob = identity(2);
            let wire = compose_call_name(&bob, &CALL, "peer", 1, "Name").unwrap();
            let n = cut.min(wire.len() - 1);
            prop_assert!(decode_call_name(&wire[..n]).is_err());
        }

        #[test]
        fn any_flipped_byte_fails_verification(at in 0usize..1000, bit in 0u8..8) {
            let (_alice, bob, group) = two_member_group();
            let wire = compose_call_name(&bob, &CALL, "bobpeer", 5, "Bobby").unwrap();
            let i = at % wire.len();
            let mut bad = wire;
            bad[i] ^= 1 << bit;
            prop_assert!(verify_call_name(
                &group, &CALL, "bobpeer", bob.signature_public_key(), &bad
            ).is_err());
        }
    }
}
