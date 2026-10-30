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
//! - [`ds_mailbox_signer`] - the account's mailbox key (ADR-0078, derived from
//!   the account seed; never the identity key) at [`ds_mailbox_epoch`], for
//!   the account's own mailbox.
//! - [`DsProofSigner::for_reach`] - a reach key (ADR-0060 §2) at
//!   [`ds_reach_epoch`], for a write to someone else's closed mailbox; a
//!   delegate signs with it too, over its own delegate seed, next to the
//!   [`sign_delegation`] header the reach-key holder issued.
//!
//! `sign` reads the wall clock and a CSPRNG nonce here; the pure core takes
//! both as arguments so the browser build can supply its own clock.
//!
//! [ADR-0053]: ../../../docs/adr/0053-ds-journal-authorisation.md

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lakko_ds_proof::{
    sign_delegation as inner_sign_delegation, DsProofSigner as Inner, RequestParts,
    ACCESS_KEY_LABEL, DELEGATION_HEADER_NAME, MAILBOX_EPOCH, MAX_DELEGATION_TTL_MS, NONCE_LEN,
    PROOF_HEADER_NAME, REACH_EPOCH,
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

fn key32(bytes: &[u8], what: &str) -> Result<[u8; 32], FfiError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        FfiError::Reach(format!(
            "keyLength: {what} must be 32 bytes, got {}",
            bytes.len()
        ))
    })
}

impl DsProofSigner {
    /// Wrap a core signer, for a sibling module that derives its own key
    /// (`circle.rs`).
    pub(crate) const fn from_inner(inner: Inner) -> Self {
        Self { inner }
    }
}

#[uniffi::export]
impl DsProofSigner {
    /// A reach-key signer (ADR-0060 §2) at [`ds_reach_epoch`]: signs a write
    /// to a closed identity mailbox with a 32-byte reach seed the owner
    /// registered (or with a delegate seed, next to a delegation). Its proof
    /// header names the signing key (a sixth field, inside the signed bytes)
    /// so the DS checks exactly that key; nothing for the caller to add.
    ///
    /// # Errors
    ///
    /// [`FfiError::Reach`] `seedLength` — `reach_seed` is not 32 bytes.
    #[uniffi::constructor]
    pub fn for_reach(reach_seed: Vec<u8>) -> Result<Self, FfiError> {
        let seed = <[u8; 32]>::try_from(reach_seed.as_slice()).map_err(|_| {
            FfiError::Reach(format!(
                "seedLength: reach seed must be 32 bytes, got {}",
                reach_seed.len()
            ))
        })?;
        Ok(Self {
            inner: Inner::for_reach(&seed),
        })
    }

    /// A group access signer rebuilt from a recorded access seed (ADR-0081):
    /// the 32-byte seed `dsAccessSeed` returned at `epoch`. It exists for a
    /// PublisherGroup commit, which merges eagerly: the seed of the epoch the
    /// commit left is recorded with the commit, so a re-send after a restart
    /// can still prove that epoch (the Delivery Service then appends it, or
    /// answers the original row, or `409`).
    ///
    /// # Errors
    ///
    /// [`FfiError::Message`] when `access_seed` is not 32 bytes.
    #[uniffi::constructor]
    pub fn for_group(epoch: u64, access_seed: Vec<u8>) -> Result<Self, FfiError> {
        let seed = <[u8; 32]>::try_from(access_seed.as_slice()).map_err(|_| {
            FfiError::Message(format!(
                "access seed must be 32 bytes, got {}",
                access_seed.len()
            ))
        })?;
        Ok(Self {
            inner: Inner::for_group(epoch, &seed),
        })
    }

    /// The epoch this signer proves membership at (`dsMailboxEpoch()` for a
    /// mailbox signer, `dsReachEpoch()` for a reach signer).
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
    /// a mailbox signer's key names: `{"mailbox_pk_b64": ...}` (ADR-0078).
    pub fn mailbox_claim_body_json(&self) -> String {
        self.inner.mailbox_claim_body_json()
    }

