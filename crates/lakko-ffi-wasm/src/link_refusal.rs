// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm surface for the link refusal (ADR-0079 amendment of 2026-10-19): the
//! inviter's answer to a contact request whose invite link does not count.
//! Mirror of `crates/lakko-ffi-uniffi/src/link_refusal.rs`.

use lakko_contacts::invite::LINK_ID_LEN;
use lakko_contacts::link_refusal::{
    compose_link_refusal as rust_compose, verify_link_refusal as rust_verify, LINK_REFUSAL_LEN,
    LINK_REFUSAL_REQUESTER_KEY_LEN,
};
use lakko_crypto::signing::SigningKey;
use wasm_bindgen::prelude::*;

fn fixed<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N], JsError> {
    bytes
        .try_into()
        .map_err(|_| JsError::new(&format!("bounds: {what} must be {N} bytes")))
}

/// Compose a refusal of the request `requesterAccountKey` sent through the
/// invite link `linkId`, signed by the inviter's account key
/// (`inviterSigningSeed`, the account identity's 32-byte Ed25519 seed).
///
/// # Errors
///
/// A `bounds:` error when an input has the wrong length.
#[wasm_bindgen(js_name = composeLinkRefusal)]
pub fn compose_link_refusal(
    inviter_signing_seed: &[u8],
    link_id: &[u8],
    requester_account_key: &[u8],
) -> Result<Vec<u8>, JsError> {
    let seed = fixed::<32>(inviter_signing_seed, "inviter seed")?;
    let link_id = fixed::<LINK_ID_LEN>(link_id, "link id")?;
    let requester =
        fixed::<LINK_REFUSAL_REQUESTER_KEY_LEN>(requester_account_key, "requester key")?;
    Ok(rust_compose(
        &SigningKey::from_seed(&seed),
        &link_id,
        &requester,
    ))
}

/// `true` when `wire` is a refusal of THIS account's request
/// (`ownAccountKey`) through the link `expectedLinkId`, signed by
/// `inviterKey` (the account key the invite link named). Anything else,
/// inputs of the wrong length included, is `false`.
#[wasm_bindgen(js_name = verifyLinkRefusal)]
#[must_use]
pub fn verify_link_refusal(
    wire: &[u8],
    inviter_key: &[u8],
    expected_link_id: &[u8],
    own_account_key: &[u8],
) -> bool {
    let (Ok(link_id), Ok(own)) = (
        <[u8; LINK_ID_LEN]>::try_from(expected_link_id),
        <[u8; LINK_REFUSAL_REQUESTER_KEY_LEN]>::try_from(own_account_key),
    ) else {
        return false;
    };
    rust_verify(wire, inviter_key, &link_id, &own).is_ok()
}

/// The one length of a refusal.
#[wasm_bindgen(js_name = linkRefusalLen)]
#[must_use]
pub fn link_refusal_len() -> u32 {
    u32::try_from(LINK_REFUSAL_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_composed_here_verifies_only_for_its_inviter() {
        let inviter = SigningKey::from_seed(&[7u8; 32]).verifying_key().to_bytes();
        let other = SigningKey::from_seed(&[8u8; 32]).verifying_key().to_bytes();
        let wire = compose_link_refusal(&[7; 32], &[1; 16], &[2; 32]).unwrap();
        assert_eq!(wire.len(), link_refusal_len() as usize);
        assert!(verify_link_refusal(&wire, &inviter, &[1; 16], &[2; 32]));
        assert!(!verify_link_refusal(&wire, &other, &[1; 16], &[2; 32]));
        assert!(!verify_link_refusal(&wire, &inviter, &[3; 16], &[2; 32]));
        assert!(!verify_link_refusal(&wire, &inviter, &[1; 16], &[4; 32]));
    }
}
