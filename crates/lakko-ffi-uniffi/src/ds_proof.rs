// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI exposure of `lakko-ds-proof` - the client half of [ADR-0053].
//!
//! A claimed journal on the Delivery Service answers only a request that
//! carries an `x-lakko-ds-proof` header signed with a key it was claimed
//! under. This module hands a client one [`DsProofSigner`] per journal per
//! epoch and the two request bodies a claim needs, so no client derives the
//! key, spells the canonical bytes, or names a JSON field itself.
//!
//! Three sources of a signer:
//! - `Group.dsAccessSigner(identity)` / `DeviceGroup.dsAccessSigner` /
//!   `PublisherGroup.dsAccessSigner` - the per-epoch group key from the MLS
//!   exporter (`lakko/ds/access/v1`, context = the group id). Re-derive after
//!   every commit: the signer carries the epoch it was made at, and the server
//!   keeps the two newest, so a signer one epoch behind still opens the
//!   journal while a peer is applying the commit, and one two behind does not.
//! - [`ds_mailbox_signer`] - the identity key at [`ds_mailbox_epoch`], for the
//!   device's own mailbox.
//!
//! `sign` reads the wall clock and a CSPRNG nonce here; the pure core takes
//! both as arguments so the browser build can supply its own clock.
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

/// One journal's proof signer at one epoch. See the module doc.
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
    /// The epoch this signer proves membership at (`dsMailboxEpoch()` for a
    /// mailbox signer).
    pub fn epoch(&self) -> u64 {
        self.inner.epoch()
    }

    /// The 32-byte Ed25519 verifying key a claim registers for this epoch.
    pub fn verifying_key(&self) -> Vec<u8> {
        self.inner.verifying_key().to_bytes().to_vec()
    }

    /// The `x-lakko-ds-proof` header value for one request. `path` is the
    /// request path without the query (`/ds/group/<hex>/messages`), `query`
    /// the raw query string without `?` (empty when none), `body` the exact
    /// bytes sent (empty for a `GET`). Every call mints a fresh nonce; a value
    /// is good for one request within the server's two-minute window.
    pub fn sign(&self, method: String, path: String, query: String, body: Vec<u8>) -> String {
        let parts = RequestParts {
            method: &method,
            canonical_path: &path,
            query: &query,
            body: &body,
        };
        self.inner.sign_at(&parts, now_ms(), fresh_nonce()).encode()
    }

    /// The JSON body of `PUT /ds/group/{hex}/access` registering this
    /// signer's key at its epoch.
    ///
    /// **Superseded by ADR-0057; the Delivery Service no longer accepts this
    /// body** (`PUT .../access` now takes a signed statement, and an epoch
    /// key registers nothing). Kept only until the clients move to the
    /// statement signers; do not add a caller.
    pub fn group_claim_body_json(&self) -> String {
        self.inner.group_claim_body_json()
    }

    /// The JSON body of `PUT /ds/mailbox/{hex}/access` claiming the mailbox
    /// a mailbox signer's identity key names.
    pub fn mailbox_claim_body_json(&self) -> String {
        self.inner.mailbox_claim_body_json()
    }

    /// The 32-byte pseudonym of the mailbox a mailbox signer's key names.
    pub fn mailbox_pseudonym(&self) -> Vec<u8> {
        self.inner.mailbox_pseudonym().to_vec()
    }
}

/// The request header a proof travels in.
#[uniffi::export]
pub fn ds_proof_header_name() -> String {
    PROOF_HEADER_NAME.to_string()
}

/// The epoch a mailbox proof is signed at (a mailbox has no MLS epoch).
#[uniffi::export]
pub fn ds_mailbox_epoch() -> u64 {
    MAILBOX_EPOCH
}

/// The signer for `identity`'s own mailbox: its identity key at
/// [`ds_mailbox_epoch`]. The claim body it produces names that key, and the
/// server accepts the claim only when the mailbox pseudonym derives from it.
#[uniffi::export]
pub fn ds_mailbox_signer(identity: &Identity) -> Arc<DsProofSigner> {
    Arc::new(DsProofSigner {
        inner: Inner::for_mailbox(&identity.inner.ed25519_seed_bytes()),
    })
}

/// The group access signer at the group's current epoch - the one exporter
/// derivation every `dsAccessSigner` method shares.
///
/// # Errors
///
/// The MLS exporter's own failures (an inactive group after the device's
/// removal, chiefly).
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

/// The access signer for the epoch a staged sweep moves to (N+1) while the
/// group still sits at N, or `None` when nothing is staged. The committer
/// registers this key, proven under N, BEFORE publishing the sweep.
///
/// # Errors
///
/// The MLS exporter's own failures.
pub(crate) fn staged_group_signer(
    group: &MlsGroup,
    identity: &MlsIdentity,
) -> Result<Option<Arc<DsProofSigner>>, FfiError> {
    let group_id = group.group_id();
    let Some(secret) = group.staged_export_secret(identity, ACCESS_KEY_LABEL, &group_id, 32)?
    else {
        return Ok(None);
    };
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&secret);
    Ok(Some(Arc::new(DsProofSigner {
        inner: Inner::for_group(group.epoch().saturating_add(1), &seed),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_ds_proof::{verify, Proof};

    #[test]
    fn a_group_signer_is_verified_by_the_server_under_the_registered_key() {
        let identity = MlsIdentity::generate("a").unwrap();
        let group = MlsGroup::create(&identity).unwrap();
        let signer = group_signer(&group, &identity).unwrap();
        assert_eq!(signer.epoch(), group.epoch());
        let header = signer.sign(
            "GET".into(),
            "/ds/group/00/messages".into(),
            "after=0".into(),
            vec![],
        );
        let proof = Proof::parse(&header).unwrap();
        let key = lakko_ds_proof::VerifyingKey::from_bytes(&signer.verifying_key()).unwrap();
        let parts = RequestParts {
            method: "GET",
            canonical_path: "/ds/group/00/messages",
            query: "after=0",
            body: b"",
        };
        assert!(verify(&proof, &parts, &key, now_ms()).is_ok());
        // Two calls never share a nonce.
        let again = signer.sign(
            "GET".into(),
            "/ds/group/00/messages".into(),
            "after=0".into(),
            vec![],
        );
        assert_ne!(header, again);
    }

    #[test]
    fn the_mailbox_signer_names_the_identity_mailbox() {
        let identity = Identity::new("m".into()).unwrap();
        let signer = ds_mailbox_signer(&identity);
        assert_eq!(signer.epoch(), ds_mailbox_epoch());
        let vk = lakko_ds_proof::VerifyingKey::from_bytes(&signer.verifying_key()).unwrap();
        let pseudonym: [u8; 32] = signer.mailbox_pseudonym().try_into().unwrap();
        assert!(lakko_ds_proof::mailbox_owner_matches(&pseudonym, &vk));
        assert!(signer
            .mailbox_claim_body_json()
            .starts_with("{\"identity_pk_b64\":\""));
    }
}
