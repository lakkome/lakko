// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the link-founded note (ADR-0079, ordering of
//! 2026-10-17): the device that won an invite-link request names the
//! one-to-one it founded, with the founding Welcome, in the account's own
//! DeviceGroup BEFORE delivering that Welcome to anyone.
//!
//! Mirror of `crates/lakko-ffi-wasm/src/link_founded.rs` for iOS + Android.
//! Sent ONLY in the account's DeviceGroup (as a `serviceEnvelope` message).
//! A receiver routes it with [`is_link_founded_envelope`], decodes it with
//! [`decode_link_founded_envelope`], keeps the earliest note per request from
//! a resolvable DeviceGroup member (C-68), and never founds for a request it
//! holds a note for: it writes the note's Welcome to the requester instead.

use lakko_contacts::invite::LINK_ID_LEN;
use lakko_contacts::link_claim::LINK_CLAIM_REQUESTER_KEY_LEN;
use lakko_contacts::link_founded::{
    compose_link_founded as rust_compose, decode_link_founded as rust_decode, LinkFounded,
    LinkFoundedError, LINK_FOUNDED_ENVELOPE_DOMAIN_V1, LINK_FOUNDED_ENVELOPE_MAX_LEN,
    LINK_FOUNDED_ENVELOPE_MIN_LEN, LINK_FOUNDED_GROUP_ID_LEN,
};

use crate::FfiError;

// A note must fit the DeviceGroup `serviceEnvelope` carrier.
const _: () = assert!(LINK_FOUNDED_ENVELOPE_MAX_LEN <= lakko_mls::MAX_SERVICE_ENVELOPE_LEN);

fn wire_error(e: LinkFoundedError) -> FfiError {
    FfiError::Contacts(format!("link founded {}: {e}", e.code()))
}

fn bounds(what: &str) -> FfiError {
    FfiError::Contacts(format!("link founded: {what}"))
}

/// A decoded note: the request, the founded one-to-one and its Welcome.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiLinkFounded {
    /// The invite link's 16-byte id.
    pub link_id: Vec<u8>,
    /// The requester's 32-byte account key.
    pub requester_account_key: Vec<u8>,
    /// `link_id ‖ requester_account_key` (48 bytes): the claim bookkeeping's
    /// key.
    pub request_key: Vec<u8>,
    /// The founded one-to-one's 32-byte group id.
    pub group_id: Vec<u8>,
    /// The founding Welcome (wire bytes).
    pub welcome: Vec<u8>,
}

impl From<LinkFounded> for FfiLinkFounded {
    fn from(n: LinkFounded) -> Self {
        Self {
            link_id: n.link_id.to_vec(),
            requester_account_key: n.requester_account_key.to_vec(),
            request_key: n.request_key().to_vec(),
            group_id: n.group_id.to_vec(),
            welcome: n.welcome,
        }
    }
}

/// Compose a note: the bytes to hand to
/// `DeviceGroup.createServiceEnvelopeMessage`.
///
/// # Errors
///
/// [`FfiError::Contacts`] for a wrong-length id or key, or a Welcome that is
/// empty or longer than the bound.
#[uniffi::export]
pub fn compose_link_founded_envelope(
    link_id: Vec<u8>,
    requester_account_key: Vec<u8>,
    group_id: Vec<u8>,
    welcome: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let link_id: [u8; LINK_ID_LEN] = link_id
        .try_into()
        .map_err(|_| bounds("link id must be 16 bytes"))?;
    let requester_account_key: [u8; LINK_CLAIM_REQUESTER_KEY_LEN] = requester_account_key
        .try_into()
        .map_err(|_| bounds("requester key must be 32 bytes"))?;
    let group_id: [u8; LINK_FOUNDED_GROUP_ID_LEN] = group_id
        .try_into()
        .map_err(|_| bounds("group id must be 32 bytes"))?;
    rust_compose(&LinkFounded {
        link_id,
        requester_account_key,
        group_id,
        welcome,
    })
    .map_err(wire_error)
}

/// Strictly decode a note. Fail-closed.
///
/// # Errors
///
/// [`FfiError::Contacts`] carrying the frozen reason code.
#[uniffi::export]
pub fn decode_link_founded_envelope(plaintext: Vec<u8>) -> Result<FfiLinkFounded, FfiError> {
    rust_decode(&plaintext).map(Into::into).map_err(wire_error)
}

