// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for sealed-sender mailboxes ([ADR-0078] section 1).
//! Mirror of `crates/lakko-ffi-uniffi/src/mailbox.rs`.
//!
//! A personal mailbox is named after a **mailbox key** every device of the
//! account derives from the account seed - not the identity key - and every
//! row in it is sealed to that key and padded to a size bucket:
//!
//! - `MailboxKey.fromAccountIdentity(identity)` - the account's own mailbox
//!   key: its public key and address (claim with `dsMailboxSigner`), and
//!   `openMailboxRow` for every row read back.
//! - `signMailboxBinding(identity)` - the 97-byte binding the account hands
//!   out with every mailbox key (it rides inside every reach grant already).
//! - `verifyMailboxBinding(binding, accountPk)` - what a **writer** runs,
//!   against the recipient's pinned account key, before it writes anything;
//!   a binding that does not verify means "do not write".
//! - `sealMailboxRow` / `sealMailboxRowToBinding` - seal a contact request,
//!   ack, Welcome or Welcome rendezvous as one row. The second verifies the
//!   binding first; prefer it.
//!
//! A refusal throws an `Error` whose `mailboxError` own property is the
//! frozen code (`badLength`, `unknownVersion`, `badKey`, `badBinding`,
//! `bodyTooLong`, `notOpened`, `unknownKind`, `badPadding`, `keyLength`).
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use lakko_contacts::mailbox::{
    self as core, MailboxBinding, MailboxError, MAILBOX_BINDING_LEN, MAILBOX_MAX_BODY_LEN,
};
use wasm_bindgen::prelude::*;

use crate::contacts::coded_error;
use crate::mls::Identity;

/// A refusal on its way to JS: the frozen code and a sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refusal {
    code: &'static str,
    message: String,
}

impl From<MailboxError> for Refusal {
    fn from(e: MailboxError) -> Self {
        Self {
            code: e.code(),
            message: e.to_string(),
        }
    }
}

fn js<T>(r: Result<T, Refusal>) -> Result<T, JsValue> {
    r.map_err(|e| coded_error(&e.message, "mailboxError", e.code))
}

fn key32(bytes: &[u8], what: &str) -> Result<[u8; 32], Refusal> {
    <[u8; 32]>::try_from(bytes).map_err(|_| Refusal {
        code: "keyLength",
        message: format!("{what} must be 32 bytes, got {}", bytes.len()),
    })
}

/// What a mailbox row carries (ADR-0078). The numeric values are the frozen
/// wire bytes.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailboxKind {
    /// A signed `ContactRequest`.
    ContactRequest = 1,
    /// An MLS Welcome.
    Welcome = 2,
    /// A signed `ContactAck`.
    ContactAck = 3,
    /// A Welcome rendezvous (the channel co-publisher join).
    WelcomeRendezvous = 4,
    /// A signed link refusal (ADR-0079, 2026-10-19).
    LinkRefusal = 5,
}

impl From<MailboxKind> for core::MailboxKind {
    fn from(k: MailboxKind) -> Self {
        match k {
            MailboxKind::ContactRequest => Self::ContactRequest,
            MailboxKind::Welcome => Self::Welcome,
            MailboxKind::ContactAck => Self::ContactAck,
            MailboxKind::WelcomeRendezvous => Self::WelcomeRendezvous,
            MailboxKind::LinkRefusal => Self::LinkRefusal,
        }
    }
}

impl From<core::MailboxKind> for MailboxKind {
    fn from(k: core::MailboxKind) -> Self {
        match k {
            core::MailboxKind::ContactRequest => Self::ContactRequest,
            core::MailboxKind::Welcome => Self::Welcome,
            core::MailboxKind::ContactAck => Self::ContactAck,
            core::MailboxKind::WelcomeRendezvous => Self::WelcomeRendezvous,
            core::MailboxKind::LinkRefusal => Self::LinkRefusal,
        }
    }
}

/// An opened mailbox row.
#[wasm_bindgen]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedMailboxRow {
    kind: MailboxKind,
    body: Vec<u8>,
}

#[wasm_bindgen]
impl OpenedMailboxRow {
    /// What the row carries.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kind(&self) -> MailboxKind {
        self.kind
    }

    /// The inner wire (a request, an ack, a Welcome, a rendezvous),
    /// unchanged.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn body(&self) -> Vec<u8> {
        self.body.clone()
    }
}

