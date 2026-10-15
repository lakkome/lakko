// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The [ADR-0053] request proof for the extension's read-only DS fetch.
//!
//! A push wake previews the newest row of a journal, and once that journal is
//! claimed the fetch needs a proof like any other read. The signer is built
//! from the throwaway restored group (`Group.dsAccessSigner`) or from the
//! account's mailbox key (ADR-0078, derived from the account seed) for a
//! mailbox wake ([`ds_mailbox_signer`]); it signs a `GET`,
//! which is the only request this process makes. There is deliberately no
//! claim body here - registering a key is a write to the journal's posture
//! and belongs to the foreground app (C-59).
//!
//! [ADR-0053]: ../../../docs/adr/0053-ds-journal-authorisation.md

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lakko_ds_proof::{
    DsProofSigner as Inner, RequestParts, ACCESS_KEY_LABEL, MAILBOX_EPOCH, NONCE_LEN,
    PROOF_HEADER_NAME,
};
use lakko_mls::{Group as MlsGroup, Identity as MlsIdentity};

use crate::{FfiError, Identity};

/// One journal's proof signer at one epoch.
#[derive(uniffi::Object)]
pub struct DsProofSigner {
    inner: Inner,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn fresh_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    nonce
}

#[uniffi::export]
impl DsProofSigner {
    /// The epoch this signer proves membership at.
    pub fn epoch(&self) -> u64 {
        self.inner.epoch()
    }

    /// The `x-lakko-ds-proof` header value for one request; same contract as
    /// the full surface (`path` without the query, `query` without `?`,
    /// `body` the exact bytes - empty for the `GET` this process makes).
    pub fn sign(&self, method: String, path: String, query: String, body: Vec<u8>) -> String {
        let parts = RequestParts {
            method: &method,
            canonical_path: &path,
            query: &query,
            body: &body,
        };
        self.inner.sign_at(&parts, now_ms(), fresh_nonce()).encode()
    }
}

/// The request header a proof travels in.
#[uniffi::export]
pub fn ds_proof_header_name() -> String {
    PROOF_HEADER_NAME.to_string()
}

/// The epoch a mailbox proof is signed at.
#[uniffi::export]
pub fn ds_mailbox_epoch() -> u64 {
    MAILBOX_EPOCH
}

/// The signer for the own mailbox of the account whose identity is
/// `account_identity`: its mailbox key (ADR-0078; never the identity key).
#[uniffi::export]
pub fn ds_mailbox_signer(account_identity: &Identity) -> Arc<DsProofSigner> {
    Arc::new(DsProofSigner {
        inner: Inner::for_account_mailbox(&account_identity.inner.ed25519_seed_bytes()),
    })
}

/// The group access signer at the group's current epoch.
///
/// # Errors
///
/// The MLS exporter's own failures.
pub(crate) fn group_signer(
    group: &MlsGroup,
    identity: &MlsIdentity,
) -> Result<Arc<DsProofSigner>, FfiError> {
    let group_id = group.group_id();
    let secret = group.export_secret(identity, ACCESS_KEY_LABEL, &group_id, 32)?;
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&secret);
    Ok(Arc::new(DsProofSigner {
        inner: Inner::for_group(group.epoch(), &seed),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_ds_proof::{verify, Proof};

    #[test]
    fn the_extension_signer_matches_the_full_surface_derivation() {
        let identity = MlsIdentity::generate("e").unwrap();
        let group = MlsGroup::create(&identity, lakko_mls::GroupKind::Group).unwrap();
        let signer = group_signer(&group, &identity).unwrap();
        // The same exporter call the full surface makes yields the same key.
        let secret = group
            .export_secret(&identity, ACCESS_KEY_LABEL, &group.group_id(), 32)
            .unwrap();
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&secret);
        let expected = Inner::for_group(group.epoch(), &seed).verifying_key();
        let header = signer.sign("GET".into(), "/p".into(), String::new(), vec![]);
        let proof = Proof::parse(&header).unwrap();
        let parts = RequestParts {
            method: "GET",
            canonical_path: "/p",
            query: "",
            body: b"",
        };
        assert!(verify(&proof, &parts, &expected, now_ms()).is_ok());
    }
}
