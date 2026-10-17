// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the link-request claim (ADR-0079, reworked
//! 2026-10-17): one device of the inviter's account claims a contact request
//! sent through one of the account's invite links, so that only one device
//! answers it.
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/link_claim.rs` for the web client.
//! The claim is sent ONLY in the account's DeviceGroup (as a
//! `serviceEnvelope` message). A receiver routes it with `isLinkClaimEnvelope`,
//! decodes it with `decodeLinkClaimEnvelope`, honours it only from a
//! resolvable DeviceGroup member (C-68), keeps each claim's DS message id
//! with its author's leaf key, and decides with `linkClaimOutcome`: the
//! earliest claim from a device in the current roster wins, and the device
//! whose leaf posted it builds.
//!
//! # Errors
//!
//! Every function throws a string `"<code>: <reason>"`; the codes are the
//! frozen `LinkClaimError::code` values plus `bounds` for a wrong-length
//! input.

use lakko_contacts::invite::LINK_ID_LEN;
use lakko_contacts::link_claim::{
    compose_link_claim as rust_compose, decode_link_claim as rust_decode,
    link_claim_answered_elsewhere as rust_answered, link_claim_outcome as rust_outcome,
    LinkClaim as RustLinkClaim, LinkClaimOutcome, ObservedLinkClaim, LINK_CLAIM_AUTHOR_KEY_LEN,
    LINK_CLAIM_ENVELOPE_DOMAIN_V1, LINK_CLAIM_ENVELOPE_LEN, LINK_CLAIM_REQUESTER_KEY_LEN,
};
use wasm_bindgen::prelude::*;

// A claim must fit the DeviceGroup `serviceEnvelope` carrier.
const _: () = assert!(LINK_CLAIM_ENVELOPE_LEN <= lakko_mls::MAX_SERVICE_ENVELOPE_LEN);

fn claim_of(link_id: &[u8], requester_key: &[u8]) -> Result<RustLinkClaim, String> {
    let link_id: [u8; LINK_ID_LEN] = link_id
        .try_into()
        .map_err(|_| "bounds: link id must be 16 bytes".to_string())?;
    let requester_account_key: [u8; LINK_CLAIM_REQUESTER_KEY_LEN] = requester_key
        .try_into()
        .map_err(|_| "bounds: requester key must be 32 bytes".to_string())?;
    Ok(RustLinkClaim {
        link_id,
        requester_account_key,
    })
}

/// A decoded claim: the request it names.
#[wasm_bindgen(js_name = LinkClaim)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WasmLinkClaim {
    inner: RustLinkClaim,
}

impl From<RustLinkClaim> for WasmLinkClaim {
    fn from(inner: RustLinkClaim) -> Self {
        Self { inner }
    }
}

#[wasm_bindgen(js_class = LinkClaim)]
impl WasmLinkClaim {
    /// The invite link's 16-byte id.
    #[wasm_bindgen(getter, js_name = linkId)]
    pub fn link_id(&self) -> Vec<u8> {
        self.inner.link_id.to_vec()
    }
    /// The requester's 32-byte account key.
    #[wasm_bindgen(getter, js_name = requesterAccountKey)]
    pub fn requester_account_key(&self) -> Vec<u8> {
        self.inner.requester_account_key.to_vec()
    }
    /// `linkId ‖ requesterAccountKey` (48 bytes): the key every client files
    /// its claim bookkeeping under.
    #[wasm_bindgen(getter, js_name = requestKey)]
    pub fn request_key(&self) -> Vec<u8> {
        self.inner.request_key().to_vec()
    }
}

/// Compose a claim envelope: the bytes to hand to
/// `DeviceGroup.createServiceEnvelopeMessage`.
///
/// # Errors
///
/// `"bounds: ..."` on a wrong-length input.
#[wasm_bindgen(js_name = composeLinkClaimEnvelope)]
pub fn compose_link_claim_envelope(
    link_id: &[u8],
    requester_account_key: &[u8],
) -> Result<Vec<u8>, String> {
    Ok(rust_compose(&claim_of(link_id, requester_account_key)?))
}

/// Strictly decode a claim envelope. Fail-closed.
///
/// # Errors
///
/// `"<code>: <reason>"` with the frozen reason code.
#[wasm_bindgen(js_name = decodeLinkClaimEnvelope)]
pub fn decode_link_claim_envelope(plaintext: &[u8]) -> Result<WasmLinkClaim, String> {
    rust_decode(plaintext)
        .map(Into::into)
        .map_err(|e| format!("{}: {e}", e.code()))
}

/// `true` when `bytes` open with the claim domain: the cheap routing question
/// a client asks of a DeviceGroup service envelope.
#[wasm_bindgen(js_name = isLinkClaimEnvelope)]
#[must_use]
pub fn is_link_claim_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(LINK_CLAIM_ENVELOPE_DOMAIN_V1)
}

/// The request key `linkId ‖ requesterAccountKey` (48 bytes).
///
/// # Errors
///
/// `"bounds: ..."` on a wrong-length input.
#[wasm_bindgen(js_name = linkClaimRequestKey)]
pub fn link_claim_request_key(
    link_id: &[u8],
    requester_account_key: &[u8],
) -> Result<Vec<u8>, String> {
    Ok(claim_of(link_id, requester_account_key)?
        .request_key()
        .to_vec())
}

fn keys_of(flat: &[u8], what: &str) -> Result<Vec<[u8; LINK_CLAIM_AUTHOR_KEY_LEN]>, String> {
    if flat.len() % LINK_CLAIM_AUTHOR_KEY_LEN != 0 {
        return Err(format!("bounds: {what} must be whole 32-byte keys"));
    }
    flat.chunks_exact(LINK_CLAIM_AUTHOR_KEY_LEN)
        .map(|c| {
            c.try_into()
                .map_err(|_| format!("bounds: {what} must be whole 32-byte keys"))
        })
        .collect()
}

fn claims_of(ids: &[u64], authors: &[u8]) -> Result<Vec<ObservedLinkClaim>, String> {
    let authors = keys_of(authors, "claim authors")?;
    if authors.len() != ids.len() {
        return Err("bounds: one author per claim".to_string());
    }
    Ok(ids
        .iter()
        .zip(authors)
        .map(|(id, author_key)| ObservedLinkClaim {
            message_id: *id,
            author_key,
        })
        .collect())
}

/// How a request's claims end, for one device.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkClaimDecision {
    kind: u8,
    message_id: u64,
}

#[wasm_bindgen]
impl LinkClaimDecision {
    /// This device's claim wins: it answers the request.
    #[wasm_bindgen(getter)]
    pub fn won(&self) -> bool {
        self.kind == 1
    }
    /// Another device's claim wins: build nothing.
    #[wasm_bindgen(getter)]
    pub fn lost(&self) -> bool {
        self.kind == 2
    }
    /// No claim from a device in the current roster: post (again) later.
    #[wasm_bindgen(getter, js_name = noWinner)]
    pub fn no_winner(&self) -> bool {
        self.kind == 0
    }
    /// The DS message id of the winning claim; `0n` when there is none.
    #[wasm_bindgen(getter, js_name = messageId)]
    pub fn message_id(&self) -> u64 {
        self.message_id
    }
}

/// How the claims for one request end for the device whose DeviceGroup leaf
/// key is `ownKey`. `claimIds[i]` was posted by the device whose leaf key is
/// `claimAuthors[32*i..32*i+32]` (this device's own claims carry `ownKey`);
/// `rosterKeys` is the DeviceGroup's current leaf keys, concatenated. The
/// earliest claim from a roster device wins, and its author builds.
///
/// # Errors
///
/// `"bounds: ..."` when a key is not 32 bytes or the lists do not match.
#[wasm_bindgen(js_name = linkClaimOutcome)]
pub fn link_claim_outcome(
    own_key: &[u8],
    claim_ids: &[u64],
    claim_authors: &[u8],
    roster_keys: &[u8],
) -> Result<LinkClaimDecision, String> {
    let own: [u8; LINK_CLAIM_AUTHOR_KEY_LEN] = own_key
        .try_into()
        .map_err(|_| "bounds: own key must be 32 bytes".to_string())?;
    let claims = claims_of(claim_ids, claim_authors)?;
    let roster = keys_of(roster_keys, "roster keys")?;
    Ok(match rust_outcome(&own, &claims, &roster) {
        LinkClaimOutcome::Won { message_id } => LinkClaimDecision {
            kind: 1,
            message_id,
        },
        LinkClaimOutcome::Lost { winner_message_id } => LinkClaimDecision {
            kind: 2,
            message_id: winner_message_id,
        },
        LinkClaimOutcome::NoWinner => LinkClaimDecision {
            kind: 0,
            message_id: 0,
        },
    })
}

/// Whether a device that holds NO claim of its own treats the request as
/// answered: some device in the current roster claimed it.
///
/// # Errors
///
/// `"bounds: ..."` when a key is not 32 bytes or the lists do not match.
#[wasm_bindgen(js_name = linkClaimAnsweredElsewhere)]
pub fn link_claim_answered_elsewhere(
    claim_ids: &[u64],
    claim_authors: &[u8],
    roster_keys: &[u8],
) -> Result<bool, String> {
    Ok(rust_answered(
        &claims_of(claim_ids, claim_authors)?,
        &keys_of(roster_keys, "roster keys")?,
    ))
}

/// The one length of a claim envelope.
#[wasm_bindgen(js_name = linkClaimEnvelopeLen)]
#[must_use]
pub fn link_claim_envelope_len() -> u32 {
    u32::try_from(LINK_CLAIM_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_decode_and_the_outcome_agree() {
        let wire = compose_link_claim_envelope(&[1; 16], &[2; 32]).unwrap();
        assert!(is_link_claim_envelope(&wire));
        assert_eq!(wire.len(), link_claim_envelope_len() as usize);
        let decoded = decode_link_claim_envelope(&wire).unwrap();
        assert_eq!(decoded.link_id(), vec![1; 16]);
        assert_eq!(decoded.requester_account_key(), vec![2; 32]);
        assert_eq!(
            decoded.request_key(),
            link_claim_request_key(&[1; 16], &[2; 32]).unwrap()
        );
        let roster = [[1u8; 32], [2; 32]].concat();
        let won = link_claim_outcome(&[1; 32], &[9, 12], &[[1u8; 32], [2; 32]].concat(), &roster)
            .unwrap();
        assert!(won.won() && won.message_id() == 9);
        let lost =
            link_claim_outcome(&[1; 32], &[9, 4], &[[1u8; 32], [2; 32]].concat(), &roster).unwrap();
        assert!(lost.lost() && lost.message_id() == 4);
        let removed =
            link_claim_outcome(&[1; 32], &[3, 9], &[[7u8; 32], [1; 32]].concat(), &roster).unwrap();
        assert!(removed.won() && removed.message_id() == 9);
        assert!(link_claim_outcome(&[1; 32], &[], &[], &roster)
            .unwrap()
            .no_winner());
        assert!(link_claim_answered_elsewhere(&[4], &[2; 32], &roster).unwrap());
        assert!(!link_claim_answered_elsewhere(&[4], &[7; 32], &roster).unwrap());
        assert!(link_claim_outcome(&[1; 32], &[4], &[], &roster).is_err());
        assert!(compose_link_claim_envelope(&[1; 15], &[2; 32]).is_err());
        assert!(decode_link_claim_envelope(b"nope").is_err());
    }
}
