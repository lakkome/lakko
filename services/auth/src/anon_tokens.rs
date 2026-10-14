// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Anonymous-token issuance ([ADR-0054] decision 2).
//!
//! `POST /auth/tokens` (bearer) evaluates a batch of blinded elements under the
//! `(context, epoch)` issuance key and counts the batch against the account's
//! quota for that `(context, epoch)`. The client unblinds the evaluations into
//! tokens it later spends at `services/discovery`, `services/ds` or
//! `services/push-gw`, which verify them with the same master secret and never
//! learn which account they came from. `GET /auth/tokens/key/{context}/{epoch}`
//! (anonymous) publishes the public key a client checks the DLEQ proof against;
//! a client must take the key from there rather than trust the one echoed in
//! the issuance response, or an issuer could tag it with a key of its own.
//!
//! What this service learns: an account, a context, an epoch and a count -
//! what the session already tells it. It never sees a nonce, a token or a
//! VOPRF output, and it stores only the count
//! (`anon_token_issuance`, migration `0019`).
//!
//! # Configuration
//!
//! - `LAKKO_ANON_TOKEN_MASTER_SECRET` - 64 hex characters, shared with every
//!   verifying service. Unset: both routes answer 503 and startup logs a
//!   warning (dormant). Malformed: startup fails.
//! - `LAKKO_ANON_TOKEN_WINDOW_SECS` - the epoch window, default 86400 (one
//!   day). Must match every verifier. Malformed or zero: startup fails.
//! - `LAKKO_ANON_TOKEN_QUOTA_<CONTEXT>` - tokens per account per epoch, one
//!   variable per context (`DISCOVERY_LOOKUP`, `DISCOVERY_REGISTER`,
//!   `INVITE_CONSUME`, `DS_KNOCK`, `PUSH_REGISTER`, `CHANNEL_SUBSCRIBE`,
//!   `DIRECTORY_SEARCH`, `VAULT_CLAIM`, `BLOB_UPLOAD`, `BLOB_READ`). `0`
//!   stops issuance for that context. Malformed: startup fails.
//!
//! # Failure posture
//!
//! Over quota is `429 anon-token-quota-exhausted` with `Retry-After` set to the
//! end of the epoch - never a 401, so a client never treats it as a lost
//! session. A store error is a 500 and issues nothing: the quota fails closed
//! (it is the rationing point the scheme rests on).
//!
//! [ADR-0054]: ../../../docs/adr/0054-anti-abuse-without-client-work.md

use std::sync::Arc;

use anyhow::Context as _;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::Extension;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_anon_token::{
    validate_blinded_batch, AnonTokenError, IssuerKey, TokenContext, BLINDED_ELEMENT_LEN, MAX_BATCH,
};
use lakko_anon_token_server::{reserve_issuance, AnonTokenServerConfig, IssuanceDecision};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::webauthn::AuthenticatedAccount;
use crate::AppState;

/// Default quotas, tokens per account per epoch (one day by default).
///
/// Chosen, not measured (ADR-0054 open question 3): a generous ceiling over
/// "a handful of times a day" for each action, low enough that one account is
/// not a meaningful source of tokens for a flood. A lookup may cost two tokens
/// (OPRF evaluation + lookup) depending on how S3 spends them.
///
/// `channel-subscribe` and `directory-search` (ADR-0059 sections 4 and 5)
/// are chosen, not measured, too (ADR-0059 open question 4): a person
/// subscribes to a few channels a day and searches the directory a few
/// times; a count inflated by one account moves by at most its quota.
///
/// `vault-claim`, `blob-upload`, `blob-read` and the raised `ds-knock` are
/// chosen, not measured, as well:
///
/// - `vault-claim` (ADR-0064 section 1): a vault is claimed once per
///   conversation or channel, so the quota bounds how many conversations an
///   account can open a vault for in a day.
/// - `blob-upload` and `blob-read` (ADR-0065 section 5): one token buys an
///   hour's pass, a device takes a pass at most once an hour (24 a day), and
///   an account has several devices; 96 covers four devices holding a pass
///   around the clock.
/// - `ds-knock`: ADR-0064 has a signed-in device knock once per conversation
///   it is not in yet, so the earlier 20 a day left most of an account's
///   conversations without a knock.
const DEFAULT_QUOTAS: [(TokenContext, u32); CONTEXT_COUNT] = [
    (TokenContext::DiscoveryLookup, 100),
    (TokenContext::DiscoveryRegister, 10),
    (TokenContext::InviteConsume, 30),
    (TokenContext::DsKnock, 200),
    (TokenContext::PushRegister, 100),
    (TokenContext::ChannelSubscribe, 20),
    (TokenContext::DirectorySearch, 30),
    (TokenContext::VaultClaim, 100),
    (TokenContext::BlobUpload, 96),
    (TokenContext::BlobRead, 96),
];

