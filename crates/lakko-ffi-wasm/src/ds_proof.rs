// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen exposure of `lakko-ds-proof` - the client half of [ADR-0053].
//!
//! Mirror of `lakko-ffi-uniffi/src/ds_proof.rs`. A claimed journal on the
//! Delivery Service answers only a request carrying an `x-lakko-ds-proof`
//! header signed with a key it was claimed under; this module hands the web
//! client one [`DsProofSigner`] per journal per epoch (`Group.dsAccessSigner`
//! / `DeviceGroup.dsAccessSigner` / `PublisherGroup.dsAccessSigner`, or
//! [`ds_mailbox_signer`] for the account's own mailbox, signed by its
//! ADR-0078 mailbox key, never the identity key) and the two claim
//! bodies, so `DsClient` neither derives a key nor spells a field name.
//! `DsProofSigner.forReach(seed)` signs a write to someone else's closed
//! mailbox under a reach key (ADR-0060 §2), and `signDelegation` hands one
//! such write to another key.
//!
//! The clock is `Date.now()` on wasm32 (there is no `SystemTime` in the
//! browser) and the system clock on the host, where the unit tests run; the
//! nonce is `getrandom` under its `js` feature either way.
//!
//! [ADR-0053]: ../../../docs/adr/0053-ds-journal-authorisation.md

use lakko_ds_proof::{
    sign_delegation as inner_sign_delegation, DsProofSigner as Inner, RequestParts,
    ACCESS_KEY_LABEL, DELEGATION_HEADER_NAME, MAILBOX_EPOCH, MAX_DELEGATION_TTL_MS, NONCE_LEN,
    PROOF_HEADER_NAME, REACH_EPOCH,
};
use lakko_mls::{Group as MlsGroup, Identity as MlsIdentity};
use wasm_bindgen::prelude::*;

use crate::mls::Identity;

/// One journal's proof signer at one epoch. See the module doc.
#[wasm_bindgen]
pub struct DsProofSigner {
    inner: Inner,
}

impl DsProofSigner {
    /// Wrap a core signer, for a sibling module that derives its own key
    /// (`circle.rs`).
    pub(crate) const fn from_inner(inner: Inner) -> Self {
        Self { inner }
    }
}

#[cfg(target_arch = "wasm32")]
fn now_ms() -> u64 {
    // `Date.now()` is integral milliseconds carried in an f64; a negative or
    // non-finite value (a broken clock) reads as 0 and the server refuses.
    let now = js_sys::Date::now();
    if now.is_finite() && now >= 0.0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ms = now as u64;
        ms
    } else {
        0
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn fresh_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    nonce
}

fn reach_len_refusal(code: &str, what: &str, want: usize, got: usize) -> JsValue {
    crate::contacts::coded_error(
        &format!("{what} must be {want} bytes, got {got}"),
        "reachError",
        code,
    )
}

fn for_reach_inner(reach_seed: &[u8]) -> Option<DsProofSigner> {
    let seed = <[u8; 32]>::try_from(reach_seed).ok()?;
    Some(DsProofSigner {
        inner: Inner::for_reach(&seed),
    })
}

#[wasm_bindgen]
impl DsProofSigner {
    /// A group access signer rebuilt from a recorded access seed (ADR-0081):
    /// the 32-byte seed `dsAccessSeed` returned at `epoch`. It exists for a
    /// PublisherGroup commit, which merges eagerly: the seed of the epoch the
    /// commit left is recorded with the commit, so a re-send after a restart
    /// can still prove that epoch (the Delivery Service then appends it, or
    /// answers the original row, or `409`).
    ///
    /// # Errors
    ///
    /// Throws when `accessSeed` is not 32 bytes.
    #[wasm_bindgen(js_name = forGroup)]
    pub fn for_group(epoch: u64, access_seed: &[u8]) -> Result<DsProofSigner, JsValue> {
        let seed = <[u8; 32]>::try_from(access_seed).map_err(|_| {
            JsValue::from(js_sys::Error::new(&format!(
                "access seed must be 32 bytes, got {}",
                access_seed.len()
            )))
        })?;
        Ok(DsProofSigner {
            inner: Inner::for_group(epoch, &seed),
        })
    }

