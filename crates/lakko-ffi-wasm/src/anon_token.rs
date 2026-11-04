// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen exposure of the client half of `lakko-anon-token`
//! ([ADR-0054] decision 2, S3). Mirror of
//! `lakko-ffi-uniffi/src/anon_token.rs`; see there for the exchange.
//!
//! JS shape: `new AnonTokenRequest(context, epoch, count)` (epoch a
//! `bigint`), `issueBodyJson()` for `POST /auth/tokens`, then
//! `finalize(evaluations, proof, publicKey)` with the public key fetched from
//! `GET /auth/tokens/key/{context}/{epoch}` - never the one echoed by the
//! issuance response. It returns `AnonToken`s whose `headerValue` goes in the
//! `anonTokenHeaderName()` header of the spending request.
//!
//! A refusal throws an `Error` whose message is `"<code>: <reason>"`, the
//! frozen `AnonTokenError::code()` first - the same text the UniFFI surface
//! carries - or `"already-finalized: ..."` on a second `finalize`.
//!
//! [ADR-0054]: ../../../docs/adr/0054-anti-abuse-without-client-work.md

use std::sync::Mutex;

use lakko_anon_token::{
    epoch_accepted, epoch_at, AnonTokenError, TokenContext, TokenRequest, DEFAULT_WINDOW_SECS,
    MAX_BATCH, TOKEN_HEADER,
};
use wasm_bindgen::prelude::*;

/// The thrown message for a refusal (`"<code>: <reason>"`).
fn message(e: AnonTokenError) -> String {
    format!("{}: {e}", e.code())
}

const ALREADY_FINALIZED: &str = "already-finalized: anonymous-token request already finalized";

/// One finalized token, ready to spend.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnonToken {
    context: String,
    epoch: u64,
    header_value: String,
}

#[wasm_bindgen]
impl AnonToken {
    /// The context name it pays for (`discovery-lookup`, `ds-knock`, ...).
    #[wasm_bindgen(getter)]
    pub fn context(&self) -> String {
        self.context.clone()
    }

    /// The issuance epoch (a `bigint`). Spend it while
    /// `anonTokenIsLive(epoch, current)`.
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The `x-lakko-anon-token` header value (base64url, no padding). Store
    /// it as is; it is the whole token.
    #[wasm_bindgen(getter, js_name = headerValue)]
    pub fn header_value(&self) -> String {
        self.header_value.clone()
    }
}

/// A batch of blinded token requests awaiting the issuer. Holds the blinding
/// factors: keep it in memory only, and finalize it once.
#[wasm_bindgen]
pub struct AnonTokenRequest {
    inner: Mutex<Option<TokenRequest>>,
    context: TokenContext,
    epoch: u64,
    blinded: Vec<String>,
}

impl AnonTokenRequest {
    fn new_inner(context: &str, epoch: u64, count: u32) -> Result<Self, AnonTokenError> {
        let context = TokenContext::from_name(context)?;
        let count = usize::try_from(count).map_err(|_| AnonTokenError::BadBatchSize)?;
        let req = TokenRequest::new(context, epoch, count)?;
        let blinded = req.blinded_elements_b64url();
        Ok(Self {
            inner: Mutex::new(Some(req)),
            context,
            epoch,
            blinded,
        })
    }

    fn finalize_inner(
        &self,
        evaluations_b64url: &[String],
        proof_b64url: &str,
        public_key_b64url: &str,
    ) -> Result<Vec<AnonToken>, String> {
        let req = self
            .inner
            .lock()
            .map_err(|_| "internal: anonymous-token request lock poisoned".to_string())?
            .take()
            .ok_or_else(|| ALREADY_FINALIZED.to_string())?;
        let tokens = req
            .finalize_b64url(evaluations_b64url, proof_b64url, public_key_b64url)
            .map_err(message)?;
        Ok(tokens
            .into_iter()
            .map(|t| AnonToken {
                context: t.context.name().to_string(),
                epoch: t.epoch,
                header_value: t.to_header_value(),
            })
            .collect())
    }
}

#[wasm_bindgen]
impl AnonTokenRequest {
    /// Start a request for `count` tokens of `context` (a context name) at
    /// `epoch`. Throws `unknown-context: ...` or `bad-batch-size: ...`.
    #[wasm_bindgen(constructor)]
    pub fn new(context: &str, epoch: u64, count: u32) -> Result<Self, JsError> {
        Self::new_inner(context, epoch, count).map_err(|e| JsError::new(&message(e)))
    }

