// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the link-founded note (ADR-0079, ordering of
//! 2026-10-17): the device that won an invite-link request names the
//! one-to-one it founded, with the founding Welcome, in the account's own
//! DeviceGroup BEFORE delivering that Welcome to anyone.
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/link_founded.rs` for the web
//! client.
//!
//! # Errors
//!
//! Every function throws a string `"<code>: <reason>"`; the codes are the
//! frozen `LinkFoundedError::code` values plus `bounds` for a wrong-length
//! input.

use lakko_contacts::invite::LINK_ID_LEN;
use lakko_contacts::link_claim::LINK_CLAIM_REQUESTER_KEY_LEN;
use lakko_contacts::link_founded::{
    compose_link_founded as rust_compose, decode_link_founded as rust_decode,
    LinkFounded as RustLinkFounded, LINK_FOUNDED_ENVELOPE_DOMAIN_V1, LINK_FOUNDED_ENVELOPE_MAX_LEN,
    LINK_FOUNDED_ENVELOPE_MIN_LEN, LINK_FOUNDED_GROUP_ID_LEN,
};
use wasm_bindgen::prelude::*;

// A note must fit the DeviceGroup `serviceEnvelope` carrier.
const _: () = assert!(LINK_FOUNDED_ENVELOPE_MAX_LEN <= lakko_mls::MAX_SERVICE_ENVELOPE_LEN);

/// A decoded note: the request, the founded one-to-one and its Welcome.
#[wasm_bindgen(js_name = LinkFounded)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmLinkFounded {
    inner: RustLinkFounded,
}

impl From<RustLinkFounded> for WasmLinkFounded {
    fn from(inner: RustLinkFounded) -> Self {
        Self { inner }
    }
}

#[wasm_bindgen(js_class = LinkFounded)]
impl WasmLinkFounded {
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
    /// `linkId ‖ requesterAccountKey` (48 bytes).
    #[wasm_bindgen(getter, js_name = requestKey)]
    pub fn request_key(&self) -> Vec<u8> {
        self.inner.request_key().to_vec()
    }
    /// The founded one-to-one's 32-byte group id.
    #[wasm_bindgen(getter, js_name = groupId)]
    pub fn group_id(&self) -> Vec<u8> {
        self.inner.group_id.to_vec()
    }
    /// The founding Welcome (wire bytes).
    #[wasm_bindgen(getter)]
    pub fn welcome(&self) -> Vec<u8> {
        self.inner.welcome.clone()
    }
}

/// Compose a note: the bytes to hand to
/// `DeviceGroup.createServiceEnvelopeMessage`.
///
/// # Errors
///
/// `"bounds: ..."` for a wrong-length id or key; `"welcomeLength: ..."` for
/// an empty or oversized Welcome.
#[wasm_bindgen(js_name = composeLinkFoundedEnvelope)]
pub fn compose_link_founded_envelope(
    link_id: &[u8],
    requester_account_key: &[u8],
    group_id: &[u8],
    welcome: &[u8],
) -> Result<Vec<u8>, String> {
    let link_id: [u8; LINK_ID_LEN] = link_id
        .try_into()
        .map_err(|_| "bounds: link id must be 16 bytes".to_string())?;
    let requester_account_key: [u8; LINK_CLAIM_REQUESTER_KEY_LEN] = requester_account_key
        .try_into()
        .map_err(|_| "bounds: requester key must be 32 bytes".to_string())?;
    let group_id: [u8; LINK_FOUNDED_GROUP_ID_LEN] = group_id
        .try_into()
        .map_err(|_| "bounds: group id must be 32 bytes".to_string())?;
    rust_compose(&RustLinkFounded {
        link_id,
        requester_account_key,
        group_id,
        welcome: welcome.to_vec(),
    })
    .map_err(|e| format!("{}: {e}", e.code()))
}

/// Strictly decode a note. Fail-closed.
///
/// # Errors
///
/// `"<code>: <reason>"` with the frozen reason code.
#[wasm_bindgen(js_name = decodeLinkFoundedEnvelope)]
pub fn decode_link_founded_envelope(plaintext: &[u8]) -> Result<WasmLinkFounded, String> {
    rust_decode(plaintext)
        .map(Into::into)
        .map_err(|e| format!("{}: {e}", e.code()))
}

/// `true` when `bytes` open with the note's domain.
#[wasm_bindgen(js_name = isLinkFoundedEnvelope)]
#[must_use]
pub fn is_link_founded_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(LINK_FOUNDED_ENVELOPE_DOMAIN_V1)
}

/// The shortest well-formed note.
#[wasm_bindgen(js_name = linkFoundedEnvelopeMinLen)]
#[must_use]
pub fn link_founded_envelope_min_len() -> u32 {
    u32::try_from(LINK_FOUNDED_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// The longest well-formed note.
#[wasm_bindgen(js_name = linkFoundedEnvelopeMaxLen)]
#[must_use]
pub fn link_founded_envelope_max_len() -> u32 {
    u32::try_from(LINK_FOUNDED_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_and_decode_agree() {
        let wire = compose_link_founded_envelope(&[1; 16], &[2; 32], &[3; 32], &[4; 9]).unwrap();
        assert!(is_link_founded_envelope(&wire));
        let decoded = decode_link_founded_envelope(&wire).unwrap();
        assert_eq!(decoded.group_id(), vec![3; 32]);
        assert_eq!(decoded.welcome(), vec![4; 9]);
        assert_eq!(decoded.request_key()[..16], [1; 16]);
        assert!(compose_link_founded_envelope(&[1; 16], &[2; 32], &[3; 32], &[]).is_err());
        assert!(compose_link_founded_envelope(&[1; 16], &[2; 32], &[3; 31], &[4]).is_err());
        assert!(decode_link_founded_envelope(b"nope").is_err());
        assert!(link_founded_envelope_min_len() < link_founded_envelope_max_len());
    }
}
