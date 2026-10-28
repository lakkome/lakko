// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the link refusal (ADR-0079 amendment of 2026-10-19):
//! the inviter's answer to a contact request whose invite link does not count.
//!
//! Mirror of `crates/lakko-ffi-wasm/src/link_refusal.rs` for iOS + Android.
//! The inviter composes it with its account identity's Ed25519 seed and sends
//! it as a sealed mailbox row of kind `LinkRefusal` through the request's
//! `reply` grant. The requester checks it with [`verify_link_refusal`] against
//! the account key the invite link named before it removes the pending
//! contact.

use lakko_contacts::invite::LINK_ID_LEN;
use lakko_contacts::link_refusal::{
    compose_link_refusal as rust_compose, verify_link_refusal as rust_verify, LINK_REFUSAL_LEN,
    LINK_REFUSAL_REQUESTER_KEY_LEN,
};
use lakko_crypto::signing::SigningKey;

use crate::FfiError;

fn fixed<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N], FfiError> {
    bytes
        .try_into()
        .map_err(|_| FfiError::Contacts(format!("link refusal: {what} must be {N} bytes")))
}

/// Compose a refusal of the request `requester_account_key` sent through the
/// invite link `link_id`, signed by the inviter's account key
/// (`inviter_signing_seed`, the account identity's 32-byte Ed25519 seed).
///
/// # Errors
///
/// [`FfiError::Contacts`] when an input has the wrong length.
#[uniffi::export]
pub fn compose_link_refusal(
    inviter_signing_seed: Vec<u8>,
    link_id: Vec<u8>,
    requester_account_key: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let seed = fixed::<32>(&inviter_signing_seed, "inviter seed")?;
    let link_id = fixed::<LINK_ID_LEN>(&link_id, "link id")?;
    let requester =
        fixed::<LINK_REFUSAL_REQUESTER_KEY_LEN>(&requester_account_key, "requester key")?;
    Ok(rust_compose(
        &SigningKey::from_seed(&seed),
        &link_id,
        &requester,
    ))
}

/// `true` when `wire` is a refusal of THIS account's request (`own_account_key`)
/// through the link `expected_link_id`, signed by `inviter_key` (the account
/// key the invite link named, kept with the pending contact). Anything else,
/// inputs of the wrong length included, is `false`: the caller changes
/// nothing.
#[uniffi::export]
#[must_use]
pub fn verify_link_refusal(
    wire: Vec<u8>,
    inviter_key: Vec<u8>,
    expected_link_id: Vec<u8>,
    own_account_key: Vec<u8>,
) -> bool {
    let (Ok(link_id), Ok(own)) = (
        <[u8; LINK_ID_LEN]>::try_from(expected_link_id.as_slice()),
        <[u8; LINK_REFUSAL_REQUESTER_KEY_LEN]>::try_from(own_account_key.as_slice()),
    ) else {
        return false;
    };
    rust_verify(&wire, &inviter_key, &link_id, &own).is_ok()
}

/// The one length of a refusal.
#[uniffi::export]
#[must_use]
pub const fn link_refusal_len() -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    {
        LINK_REFUSAL_LEN as u32
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_composed_here_verifies_only_for_its_inviter_link_and_requester() {
        let seed = vec![7u8; 32];
        let inviter = SigningKey::from_seed(&[7u8; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        let other = SigningKey::from_seed(&[8u8; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        let wire = compose_link_refusal(seed, vec![1; 16], vec![2; 32]).unwrap();
        assert_eq!(wire.len(), link_refusal_len() as usize);
        assert!(verify_link_refusal(
            wire.clone(),
            inviter.clone(),
            vec![1; 16],
            vec![2; 32]
        ));
        assert!(!verify_link_refusal(
            wire.clone(),
            other,
            vec![1; 16],
            vec![2; 32]
        ));
        assert!(!verify_link_refusal(
            wire.clone(),
            inviter.clone(),
            vec![3; 16],
            vec![2; 32]
        ));
        assert!(!verify_link_refusal(
            wire.clone(),
            inviter.clone(),
            vec![1; 16],
            vec![4; 32]
        ));
        assert!(!verify_link_refusal(
            wire,
            inviter,
            vec![1; 15],
            vec![2; 32]
        ));
        assert!(compose_link_refusal(vec![7; 31], vec![1; 16], vec![2; 32]).is_err());
    }
}
