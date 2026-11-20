// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface of ADR-0097: supporter codes and the supporters
//! list. Mirror of `crates/lakko-ffi-uniffi/src/supporters.rs`; see there for
//! the exchange.
//!
//! JS shape: `supporterCodeNormalize(text)`, `supporterCodeEpochAt(unixSecs)`
//! (a `bigint`), `new SupporterCodeRequest(code, epoch)` with
//! `blindedElementB64url` / `redeemBodyJson()` /
//! `finalize(evaluation, proof, publicKey)` -> the header value, and
//! `verifySupportersList(bytes, verifyingKey)` -> `VerifiedSupportersList`
//! (`listVersion`, `signedAtMs`, `names`).
//!
//! A refusal throws an `Error` whose message is `"<code>: <reason>"`:
//! `bad-supporter-code`, the `lakko-anon-token` codes of a finalize,
//! `already-finalized`, or `supporters-list` for any list failure.

use std::sync::Mutex;

use lakko_anon_token::{SupporterCode, TokenRequest, BETA_PLACE_WINDOW_SECS};
use lakko_attest::verify_supporters_list as inner_verify;
use lakko_crypto::signing::VerifyingKey;
use wasm_bindgen::prelude::*;

/// A verified supporters list.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSupportersList {
    list_version: u64,
    signed_at_ms: i64,
    names: Vec<String>,
}

#[wasm_bindgen]
impl VerifiedSupportersList {
    /// Monotone version (a `bigint`): keep the highest verified.
    #[wasm_bindgen(getter, js_name = listVersion)]
    pub fn list_version(&self) -> u64 {
        self.list_version
    }

    /// When it was signed, Unix milliseconds (a `bigint`).
    #[wasm_bindgen(getter, js_name = signedAtMs)]
    pub fn signed_at_ms(&self) -> i64 {
        self.signed_at_ms
    }

    /// The names, in file order.
    #[wasm_bindgen(getter)]
    pub fn names(&self) -> Vec<String> {
        self.names.clone()
    }
}

fn verify_inner(bytes: &[u8], verifying_key: &[u8]) -> Result<VerifiedSupportersList, String> {
    let vk = VerifyingKey::from_bytes(verifying_key)
        .map_err(|_| "supporters-list: release key is not a valid Ed25519 key".to_string())?;
    let list = inner_verify(bytes, &vk).map_err(|e| format!("supporters-list: {e}"))?;
    Ok(VerifiedSupportersList {
        list_version: list.list_version,
        signed_at_ms: list.signed_at_unix_millis,
        names: list.names,
    })
}

/// Verify a fetched supporters list under the pinned release key (32
/// bytes). Throws `supporters-list: ...` on any failure (show no list).
#[wasm_bindgen(js_name = verifySupportersList)]
pub fn verify_supporters_list(
    bytes: &[u8],
    verifying_key: &[u8],
) -> Result<VerifiedSupportersList, JsError> {
    verify_inner(bytes, verifying_key).map_err(|m| JsError::new(&m))
}

fn normalize_inner(text: &str) -> Result<String, String> {
    SupporterCode::parse(text)
        .map(|c| c.to_display())
        .map_err(|e| format!("{}: {e}", e.code()))
}

/// The display form of typed supporter-code text. Throws
/// `bad-supporter-code: ...`.
#[wasm_bindgen(js_name = supporterCodeNormalize)]
pub fn supporter_code_normalize(text: &str) -> Result<String, JsError> {
    normalize_inner(text).map_err(|m| JsError::new(&m))
}

/// The `beta-place` epoch at `unixSecs` (30-day window), a `bigint`.
#[wasm_bindgen(js_name = supporterCodeEpochAt)]
#[must_use]
pub fn supporter_code_epoch_at(unix_secs: u64) -> u64 {
    unix_secs / BETA_PLACE_WINDOW_SECS
}

/// The `beta-place` window in seconds (30 days), a `bigint`.
#[wasm_bindgen(js_name = betaPlaceWindowSecs)]
#[must_use]
pub fn beta_place_window_secs() -> u64 {
    BETA_PLACE_WINDOW_SECS
}

/// The redeem request for one supporter code at one epoch.
#[wasm_bindgen]
pub struct SupporterCodeRequest {
    inner: Mutex<Option<TokenRequest>>,
    code_display: String,
    epoch: u64,
    blinded: String,
}

impl SupporterCodeRequest {
    fn new_inner(code: &str, epoch: u64) -> Result<Self, String> {
        let code = SupporterCode::parse(code).map_err(|e| format!("{}: {e}", e.code()))?;
        let req = code
            .token_request(epoch)
            .map_err(|e| format!("{}: {e}", e.code()))?;
        let blinded = req
            .blinded_elements_b64url()
            .into_iter()
            .next()
            .ok_or_else(|| "internal: no blinded element".to_string())?;
        Ok(Self {
            inner: Mutex::new(Some(req)),
            code_display: code.to_display(),
            epoch,
            blinded,
        })
    }