/// How many contexts exist; the quota table has one row per context.
const CONTEXT_COUNT: usize = TokenContext::ALL.len();

/// The env var carrying the quota for one context.
fn quota_var(context: TokenContext) -> String {
    format!(
        "LAKKO_ANON_TOKEN_QUOTA_{}",
        context.name().replace('-', "_").to_ascii_uppercase()
    )
}

/// Issuance configuration: the shared master + window, and the quotas.
#[derive(Debug, Clone)]
pub(crate) struct AnonTokenIssuer {
    pub(crate) server: AnonTokenServerConfig,
    quotas: [(TokenContext, u32); CONTEXT_COUNT],
}

impl AnonTokenIssuer {
    /// Build from explicit parts (tests).
    #[cfg(test)]
    pub(crate) const fn new(
        server: AnonTokenServerConfig,
        quotas: [(TokenContext, u32); CONTEXT_COUNT],
    ) -> Self {
        Self { server, quotas }
    }

    /// The quota for `context`.
    fn quota(&self, context: TokenContext) -> u32 {
        self.quotas
            .iter()
            .find(|(c, _)| *c == context)
            .map_or(0, |(_, q)| *q)
    }
}

/// Load the issuance configuration from the environment.
///
/// `Ok(None)` when `LAKKO_ANON_TOKEN_MASTER_SECRET` is unset (dormant).
///
/// # Errors
///
/// A malformed master secret, window or quota.
pub(crate) fn load_anon_tokens() -> anyhow::Result<Option<AnonTokenIssuer>> {
    let Some(server) = AnonTokenServerConfig::from_env()? else {
        return Ok(None);
    };
    let quotas = quotas_from(|var| std::env::var(var).ok())?;
    Ok(Some(AnonTokenIssuer { server, quotas }))
}

