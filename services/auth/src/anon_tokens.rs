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
//!   `DIRECTORY_SEARCH`, `VAULT_CLAIM`, `BLOB_UPLOAD`, `BLOB_READ`,
//!   `MAILBOX_WRITE`, `CALL_JOIN`). `0` stops issuance for that context.
//!   Malformed: startup fails.
//! - `LAKKO_ANON_TOKEN_QUOTA_CALL_JOIN_YOUNG` - the `call-join` quota of an
//!   account younger than `LAKKO_AUTH_YOUNG_ACCOUNT_AGE_SECS` (ADR-0078
//!   section 2, the rebuilt ADR-0033 fresh-account signal; defaults 8 and
//!   86400). One issuer key family serves every account (decision 2): the
//!   young quota is the only difference, so a token never shows at spend
//!   whether its account was young.
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
/// - `mailbox-write` (ADR-0078 section 1): spent only by a write with no
///   reach proof to a personal mailbox that is open (Everyone) or not yet
///   claimed - in practice a contact request to someone found in Everyone.
///   Writes under a reach key or a delegation spend none. 50 a day is chosen,
///   not measured: well above the requests a person sends, low enough that
///   one account is a poor source for flooding open mailboxes.
/// - `discovery-lookup`: one search spends up to two since ADR-0087 (the
///   `@name`, one namespace for people, groups and channels, and a name
///   term; it was three with a separate channel handle), and the search field asks the network as the person
///   types (`docs/ux/who-can-find-me.md` section 6.1, 2026-10-25). 300 a day
///   is chosen, not measured; the earlier 100 covered about 33 searches.
/// - `call-join` (ADR-0078 section 2): one token per call admission (a join,
///   a reconnect). 100 a day is chosen, not measured: far above the calls a
///   person joins, and the per-room cap bounds what one account can do to one
///   room. A young account draws [`DEFAULT_YOUNG_CALL_JOIN_QUOTA`] instead.
/// - `push-register` (ADR-0078 section 3): since push-gw holds no device
///   pseudonym, every group subscription is its own sealed row and each
///   subscribe or unsubscribe spends a token, and a token change re-sends
///   every subscription of the device. 400 a day is chosen, not measured: a
///   device in about a hundred groups can re-send all of them a few times a
///   day; the earlier 100 assumed one registration per device.
/// - `discovery-register` (ADR-0087 section 3): every group and channel
///   claims an @name, and a person retrying a taken public name spends
///   another. 20 a day is chosen, not measured; the earlier 10 covered
///   usernames and channel handles only.
/// - `circle-claim` (ADR-0086): an account claims one directory, again after a
///   key reset or a tree started over. 10 a day is chosen, not measured.
/// - `channel-react` (ADR-0087 section 13a): one token per reaction added
///   under a channel post; taking one off spends none. 500 a day is chosen,
///   not measured, with room to spare: far above the reactions a person gives
///   in a day, and a person out of tokens sees the reaction drawn at once and
///   sent at the next refill. A modified client can add up to its quota to a
///   counter; the ADR accepts that.
/// - `channel-comment` (ADR-0087 section 13a): one token per comment added;
///   deleting one's own spends none. 200 a day is chosen, not measured, with
///   room to spare over what a person writes in a day.
/// - `report` (ADR-0089 section 2): one token per report sent to the
///   moderators. 10 a day is chosen, not measured: a person reports rarely,
///   and the limit is what keeps one account from flooding the moderation
///   queue without the queue learning who reported.
const DEFAULT_QUOTAS: [(TokenContext, u32); CONTEXT_COUNT] = [
    (TokenContext::DiscoveryLookup, 300),
    (TokenContext::DiscoveryRegister, 20),
    (TokenContext::InviteConsume, 30),
    (TokenContext::DsKnock, 200),
    (TokenContext::PushRegister, 400),
    (TokenContext::ChannelSubscribe, 20),
    (TokenContext::DirectorySearch, 30),
    (TokenContext::VaultClaim, 100),
    (TokenContext::BlobUpload, 96),
    (TokenContext::BlobRead, 96),
    (TokenContext::MailboxWrite, 50),
    (TokenContext::CallJoin, 100),
    (TokenContext::CircleClaim, 10),
    (TokenContext::ChannelReact, 500),
    (TokenContext::ChannelComment, 200),
    (TokenContext::Report, 10),
];

/// The `call-join` quota per epoch of a young account (ADR-0078 section 2,
/// decision 2). Chosen, not measured: a guest who opened a call link joins
/// that call and reconnects a few times; a burst of fresh accounts each
/// buys only this many admissions.
const DEFAULT_YOUNG_CALL_JOIN_QUOTA: u32 = 8;

/// An account younger than this draws the young `call-join` quota. One day:
/// an ADR-0032 guest that never claims a name is deleted after a day.
const DEFAULT_YOUNG_ACCOUNT_AGE_SECS: u64 = 86_400;

/// Env var of the young `call-join` quota.
const YOUNG_CALL_JOIN_QUOTA_VAR: &str = "LAKKO_ANON_TOKEN_QUOTA_CALL_JOIN_YOUNG";