/// The account's own mailbox key. Holds the secret; never leaves the device.
#[wasm_bindgen]
pub struct MailboxKey {
    inner: core::MailboxKey,
}

#[wasm_bindgen]
impl MailboxKey {
    /// The mailbox key of the account whose identity is `accountIdentity`
    /// (derived from its 32-byte account seed; every device derives the
    /// same).
    #[wasm_bindgen(js_name = fromAccountIdentity)]
    #[must_use]
    pub fn from_account_identity(account_identity: &Identity) -> MailboxKey {
        Self {
            inner: core::MailboxKey::from_account_seed(
                &account_identity.mls_identity().ed25519_seed_bytes(),
            ),
        }
    }

    /// The 32-byte Ed25519 public key the claim registers.
    #[wasm_bindgen(js_name = publicKey)]
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public().to_vec()
    }

    /// The 32-byte address the mailbox is stored under on the Delivery
    /// Service (`/ds/mailbox/{hex}/...`).
    #[must_use]
    pub fn address(&self) -> Vec<u8> {
        self.inner.address().to_vec()
    }
}

fn mailbox_address_inner(mailbox_pk: &[u8]) -> Result<Vec<u8>, Refusal> {
    Ok(core::mailbox_address(&key32(mailbox_pk, "mailbox key")?).to_vec())
}

/// The 32-byte address of the mailbox named by the 32-byte `mailboxPk`.
///
/// # Errors
///
/// Throws with `mailboxError` `keyLength`.
#[wasm_bindgen(js_name = mailboxAddress)]
pub fn mailbox_address(mailbox_pk: &[u8]) -> Result<Vec<u8>, JsValue> {
    js(mailbox_address_inner(mailbox_pk))
}

/// The 97-byte mailbox binding of the account whose identity is
/// `accountIdentity`: its mailbox key signed by its identity key.
/// Deterministic, so every device produces the same bytes.
#[wasm_bindgen(js_name = signMailboxBinding)]
#[must_use]
pub fn sign_mailbox_binding(account_identity: &Identity) -> Vec<u8> {
    MailboxBinding::for_account_seed(&account_identity.mls_identity().ed25519_seed_bytes())
        .encode()
        .to_vec()
}

fn verify_mailbox_binding_inner(binding: &[u8], account_pk: &[u8]) -> Result<[u8; 32], Refusal> {
    let account = key32(account_pk, "account key")?;
    let binding = MailboxBinding::decode(binding)?;
    Ok(binding.verify(&account)?)
}

/// Verify a 97-byte binding against the recipient's 32-byte account key as
/// the writer holds it pinned, and return the 32-byte mailbox key it names.
/// A writer runs this before it seals or writes anything; on a throw it
/// writes nothing.
///
/// # Errors
///
/// Throws with `mailboxError` `badLength` / `unknownVersion`, `badBinding`,
/// `badKey`, `keyLength`.
#[wasm_bindgen(js_name = verifyMailboxBinding)]
pub fn verify_mailbox_binding(binding: &[u8], account_pk: &[u8]) -> Result<Vec<u8>, JsValue> {
    js(verify_mailbox_binding_inner(binding, account_pk).map(|pk| pk.to_vec()))
}

fn seal_mailbox_row_inner(
    mailbox_pk: &[u8],
    kind: MailboxKind,
    body: &[u8],
) -> Result<Vec<u8>, Refusal> {
    let pk = key32(mailbox_pk, "mailbox key")?;
    Ok(core::seal_mailbox_row(&pk, kind.into(), body)?)
}

/// Seal `body` as one mailbox row of `kind` to the 32-byte `mailboxPk`,
/// which must come from `verifyMailboxBinding` (or
/// `ReachGrant.mailboxPkVerified`). Post the row to
/// `/ds/mailbox/{mailboxAddress(mailboxPk)}/messages`.
///
/// # Errors
///
/// Throws with `mailboxError` `keyLength`, `badKey`, `bodyTooLong`.
#[wasm_bindgen(js_name = sealMailboxRow)]
pub fn seal_mailbox_row(
    mailbox_pk: &[u8],
    kind: MailboxKind,
    body: &[u8],
) -> Result<Vec<u8>, JsValue> {
    js(seal_mailbox_row_inner(mailbox_pk, kind, body))
}