    /// A reach-key signer (ADR-0060 §2) at `dsReachEpoch()`: signs a write to
    /// a closed identity mailbox with a 32-byte reach seed the owner
    /// registered (or a delegate seed, next to a delegation). Its proof header
    /// names the signing key (a sixth field, inside the signed bytes) so the
    /// DS checks exactly that key; nothing for the caller to add.
    ///
    /// # Errors
    ///
    /// Throws an `Error` with `reachError` `seedLength`.
    #[wasm_bindgen(js_name = forReach)]
    pub fn for_reach(reach_seed: &[u8]) -> Result<DsProofSigner, JsValue> {
        for_reach_inner(reach_seed)
            .ok_or_else(|| reach_len_refusal("seedLength", "reach seed", 32, reach_seed.len()))
    }

    /// The epoch this signer proves membership at (`dsMailboxEpoch()` for a
    /// mailbox signer, `dsReachEpoch()` for a reach signer). A `bigint`.
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch()
    }

    /// The 32-byte Ed25519 verifying key a claim registers for this epoch.
    #[wasm_bindgen(js_name = verifyingKey)]
    pub fn verifying_key(&self) -> Vec<u8> {
        self.inner.verifying_key().to_bytes().to_vec()
    }

    /// The `x-lakko-ds-proof` header value for one request. `path` is the
    /// request path without the query, `query` the raw query string without
    /// `?` (empty when none), `body` the exact bytes sent (empty for a `GET`).
    /// Every call mints a fresh nonce; a value is good for one request within
    /// the server's two-minute window.
    pub fn sign(&self, method: &str, path: &str, query: &str, body: &[u8]) -> String {
        let parts = RequestParts {
            method,
            canonical_path: path,
            query,
            body,
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
    #[wasm_bindgen(js_name = groupClaimBodyJson)]
    pub fn group_claim_body_json(&self) -> String {
        self.inner.group_claim_body_json()
    }

    /// The JSON body of `PUT /ds/mailbox/{hex}/access` claiming the mailbox
    /// a mailbox signer's key names: `{"mailbox_pk_b64": ...}` (ADR-0078).
    #[wasm_bindgen(js_name = mailboxClaimBodyJson)]
    pub fn mailbox_claim_body_json(&self) -> String {
        self.inner.mailbox_claim_body_json()
    }

    /// The 32-byte address of the mailbox a mailbox signer's key names
    /// (ADR-0078): the path segment of `/ds/mailbox/{hex}/...`.
    #[wasm_bindgen(js_name = mailboxAddress)]
    pub fn mailbox_address(&self) -> Vec<u8> {
        self.inner.mailbox_address().to_vec()
    }
}

/// The request header a proof travels in.
#[wasm_bindgen(js_name = dsProofHeaderName)]
#[must_use]
pub fn ds_proof_header_name() -> String {
    PROOF_HEADER_NAME.to_string()
}

/// The epoch a mailbox proof is signed at (a mailbox has no MLS epoch). A
/// `bigint`.
#[wasm_bindgen(js_name = dsMailboxEpoch)]
#[must_use]
pub fn ds_mailbox_epoch() -> u64 {
    MAILBOX_EPOCH
}

/// The epoch label a reach-key proof carries (`2n ** 64n - 1n`): never a
/// group epoch, never `dsMailboxEpoch()`. A `bigint`.
#[wasm_bindgen(js_name = dsReachEpoch)]
#[must_use]
pub fn ds_reach_epoch() -> u64 {
    REACH_EPOCH
}

/// The request header a reach-key delegation travels in.
#[wasm_bindgen(js_name = delegationHeaderName)]
#[must_use]
pub fn delegation_header_name() -> String {
    DELEGATION_HEADER_NAME.to_string()
}

/// The longest a delegation may still have to live when the DS checks it,
/// in ms (24 hours). A `bigint`.
#[wasm_bindgen(js_name = maxDelegationTtlMs)]
#[must_use]
pub fn max_delegation_ttl_ms() -> u64 {
    MAX_DELEGATION_TTL_MS
}

type LenRefusal = (&'static str, &'static str, usize, usize);

fn sign_delegation_inner(
    issuer_seed: &[u8],
    mailbox: &[u8],
    delegate_pk: &[u8],
    expires_at_ms: u64,
    nonce: &[u8],
) -> Result<String, LenRefusal> {
    signed_delegation(issuer_seed, mailbox, delegate_pk, expires_at_ms, nonce)
        .map(|d| d.to_header_value())
}

fn signed_delegation(
    issuer_seed: &[u8],
    mailbox: &[u8],
    delegate_pk: &[u8],
    expires_at_ms: u64,
    nonce: &[u8],
) -> Result<lakko_ds_proof::Delegation, LenRefusal> {
    let seed = <[u8; 32]>::try_from(issuer_seed)
        .map_err(|_| ("seedLength", "issuer seed", 32, issuer_seed.len()))?;
    let mailbox = <[u8; 32]>::try_from(mailbox)
        .map_err(|_| ("keyLength", "mailbox address", 32, mailbox.len()))?;
    let delegate = <[u8; 32]>::try_from(delegate_pk)
        .map_err(|_| ("keyLength", "delegate key", 32, delegate_pk.len()))?;
    let nonce = <[u8; NONCE_LEN]>::try_from(nonce)
        .map_err(|_| ("nonceLength", "nonce", NONCE_LEN, nonce.len()))?;
    Ok(inner_sign_delegation(
        &seed,
        &mailbox,
        &delegate,
        expires_at_ms,
        nonce,
    ))
}

/// Sign a delegation (ADR-0060 §2): the holder of the reach seed
/// `issuerSeed` authorises ONE write by `delegatePk` to the 32-byte `mailbox`
/// pseudonym until `expiresAtMs`, under a fresh 16-byte `nonce` the DS
/// consumes. Returns the `delegationHeaderName()` header value; the delegate
/// sends it with a proof signed by `DsProofSigner.forReach(delegateSeed)`.
///
/// # Errors
///
/// Throws an `Error` with `reachError` `seedLength` / `keyLength` /
/// `nonceLength`.
#[wasm_bindgen(js_name = signDelegation)]
pub fn sign_delegation(
    issuer_seed: &[u8],
    mailbox: &[u8],
    delegate_pk: &[u8],
    expires_at_ms: u64,
    nonce: &[u8],
) -> Result<String, JsValue> {
    sign_delegation_inner(issuer_seed, mailbox, delegate_pk, expires_at_ms, nonce)
        .map_err(|(code, what, want, got)| reach_len_refusal(code, what, want, got))
}

/// `signDelegation`, returning the raw 185-byte delegation instead of the
/// header value: the blob a committer-delegation envelope carries (ADR-0060
/// §6, `composeCommitterDelegationEnvelope`). The proposer signs one per
/// committer candidate, the delegate being that candidate's account
/// signature key and the mailbox the added person's.
///
/// # Errors
///
/// As `signDelegation`.
#[wasm_bindgen(js_name = signDelegationBlob)]
pub fn sign_delegation_blob(
    issuer_seed: &[u8],
    mailbox: &[u8],
    delegate_pk: &[u8],
    expires_at_ms: u64,
    nonce: &[u8],
) -> Result<Vec<u8>, JsValue> {
    signed_delegation(issuer_seed, mailbox, delegate_pk, expires_at_ms, nonce)
        .map(|d| d.encode().to_vec())
        .map_err(|(code, what, want, got)| reach_len_refusal(code, what, want, got))
}

/// The signer for the own mailbox of the account whose identity is
/// `accountIdentity`: its **mailbox key** (ADR-0078, derived from the account
/// seed; never the identity key) at `dsMailboxEpoch()`.
#[wasm_bindgen(js_name = dsMailboxSigner)]
#[must_use]
pub fn ds_mailbox_signer(account_identity: &Identity) -> DsProofSigner {
    DsProofSigner {
        inner: Inner::for_account_mailbox(&account_identity.mls_identity().ed25519_seed_bytes()),
    }
}

/// The signer for the account's **continuity** mailbox (ADR-0101 section 1,
/// derived from the account seed under its own context; never the real
/// mailbox key) at `dsMailboxEpoch()`: claim, read and register its one
/// continuity reach key with it.
#[wasm_bindgen(js_name = dsContinuityMailboxSigner)]
#[must_use]
pub fn ds_continuity_mailbox_signer(account_identity: &Identity) -> DsProofSigner {
    DsProofSigner {
        inner: Inner::for_mailbox(
            lakko_contacts::continuity::continuity_mailbox_seed(
                &account_identity.mls_identity().ed25519_seed_bytes(),
            )
            .as_bytes(),
        ),
    }
}

/// The 32-byte access seed of the group's current epoch (the input of
/// [`group_signer`]), for a commit that must record it (ADR-0081).
pub(crate) fn group_access_seed(
    group: &MlsGroup,
    identity: &MlsIdentity,
) -> Result<Vec<u8>, JsValue> {
    let group_id = group.group_id();
    let secret = group
        .export_secret(identity, ACCESS_KEY_LABEL, &group_id, 32)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    Ok(secret.to_vec())
}

/// The 32-byte access seed of the epoch a staged commit (or a held external
/// entry) moves to, `None` when nothing is staged (ADR-0064 section 7: what an
/// owner device saves in the channel's vault head with the staged GroupInfo).
pub(crate) fn staged_group_access_seed(
    group: &MlsGroup,
    identity: &MlsIdentity,
) -> Result<Option<Vec<u8>>, JsValue> {
    let group_id = group.group_id();
    Ok(group
        .staged_export_secret(identity, ACCESS_KEY_LABEL, &group_id, 32)
        .map_err(|e| JsValue::from_str(&e.to_string()))?
        .map(|secret| secret.to_vec()))
}

/// The group access signer at the group's current epoch - the one exporter
/// derivation every `dsAccessSigner` method shares.
pub(crate) fn group_signer(
    group: &MlsGroup,
    identity: &MlsIdentity,
) -> Result<DsProofSigner, JsValue> {
    let group_id = group.group_id();
    let secret = group
        .export_secret(identity, ACCESS_KEY_LABEL, &group_id, 32)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&secret);
    Ok(DsProofSigner {
        inner: Inner::for_group(group.epoch(), &seed),
    })
}

