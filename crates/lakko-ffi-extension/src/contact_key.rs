// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The retired-`contact`-key rule for a mailbox row (ADR-0060 section 1,
//! amended 2026-11-06), on the extension surface so a background path that
//! reads the identity mailbox applies the same rule as the app and never
//! shows a row the app would drop.
//!
//! Removing a contact rotates the account's shared `contact` reach key; the
//! owner keeps eight retired versions registered so a contact offline across
//! a rotation can still write. The Delivery Service reports, per mailbox row,
//! the reach key that admitted it. A row under a retired key from a CURRENT
//! contact is processed (the app then sends that contact the current grant),
//! from anyone else it is dropped. This surface carries the verdict only:
//! the regrant is a send, which this process never makes (C-59).

use lakko_contacts::reach::{
    classify_contact_key, contact_key_row_verdict, ContactKeyRowVerdict, REACH_SEED_LEN,
};

use crate::FfiError;

/// What the receiving device does with a mailbox row (mirror of
/// `lakko_contacts::reach::ContactKeyRowVerdict`, same names as the app
/// framework's).
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiContactKeyRowVerdict {
    /// Process the row as before.
    Process,
    /// Process it; the app sends the sender the current grant (never here).
    ProcessAndRegrant,
    /// Drop it: show nothing, log nothing that names anyone.
    Drop,
}

impl From<ContactKeyRowVerdict> for FfiContactKeyRowVerdict {
    fn from(v: ContactKeyRowVerdict) -> Self {
        match v {
            ContactKeyRowVerdict::Process => Self::Process,
            ContactKeyRowVerdict::ProcessAndRegrant => Self::ProcessAndRegrant,
            ContactKeyRowVerdict::Drop => Self::Drop,
        }
    }
}

fn key32(bytes: &[u8], what: &str) -> Result<[u8; REACH_SEED_LEN], FfiError> {
    <[u8; REACH_SEED_LEN]>::try_from(bytes).map_err(|_| {
        FfiError::Contacts(format!(
            "seedLength: {what} must be {REACH_SEED_LEN} bytes, got {}",
            bytes.len()
        ))
    })
}

/// The one rule: classify the row's admitting key (`proving_key`, its
/// `reach_pk_b64` decoded, or `None` for a proofless row) against this
/// account's `contact` keys at `current_contact_version`, then judge it with
/// `sender_is_contact` (the account the row's verified content names as its
/// sender is a CURRENT contact).
///
/// # Errors
///
/// [`FfiError::Contacts`] `seedLength` for a seed or key that is not 32
/// bytes.
#[uniffi::export]
pub fn mailbox_row_contact_verdict(
    account_seed: Vec<u8>,
    current_contact_version: u32,
    proving_key: Option<Vec<u8>>,
    sender_is_contact: bool,
) -> Result<FfiContactKeyRowVerdict, FfiError> {
    let Some(key) = proving_key else {
        return Ok(FfiContactKeyRowVerdict::Process);
    };
    let account = key32(&account_seed, "account seed")?;
    let key = key32(&key, "proving key")?;
    let standing = classify_contact_key(&account, current_contact_version, &key);
    Ok(contact_key_row_verdict(standing, sender_is_contact).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::reach::{reach_seed, reach_verifying_key, ReachClass};

    #[test]
    fn the_extension_applies_the_core_rule() {
        let seed = [0x42u8; 32];
        let pk = |v: u32| {
            reach_verifying_key(
                reach_seed(&seed, ReachClass::Contact, b"", v)
                    .unwrap()
                    .as_bytes(),
            )
            .to_vec()
        };
        let verdict = |key: Option<Vec<u8>>, contact: bool| {
            mailbox_row_contact_verdict(seed.to_vec(), 9, key, contact).unwrap()
        };
        assert_eq!(verdict(Some(pk(4)), false), FfiContactKeyRowVerdict::Drop);
        assert_eq!(
            verdict(Some(pk(4)), true),
            FfiContactKeyRowVerdict::ProcessAndRegrant
        );
        assert_eq!(
            verdict(Some(pk(9)), false),
            FfiContactKeyRowVerdict::Process
        );
        assert_eq!(verdict(None, false), FfiContactKeyRowVerdict::Process);
        assert!(mailbox_row_contact_verdict(seed.to_vec(), 9, Some(vec![0; 3]), true).is_err());
    }
}
