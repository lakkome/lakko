// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for sealed-sender mailboxes ([ADR-0078] section 1). Mirror
//! of `crates/lakko-ffi-wasm/src/mailbox.rs`.
//!
//! A personal mailbox is named after a **mailbox key** every device of the
//! account derives from the account seed - not the identity key - and every
//! row in it is sealed to that key and padded to a size bucket:
//!
//! - [`MailboxKey::from_account_identity`] - the account's own mailbox key:
//!   its public key and address (claim with `dsMailboxSigner`), and
//!   [`open_mailbox_row`] for every row read back.
//! - [`sign_mailbox_binding`] - the 97-byte binding the account hands out
//!   with every mailbox key (it rides inside every reach grant already).
//! - [`verify_mailbox_binding`] - what a **writer** runs, against the
//!   recipient's pinned account key, before it writes anything; a binding
//!   that does not verify means "do not write".
//! - [`seal_mailbox_row`] / [`seal_mailbox_row_to_binding`] - seal a contact
//!   request, ack, Welcome or Welcome rendezvous as one row. The second
//!   verifies the binding first; prefer it.
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use std::sync::Arc;

use lakko_contacts::mailbox::{
    self as core, MailboxBinding, MailboxError, MailboxKind, MAILBOX_BINDING_LEN,
    MAILBOX_MAX_BODY_LEN,
};

use crate::{FfiError, Identity};

/// The one mapping from a core refusal to this surface:
/// `"<code>: <reason>"` under [`FfiError::Mailbox`].
pub(crate) fn mailbox_err(e: MailboxError) -> FfiError {
    FfiError::Mailbox(format!("{}: {e}", e.code()))
}

/// A 32-byte argument, or `keyLength`.
pub(crate) fn key32(bytes: &[u8], what: &str) -> Result<[u8; 32], FfiError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        FfiError::Mailbox(format!(
            "keyLength: {what} must be 32 bytes, got {}",
            bytes.len()
        ))
    })
}

/// What a mailbox row carries (ADR-0078). The byte values are frozen.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiMailboxKind {
    /// A signed `ContactRequest`.
    ContactRequest,
    /// An MLS Welcome.
    Welcome,
    /// A signed `ContactAck`.
    ContactAck,
    /// A Welcome rendezvous (the channel co-publisher join).
    WelcomeRendezvous,
    /// A signed link refusal (ADR-0079, 2026-10-19).
    LinkRefusal,
    /// A conversation re-entry request (ADR-0091 phase 2, route 2).
    /// Appended: a uniffi enum lowers by ordinal.
    ConversationReentry,
    /// A channel return (ADR-0101 section 7), in a continuity mailbox.
    /// Appended: a uniffi enum lowers by ordinal.
    ChannelReturn,
}

impl From<FfiMailboxKind> for MailboxKind {
    fn from(k: FfiMailboxKind) -> Self {
        match k {
            FfiMailboxKind::ContactRequest => Self::ContactRequest,
            FfiMailboxKind::Welcome => Self::Welcome,
            FfiMailboxKind::ContactAck => Self::ContactAck,
            FfiMailboxKind::WelcomeRendezvous => Self::WelcomeRendezvous,
            FfiMailboxKind::LinkRefusal => Self::LinkRefusal,
            FfiMailboxKind::ConversationReentry => Self::ConversationReentry,
            FfiMailboxKind::ChannelReturn => Self::ChannelReturn,
        }
    }
}

impl From<MailboxKind> for FfiMailboxKind {
    fn from(k: MailboxKind) -> Self {
        match k {
            MailboxKind::ContactRequest => Self::ContactRequest,
            MailboxKind::Welcome => Self::Welcome,
            MailboxKind::ContactAck => Self::ContactAck,
            MailboxKind::WelcomeRendezvous => Self::WelcomeRendezvous,
            MailboxKind::LinkRefusal => Self::LinkRefusal,
            MailboxKind::ConversationReentry => Self::ConversationReentry,
            MailboxKind::ChannelReturn => Self::ChannelReturn,
        }
    }
}

/// An opened mailbox row.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiOpenedMailboxRow {
    /// What the row carries.
    pub kind: FfiMailboxKind,
    /// The inner wire (a request, an ack, a Welcome, a rendezvous), unchanged.
    pub body: Vec<u8>,
}

/// The account's own mailbox key. Holds the secret; never leaves the device.
#[derive(uniffi::Object)]
pub struct MailboxKey {
    inner: core::MailboxKey,
}