/// The access signer for the epoch a staged sweep moves to (N+1) while the
/// group still sits at N, or `None` when nothing is staged.
pub(crate) fn staged_group_signer(
    group: &MlsGroup,
    identity: &MlsIdentity,
) -> Result<Option<DsProofSigner>, JsValue> {
    let group_id = group.group_id();
    let Some(secret) = group
        .staged_export_secret(identity, ACCESS_KEY_LABEL, &group_id, 32)
        .map_err(|e| JsValue::from_str(&e.to_string()))?
    else {
        return Ok(None);
    };
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&secret);
    Ok(Some(DsProofSigner {
        inner: Inner::for_group(group.epoch().saturating_add(1), &seed),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_ds_proof::{verify, Proof};

    #[test]
    fn a_signer_rebuilt_from_a_recorded_seed_is_the_epochs_signer() {
        // ADR-0081: see the UniFFI twin of this test.
        let identity = MlsIdentity::generate("s").unwrap();
        let group = MlsGroup::create(&identity, lakko_mls::GroupKind::Group).unwrap();
        let live = group_signer(&group, &identity).unwrap();
        let seed = group_access_seed(&group, &identity).unwrap();
        let rebuilt = DsProofSigner::for_group(group.epoch(), &seed).unwrap();
        assert_eq!(rebuilt.epoch(), live.epoch());
        assert_eq!(rebuilt.verifying_key(), live.verifying_key());
    }

    /// G-55: `lakko-mls` derives the access seed itself (the entry wrap key
    /// input); it must be the value this surface hands out.
    #[test]
    fn the_core_derives_the_same_access_seed() {
        assert_eq!(lakko_mls::DS_ACCESS_EXPORTER_LABEL, ACCESS_KEY_LABEL);
        let identity = MlsIdentity::generate("g55").unwrap();
        let group = MlsGroup::create(&identity, lakko_mls::GroupKind::Group).unwrap();
        let seed = group_access_seed(&group, &identity).unwrap();
        assert_eq!(
            group.ds_access_seed(&identity).unwrap().as_slice(),
            seed.as_slice()
        );
    }

    #[test]
    fn a_group_signer_is_verified_by_the_server_under_the_registered_key() {
        let identity = MlsIdentity::generate("w").unwrap();
        let group = MlsGroup::create(&identity, lakko_mls::GroupKind::Group).unwrap();
        let signer = group_signer(&group, &identity).unwrap();
        assert_eq!(signer.epoch(), group.epoch());
        let header = signer.sign("POST", "/ds/group/00/messages", "", b"body");
        let proof = Proof::parse(&header).unwrap();
        let key = lakko_ds_proof::VerifyingKey::from_bytes(&signer.verifying_key()).unwrap();
        let parts = RequestParts {
            method: "POST",
            canonical_path: "/ds/group/00/messages",
            query: "",
            body: b"body",
        };
        assert!(verify(&proof, &parts, &key, now_ms()).is_ok());
        assert!(signer.group_claim_body_json().starts_with("{\"epoch\":"));
    }

    #[test]
    fn a_delegate_writes_under_a_delegation_the_server_verifies() {
        let delegate = DsProofSigner::for_reach(&[0x72u8; 32]).unwrap();
        assert_eq!(delegate.epoch(), ds_reach_epoch());
        assert_ne!(ds_reach_epoch(), ds_mailbox_epoch());
        let mailbox = [0x70u8; 32];
        let expires = now_ms() + 60_000;
        let header = sign_delegation(
            &[0x71u8; 32],
            &mailbox,
            &delegate.verifying_key(),
            expires,
            &[0x73u8; 16],
        )
        .unwrap();
        let d = lakko_ds_proof::Delegation::parse_header_value(&header).unwrap();
        lakko_ds_proof::verify_delegation(&d, &mailbox, now_ms()).unwrap();
        // The delegate's proof names the delegate key, as the DS requires.
        let proof_header = delegate.sign("POST", "/ds/mailbox/00/messages", "", b"welcome");
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
        assert_eq!(delegation_header_name(), "x-lakko-ds-delegation");
        assert_eq!(max_delegation_ttl_ms(), 86_400_000);
        assert!(for_reach_inner(&[0; 31]).is_none());
        assert_eq!(
            sign_delegation_inner(&[0; 32], &mailbox, &[0; 32], expires, &[0; 15])
                .unwrap_err()
                .0,
            "nonceLength"
        );
    }

    #[test]
    fn the_mailbox_signer_names_the_accounts_mailbox_with_the_mailbox_key() {
        let identity = MlsIdentity::generate("m").unwrap();
        let expected = Inner::for_account_mailbox(&identity.ed25519_seed_bytes());
        let signer = DsProofSigner {
            inner: expected.clone(),
        };
        assert_eq!(signer.epoch(), ds_mailbox_epoch());
        assert_eq!(
            signer.mailbox_address(),
            expected.mailbox_address().to_vec()
        );
        let mailbox = lakko_ds_proof::MailboxKey::from_account_seed(&identity.ed25519_seed_bytes());
        assert_eq!(signer.verifying_key(), mailbox.public().to_vec());
        assert_eq!(signer.mailbox_address(), mailbox.address().to_vec());
        assert_eq!(ds_proof_header_name(), "x-lakko-ds-proof");
    }
}