/// `true` when `bytes` open with the note's domain: the cheap routing
/// question a client asks of a DeviceGroup service envelope.
#[uniffi::export]
#[must_use]
pub fn is_link_founded_envelope(bytes: Vec<u8>) -> bool {
    bytes.starts_with(LINK_FOUNDED_ENVELOPE_DOMAIN_V1)
}

/// The shortest well-formed note.
#[uniffi::export]
#[must_use]
pub fn link_founded_envelope_min_len() -> u32 {
    u32::try_from(LINK_FOUNDED_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// The longest well-formed note.
#[uniffi::export]
#[must_use]
pub fn link_founded_envelope_max_len() -> u32 {
    u32::try_from(LINK_FOUNDED_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::link_founded::LINK_FOUNDED_MAX_WELCOME_LEN;
    use lakko_mls::{group, key_package, Group, GroupKind, Identity};

    #[test]
    fn compose_decode_and_dispatch_agree() {
        let wire = compose_link_founded_envelope(vec![1; 16], vec![2; 32], vec![3; 32], vec![4; 9])
            .unwrap();
        assert!(is_link_founded_envelope(wire.clone()));
        let decoded = decode_link_founded_envelope(wire.clone()).unwrap();
        assert_eq!(decoded.group_id, vec![3; 32]);
        assert_eq!(decoded.welcome, vec![4; 9]);
        assert_eq!(decoded.request_key[..16], [1; 16]);
        let dispatched = crate::contacts::dispatch_incoming_envelope(wire).unwrap();
        assert_eq!(
            dispatched.kind,
            crate::contacts::FfiIncomingEnvelopeKind::LinkFounded
        );
        assert_eq!(dispatched.link_founded, Some(decoded));
        assert!(dispatched.payload.is_empty());
        assert!(
            compose_link_founded_envelope(vec![1; 15], vec![2; 32], vec![3; 32], vec![4]).is_err()
        );
        assert!(
            compose_link_founded_envelope(vec![1; 16], vec![2; 32], vec![3; 31], vec![4]).is_err()
        );
        assert!(
            compose_link_founded_envelope(vec![1; 16], vec![2; 32], vec![3; 32], vec![]).is_err()
        );
        assert!(decode_link_founded_envelope(b"nope".to_vec()).is_err());
    }

    /// The bound is MEASURED, not guessed: a one-to-one founded with the most
    /// devices an account may hold on each side (ADR-0050, 8 per account: the
    /// founder plus 7 of its own devices, and 8 of the requester's, all bound
    /// device leaves) produces a Welcome that fits the note with room to
    /// spare. The measured size is printed with `--nocapture`.
    #[test]
    fn a_maximal_one_to_one_welcome_fits_the_note() {
        let per_account = lakko_mls::group_proposals::MAX_LEAVES_PER_ACCOUNT;
        let a = Identity::from_seed(&[0xA0; 64]).unwrap();
        let b = Identity::from_seed(&[0xB0; 64]).unwrap();
        let device = |owner: &Identity, tag: u8| {
            Identity::from_seed_bound(&[tag; 32], &owner.ed25519_seed_bytes(), 1_700_000_000)
                .unwrap()
        };
        let founder = device(&a, 1);
        let mut kps = Vec::new();
        for t in 0..u8::try_from(per_account - 1).unwrap() {
            kps.push(
                key_package::serialize(&device(&a, 2 + t).create_key_package().unwrap()).unwrap(),
            );
        }
        for t in 0..u8::try_from(per_account).unwrap() {
            kps.push(
                key_package::serialize(&device(&b, 40 + t).create_key_package().unwrap()).unwrap(),
            );
        }
        let mut g = Group::create(&founder, GroupKind::OneToOne).unwrap();
        let refs: Vec<&[u8]> = kps.iter().map(Vec::as_slice).collect();
        let added = g.add_members_from_wire(&founder, &refs).unwrap();
        let welcome = group::encode_message(&added.welcome).unwrap();
        println!(
            "measured maximal one-to-one Welcome: {} bytes ({} added leaves)",
            welcome.len(),
            refs.len()
        );
        assert!(
            welcome.len() * 2 <= LINK_FOUNDED_MAX_WELCOME_LEN,
            "{}",
            welcome.len()
        );
        let wire =
            compose_link_founded_envelope(vec![1; 16], vec![2; 32], g.group_id(), welcome).unwrap();
        assert!(wire.len() <= lakko_mls::MAX_SERVICE_ENVELOPE_LEN);
    }
}
