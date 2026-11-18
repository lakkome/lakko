// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0090 section 4: the client half of the channel-poll credential on the
//! wasm surface (UniFFI mirror: `lakko-ffi-uniffi/src/anon_cred.rs`). No
//! issuer or verifier here.
//!
//! Errors throw an `Error` whose message is `"<code>: <reason>"`
//! (`lakko_anon_cred::AnonCredError::code`, or `bad-length`).

use lakko_anon_cred as core;
use wasm_bindgen::prelude::*;

fn cred_err(e: core::AnonCredError) -> JsError {
    JsError::new(&format!("{}: {e}", e.code()))
}

fn fixed<const N: usize>(v: &[u8], what: &str) -> Result<[u8; N], JsError> {
    <[u8; N]>::try_from(v).map_err(|_| {
        JsError::new(&format!(
            "bad-length: {what} must be {N} bytes, got {}",
            v.len()
        ))
    })
}

/// A period's issuer public key, as fetched from the issuer's key route.
#[wasm_bindgen]
pub struct AnonIssuerPublicKey {
    inner: core::IssuerPublicKey,
}

#[wasm_bindgen]
impl AnonIssuerPublicKey {
    /// Decode the 96-byte key of `period` (a `bigint`).
    ///
    /// # Errors
    ///
    /// `malformed`, `non-canonical`, `identity-point`.
    #[wasm_bindgen]
    pub fn decode(period: u64, bytes: &[u8]) -> Result<AnonIssuerPublicKey, JsError> {
        Ok(Self {
            inner: core::IssuerPublicKey::decode(period, bytes).map_err(cred_err)?,
        })
    }

    /// The period.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn period(&self) -> u64 {
        self.inner.period()
    }

    /// The 96-byte key.
    #[wasm_bindgen(js_name = toBytes)]
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.inner.to_bytes().to_vec()
    }
}

/// A presentation for one vote.
#[wasm_bindgen]
pub struct AnonPresentation {
    bytes: Vec<u8>,
    tag: Vec<u8>,
    period: u64,
}

#[wasm_bindgen]
impl AnonPresentation {
    /// The wire bytes for `services/channels`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    /// The per-poll tag.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn tag(&self) -> Vec<u8> {
        self.tag.clone()
    }

    /// The credential period.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn period(&self) -> u64 {
        self.period
    }
}

/// The account's credential secret (from the lineage seed; wiped on drop).
#[wasm_bindgen]
pub struct AnonCredentialSecret {
    inner: core::CredentialSecret,
}

#[wasm_bindgen]
impl AnonCredentialSecret {
    /// From the 32-byte lineage seed.
    ///
    /// # Errors
    ///
    /// `bad-length`, `zero-scalar`.
    #[wasm_bindgen(js_name = fromLineageSeed)]
    pub fn from_lineage_seed(lineage_seed: &[u8]) -> Result<AnonCredentialSecret, JsError> {
        let seed = fixed::<32>(lineage_seed, "lineageSeed")?;
        Ok(Self {
            inner: core::CredentialSecret::from_lineage_seed(&seed).map_err(cred_err)?,
        })
    }

    /// The 32-byte commitment auth stores.
    #[wasm_bindgen]
    #[must_use]
    pub fn commitment(&self) -> Vec<u8> {
        self.inner.commitment().to_bytes().to_vec()
    }

    /// The commitment registration for `PUT /auth/anon-credential/commitment`.
    ///
    /// # Errors
    ///
    /// `randomness-unavailable`.
    #[wasm_bindgen]
    pub fn registration(&self) -> Result<Vec<u8>, JsError> {
        Ok(self.inner.register().map_err(cred_err)?.to_bytes())
    }

    /// An issuance request for `key`'s period.
    ///
    /// # Errors
    ///
    /// `randomness-unavailable`.
    #[wasm_bindgen(js_name = issuanceRequest)]
    pub fn issuance_request(&self, key: &AnonIssuerPublicKey) -> Result<Vec<u8>, JsError> {
        Ok(self
            .inner
            .issuance_request(&key.inner)
            .map_err(cred_err)?
            .to_bytes())
    }

    /// Check the issuer's response and return the credential to store.
    ///
    /// # Errors
    ///
    /// `malformed`, `wrong-period`, `bad-proof`.
    #[wasm_bindgen(js_name = finalizeIssuance)]
    pub fn finalize_issuance(
        &self,
        key: &AnonIssuerPublicKey,
        response: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        let response = core::IssuanceResponse::decode(response).map_err(cred_err)?;
        Ok(self
            .inner
            .finalize_issuance(&key.inner, &response)
            .map_err(cred_err)?
            .to_bytes()
            .to_vec())
    }

    /// A fresh presentation of `credential` for the poll at `postCursor`
    /// (`bigint`) in the 16-byte `channelId`, bound to `message`.
    ///
    /// # Errors
    ///
    /// `malformed`, `wrong-period`, `message-too-long`, `bad-length`,
    /// `randomness-unavailable`.
    #[wasm_bindgen]
    pub fn present(
        &self,
        credential: &[u8],
        key: &AnonIssuerPublicKey,
        channel_id: &[u8],
        post_cursor: u64,
        message: &[u8],
    ) -> Result<AnonPresentation, JsError> {
        let credential = core::Credential::decode(credential).map_err(cred_err)?;
        let scope =
            core::PollScope::channel_post(fixed::<16>(channel_id, "channelId")?, post_cursor);
        let p = self
            .inner
            .present(&credential, &key.inner, &scope, message)
            .map_err(cred_err)?;
        Ok(AnonPresentation {
            bytes: p.to_bytes(),
            tag: p.tag().to_vec(),
            period: p.period(),
        })
    }
}

/// The credential period at `unixSecs` (`bigint`).
#[wasm_bindgen(js_name = anonCredPeriodAt)]
#[must_use]
pub fn anon_cred_period_at(unix_secs: u64) -> u64 {
    core::period_at(unix_secs)
}

/// The period length, seconds.
#[wasm_bindgen(js_name = anonCredPeriodSecs)]
#[must_use]
pub fn anon_cred_period_secs() -> u64 {
    core::PERIOD_SECS
}

/// The period of a stored credential.
///
/// # Errors
///
/// `malformed`, `unknown-version`, `non-canonical`.
#[wasm_bindgen(js_name = anonCredCredentialPeriod)]
pub fn anon_cred_credential_period(credential: &[u8]) -> Result<u64, JsError> {
    Ok(core::Credential::decode(credential)
        .map_err(cred_err)?
        .period())
}