#[uniffi::export]
impl MailboxKey {
    /// The mailbox key of the account whose identity is `account_identity`
    /// (derived from its 32-byte account seed; every device derives the
    /// same).
    #[uniffi::constructor]
    pub fn from_account_identity(account_identity: &Identity) -> Arc<Self> {
        Arc::new(Self {
            inner: core::MailboxKey::from_account_seed(
                &account_identity.inner.ed25519_seed_bytes(),
            ),
        })
    }

    /// The **continuity** mailbox key of the account (ADR-0101 section 1):
    /// a second mailbox, never the real one, a reset identity announces for
    /// a window. Read and open its rows exactly as the real one; claim it
    /// with `dsContinuityMailboxSigner`.
    #[uniffi::constructor]
    pub fn continuity_from_account_identity(account_identity: &Identity) -> Arc<Self> {
        Arc::new(Self {
            inner: lakko_contacts::continuity::continuity_mailbox_key(
                &account_identity.inner.ed25519_seed_bytes(),
            ),
        })
    }

    /// The 32-byte Ed25519 public key the claim registers.
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public().to_vec()
    }

    /// The 32-byte address the mailbox is stored under on the Delivery
    /// Service (`/ds/mailbox/{hex}/...`).
    pub fn address(&self) -> Vec<u8> {
        self.inner.address().to_vec()
    }
}

/// The 32-byte address of the mailbox named by the 32-byte `mailbox_pk`.
///
/// # Errors
///
/// [`FfiError::Mailbox`] `keyLength`.
#[uniffi::export]
pub fn mailbox_address(mailbox_pk: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(core::mailbox_address(&key32(&mailbox_pk, "mailbox key")?).to_vec())
}

/// The 97-byte mailbox binding of the account whose identity is
/// `account_identity`: its mailbox key signed by its identity key.
/// Deterministic, so every device produces the same bytes.
#[uniffi::export]
pub fn sign_mailbox_binding(account_identity: &Identity) -> Vec<u8> {
    MailboxBinding::for_account_seed(&account_identity.inner.ed25519_seed_bytes())
        .encode()
        .to_vec()
}

/// Verify a 97-byte binding against the recipient's 32-byte account key as
/// the writer holds it pinned, and return the 32-byte mailbox key it names.
/// A writer runs this before it seals or writes anything; on an error it
/// writes nothing.
///
/// # Errors
///
/// [`FfiError::Mailbox`] `badLength` / `unknownVersion` (malformed binding),
/// `badBinding` (signed by someone else), `badKey`, `keyLength`.
#[uniffi::export]
pub fn verify_mailbox_binding(binding: Vec<u8>, account_pk: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let account = key32(&account_pk, "account key")?;
    let binding = MailboxBinding::decode(&binding).map_err(mailbox_err)?;
    Ok(binding.verify(&account).map_err(mailbox_err)?.to_vec())
}

/// Seal `body` as one mailbox row of `kind` to the 32-byte `mailbox_pk`,
/// which must come from [`verify_mailbox_binding`] (or
/// `reachGrantMailboxPkVerified`). Post the row to
/// `/ds/mailbox/{mailboxAddress(mailbox_pk)}/messages`.
///
/// # Errors
///
/// [`FfiError::Mailbox`] `keyLength`, `badKey`, `bodyTooLong`.
#[uniffi::export]
pub fn seal_mailbox_row(
    mailbox_pk: Vec<u8>,
    kind: FfiMailboxKind,
    body: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let pk = key32(&mailbox_pk, "mailbox key")?;
    core::seal_mailbox_row(&pk, kind.into(), &body).map_err(mailbox_err)
}

/// [`verify_mailbox_binding`] then [`seal_mailbox_row`] in one call: the
/// form a writer should use, since it cannot seal without the check.
///
/// # Errors
///
/// Every error of the two.
#[uniffi::export]
pub fn seal_mailbox_row_to_binding(
    binding: Vec<u8>,
    account_pk: Vec<u8>,
    kind: FfiMailboxKind,
    body: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let pk = verify_mailbox_binding(binding, account_pk)?;
    seal_mailbox_row(pk, kind, body)
}

