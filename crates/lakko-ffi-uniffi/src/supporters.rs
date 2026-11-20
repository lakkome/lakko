// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface of ADR-0097: supporter codes and the supporters list.
//! Mirror of `crates/lakko-ffi-wasm/src/supporters.rs`.
//!
//! # Supporter code
//!
//! 1. `supporterCodeNormalize(text)` - the display form (`XXXX-XXXX-XXXX-XXXX`)
//!    of typed text, or `FfiError::AnonToken("bad-supporter-code: ..")`.
//! 2. `SupporterCodeRequest(code, epoch)` with
//!    `epoch = supporterCodeEpochAt(nowUnixSecs)`; the blinded element is
//!    derived from the code and the epoch, so building it again after a lost
//!    response sends the same one.
//! 3. `POST /auth/supporter-code/redeem` (no session) with `redeemBodyJson()`.
//!    A `400 epoch-not-issuable` carries `current_epoch`: rebuild for it.
//! 4. `GET /auth/tokens/key/beta-place/{epoch}`, then
//!    `finalize(evaluation, proof, publicKey)` -> the `x-lakko-anon-token`
//!    header value, kept on the device and sent with the first
//!    `PUT /auth/login-handle` while `anonTokenIsLive(epoch, current)` (30-day
//!    epochs).
//!
//! # Supporters list
//!
//! `verifySupportersList(bytes, verifyingKey)` under the pinned release key;
//! `FfiError::SupportersList` on any failure (treat as absent).

use std::sync::Mutex;

use lakko_anon_token::{SupporterCode, TokenContext, TokenRequest, BETA_PLACE_WINDOW_SECS};
use lakko_attest::verify_supporters_list as inner_verify;
use lakko_crypto::signing::VerifyingKey;

use crate::FfiError;

/// A verified supporters list.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct VerifiedSupportersList {
    /// Monotone version: keep the highest verified, ignore a lower one.
    pub list_version: u64,
    /// When it was signed, Unix milliseconds.
    pub signed_at_ms: i64,
    /// The names, in file order.
    pub names: Vec<String>,
}

/// Verify a fetched supporters list under the pinned release verifying key
/// (32 bytes).
///
/// # Errors
///
/// [`FfiError::SupportersList`] on any failure; the client then shows no
/// list (ADR-0097 section 2).
#[uniffi::export]
pub fn verify_supporters_list(
    bytes: Vec<u8>,
    verifying_key: Vec<u8>,
) -> Result<VerifiedSupportersList, FfiError> {
    let vk = VerifyingKey::from_bytes(&verifying_key)
        .map_err(|_| FfiError::SupportersList("release key is not a valid Ed25519 key".into()))?;
    let list = inner_verify(&bytes, &vk).map_err(|e| FfiError::SupportersList(e.to_string()))?;
    Ok(VerifiedSupportersList {
        list_version: list.list_version,
        signed_at_ms: list.signed_at_unix_millis,
        names: list.names,
    })
}

/// The display form of typed supporter-code text (forgiving: case, spaces,
/// dashes, O/I/L).
///
/// # Errors
///
/// [`FfiError::AnonToken`] with code `bad-supporter-code`.
#[uniffi::export]
pub fn supporter_code_normalize(text: String) -> Result<String, FfiError> {
    Ok(SupporterCode::parse(&text)?.to_display())
}

/// The `beta-place` epoch at `unix_secs` (30-day window).
#[uniffi::export]
pub fn supporter_code_epoch_at(unix_secs: u64) -> u64 {
    unix_secs / BETA_PLACE_WINDOW_SECS
}

/// The `beta-place` window in seconds (30 days).
#[uniffi::export]
pub fn beta_place_window_secs() -> u64 {
    BETA_PLACE_WINDOW_SECS
}

/// The redeem request for one supporter code at one epoch.
#[derive(uniffi::Object)]
pub struct SupporterCodeRequest {
    inner: Mutex<Option<TokenRequest>>,
    code_display: String,
    epoch: u64,
    blinded: String,
}

#[uniffi::export]
impl SupporterCodeRequest {
    /// Parse `code` and derive the request for `epoch`.
    ///
    /// # Errors
    ///
    /// [`FfiError::AnonToken`] with code `bad-supporter-code` or `internal`.
    #[uniffi::constructor]
    pub fn new(code: String, epoch: u64) -> Result<Self, FfiError> {
        let code = SupporterCode::parse(&code)?;
        let req = code.token_request(epoch)?;
        let blinded = req
            .blinded_elements_b64url()
            .into_iter()
            .next()
            .ok_or_else(|| FfiError::AnonToken("internal: no blinded element".into()))?;
        Ok(Self {
            inner: Mutex::new(Some(req)),
            code_display: code.to_display(),
            epoch,
            blinded,
        })
    }