    /// The 32-byte address of the mailbox a mailbox signer's key names
    /// (ADR-0078): the path segment of `/ds/mailbox/{hex}/...`.
    pub fn mailbox_address(&self) -> Vec<u8> {
        self.inner.mailbox_address().to_vec()
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

/// The epoch label a reach-key proof carries (`u64::MAX`): never a group
/// epoch, never [`ds_mailbox_epoch`].
#[uniffi::export]
pub fn ds_reach_epoch() -> u64 {
    REACH_EPOCH
}

/// The request header a reach-key delegation travels in.
#[uniffi::export]
pub fn delegation_header_name() -> String {
    DELEGATION_HEADER_NAME.to_string()
}

/// The longest a delegation may still have to live when the DS checks it,
/// in ms (24 hours).
#[uniffi::export]
pub fn max_delegation_ttl_ms() -> u64 {
    MAX_DELEGATION_TTL_MS
}

/// Sign a delegation (ADR-0060 §2): the holder of the reach seed
/// `issuer_seed` authorises ONE write by `delegate_pk` to the 32-byte
/// `mailbox` address until `expires_at_ms`, under a fresh 16-byte `nonce`
/// the DS consumes. Returns the [`delegation_header_name`] header value.
/// The delegate sends it with a proof signed by
/// `DsProofSigner.forReach(delegateSeed)`. A delegation issued for longer
/// than [`max_delegation_ttl_ms`] ahead is refused by the DS until it comes
/// within range.
///
/// # Errors
///
/// [`FfiError::Reach`] `seedLength` / `keyLength` / `nonceLength` for an
/// argument of the wrong length.
#[uniffi::export]
pub fn sign_delegation(
    issuer_seed: Vec<u8>,
    mailbox: Vec<u8>,
    delegate_pk: Vec<u8>,
    expires_at_ms: u64,
    nonce: Vec<u8>,
) -> Result<String, FfiError> {
    Ok(
        signed_delegation(&issuer_seed, &mailbox, &delegate_pk, expires_at_ms, &nonce)?
            .to_header_value(),
    )
}

/// [`sign_delegation`], returning the raw 185-byte delegation instead of the
/// header value: the blob a committer-delegation envelope carries (ADR-0060
/// §6, `composeCommitterDelegationEnvelope`). The proposer signs one per
/// committer candidate, the delegate being that candidate's account
/// signature key and the mailbox the added person's. The committer turns
/// the one naming it back into a header with `delegationHeaderValue`.
///
/// # Errors
///
/// As [`sign_delegation`].
#[uniffi::export]
pub fn sign_delegation_blob(
    issuer_seed: Vec<u8>,
    mailbox: Vec<u8>,
    delegate_pk: Vec<u8>,
    expires_at_ms: u64,
    nonce: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    Ok(
        signed_delegation(&issuer_seed, &mailbox, &delegate_pk, expires_at_ms, &nonce)?
            .encode()
            .to_vec(),
    )
}

fn signed_delegation(
    issuer_seed: &[u8],
    mailbox: &[u8],
    delegate_pk: &[u8],
    expires_at_ms: u64,
    nonce: &[u8],
) -> Result<lakko_ds_proof::Delegation, FfiError> {
    let seed = <[u8; 32]>::try_from(issuer_seed).map_err(|_| {
        FfiError::Reach(format!(
            "seedLength: issuer seed must be 32 bytes, got {}",
            issuer_seed.len()
        ))
    })?;
    let mailbox = key32(mailbox, "mailbox address")?;
    let delegate = key32(delegate_pk, "delegate key")?;
    let nonce = <[u8; NONCE_LEN]>::try_from(nonce).map_err(|_| {
        FfiError::Reach(format!(
            "nonceLength: nonce must be {NONCE_LEN} bytes, got {}",
            nonce.len()
        ))
    })?;
    Ok(inner_sign_delegation(
        &seed,
        &mailbox,
        &delegate,
        expires_at_ms,
        nonce,
    ))
}

/// The signer for the own mailbox of the account whose identity is
/// `account_identity`: its **mailbox key** (ADR-0078, derived from the
/// account seed; never the identity key) at [`ds_mailbox_epoch`]. The claim
/// body it produces names that key, and the server accepts the claim only
/// when the mailbox address derives from it.
#[uniffi::export]
pub fn ds_mailbox_signer(account_identity: &Identity) -> Arc<DsProofSigner> {
    Arc::new(DsProofSigner {
        inner: Inner::for_account_mailbox(&account_identity.inner.ed25519_seed_bytes()),
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

/// The 32-byte access seed of the group's current epoch (the input of
/// [`group_signer`]), for a commit that must record it (ADR-0081).
///
/// # Errors
///
/// The MLS exporter's own failures.
pub(crate) fn group_access_seed(
    group: &MlsGroup,
    identity: &MlsIdentity,
) -> Result<Vec<u8>, FfiError> {
    let group_id = group.group_id();
    let secret = group.export_secret(identity, ACCESS_KEY_LABEL, &group_id, 32)?;
    Ok(secret.to_vec())
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
    fn a_signer_rebuilt_from_a_recorded_seed_is_the_epochs_signer() {
        // ADR-0081: a PublisherGroup commit records the seed of the epoch it
        // left; the signer rebuilt from it after a restart must prove that
        // epoch under the key the Delivery Service registered for it.
        let identity = MlsIdentity::generate("s").unwrap();
        let group = MlsGroup::create(&identity, lakko_mls::GroupKind::Group).unwrap();
        let live = group_signer(&group, &identity).unwrap();
        let seed = group_access_seed(&group, &identity).unwrap();
        let rebuilt = DsProofSigner::for_group(group.epoch(), seed).unwrap();
        assert_eq!(rebuilt.epoch(), live.epoch());
        assert_eq!(rebuilt.verifying_key(), live.verifying_key());
        assert!(DsProofSigner::for_group(0, vec![0u8; 31]).is_err());
    }

    #[test]
    fn a_group_signer_is_verified_by_the_server_under_the_registered_key() {
        let identity = MlsIdentity::generate("a").unwrap();
        let group = MlsGroup::create(&identity, lakko_mls::GroupKind::Group).unwrap();
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
    fn a_delegate_writes_under_a_delegation_the_server_verifies() {
        let reach_seed = vec![0x71u8; 32];
        let delegate = DsProofSigner::for_reach(vec![0x72u8; 32]).unwrap();
        assert_eq!(delegate.epoch(), ds_reach_epoch());
        assert_ne!(ds_reach_epoch(), ds_mailbox_epoch());
        let mailbox = vec![0x70u8; 32];
        let expires = now_ms() + 60_000;
        let header = sign_delegation(
            reach_seed.clone(),
            mailbox.clone(),
            delegate.verifying_key(),
            expires,
            vec![0x73u8; 16],
        )
        .unwrap();
        let d = lakko_ds_proof::Delegation::parse_header_value(&header).unwrap();
        let mailbox_arr: [u8; 32] = mailbox.clone().try_into().unwrap();
        lakko_ds_proof::verify_delegation(&d, &mailbox_arr, now_ms()).unwrap();
        // The delegate's proof names the delegate key, as the DS requires.
        let proof_header = delegate.sign(
            "POST".into(),
            "/ds/mailbox/00/messages".into(),
            String::new(),
            b"welcome".to_vec(),
        );
        assert_eq!(proof_header.split('.').count(), 6);
        let proof = Proof::parse(&proof_header).unwrap();
        lakko_ds_proof::verify_delegated_proof(
            &d,
            &proof,
            &RequestParts {
                method: "POST",
                canonical_path: "/ds/mailbox/00/messages",
                query: "",
                body: b"welcome",
            },
            now_ms(),
        )
        .unwrap();
        assert_eq!(
            d.issuer_pk.to_vec(),
            DsProofSigner::for_reach(reach_seed.clone())
                .unwrap()
                .verifying_key()
        );
        assert_eq!(delegation_header_name(), "x-lakko-ds-delegation");
        assert_eq!(max_delegation_ttl_ms(), 86_400_000);

        assert!(matches!(
            DsProofSigner::for_reach(vec![0; 31]),
            Err(FfiError::Reach(_))
        ));
        assert!(matches!(
            sign_delegation(reach_seed, mailbox, vec![0; 32], expires, vec![0; 15]),
            Err(FfiError::Reach(_))
        ));
    }

    #[test]
    fn the_mailbox_signer_names_the_accounts_mailbox_with_the_mailbox_key() {
        let identity = Identity::new("m".into()).unwrap();
        let signer = ds_mailbox_signer(&identity);
        assert_eq!(signer.epoch(), ds_mailbox_epoch());
        let vk = lakko_ds_proof::VerifyingKey::from_bytes(&signer.verifying_key()).unwrap();
        let address: [u8; 32] = signer.mailbox_address().try_into().unwrap();
        assert!(lakko_ds_proof::mailbox_owner_matches(&address, &vk));
        // ADR-0078: the mailbox key, never the identity key.
        let mailbox =
            lakko_ds_proof::MailboxKey::from_account_seed(&identity.inner.ed25519_seed_bytes());
        assert_eq!(signer.verifying_key(), mailbox.public().to_vec());
        assert_ne!(
            signer.verifying_key(),
            lakko_ds_proof::SigningKey::from_seed(&identity.inner.ed25519_seed_bytes())
                .verifying_key()
                .to_bytes()
                .to_vec()
        );
        assert!(signer
            .mailbox_claim_body_json()
            .starts_with("{\"mailbox_pk_b64\":\""));
    }
}