/// The quota table: the defaults, each replaced by its
/// `LAKKO_ANON_TOKEN_QUOTA_<CONTEXT>` value when `lookup` returns one. An
/// unset or blank variable keeps the default.
///
/// # Errors
///
/// A value that is not a non-negative integer.
fn quotas_from(
    lookup: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<[(TokenContext, u32); CONTEXT_COUNT]> {
    let mut quotas = DEFAULT_QUOTAS;
    for (context, quota) in &mut quotas {
        let var = quota_var(*context);
        if let Some(raw) = lookup(&var) {
            let raw = raw.trim();
            if !raw.is_empty() {
                *quota = raw
                    .parse()
                    .with_context(|| format!("{var} must be a non-negative integer"))?;
            }
        }
    }
    Ok(quotas)
}

/// Log the startup state, loudly when dormant.
pub(crate) fn log_startup(issuer: Option<&AnonTokenIssuer>) {
    if let Some(i) = issuer {
        tracing::info!(
            window_secs = i.server.window_secs,
            quotas = ?i.quotas.iter().map(|(c, q)| (c.name(), *q)).collect::<Vec<_>>(),
            "anonymous-token issuance enabled"
        );
    } else {
        tracing::warn!(
            "anonymous-token issuance DISABLED: LAKKO_ANON_TOKEN_MASTER_SECRET is not set; \
             /auth/tokens answers 503 and no client can obtain tokens"
        );
    }
}

// ----------------------------------------------------------------
// Errors
// ----------------------------------------------------------------

/// Errors on the token routes, each with a stable machine-readable code.
#[derive(Debug)]
pub(crate) enum TokenApiError {
    /// No master secret configured (503 `anon-tokens-disabled`).
    Disabled,
    /// Not a known context (400 `unknown-context`).
    UnknownContext,
    /// Empty or oversized batch (400 `bad-batch-size`).
    BadBatchSize,
    /// A blinded element that is not valid base64url or not a valid point
    /// (400 `bad-blinded-element`).
    BadBlindedElement,
    /// An explicit epoch that is not the current one (400
    /// `epoch-not-issuable`).
    EpochNotIssuable { current_epoch: u64 },
    /// A key requested for an epoch that is not the previous, current or next
    /// one (404 `epoch-out-of-range`).
    EpochOutOfRange,
    /// Over quota (429 `anon-token-quota-exhausted`).
    QuotaExhausted {
        remaining: u32,
        retry_after_secs: u64,
    },
    /// Store or unreachable crypto failure (500 `internal`).
    Internal,
}

impl IntoResponse for TokenApiError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::Disabled => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({ "error": "anon-tokens-disabled" }),
            ),
            Self::UnknownContext => (
                StatusCode::BAD_REQUEST,
                json!({ "error": "unknown-context" }),
            ),
            Self::BadBatchSize => (
                StatusCode::BAD_REQUEST,
                json!({ "error": "bad-batch-size", "max_batch": MAX_BATCH }),
            ),
            Self::BadBlindedElement => (
                StatusCode::BAD_REQUEST,
                json!({ "error": "bad-blinded-element" }),
            ),
            Self::EpochNotIssuable { current_epoch } => (
                StatusCode::BAD_REQUEST,
                json!({ "error": "epoch-not-issuable", "current_epoch": current_epoch }),
            ),
            Self::EpochOutOfRange => (
                StatusCode::NOT_FOUND,
                json!({ "error": "epoch-out-of-range" }),
            ),
            Self::QuotaExhausted {
                remaining,
                retry_after_secs,
            } => {
                let mut resp = (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(json!({
                        "error": "anon-token-quota-exhausted",
                        "remaining": remaining,
                        "retry_after_secs": retry_after_secs,
                    })),
                )
                    .into_response();
                if let Ok(v) = HeaderValue::from_str(&retry_after_secs.to_string()) {
                    resp.headers_mut().insert(header::RETRY_AFTER, v);
                }
                return resp;
            },
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "internal" }),
            ),
        };
        (status, Json(body)).into_response()
    }
}

fn issuer(state: &AppState) -> Result<&Arc<AnonTokenIssuer>, TokenApiError> {
    state.anon_tokens.as_ref().ok_or(TokenApiError::Disabled)
}

// ----------------------------------------------------------------
// POST /auth/tokens
// ----------------------------------------------------------------

/// Request body for `POST /auth/tokens`.
#[derive(Deserialize)]
pub(crate) struct IssueReq {
    /// The context name (`discovery-lookup`, `ds-knock`, ...).
    pub context: String,
    /// Optional; when present it must be the current epoch. A client normally
    /// omits it and reads the epoch from the response.
    #[serde(default)]
    pub epoch: Option<u64>,
    /// Base64url (no padding) of each 32-byte blinded element, at most
    /// `MAX_BATCH`.
    pub blinded_elements: Vec<String>,
}

/// Response body for `POST /auth/tokens`.
#[derive(Serialize)]
pub(crate) struct IssueRes {
    /// The context name, echoed.
    pub context: &'static str,
    /// The epoch the evaluations belong to (the current one).
    pub epoch: u64,
    /// Base64url evaluated elements, same order as the request.
    pub evaluations: Vec<String>,
    /// Base64url batched DLEQ proof.
    pub proof: String,
    /// Base64url public key for `(context, epoch)` - informational; a client
    /// checks the proof against the key it fetched from the key route.
    pub public_key: String,
    /// Tokens left for this account in this `(context, epoch)`.
    pub remaining: u32,
    /// Unix second at which the epoch (and the quota) rolls over.
    pub epoch_ends_at: u64,
}

