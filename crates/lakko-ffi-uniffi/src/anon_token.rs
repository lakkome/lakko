// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI exposure of the client half of `lakko-anon-token` ([ADR-0054]
//! decision 2, S3).
//!
//! A client keeps a small pool of anonymous tokens per context, drawn from
//! `services/auth` with its session and spent one per anonymous action at
//! discovery, the DS or push-gw. This module is the cryptography of that
//! exchange; the pool, the network and the timing are the client's.
//!
//! The exchange, in order:
//!
//! 1. `AnonTokenRequest(context, epoch, count)` - `epoch` is
//!    `anonTokenEpochAt(nowUnixSecs, windowSecs)`; `count` in
//!    `1..=anonTokenMaxBatch()`.
//! 2. `POST /auth/tokens` (bearer) with `issueBodyJson()`. A `400
//!    epoch-not-issuable` carries `current_epoch`: rebuild the request for it.
//! 3. `GET /auth/tokens/key/{context}/{epoch}` - the public key, fetched from
//!    that route and **never** taken from the issuance response (an issuer
//!    that served a per-account key could link issuance to spending; the
//!    route is the key everyone gets).
//! 4. `finalize(evaluations, proof, publicKey)` - all or nothing; a proof
//!    that does not verify under the key yields no token.
//! 5. Spend one: header `anonTokenHeaderName()` = `AnonToken.headerValue`,
//!    while `anonTokenIsLive(token.epoch, currentEpoch)`.
//!
//! [ADR-0054]: ../../../docs/adr/0054-anti-abuse-without-client-work.md

use std::sync::Mutex;

use lakko_anon_token::{
    epoch_accepted, epoch_at, AnonTokenError, TokenContext, TokenRequest, DEFAULT_WINDOW_SECS,
    MAX_BATCH, TOKEN_HEADER,
};

use crate::FfiError;

impl From<AnonTokenError> for FfiError {
    fn from(e: AnonTokenError) -> Self {
        Self::AnonToken(format!("{}: {e}", e.code()))
    }
}

/// One finalized token, ready to spend.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AnonToken {
    /// The context name it pays for (`discovery-lookup`, `ds-knock`, ...).
    pub context: String,
    /// The issuance epoch. Spend it while `anonTokenIsLive(epoch, current)`.
    pub epoch: u64,
    /// The `x-lakko-anon-token` header value (base64url, no padding). Store
    /// it as is; it is the whole token.
    pub header_value: String,
}

/// A batch of blinded token requests awaiting the issuer. Holds the blinding
/// factors: keep it in memory only, and finalize it once.
#[derive(uniffi::Object)]
pub struct AnonTokenRequest {
    /// `Mutex<Option<..>>` so `finalize` (which consumes the inner request)
    /// can take the slot; a second call is refused.
    inner: Mutex<Option<TokenRequest>>,
    context: TokenContext,
    epoch: u64,
    blinded: Vec<String>,
}

