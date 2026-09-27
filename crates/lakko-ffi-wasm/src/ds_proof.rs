// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen exposure of `lakko-ds-proof` - the client half of [ADR-0053].
//!
//! Mirror of `lakko-ffi-uniffi/src/ds_proof.rs`. A claimed journal on the
//! Delivery Service answers only a request carrying an `x-lakko-ds-proof`
//! header signed with a key it was claimed under; this module hands the web
//! client one [`DsProofSigner`] per journal per epoch (`Group.dsAccessSigner`
//! / `DeviceGroup.dsAccessSigner` / `PublisherGroup.dsAccessSigner`, or
//! [`ds_mailbox_signer`] for the device's own mailbox) and the two claim
//! bodies, so `DsClient` neither derives a key nor spells a field name.
//!
//! The clock is `Date.now()` on wasm32 (there is no `SystemTime` in the
//! browser) and the system clock on the host, where the unit tests run; the
//! nonce is `getrandom` under its `js` feature either way.
//!
//! [ADR-0053]: ../../../docs/adr/0053-ds-journal-authorisation.md

use lakko_ds_proof::{
    DsProofSigner as Inner, RequestParts, ACCESS_KEY_LABEL, MAILBOX_EPOCH, NONCE_LEN,
    PROOF_HEADER_NAME,
};
use lakko_mls::{Group as MlsGroup, Identity as MlsIdentity};
use wasm_bindgen::prelude::*;

use crate::mls::Identity;

/// One journal's proof signer at one epoch. See the module doc.
#[wasm_bindgen]
pub struct DsProofSigner {
    inner: Inner,
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

#[wasm_bindgen]
impl DsProofSigner {
    /// The epoch this signer proves membership at (`dsMailboxEpoch()` for a
    /// mailbox signer). A `bigint`.
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
    #[wasm_bindgen(js_name = groupClaimBodyJson)]
    pub fn group_claim_body_json(&self) -> String {
        self.inner.group_claim_body_json()
    }

    /// The JSON body of `PUT /ds/mailbox/{hex}/access` claiming the mailbox
    /// a mailbox signer's identity key names.
    #[wasm_bindgen(js_name = mailboxClaimBodyJson)]
    pub fn mailbox_claim_body_json(&self) -> String {
        self.inner.mailbox_claim_body_json()
    }

    /// The 32-byte pseudonym of the mailbox a mailbox signer's key names.
    #[wasm_bindgen(js_name = mailboxPseudonym)]
    pub fn mailbox_pseudonym(&self) -> Vec<u8> {
        self.inner.mailbox_pseudonym().to_vec()
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

/// The signer for `identity`'s own mailbox: its identity key at
/// `dsMailboxEpoch()`.
#[wasm_bindgen(js_name = dsMailboxSigner)]
#[must_use]
pub fn ds_mailbox_signer(identity: &Identity) -> DsProofSigner {
    DsProofSigner {
        inner: Inner::for_mailbox(&identity.mls_identity().ed25519_seed_bytes()),
    }
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
    fn a_group_signer_is_verified_by_the_server_under_the_registered_key() {
        let identity = MlsIdentity::generate("w").unwrap();
        let group = MlsGroup::create(&identity).unwrap();
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
    fn the_mailbox_signer_names_the_identity_mailbox() {
        let identity = MlsIdentity::generate("m").unwrap();
        let expected = Inner::for_mailbox(&identity.ed25519_seed_bytes());
        let signer = DsProofSigner {
            inner: expected.clone(),
        };
        assert_eq!(signer.epoch(), ds_mailbox_epoch());
        assert_eq!(
            signer.mailbox_pseudonym(),
            expected.mailbox_pseudonym().to_vec()
        );
        assert_eq!(ds_proof_header_name(), "x-lakko-ds-proof");
    }
}
