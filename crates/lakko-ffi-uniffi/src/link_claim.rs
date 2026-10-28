// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the link-request claim (ADR-0079, reworked
//! 2026-10-17): one device of the inviter's account claims a contact request
//! sent through one of the account's invite links, so that only one device
//! answers it.
//!
//! Mirror of `crates/lakko-ffi-wasm/src/link_claim.rs` for iOS + Android. The
//! claim is sent ONLY in the account's DeviceGroup (as a `serviceEnvelope`
//! message). A receiver routes it with [`is_link_claim_envelope`], decodes it
//! with [`decode_link_claim_envelope`], honours it only from a resolvable
//! DeviceGroup member (C-68), keeps each claim's DS message id with its
//! author's leaf key, and decides with [`link_claim_outcome`] under its
//! per-group lock (C-62 sub-invariant 5): the earliest claim from a device in
//! the current roster wins, and the device whose leaf posted it builds.

use lakko_contacts::invite::LINK_ID_LEN;
use lakko_contacts::link_claim::{
    compose_link_claim as rust_compose, decode_link_claim as rust_decode,
    link_claim_answered_elsewhere as rust_answered, link_claim_outcome as rust_outcome, LinkClaim,
    LinkClaimError, LinkClaimOutcome, ObservedLinkClaim, LINK_CLAIM_AUTHOR_KEY_LEN,
    LINK_CLAIM_ENVELOPE_DOMAIN_V1, LINK_CLAIM_ENVELOPE_LEN, LINK_CLAIM_REQUESTER_KEY_LEN,
};

use crate::FfiError;

// A claim must fit the DeviceGroup `serviceEnvelope` carrier.
const _: () = assert!(LINK_CLAIM_ENVELOPE_LEN <= lakko_mls::MAX_SERVICE_ENVELOPE_LEN);

fn wire_error(e: LinkClaimError) -> FfiError {
    FfiError::Contacts(format!("link claim {}: {e}", e.code()))
}

fn claim_of(link_id: &[u8], requester_key: &[u8]) -> Result<LinkClaim, FfiError> {
    let link_id: [u8; LINK_ID_LEN] = link_id
        .try_into()
        .map_err(|_| FfiError::Contacts("link claim: link id must be 16 bytes".to_string()))?;
    let requester_account_key: [u8; LINK_CLAIM_REQUESTER_KEY_LEN] =
        requester_key.try_into().map_err(|_| {
            FfiError::Contacts("link claim: requester key must be 32 bytes".to_string())
        })?;
    Ok(LinkClaim {
        link_id,
        requester_account_key,
    })
}

/// A decoded claim: the request it names.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiLinkClaim {
    /// The invite link's 16-byte id.
    pub link_id: Vec<u8>,
    /// The requester's 32-byte account key.
    pub requester_account_key: Vec<u8>,
    /// `link_id ‖ requester_account_key` (48 bytes): the key every client
    /// files its claim bookkeeping under.
    pub request_key: Vec<u8>,
}

impl From<LinkClaim> for FfiLinkClaim {
    fn from(c: LinkClaim) -> Self {
        Self {
            link_id: c.link_id.to_vec(),
            requester_account_key: c.requester_account_key.to_vec(),
            request_key: c.request_key().to_vec(),
        }
    }
}

/// One claim for a request as this device holds it: its DS message id and
/// the DeviceGroup leaf signature key of the device that posted it (resolved
/// from the MLS-authenticated sender leaf at receipt, or this device's own
/// leaf key for its own claims).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiObservedLinkClaim {
    /// The DS message id of the claim's row.
    pub message_id: u64,
    /// The 32-byte leaf signature key of the device that posted it.
    pub author_key: Vec<u8>,
}

/// How a request's claims end, for one device.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiLinkClaimOutcome {
    /// The winning claim is this device's: this device answers the request.
    Won {
        /// The DS message id of the winning claim.
        message_id: u64,
    },
    /// Another device's claim wins: build nothing.
    Lost {
        /// The DS message id of the winning claim.
        winner_message_id: u64,
    },
    /// No claim from a device in the current roster: nobody has won yet;
    /// post (again) and decide later.
    NoWinner,
}

fn author_key(bytes: &[u8]) -> Result<[u8; LINK_CLAIM_AUTHOR_KEY_LEN], FfiError> {
    bytes
        .try_into()
        .map_err(|_| FfiError::Contacts("link claim: a leaf key must be 32 bytes".to_string()))
}

fn observed(claims: &[FfiObservedLinkClaim]) -> Result<Vec<ObservedLinkClaim>, FfiError> {
    claims
        .iter()
        .map(|c| {
            Ok(ObservedLinkClaim {
                message_id: c.message_id,
                author_key: author_key(&c.author_key)?,
            })
        })
        .collect()
}

fn roster(keys: &[Vec<u8>]) -> Result<Vec<[u8; LINK_CLAIM_AUTHOR_KEY_LEN]>, FfiError> {
    keys.iter().map(|k| author_key(k)).collect()
}

/// Compose a claim envelope: the bytes to hand to
/// `DeviceGroup.createServiceEnvelopeMessage`.
///
/// # Errors
///
/// [`FfiError::Contacts`] when the link id is not 16 bytes or the requester
/// key not 32.
#[uniffi::export]
pub fn compose_link_claim_envelope(
    link_id: Vec<u8>,
    requester_account_key: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    Ok(rust_compose(&claim_of(&link_id, &requester_account_key)?))
}