    /// The context name this request is for.
    #[wasm_bindgen(getter)]
    pub fn context(&self) -> String {
        self.context.name().to_string()
    }

    /// The epoch this request is for (a `bigint`).
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The number of tokens requested.
    #[wasm_bindgen(getter)]
    pub fn count(&self) -> u32 {
        u32::try_from(self.blinded.len()).unwrap_or(u32::MAX)
    }

    /// The blinded elements, base64url without padding, in order.
    #[wasm_bindgen(js_name = blindedElementsB64url)]
    pub fn blinded_elements_b64url(&self) -> Vec<String> {
        self.blinded.clone()
    }

    /// The JSON body of `POST /auth/tokens`:
    /// `{"context":..,"epoch":..,"blinded_elements":[..]}`.
    #[wasm_bindgen(js_name = issueBodyJson)]
    pub fn issue_body_json(&self) -> String {
        issue_body_json(self.context, self.epoch, &self.blinded)
    }

    /// Check the issuer's proof against `publicKeyB64url` (from the key
    /// route) and unblind every evaluation into a token. All or nothing.
    /// Throws `proof-verification: ...` when the issuer did not evaluate under
    /// that key (discard the batch), `count-mismatch` / `bad-element` /
    /// `bad-public-key` for a malformed response, `already-finalized` on a
    /// second call.
    pub fn finalize(
        &self,
        evaluations_b64url: Vec<String>,
        proof_b64url: &str,
        public_key_b64url: &str,
    ) -> Result<Vec<AnonToken>, JsError> {
        self.finalize_inner(&evaluations_b64url, proof_b64url, public_key_b64url)
            .map_err(|m| JsError::new(&m))
    }
}