    fn finalize_inner(
        &self,
        evaluation_b64url: &str,
        proof_b64url: &str,
        public_key_b64url: &str,
    ) -> Result<String, String> {
        let req = self
            .inner
            .lock()
            .map_err(|_| "internal: supporter-code request lock poisoned".to_string())?
            .take()
            .ok_or_else(|| {
                "already-finalized: supporter-code request already finalized".to_string()
            })?;
        let token = req
            .finalize_b64url(&[evaluation_b64url], proof_b64url, public_key_b64url)
            .map_err(|e| format!("{}: {e}", e.code()))?
            .into_iter()
            .next()
            .ok_or_else(|| "internal: no token".to_string())?;
        Ok(token.to_header_value())
    }
}

#[wasm_bindgen]
impl SupporterCodeRequest {
    /// Parse `code` and derive the request for `epoch` (a `bigint`). Throws
    /// `bad-supporter-code: ...`.
    #[wasm_bindgen(constructor)]
    pub fn new(code: &str, epoch: u64) -> Result<Self, JsError> {
        Self::new_inner(code, epoch).map_err(|m| JsError::new(&m))
    }

    /// The epoch this request is for (a `bigint`).
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The blinded element, base64url without padding.
    #[wasm_bindgen(getter, js_name = blindedElementB64url)]
    pub fn blinded_element_b64url(&self) -> String {
        self.blinded.clone()
    }

    /// The JSON body of `POST /auth/supporter-code/redeem`.
    #[wasm_bindgen(js_name = redeemBodyJson)]
    pub fn redeem_body_json(&self) -> String {
        format!(
            "{{\"code\":\"{}\",\"epoch\":{},\"blinded_element\":\"{}\"}}",
            self.code_display, self.epoch, self.blinded
        )
    }

    /// Check the proof against `publicKeyB64url` (from
    /// `GET /auth/tokens/key/beta-place/{epoch}`) and return the
    /// `x-lakko-anon-token` header value. Throws a coded message; a second
    /// call throws `already-finalized: ...`.
    pub fn finalize(
        &self,
        evaluation_b64url: &str,
        proof_b64url: &str,
        public_key_b64url: &str,
    ) -> Result<String, JsError> {
        self.finalize_inner(evaluation_b64url, proof_b64url, public_key_b64url)
            .map_err(|m| JsError::new(&m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_anon_token::{
        decode_header_value, encode_b64url, verify_token, IssuerKey, MasterSecret, TokenContext,
    };
    use lakko_attest::SupportersList;
    use lakko_crypto::signing::SigningKey;

    #[test]
    fn a_code_redeems_into_a_header_that_verifies() {
        let master = MasterSecret::from_bytes([0x41; 32]);
        let epoch = supporter_code_epoch_at(1_790_000_000);
        let code = SupporterCode::generate().to_display();
        let req = SupporterCodeRequest::new_inner(&code, epoch).unwrap();
        assert_eq!(
            req.redeem_body_json(),
            format!(
                "{{\"code\":\"{code}\",\"epoch\":{epoch},\"blinded_element\":\"{}\"}}",
                req.blinded_element_b64url()
            )
        );
        let key = IssuerKey::derive(&master, TokenContext::BetaPlace, epoch).unwrap();
        let blinded =
            lakko_anon_token::decode_b64url_fixed::<32>(&req.blinded_element_b64url()).unwrap();
        let iss = key.issue(&[blinded]).unwrap();
        let header = req
            .finalize_inner(
                &encode_b64url(&iss.evaluations[0]),
                &encode_b64url(&iss.proof),
                &encode_b64url(&key.public_key()),
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
        assert!(req
            .finalize_inner("", "", "")
            .unwrap_err()
            .starts_with("already-finalized"));
        assert!(normalize_inner("x")
            .unwrap_err()
            .starts_with("bad-supporter-code"));
    }

    #[test]
    fn a_supporters_list_verifies_and_a_wrong_key_does_not() {
        let sk = SigningKey::from_seed(&[3; 32]);
        let wire = SupportersList {
            list_version: 9,
            signed_at_unix_millis: 1,
            names: vec!["Grace".into()],
        }
        .sign(&sk)
        .unwrap();
        let got = verify_inner(&wire, &sk.verifying_key().to_bytes()).unwrap();
        assert_eq!(got.names(), vec!["Grace".to_string()]);
        let other = SigningKey::from_seed(&[4; 32]).verifying_key().to_bytes();
        assert!(verify_inner(&wire, &other)
            .unwrap_err()
            .starts_with("supporters-list"));
    }
}