/// Strictly decode a claim envelope. Fail-closed.
///
/// # Errors
///
/// [`FfiError::Contacts`] carrying the frozen reason code.
#[uniffi::export]
pub fn decode_link_claim_envelope(plaintext: Vec<u8>) -> Result<FfiLinkClaim, FfiError> {
    rust_decode(&plaintext).map(Into::into).map_err(wire_error)
}

/// `true` when `bytes` open with the claim domain: the cheap routing
/// question a client asks of a DeviceGroup service envelope. Says nothing
/// about validity.
#[uniffi::export]
#[must_use]
pub fn is_link_claim_envelope(bytes: Vec<u8>) -> bool {
    bytes.starts_with(LINK_CLAIM_ENVELOPE_DOMAIN_V1)
}

/// The request key `link_id ‖ requester_account_key` (48 bytes).
///
/// # Errors
///
/// [`FfiError::Contacts`] on a wrong-length input.
#[uniffi::export]
pub fn link_claim_request_key(
    link_id: Vec<u8>,
    requester_account_key: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    Ok(claim_of(&link_id, &requester_account_key)?
        .request_key()
        .to_vec())
}

/// How the claims for one request end for the device whose DeviceGroup leaf
/// key is `own_key`: the earliest claim whose author is in `roster_keys`
/// (the DeviceGroup's current leaf keys) wins, and its author builds. Pass
/// every claim held for the request, this device's own included with
/// `own_key` as the author, read off the journal past this device's latest
/// claim.
///
/// # Errors
///
/// [`FfiError::Contacts`] when a key is not 32 bytes.
#[uniffi::export]
pub fn link_claim_outcome(
    own_key: Vec<u8>,
    claims: Vec<FfiObservedLinkClaim>,
    roster_keys: Vec<Vec<u8>>,
) -> Result<FfiLinkClaimOutcome, FfiError> {
    let own = author_key(&own_key)?;
    Ok(
        match rust_outcome(&own, &observed(&claims)?, &roster(&roster_keys)?) {
            LinkClaimOutcome::Won { message_id } => FfiLinkClaimOutcome::Won { message_id },
            LinkClaimOutcome::Lost { winner_message_id } => {
                FfiLinkClaimOutcome::Lost { winner_message_id }
            },
            LinkClaimOutcome::NoWinner => FfiLinkClaimOutcome::NoWinner,
        },
    )
}

/// Whether a device that holds NO claim of its own treats the request as
/// answered: some device in the current roster claimed it.
///
/// # Errors
///
/// [`FfiError::Contacts`] when a key is not 32 bytes.
#[uniffi::export]
pub fn link_claim_answered_elsewhere(
    claims: Vec<FfiObservedLinkClaim>,
    roster_keys: Vec<Vec<u8>>,
) -> Result<bool, FfiError> {
    Ok(rust_answered(&observed(&claims)?, &roster(&roster_keys)?))
}

/// The one length of a claim envelope.
#[uniffi::export]
#[must_use]
pub fn link_claim_envelope_len() -> u32 {
    u32::try_from(LINK_CLAIM_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_decode_dispatch_and_the_outcome_agree() {
        let wire = compose_link_claim_envelope(vec![1; 16], vec![2; 32]).unwrap();
        assert!(is_link_claim_envelope(wire.clone()));
        assert_eq!(wire.len(), link_claim_envelope_len() as usize);
        let decoded = decode_link_claim_envelope(wire.clone()).unwrap();
        assert_eq!(decoded.link_id, vec![1; 16]);
        assert_eq!(decoded.requester_account_key, vec![2; 32]);
        assert_eq!(
            decoded.request_key,
            link_claim_request_key(vec![1; 16], vec![2; 32]).unwrap()
        );
        let dispatched = crate::contacts::dispatch_incoming_envelope(wire).unwrap();
        assert_eq!(
            dispatched.kind,
            crate::contacts::FfiIncomingEnvelopeKind::LinkClaim
        );
        assert_eq!(dispatched.link_claim, Some(decoded));
        assert!(dispatched.payload.is_empty());
        let claim = |id: u64, k: u8| FfiObservedLinkClaim {
            message_id: id,
            author_key: vec![k; 32],
        };
        let roster = vec![vec![1; 32], vec![2; 32]];
        assert_eq!(
            link_claim_outcome(vec![1; 32], vec![claim(9, 1), claim(12, 2)], roster.clone())
                .unwrap(),
            FfiLinkClaimOutcome::Won { message_id: 9 }
        );
        assert_eq!(
            link_claim_outcome(vec![1; 32], vec![claim(9, 1), claim(4, 2)], roster.clone())
                .unwrap(),
            FfiLinkClaimOutcome::Lost {
                winner_message_id: 4
            }
        );
        // A removed device's earlier claim is passed over.
        assert_eq!(
            link_claim_outcome(vec![1; 32], vec![claim(3, 7), claim(9, 1)], roster.clone())
                .unwrap(),
            FfiLinkClaimOutcome::Won { message_id: 9 }
        );
        assert_eq!(
            link_claim_outcome(vec![1; 32], vec![], roster.clone()).unwrap(),
            FfiLinkClaimOutcome::NoWinner
        );
        assert!(link_claim_answered_elsewhere(vec![claim(4, 2)], roster.clone()).unwrap());
        assert!(!link_claim_answered_elsewhere(vec![claim(4, 7)], roster.clone()).unwrap());
        assert!(link_claim_outcome(vec![1; 31], vec![], roster).is_err());
        assert!(compose_link_claim_envelope(vec![1; 15], vec![2; 32]).is_err());
        assert!(decode_link_claim_envelope(b"nope".to_vec()).is_err());
    }
}