#[uniffi::export]
impl AnonTokenRequest {
    /// Start a request for `count` tokens of `context` (a context name) at
    /// `epoch`: a fresh random nonce per token, each input blinded.
    ///
    /// # Errors
    ///
    /// [`FfiError::AnonToken`] with code `unknown-context` or
    /// `bad-batch-size` (`count` outside `1..=anonTokenMaxBatch()`).
    #[uniffi::constructor]
    pub fn new(context: String, epoch: u64, count: u32) -> Result<Self, FfiError> {
        let context = TokenContext::from_name(&context)?;
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

    /// The context name this request is for.
    pub fn context(&self) -> String {
        self.context.name().to_string()
    }

    /// The epoch this request is for.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The number of tokens requested.
    pub fn count(&self) -> u32 {
        u32::try_from(self.blinded.len()).unwrap_or(u32::MAX)
    }

    /// The blinded elements, base64url without padding, in order.
    pub fn blinded_elements_b64url(&self) -> Vec<String> {
        self.blinded.clone()
    }

    /// The JSON body of `POST /auth/tokens`:
    /// `{"context":..,"epoch":..,"blinded_elements":[..]}`. The epoch is always
    /// sent, so an issuer at another epoch refuses (`400 epoch-not-issuable`)
    /// instead of evaluating under a key the proof check would then reject.
    pub fn issue_body_json(&self) -> String {
        issue_body_json(self.context, self.epoch, &self.blinded)
    }

    /// Check the issuer's proof against `public_key_b64url` (from the key
    /// route) and unblind every evaluation into a token. All or nothing.
    ///
    /// # Errors
    ///
    /// [`FfiError::AnonToken`] with code `count-mismatch`, `bad-element`,
    /// `bad-public-key`, `proof-verification` (the issuer did not evaluate
    /// under this key - discard the batch) or `internal`;
    /// [`FfiError::AnonTokenAlreadyFinalized`] on a second call.
    pub fn finalize(
        &self,
        evaluations_b64url: Vec<String>,
        proof_b64url: String,
        public_key_b64url: String,
    ) -> Result<Vec<AnonToken>, FfiError> {
        let req = self
            .inner
            .lock()
            .map_err(|_| FfiError::LockPoisoned)?
            .take()
            .ok_or(FfiError::AnonTokenAlreadyFinalized)?;
        let tokens = req.finalize_b64url(&evaluations_b64url, &proof_b64url, &public_key_b64url)?;
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

/// The issuance body, shared with the tests.
pub(crate) fn issue_body_json(context: TokenContext, epoch: u64, blinded: &[String]) -> String {
    // Every string here is a fixed context name or base64url: nothing to
    // escape.
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
#[uniffi::export]
pub fn anon_token_header_name() -> String {
    TOKEN_HEADER.to_string()
}

/// Every context name, in wire order: `discovery-lookup`,
/// `discovery-register`, `invite-consume`, `ds-knock`, `push-register`,
/// `channel-subscribe`, `directory-search`, `vault-claim`, `blob-upload`,
/// `blob-read`, `mailbox-write`, `call-join`, `circle-claim`, `channel-react`,
/// `channel-comment`, `report`. These are the contexts a session draws from
/// `POST /auth/tokens`; `beta-place` (ADR-0097) is not among them, since only
/// a supporter code obtains it (`SupporterCodeRequest`).
#[uniffi::export]
pub fn anon_token_contexts() -> Vec<String> {
    TokenContext::ALL
        .iter()
        .filter(|c| c.is_session_issuable())
        .map(|c| c.name().to_string())
        .collect()
}

/// The default epoch window (one day). The services use this unless the
/// operator changed `LAKKO_ANON_TOKEN_WINDOW_SECS`; the issuance response's
/// `epoch` / `epoch_ends_at` are authoritative.
#[uniffi::export]
pub fn anon_token_default_window_secs() -> u64 {
    DEFAULT_WINDOW_SECS
}

/// The largest batch one request may ask for.
#[uniffi::export]
pub fn anon_token_max_batch() -> u32 {
    u32::try_from(MAX_BATCH).unwrap_or(u32::MAX)
}

/// The epoch containing `unix_secs` under `window_secs`.
///
/// # Errors
///
/// [`FfiError::AnonToken`] `invalid-window` for a zero window.
#[uniffi::export]
pub fn anon_token_epoch_at(unix_secs: u64, window_secs: u64) -> Result<u64, FfiError> {
    Ok(epoch_at(unix_secs, window_secs)?)
}

/// Whether a token of `token_epoch` is still spendable when the current epoch
/// is `current_epoch` (its own epoch and the next one). Drop it otherwise.
#[uniffi::export]
pub fn anon_token_is_live(token_epoch: u64, current_epoch: u64) -> bool {
    epoch_accepted(token_epoch, current_epoch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_anon_token::{
        decode_header_value, encode_b64url, verify_token, IssuerKey, MasterSecret,
    };

    const W: u64 = DEFAULT_WINDOW_SECS;

    fn master() -> MasterSecret {
        MasterSecret::from_bytes([0x33; 32])
    }

    /// What `services/auth` does with the body: parse, decode, evaluate.
    fn issue(req: &AnonTokenRequest, key: &IssuerKey) -> (Vec<String>, String) {
        let body: serde_json::Value = serde_json::from_str(&req.issue_body_json()).unwrap();
        assert_eq!(body["context"], req.context());
        assert_eq!(body["epoch"], req.epoch());
        let blinded: Vec<[u8; 32]> = body["blinded_elements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| lakko_anon_token::decode_b64url_fixed::<32>(v.as_str().unwrap()).unwrap())
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
        let epoch = anon_token_epoch_at(900 * W + 5, W).unwrap();
        assert_eq!(epoch, 900);
        let req = AnonTokenRequest::new("ds-knock".into(), epoch, 4).unwrap();
        assert_eq!(req.count(), 4);
        let key = IssuerKey::derive(&master(), TokenContext::DsKnock, epoch).unwrap();
        let (evals, proof) = issue(&req, &key);
        let tokens = req
            .finalize(evals, proof, encode_b64url(&key.public_key()))
            .unwrap();
        assert_eq!(tokens.len(), 4);
        for t in &tokens {
            assert_eq!(t.context, "ds-knock");
            assert_eq!(t.epoch, epoch);
            assert!(anon_token_is_live(t.epoch, epoch + 1));
            assert!(!anon_token_is_live(t.epoch, epoch + 2));
            let bytes = decode_header_value(&t.header_value).unwrap();
            assert!(verify_token(&master(), &bytes, TokenContext::DsKnock, epoch * W, W).is_ok());
        }
    }

    #[test]
    fn a_response_under_another_key_is_refused_and_yields_nothing() {
        let req = AnonTokenRequest::new("push-register".into(), 3, 2).unwrap();
        let key = IssuerKey::derive(&master(), TokenContext::PushRegister, 3).unwrap();
        let other = IssuerKey::derive(
            &MasterSecret::from_bytes([0x34; 32]),
            TokenContext::PushRegister,
            3,
        )
        .unwrap();
        let (evals, proof) = issue(&req, &other);
        let err = req
            .finalize(evals, proof, encode_b64url(&key.public_key()))
            .unwrap_err();
        assert!(matches!(&err, FfiError::AnonToken(m) if m.starts_with("proof-verification: ")));
        // The request is consumed: a second call cannot retry it.
        assert!(matches!(
            req.finalize(vec![], String::new(), String::new()),
            Err(FfiError::AnonTokenAlreadyFinalized)
        ));
    }

    #[test]
    fn bad_arguments_are_refused_with_their_codes() {
        let code = |r: Result<AnonTokenRequest, FfiError>| match r {
            Err(FfiError::AnonToken(m)) => m.split(':').next().unwrap_or_default().to_string(),
            _ => String::from("ok"),
        };
        assert_eq!(
            code(AnonTokenRequest::new("nope".into(), 1, 1)),
            "unknown-context"
        );
        assert_eq!(
            code(AnonTokenRequest::new("ds-knock".into(), 1, 0)),
            "bad-batch-size"
        );
        assert_eq!(
            code(AnonTokenRequest::new(
                "ds-knock".into(),
                1,
                anon_token_max_batch() + 1
            )),
            "bad-batch-size"
        );
        assert!(matches!(
            anon_token_epoch_at(1, 0),
            Err(FfiError::AnonToken(m)) if m.starts_with("invalid-window: ")
        ));
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
                "report",
                "rendezvous-write",
            ]
        );
        // Every listed name is one a request can be built for.
        for name in anon_token_contexts() {
            let req = AnonTokenRequest::new(name.clone(), 1, 1).unwrap();
            assert_eq!(req.context(), name);
        }
    }
}