/// `verifyMailboxBinding` then `sealMailboxRow` in one call: the form a
/// writer should use, since it cannot seal without the check.
///
/// # Errors
///
/// Every throw of the two.
#[wasm_bindgen(js_name = sealMailboxRowToBinding)]
pub fn seal_mailbox_row_to_binding(
    binding: &[u8],
    account_pk: &[u8],
    kind: MailboxKind,
    body: &[u8],
) -> Result<Vec<u8>, JsValue> {
    js(verify_mailbox_binding_inner(binding, account_pk)
        .and_then(|pk| seal_mailbox_row_inner(&pk, kind, body)))
}

fn open_mailbox_row_inner(key: &MailboxKey, row: &[u8]) -> Result<OpenedMailboxRow, Refusal> {
    let (kind, body) = core::open_mailbox_row(&key.inner, row)?;
    Ok(OpenedMailboxRow {
        kind: kind.into(),
        body,
    })
}

/// Open one row of the own mailbox. Fail-closed: an unknown version, a
/// length that is not a bucket, a row for another mailbox, an unknown kind
/// and malformed padding are all refused; the caller drops the row.
///
/// # Errors
///
/// Throws with `mailboxError` `badLength`, `unknownVersion`, `notOpened`,
/// `unknownKind`, `badPadding`.
#[wasm_bindgen(js_name = openMailboxRow)]
pub fn open_mailbox_row(key: &MailboxKey, row: &[u8]) -> Result<OpenedMailboxRow, JsValue> {
    js(open_mailbox_row_inner(key, row))
}

/// Whether `row` has the shape of a sealed mailbox row (version byte and a
/// bucket length), checkable without any key.
#[wasm_bindgen(js_name = isWellFormedMailboxRow)]
#[must_use]
pub fn is_well_formed_mailbox_row(row: &[u8]) -> bool {
    core::is_well_formed_row(row)
}

/// Length of a mailbox binding: 97.
#[wasm_bindgen(js_name = mailboxBindingLen)]
#[must_use]
pub fn mailbox_binding_len() -> usize {
    MAILBOX_BINDING_LEN
}

/// The longest body a mailbox row carries (983 035 bytes).
#[wasm_bindgen(js_name = mailboxMaxBodyLen)]
#[must_use]
pub fn mailbox_max_body_len() -> usize {
    MAILBOX_MAX_BODY_LEN
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    #[test]
    fn a_writer_seals_to_a_verified_binding_and_the_owner_opens_it() {
        let seed = [0x21u8; 32];
        let key = MailboxKey {
            inner: core::MailboxKey::from_account_seed(&seed),
        };
        let binding = MailboxBinding::for_account_seed(&seed).encode();
        assert_eq!(binding.len(), mailbox_binding_len());
        let account = SigningKey::from_seed(&seed).verifying_key().to_bytes();
        let pk = verify_mailbox_binding_inner(&binding, &account).unwrap();
        assert_eq!(pk.to_vec(), key.public_key());
        assert_eq!(mailbox_address_inner(&pk).unwrap(), key.address());
        let row = seal_mailbox_row_inner(&pk, MailboxKind::ContactAck, b"ack").unwrap();
        assert!(is_well_formed_mailbox_row(&row));
        let opened = open_mailbox_row_inner(&key, &row).unwrap();
        assert_eq!(
            (opened.kind(), opened.body()),
            (MailboxKind::ContactAck, b"ack".to_vec())
        );

        let stranger = SigningKey::from_seed(&[0x22; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            verify_mailbox_binding_inner(&binding, &stranger)
                .unwrap_err()
                .code,
            "badBinding"
        );
        assert_eq!(
            mailbox_address_inner(&[0; 31]).unwrap_err().code,
            "keyLength"
        );
        assert_eq!(
            open_mailbox_row_inner(&key, &[1, 2]).unwrap_err().code,
            "badLength"
        );
        // The numeric kinds are the core's wire bytes.
        for kind in core::MailboxKind::ALL {
            assert_eq!(MailboxKind::from(kind) as u8, kind.to_byte());
        }
    }
}
