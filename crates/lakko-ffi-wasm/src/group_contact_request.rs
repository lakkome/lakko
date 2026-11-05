// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for a contact request carried inside a group
//! (ADR-0086 section 6, UX `docs/ux/who-can-find-me.md` 6.4).
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/group_contact_request.rs` for the
//! web client. The envelope names nobody: the sender seals the ordinary
//! contact request whole to the target's X25519 key with
//! [`compose_group_contact_request_envelope`] and sends it as an application
//! message in a group both people are in. Every member meets it as the
//! `GroupContactRequest` dispatch kind (the whole envelope in `payload`) and
//! runs [`check_group_contact_request`]; `undefined` means not for this
//! member (drop silently, log nothing that names anyone).
//!
//! # Errors
//!
//! Every function throws a string `"<code>: <reason>"`; the codes are the
//! frozen `GroupContactRequestError::code` values plus `bounds` for a
//! wrong-length key.

use lakko_contacts::group_contact_request::{
    check_group_contact_request as rust_check, decode_group_contact_request as rust_decode,
    seal_group_contact_request as rust_seal, GroupContactRequestError,
    GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1, GROUP_CONTACT_REQUEST_ENVELOPE_MAX_LEN,
    GROUP_CONTACT_REQUEST_ENVELOPE_MIN_LEN,
};
use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use wasm_bindgen::prelude::*;

fn key32(bytes: &[u8], what: &str) -> Result<[u8; 32], String> {
    bytes
        .try_into()
        .map_err(|_| format!("bounds: {what} must be 32 bytes"))
}

fn code(e: GroupContactRequestError) -> String {
    format!("{}: {e}", e.code())
}

/// Seal an ordinary contact request (the bytes `composeContactRequest`
/// returned) to the target's X25519 public key — the key passed as
/// `recipientX25519Pub` to `composeContactRequest` — and frame it. The
/// result names nobody.
///
/// # Errors
///
/// `"bounds: ..."` for a key that is not 32 bytes; `"requestLength: ..."`
/// for an empty or oversized request.
#[wasm_bindgen(js_name = composeGroupContactRequestEnvelope)]
pub fn compose_group_contact_request_envelope(
    recipient_x25519_pub: &[u8],
    request: &[u8],
) -> Result<Vec<u8>, String> {
    let pk = X25519PublicKey::from_bytes(&key32(recipient_x25519_pub, "recipientX25519Pub")?)
        .map_err(|_| "bounds: recipientX25519Pub".to_string())?;
    rust_seal(&pk, request).map_err(code)
}

/// The receiver's rule. `plaintext` is the envelope (the dispatch payload),
/// `recipientX25519Priv` this member's X25519 secret (as the contact
/// request's open takes it), `senderAccountKey` the account key the carrying
/// message's MLS sender resolves to at receipt.
///
/// Returns `undefined` when the box does not open with this key (not for
/// this member, or damaged): drop it silently. Otherwise the verified
/// request wire.
///
/// # Errors
///
/// `"requestInvalid: ..."`, `"senderMismatch: ..."`, a layout code, or
/// `"bounds: ..."`; every one means drop.
#[wasm_bindgen(js_name = checkGroupContactRequest)]
pub fn check_group_contact_request(
    plaintext: &[u8],
    recipient_x25519_priv: &[u8],
    sender_account_key: &[u8],
) -> Result<Option<Vec<u8>>, String> {
    let own = X25519SecretKey::from_bytes(key32(recipient_x25519_priv, "recipientX25519Priv")?);
    let sender = key32(sender_account_key, "senderAccountKey")?;
    let envelope = rust_decode(plaintext).map_err(code)?;
    match rust_check(&envelope, &own, &sender) {
        Ok((_, wire)) => Ok(Some(wire)),
        Err(GroupContactRequestError::NotForMe) => Ok(None),
        Err(e) => Err(code(e)),
    }
}

/// `true` when `bytes` open with the envelope's domain.
#[wasm_bindgen(js_name = isGroupContactRequestEnvelope)]
#[must_use]
pub fn is_group_contact_request_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1)
}

/// The shortest well-formed envelope.
#[wasm_bindgen(js_name = groupContactRequestEnvelopeMinLen)]
#[must_use]
pub fn group_contact_request_envelope_min_len() -> u32 {
    u32::try_from(GROUP_CONTACT_REQUEST_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// The longest well-formed envelope.
#[wasm_bindgen(js_name = groupContactRequestEnvelopeMaxLen)]
#[must_use]
pub fn group_contact_request_envelope_max_len() -> u32 {
    u32::try_from(GROUP_CONTACT_REQUEST_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::request::{compose, ACCOUNT_ID_LEN};
    use lakko_crypto::signing::SigningKey;

    #[test]
    fn compose_and_check_agree() {
        let alice = SigningKey::from_seed(&[7; 32]);
        let alice_key = alice.verifying_key().to_bytes();
        let target = X25519SecretKey::from_bytes([0xCD; 32]);
        let req = compose(
            &alice,
            &[0x77; ACCOUNT_ID_LEN],
            &target.public_key(),
            [0xAA; lakko_contacts::pseudonym::PSEUDONYM_LEN],
            1_000_000,
            b"",
            None,
            None,
            None,
            b"",
        )
        .unwrap();
        let wire =
            compose_group_contact_request_envelope(&target.public_key().to_bytes(), &req).unwrap();
        assert!(is_group_contact_request_envelope(&wire));
        assert_eq!(
            check_group_contact_request(&wire, &[0xCD; 32], &alice_key).unwrap(),
            Some(req)
        );
        assert_eq!(
            check_group_contact_request(&wire, &[0xCE; 32], &alice_key).unwrap(),
            None
        );
        assert!(check_group_contact_request(&wire, &[0xCD; 32], &[9; 32])
            .unwrap_err()
            .starts_with("senderMismatch:"));
        assert!(compose_group_contact_request_envelope(&[4; 31], &[1])
            .unwrap_err()
            .starts_with("bounds:"));
        assert_eq!(group_contact_request_envelope_min_len(), 85);
    }
}
