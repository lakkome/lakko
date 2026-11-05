// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for a contact request carried inside a group (ADR-0086
//! section 6, UX `docs/ux/who-can-find-me.md` 6.4).
//!
//! Mirror of `crates/lakko-ffi-wasm/src/group_contact_request.rs` for iOS +
//! Android. The envelope names nobody: the sender composes the ordinary
//! contact request (`composeContactRequest`) and seals it whole to the
//! target's X25519 key with [`compose_group_contact_request_envelope`], then
//! sends the result as the plaintext of an application message in a group
//! both people are in. Every member meets it as the `GroupContactRequest`
//! dispatch kind (the whole envelope in `payload`) and runs
//! [`check_group_contact_request`] with its own X25519 secret and the account
//! key the MLS sender resolves to at receipt (C-68). `None` means it is not
//! for this member: drop it and log nothing that names anyone. The member it
//! opens for applies its "Who can find me" setting (`allowsScope` for this
//! group, or the sender is a contact) and hands the returned request wire to
//! the ordinary incoming-request flow (`ContactRequest.decodeAndVerify`,
//! open, accept).

use lakko_contacts::group_contact_request::{
    check_group_contact_request as rust_check, decode_group_contact_request as rust_decode,
    seal_group_contact_request as rust_seal, GroupContactRequestError,
    GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1, GROUP_CONTACT_REQUEST_ENVELOPE_MAX_LEN,
    GROUP_CONTACT_REQUEST_ENVELOPE_MIN_LEN,
};
use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};

use crate::FfiError;

fn wire_error(e: GroupContactRequestError) -> FfiError {
    FfiError::Contacts(format!("group contact request {}: {e}", e.code()))
}

fn key32(bytes: Vec<u8>, what: &str) -> Result<[u8; 32], FfiError> {
    bytes.try_into().map_err(|_| {
        FfiError::Contacts(format!(
            "group contact request bounds: {what} must be 32 bytes"
        ))
    })
}

/// Seal an ordinary contact request (`request`, the bytes
/// `composeContactRequest` returned) to the target's X25519 public key —
/// the same key passed as `recipientX25519Pub` to `composeContactRequest` —
/// and frame it: the plaintext of an application message in a group both
/// people are in. The result names nobody.
///
/// # Errors
///
/// [`FfiError::Contacts`] for a key that is not 32 bytes, or a request that
/// is empty or longer than the bound.
#[uniffi::export]
pub fn compose_group_contact_request_envelope(
    recipient_x25519_pub: Vec<u8>,
    request: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let pk = X25519PublicKey::from_bytes(&key32(recipient_x25519_pub, "recipientX25519Pub")?)
        .map_err(|_| {
            FfiError::Contacts("group contact request bounds: recipientX25519Pub".to_string())
        })?;
    rust_seal(&pk, &request).map_err(wire_error)
}

/// The receiver's rule. `plaintext` is the envelope (the dispatch payload),
/// `recipient_x25519_priv` this member's X25519 secret (as
/// `ContactRequest.openEnvelope` takes it), `sender_account_key` the
/// account key the carrying message's MLS sender resolves to at receipt.
///
/// Returns `None` when the box does not open with this key: not for this
/// member (or damaged; the two read alike). Drop it silently. Returns the
/// verified request wire when it is for this member and from its poster.
///
/// # Errors
///
/// [`FfiError::Contacts`] with the code `requestInvalid` (the opened
/// request does not verify), `senderMismatch` (it names another sender than
/// the member who posted it), a layout code, or `bounds`; every one means
/// drop.
#[uniffi::export]
pub fn check_group_contact_request(
    plaintext: Vec<u8>,
    recipient_x25519_priv: Vec<u8>,
    sender_account_key: Vec<u8>,
) -> Result<Option<Vec<u8>>, FfiError> {
    let own = X25519SecretKey::from_bytes(key32(recipient_x25519_priv, "recipientX25519Priv")?);
    let sender = key32(sender_account_key, "senderAccountKey")?;
    let envelope = rust_decode(&plaintext).map_err(wire_error)?;
    match rust_check(&envelope, &own, &sender) {
        Ok((_, wire)) => Ok(Some(wire)),
        Err(GroupContactRequestError::NotForMe) => Ok(None),
        Err(e) => Err(wire_error(e)),
    }
}

/// `true` when `bytes` open with the envelope's domain: the cheap routing
/// question.
#[uniffi::export]
#[must_use]
pub fn is_group_contact_request_envelope(bytes: Vec<u8>) -> bool {
    bytes.starts_with(GROUP_CONTACT_REQUEST_ENVELOPE_DOMAIN_V1)
}

/// The shortest well-formed envelope.
#[uniffi::export]
#[must_use]
pub fn group_contact_request_envelope_min_len() -> u32 {
    u32::try_from(GROUP_CONTACT_REQUEST_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// The longest well-formed envelope.
#[uniffi::export]
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
    fn compose_check_and_dispatch_agree() {
        let alice = SigningKey::from_seed(&[7; 32]);
        let alice_key = alice.verifying_key().to_bytes().to_vec();
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
        let wire = compose_group_contact_request_envelope(
            target.public_key().to_bytes().to_vec(),
            req.clone(),
        )
        .unwrap();
        assert!(is_group_contact_request_envelope(wire.clone()));
        assert_eq!(
            check_group_contact_request(wire.clone(), vec![0xCD; 32], alice_key.clone()).unwrap(),
            Some(req)
        );
        // Another member: not for me, not an error.
        assert_eq!(
            check_group_contact_request(wire.clone(), vec![0xCE; 32], alice_key.clone()).unwrap(),
            None
        );
        assert!(check_group_contact_request(wire.clone(), vec![0xCD; 32], vec![9; 32]).is_err());
        assert!(check_group_contact_request(wire.clone(), vec![0xCD; 31], alice_key).is_err());
        let dispatched = crate::contacts::dispatch_incoming_envelope(wire.clone()).unwrap();
        assert_eq!(
            dispatched.kind,
            crate::contacts::FfiIncomingEnvelopeKind::GroupContactRequest
        );
        assert_eq!(dispatched.payload, wire);
        assert!(compose_group_contact_request_envelope(vec![4; 31], vec![1]).is_err());
        assert!(compose_group_contact_request_envelope(vec![4; 32], vec![]).is_err());
        assert_eq!(group_contact_request_envelope_min_len(), 85);
    }
}