/// `POST /auth/tokens` - evaluate a blinded batch and count it against the
/// account's quota.
///
/// Order: validate everything that can be checked without the store (context,
/// epoch, batch size, every element's encoding), then reserve the quota, then
/// evaluate. A malformed request never consumes quota.
pub(crate) async fn issue_tokens(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<IssueReq>,
) -> Result<Json<IssueRes>, TokenApiError> {
    let issuer = issuer(&state)?;
    let context =
        TokenContext::from_name(&req.context).map_err(|_| TokenApiError::UnknownContext)?;
    let epoch = issuer.server.current_epoch();
    if let Some(e) = req.epoch {
        if e != epoch {
            return Err(TokenApiError::EpochNotIssuable {
                current_epoch: epoch,
            });
        }
    }
    if req.blinded_elements.is_empty() || req.blinded_elements.len() > MAX_BATCH {
        return Err(TokenApiError::BadBatchSize);
    }
    let blinded = req
        .blinded_elements
        .iter()
        .map(|b64| {
            let bytes = URL_SAFE_NO_PAD
                .decode(b64)
                .map_err(|_| TokenApiError::BadBlindedElement)?;
            <[u8; BLINDED_ELEMENT_LEN]>::try_from(bytes.as_slice())
                .map_err(|_| TokenApiError::BadBlindedElement)
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Point validation before the quota is touched.
    validate_blinded_batch(&blinded).map_err(|e| match e {
        AnonTokenError::BadBatchSize => TokenApiError::BadBatchSize,
        _ => TokenApiError::BadBlindedElement,
    })?;

    let count = u32::try_from(blinded.len()).map_err(|_| TokenApiError::BadBatchSize)?;
    let remaining = match reserve_issuance(
        &state.db,
        &auth.0,
        context,
        epoch,
        count,
        issuer.quota(context),
    )
    .await
    {
        Ok(IssuanceDecision::Granted { remaining }) => remaining,
        Ok(IssuanceDecision::Exhausted { remaining }) => {
            let now = lakko_anon_token_server::now_unix_secs();
            return Err(TokenApiError::QuotaExhausted {
                remaining,
                retry_after_secs: issuer
                    .server
                    .current_epoch_ends_at()
                    .saturating_sub(now)
                    .max(1),
            });
        },
        Err(e) => {
            tracing::error!(error = %e, "anon-token issuance quota store error; refusing");
            return Err(TokenApiError::Internal);
        },
    };

    let key = IssuerKey::derive(&issuer.server.master, context, epoch).map_err(|e| {
        tracing::error!(error = %e, "anon-token key derivation failed");
        TokenApiError::Internal
    })?;
    let issuance = key.issue(&blinded).map_err(|e| {
        tracing::error!(error = %e, "anon-token evaluation failed after reservation");
        TokenApiError::Internal
    })?;

    Ok(Json(IssueRes {
        context: context.name(),
        epoch,
        evaluations: issuance
            .evaluations
            .iter()
            .map(|e| URL_SAFE_NO_PAD.encode(e))
            .collect(),
        proof: URL_SAFE_NO_PAD.encode(issuance.proof),
        public_key: URL_SAFE_NO_PAD.encode(key.public_key()),
        remaining,
        epoch_ends_at: issuer.server.current_epoch_ends_at(),
    }))
}

// ----------------------------------------------------------------
// GET /auth/tokens/key/{context}/{epoch}
// ----------------------------------------------------------------

/// Response body for the public-key route.
#[derive(Serialize)]
pub(crate) struct KeyRes {
    /// The context name.
    pub context: &'static str,
    /// The epoch.
    pub epoch: u64,
    /// Base64url public key.
    pub public_key: String,
}

/// `GET /auth/tokens/key/{context}/{epoch}` - the public key for
/// `(context, epoch)`. Anonymous. Served for the previous, current and next
/// epoch only (the ones a client can issue or hold tokens for, plus the next
/// so a client near a boundary can prefetch).
pub(crate) async fn public_key(
    State(state): State<AppState>,
    Path((context, epoch)): Path<(String, u64)>,
) -> Result<Response, TokenApiError> {
    let issuer = issuer(&state)?;
    let context = TokenContext::from_name(&context).map_err(|_| TokenApiError::UnknownContext)?;
    let current = issuer.server.current_epoch();
    if epoch.saturating_add(1) < current || epoch > current.saturating_add(1) {
        return Err(TokenApiError::EpochOutOfRange);
    }
    let key = IssuerKey::derive(&issuer.server.master, context, epoch).map_err(|e| {
        tracing::error!(error = %e, "anon-token key derivation failed");
        TokenApiError::Internal
    })?;
    let mut resp = Json(KeyRes {
        context: context.name(),
        epoch,
        public_key: URL_SAFE_NO_PAD.encode(key.public_key()),
    })
    .into_response();
    // The key for an epoch never changes; a short public cache is safe.
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    Ok(resp)
}

#[cfg(test)]
mod tests;