fn issue_body_json(context: TokenContext, epoch: u64, blinded: &[String]) -> String {
    // A fixed context name and base64url strings: nothing to escape.
    let elements = blinded
        .iter()
        .map(|b| format!("\"{b}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"context\":\"{}\",\"epoch\":{epoch},\"blinded_elements\":[{elements}]}}",
        context.name()
    )
}

/// The request header a spent token travels in (`x-lakko-anon-token`).
#[wasm_bindgen(js_name = anonTokenHeaderName)]
#[must_use]
pub fn anon_token_header_name() -> String {
    TOKEN_HEADER.to_string()
}

/// Every context name, in wire order: `discovery-lookup`,
/// `discovery-register`, `invite-consume`, `ds-knock`, `push-register`,
/// `channel-subscribe`, `directory-search`, `vault-claim`, `blob-upload`,
/// `blob-read`, `mailbox-write`, `call-join`, `circle-claim`, `channel-react`,
/// `channel-comment`.
#[wasm_bindgen(js_name = anonTokenContexts)]
#[must_use]
pub fn anon_token_contexts() -> Vec<String> {
    TokenContext::ALL
        .iter()
        .map(|c| c.name().to_string())
        .collect()
}

/// The default epoch window in seconds (one day), a `bigint`.
#[wasm_bindgen(js_name = anonTokenDefaultWindowSecs)]
#[must_use]
pub fn anon_token_default_window_secs() -> u64 {
    DEFAULT_WINDOW_SECS
}

/// The largest batch one request may ask for.
#[wasm_bindgen(js_name = anonTokenMaxBatch)]
#[must_use]
pub fn anon_token_max_batch() -> u32 {
    u32::try_from(MAX_BATCH).unwrap_or(u32::MAX)
}

/// The epoch containing `unixSecs` under `windowSecs` (both `bigint`).
/// Throws `invalid-window: ...` for a zero window.
#[wasm_bindgen(js_name = anonTokenEpochAt)]
pub fn anon_token_epoch_at(unix_secs: u64, window_secs: u64) -> Result<u64, JsError> {
    epoch_at(unix_secs, window_secs).map_err(|e| JsError::new(&message(e)))
}

/// Whether a token of `tokenEpoch` is still spendable when the current epoch
/// is `currentEpoch` (its own epoch and the next one).
#[wasm_bindgen(js_name = anonTokenIsLive)]
#[must_use]
pub fn anon_token_is_live(token_epoch: u64, current_epoch: u64) -> bool {
    epoch_accepted(token_epoch, current_epoch)
}

#[cfg(test)]
mod tests {
    //! Host-side: the success paths and the inner refusals. A thrown
    //! `JsError` is a wasm-bindgen import that panics off wasm32, so the
    //! refusal paths are asserted through the `_inner` functions.

    use super::*;
    use lakko_anon_token::{
        decode_b64url_fixed, decode_header_value, encode_b64url, verify_token, IssuerKey,
        MasterSecret,
    };

    const W: u64 = DEFAULT_WINDOW_SECS;

    fn issue(req: &AnonTokenRequest, key: &IssuerKey) -> (Vec<String>, String) {
        let blinded: Vec<[u8; 32]> = req
            .blinded_elements_b64url()
            .iter()
            .map(|b| decode_b64url_fixed::<32>(b).unwrap())
            .collect();
        let issuance = key.issue(&blinded).unwrap();
        (
            issuance
                .evaluations
                .iter()
                .map(|e| encode_b64url(e))
                .collect(),
            encode_b64url(&issuance.proof),
        )
    }

    #[test]
    fn a_batch_round_trips_against_the_issuer_and_every_token_verifies() {
        let master = MasterSecret::from_bytes([0x44; 32]);
        let epoch = anon_token_epoch_at(1234 * W + 1, W).unwrap();
        let req = AnonTokenRequest::new("invite-consume", epoch, 3).unwrap();
        assert_eq!(req.count(), 3);
        let body = req.issue_body_json();
        assert!(body.starts_with("{\"context\":\"invite-consume\",\"epoch\":1234,"));
        let key = IssuerKey::derive(&master, TokenContext::InviteConsume, epoch).unwrap();
        let (evals, proof) = issue(&req, &key);
        let tokens = req
            .finalize(evals, &proof, &encode_b64url(&key.public_key()))
            .unwrap();
        assert_eq!(tokens.len(), 3);
        for t in tokens {
            assert_eq!(t.context(), "invite-consume");
            assert_eq!(t.epoch(), epoch);
            let bytes = decode_header_value(&t.header_value()).unwrap();
            assert!(
                verify_token(&master, &bytes, TokenContext::InviteConsume, epoch * W, W).is_ok()
            );
        }
    }

    #[test]
    fn a_response_under_another_key_is_refused_and_the_request_is_spent() {
        let key = IssuerKey::derive(
            &MasterSecret::from_bytes([0x44; 32]),
            TokenContext::DiscoveryLookup,
            8,
        )
        .unwrap();
        let other = IssuerKey::derive(
            &MasterSecret::from_bytes([0x45; 32]),
            TokenContext::DiscoveryLookup,
            8,
        )
        .unwrap();
        let req = AnonTokenRequest::new_inner("discovery-lookup", 8, 2).unwrap();
        let (evals, proof) = issue(&req, &other);
        let err = req
            .finalize_inner(&evals, &proof, &encode_b64url(&key.public_key()))
            .unwrap_err();
        assert!(err.starts_with("proof-verification: "), "{err}");
        let again = req.finalize_inner(&[], "", "").unwrap_err();
        assert!(again.starts_with("already-finalized: "), "{again}");
    }

    #[test]
    fn bad_arguments_are_refused_with_their_codes() {
        assert_eq!(
            AnonTokenRequest::new_inner("nope", 1, 1).err(),
            Some(AnonTokenError::UnknownContext)
        );
        assert_eq!(
            AnonTokenRequest::new_inner("ds-knock", 1, 0).err(),
            Some(AnonTokenError::BadBatchSize)
        );
        assert_eq!(
            message(AnonTokenError::InvalidWindow),
            "invalid-window: epoch window must be at least one second"
        );
        assert_eq!(anon_token_header_name(), "x-lakko-anon-token");
        assert_eq!(
            anon_token_contexts(),
            [
                "discovery-lookup",
                "discovery-register",
                "invite-consume",
                "ds-knock",
                "push-register",
                "channel-subscribe",
                "directory-search",
                "vault-claim",
                "blob-upload",
                "blob-read",
                "mailbox-write",
                "call-join",
                "circle-claim",
                "channel-react",
                "channel-comment",
            ]
        );
        // Every listed name is one a request can be built for.
        for name in anon_token_contexts() {
            let req = AnonTokenRequest::new_inner(&name, 1, 1).unwrap();
            assert_eq!(req.context(), name);
        }
        assert_eq!(anon_token_max_batch(), 32);
        assert!(anon_token_is_live(9, 10));
        assert!(!anon_token_is_live(8, 10));
    }
}