    /// The epoch this request is for.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The blinded element, base64url without padding.
    pub fn blinded_element_b64url(&self) -> String {
        self.blinded.clone()
    }

    /// The JSON body of `POST /auth/supporter-code/redeem`:
    /// `{"code":..,"epoch":..,"blinded_element":..}`.
    pub fn redeem_body_json(&self) -> String {
        redeem_body_json(&self.code_display, self.epoch, &self.blinded)
    }

    /// Check the proof against `public_key_b64url` (from
    /// `GET /auth/tokens/key/beta-place/{epoch}`) and unblind the evaluation
    /// into the `x-lakko-anon-token` header value.
    ///
    /// # Errors
    ///
    /// [`FfiError::AnonToken`] (`bad-element`, `bad-public-key`,
    /// `proof-verification`, `internal`);
    /// [`FfiError::AnonTokenAlreadyFinalized`] on a second call.
    pub fn finalize(
        &self,
        evaluation_b64url: String,
        proof_b64url: String,
        public_key_b64url: String,
    ) -> Result<String, FfiError> {
        let req = self
            .inner
            .lock()
            .map_err(|_| FfiError::LockPoisoned)?
            .take()
            .ok_or(FfiError::AnonTokenAlreadyFinalized)?;
        let token = req
            .finalize_b64url(&[evaluation_b64url], &proof_b64url, &public_key_b64url)?
            .into_iter()
            .next()
            .ok_or_else(|| FfiError::AnonToken("internal: no token".into()))?;
        Ok(token.to_header_value())
    }
}

/// The redeem body; every field is a display code, a number or base64url, so
/// nothing needs escaping.
pub(crate) fn redeem_body_json(code: &str, epoch: u64, blinded: &str) -> String {
    format!("{{\"code\":\"{code}\",\"epoch\":{epoch},\"blinded_element\":\"{blinded}\"}}")
}

/// The context name a supporter token belongs to (`beta-place`).
#[uniffi::export]
pub fn beta_place_context_name() -> String {
    TokenContext::BetaPlace.name().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_anon_token::{
        decode_header_value, encode_b64url, verify_token, IssuerKey, MasterSecret,
    };
    use lakko_attest::SupportersList;
    use lakko_crypto::signing::SigningKey;

    #[test]
    fn a_code_redeems_into_a_header_that_verifies_and_retries_match() {
        let master = MasterSecret::from_bytes([0x31; 32]);
        let epoch = supporter_code_epoch_at(1_790_000_000);
        let code = SupporterCode::generate().to_display().to_lowercase();
        let req = SupporterCodeRequest::new(code.clone(), epoch).unwrap();
        let again = SupporterCodeRequest::new(code.replace('-', " "), epoch).unwrap();
        assert_eq!(req.blinded_element_b64url(), again.blinded_element_b64url());
        let body: serde_json::Value = serde_json::from_str(&req.redeem_body_json()).unwrap();
        assert_eq!(body["epoch"], epoch);
        assert_eq!(body["code"], code.to_uppercase());

        let key = IssuerKey::derive(&master, TokenContext::BetaPlace, epoch).unwrap();
        let blinded =
            lakko_anon_token::decode_b64url_fixed::<32>(&req.blinded_element_b64url()).unwrap();
        let iss = key.issue(&[blinded]).unwrap();
        let header = again
            .finalize(
                encode_b64url(&iss.evaluations[0]),
                encode_b64url(&iss.proof),
                encode_b64url(&key.public_key()),
            )
            .unwrap();
        let bytes = decode_header_value(&header).unwrap();
        assert!(verify_token(
            &master,
            &bytes,
            TokenContext::BetaPlace,
            epoch * BETA_PLACE_WINDOW_SECS,
            86_400
        )
        .is_ok());
        assert!(matches!(
            again.finalize(String::new(), String::new(), String::new()),
            Err(FfiError::AnonTokenAlreadyFinalized)
        ));
        assert!(matches!(
            supporter_code_normalize("nope".into()),
            Err(FfiError::AnonToken(m)) if m.starts_with("bad-supporter-code")
        ));
    }

    #[test]
    fn a_supporters_list_verifies_and_a_wrong_key_does_not() {
        let sk = SigningKey::from_seed(&[9; 32]);
        let wire = SupportersList {
            list_version: 2,
            signed_at_unix_millis: 5,
            names: vec!["Ada".into()],
        }
        .sign(&sk)
        .unwrap();
        let got =
            verify_supporters_list(wire.clone(), sk.verifying_key().to_bytes().to_vec()).unwrap();
        assert_eq!(got.names, vec!["Ada".to_string()]);
        assert_eq!(got.list_version, 2);
        let other = SigningKey::from_seed(&[8; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        assert!(matches!(
            verify_supporters_list(wire, other),
            Err(FfiError::SupportersList(_))
        ));
    }
}