/// Open one row of the own mailbox. Fail-closed: an unknown version, a
/// length that is not a bucket, a row for another mailbox, an unknown kind
/// and malformed padding are all refused; the caller drops the row.
///
/// # Errors
///
/// [`FfiError::Mailbox`] `badLength`, `unknownVersion`, `notOpened`,
/// `unknownKind`, `badPadding`.
#[uniffi::export]
pub fn open_mailbox_row(key: &MailboxKey, row: Vec<u8>) -> Result<FfiOpenedMailboxRow, FfiError> {
    let (kind, body) = core::open_mailbox_row(&key.inner, &row).map_err(mailbox_err)?;
    Ok(FfiOpenedMailboxRow {
        kind: kind.into(),
        body,
    })
}

/// Whether `row` has the shape of a sealed mailbox row (version byte and a
/// bucket length), checkable without any key.
#[uniffi::export]
pub fn is_well_formed_mailbox_row(row: Vec<u8>) -> bool {
    core::is_well_formed_row(&row)
}

/// Length of a mailbox binding: 97.
#[uniffi::export]
pub fn mailbox_binding_len() -> u32 {
    u32::try_from(MAILBOX_BINDING_LEN).unwrap_or(u32::MAX)
}

/// The longest body a mailbox row carries (983 035 bytes).
#[uniffi::export]
pub fn mailbox_max_body_len() -> u32 {
    u32::try_from(MAILBOX_MAX_BODY_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account_pk(identity: &Identity) -> [u8; 32] {
        lakko_crypto::signing::SigningKey::from_seed(&identity.inner.ed25519_seed_bytes())
            .verifying_key()
            .to_bytes()
    }

    fn code_of(e: &FfiError) -> String {
        e.to_string()
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn a_writer_seals_to_a_verified_binding_and_the_owner_opens_it() {
        let alice = Identity::new("alice".into()).unwrap();
        let alice_pk = account_pk(&alice);
        let key = MailboxKey::from_account_identity(&alice);
        let binding = sign_mailbox_binding(&alice);
        assert_eq!(binding.len(), mailbox_binding_len() as usize);
        let mailbox_pk = verify_mailbox_binding(binding.clone(), alice_pk.to_vec()).unwrap();
        assert_eq!(mailbox_pk, key.public_key());
        assert_eq!(mailbox_address(mailbox_pk.clone()).unwrap(), key.address());
        for kind in [
            FfiMailboxKind::ContactRequest,
            FfiMailboxKind::Welcome,
            FfiMailboxKind::ContactAck,
            FfiMailboxKind::WelcomeRendezvous,
            FfiMailboxKind::LinkRefusal,
            FfiMailboxKind::ConversationReentry,
        ] {
            let row = seal_mailbox_row_to_binding(
                binding.clone(),
                alice_pk.to_vec(),
                kind,
                b"x".to_vec(),
            )
            .unwrap();
            assert!(is_well_formed_mailbox_row(row.clone()));
            let opened = open_mailbox_row(&key, row).unwrap();
            assert_eq!((opened.kind, opened.body), (kind, b"x".to_vec()));
        }
    }

    #[test]
    fn a_binding_of_someone_else_is_refused_with_its_code() {
        let alice = Identity::new("alice".into()).unwrap();
        let mallory = Identity::new("mallory".into()).unwrap();
        let mallory_binding = sign_mailbox_binding(&mallory);
        let alice_pk = account_pk(&alice);
        let e = verify_mailbox_binding(mallory_binding.clone(), alice_pk.to_vec()).unwrap_err();
        assert_eq!(code_of(&e), "badBinding");
        let e = seal_mailbox_row_to_binding(
            mallory_binding,
            alice_pk.to_vec(),
            FfiMailboxKind::Welcome,
            vec![],
        )
        .unwrap_err();
        assert_eq!(code_of(&e), "badBinding");
        assert_eq!(
            code_of(&verify_mailbox_binding(vec![0; 96], alice_pk.to_vec()).unwrap_err()),
            "badLength"
        );
        assert_eq!(
            code_of(&mailbox_address(vec![0; 31]).unwrap_err()),
            "keyLength"
        );
        let key = MailboxKey::from_account_identity(&alice);
        assert_eq!(
            code_of(&open_mailbox_row(&key, vec![1, 2, 3]).unwrap_err()),
            "badLength"
        );
        let other = MailboxKey::from_account_identity(&mallory);
        let row = seal_mailbox_row(other.public_key(), FfiMailboxKind::Welcome, vec![1]).unwrap();
        assert_eq!(
            code_of(&open_mailbox_row(&key, row).unwrap_err()),
            "notOpened"
        );
    }
}