/// Env var of the young-account age.
const YOUNG_ACCOUNT_AGE_VAR: &str = "LAKKO_AUTH_YOUNG_ACCOUNT_AGE_SECS";

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
    /// The `call-join` quota of a young account.
    young_call_join_quota: u32,
    /// The age under which an account is young, in seconds.
    young_age_secs: u64,
}

impl AnonTokenIssuer {
    /// Build from explicit parts (tests).
    #[cfg(test)]
    pub(crate) const fn new(
        server: AnonTokenServerConfig,
        quotas: [(TokenContext, u32); CONTEXT_COUNT],
    ) -> Self {
        Self {
            server,
            quotas,
            young_call_join_quota: DEFAULT_YOUNG_CALL_JOIN_QUOTA,
            young_age_secs: DEFAULT_YOUNG_ACCOUNT_AGE_SECS,
        }
    }

    /// Build with the default quotas (other modules' tests).
    #[cfg(test)]
    pub(crate) const fn for_test(server: AnonTokenServerConfig) -> Self {
        Self::new(server, DEFAULT_QUOTAS)
    }

    /// Set the young-account `call-join` quota and age (tests).
    #[cfg(test)]
    pub(crate) const fn with_young(mut self, quota: u32, age_secs: u64) -> Self {
        self.young_call_join_quota = quota;
        self.young_age_secs = age_secs;
        self
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
    let lookup = |var: &str| std::env::var(var).ok();
    let quotas = quotas_from(lookup)?;
    let young_call_join_quota = parsed_or(
        &lookup,
        YOUNG_CALL_JOIN_QUOTA_VAR,
        DEFAULT_YOUNG_CALL_JOIN_QUOTA,
    )?;
    let young_age_secs = parsed_or(
        &lookup,
        YOUNG_ACCOUNT_AGE_VAR,
        DEFAULT_YOUNG_ACCOUNT_AGE_SECS,
    )?;
    Ok(Some(AnonTokenIssuer {
        server,
        quotas,
        young_call_join_quota,
        young_age_secs,
    }))
}

/// A non-negative integer from `var`, or `default` when unset or blank.
///
/// # Errors
///
/// A value that does not parse as a non-negative integer.
fn parsed_or<T: std::str::FromStr>(
    lookup: &impl Fn(&str) -> Option<String>,
    var: &str,
    default: T,
) -> anyhow::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match lookup(var) {
        Some(raw) if !raw.trim().is_empty() => raw
            .trim()
            .parse()
            .with_context(|| format!("{var} must be a non-negative integer")),
        _ => Ok(default),
    }
}

/// Whether the account was created less than `age_secs` ago (DB clock).
/// A missing row reads as not young; a store error is returned (the caller
/// fails closed).
async fn account_is_young(
    pool: &sqlx::MySqlPool,
    account_id: &[u8; 16],
    age_secs: u64,
) -> Result<bool, sqlx::Error> {
    let age = i64::try_from(age_secs).unwrap_or(i64::MAX);
    let flag: Option<i64> = sqlx::query_scalar(
        "SELECT (created_at > NOW() - INTERVAL ? SECOND) FROM accounts WHERE id = ?",
    )
    .bind(age)
    .bind(&account_id[..])
    .fetch_optional(pool)
    .await?;
    Ok(flag.is_some_and(|v| v != 0))
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
        let young_quota = i.young_call_join_quota;
        tracing::info!(
            window_secs = i.server.window_secs,
            quotas = ?i.quotas.iter().map(|(c, q)| (c.name(), *q)).collect::<Vec<_>>(),
            young_quota,
            young_age_secs = i.young_age_secs,
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
    /// A suspended account asked for a context other than `blob-read`
    /// (403 `account-suspended`; ADR-0089: it may still read its chats,
    /// files included, and nothing else).
    Suspended,
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
            Self::Suspended => (
                StatusCode::FORBIDDEN,
                json!({ "error": "account-suspended" }),
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
    Extension(standing): Extension<crate::webauthn::SessionStanding>,
    Json(req): Json<IssueReq>,
) -> Result<Json<IssueRes>, TokenApiError> {
    let issuer = issuer(&state)?;
    let context =
        TokenContext::from_name(&req.context).map_err(|_| TokenApiError::UnknownContext)?;
    // ADR-0089 step 3: a suspended account reads its chats, attachments
    // included, so it may still draw `blob-read` passes; nothing else.
    if standing.suspended && context != TokenContext::BlobRead {
        return Err(TokenApiError::Suspended);
    }
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
    // ADR-0078 section 2: a young account draws a small `call-join` quota.
    // The key family is the same for every account (decision 2), so nothing
    // about the token tells the spender the account was young.
    let quota = if context == TokenContext::CallJoin {
        match account_is_young(&state.db, &auth.0, issuer.young_age_secs).await {
            Ok(true) => issuer.young_call_join_quota.min(issuer.quota(context)),
            Ok(false) => issuer.quota(context),
            Err(e) => {
                tracing::error!(error = %e, "anon-token account-age read failed; refusing");
                return Err(TokenApiError::Internal);
            },
        }
    } else {
        issuer.quota(context)
    };
    let remaining = match reserve_issuance(&state.db, &auth.0, context, epoch, count, quota).await {
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
