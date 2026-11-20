// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Supporter codes ([ADR-0097] section 1).
//!
//! A backer's code buys ONE evaluation of a `beta-place` anonymous token,
//! without a session; the first username bind (`PUT /auth/login-handle`)
//! spends that token past the beta account cap. Neither table joins a code
//! to an account: `supporter_codes` holds code hashes and evaluations, the
//! spent-set holds spend identifiers, and the blinding hides which
//! evaluation a spent token came from.
//!
//! - **Mint:** `lakko-svc-auth supporter-codes mint --count N` stores
//!   `BLAKE3-keyed(pepper, code)` per code and prints the codes once.
//! - **Redeem:** `POST /auth/supporter-code/redeem
//!   {code, epoch, blinded_element}` -> `{context, epoch, evaluation, proof}`.
//!   The client derives the blinded element from the code and the epoch
//!   (`lakko_anon_token::SupporterCode::token_request`), so a retry sends
//!   the same one; the first evaluation is stored and returned again.
//! - **Spend:** in [`crate::login_handle`], only when the cap refuses a
//!   first bind.
//!
//! # Configuration
//!
//! `LAKKO_AUTH_SUPPORTER_CODE_PEPPER` (64 hex). Unset: the redeem route
//! answers `503 supporter-codes-disabled` and the mint command refuses. The
//! redeem also needs `LAKKO_ANON_TOKEN_MASTER_SECRET` (the evaluation key).
//!
//! # Refusals of the redeem route
//!
//! | Status | `{"error": ...}` | When |
//! |---|---|---|
//! | 503 | `supporter-codes-disabled` | no pepper or no master secret |
//! | 400 | `supporter-code-malformed` | not 16 base32 symbols |
//! | 400 | `bad-blinded-element` | not base64url of a valid group element |
//! | 400 | `epoch-not-issuable` (+ `current_epoch`) | the body names another epoch |
//! | 429 | `supporter-code-limited` | the address bucket or the per-code counter |
//! | 404 | `supporter-code-unknown` | no such code |
//! | 409 | `supporter-code-mismatch` | redeemed this epoch over another element |
//! | 410 | `supporter-code-expired` | redeemed in an earlier epoch |
//! | 503 | `rate-limiter-unavailable` | the per-code counter store failed |
//! | 500 | `internal` | store or key failure |
//!
//! Logs carry the outcome only, never a code or a hash.
//!
//! [ADR-0097]: ../../../docs/adr/0097-supporter-codes-and-list.md

use std::io::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context as _;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_anon_token::{
    decode_header_value, validate_blinded_batch, verify_token, IssuerKey, SpendId, SupporterCode,
    TokenContext, BLINDED_ELEMENT_LEN, TOKEN_HEADER,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::MySqlPool;

use crate::anon_tokens::AnonTokenIssuer;
use crate::limits::{Category, LimitError, Scope};
use crate::AppState;

/// Environment variable holding the code pepper.
pub(crate) const PEPPER_VAR: &str = "LAKKO_AUTH_SUPPORTER_CODE_PEPPER";

/// Largest body the redeem route reads.
const MAX_BODY_BYTES: usize = 1024;

/// Most codes one mint command makes.
const MAX_MINT: u32 = 100_000;

/// The keyed-hash key for code rows. Its `Debug` prints nothing.
#[derive(Clone)]
pub(crate) struct SupporterPepper(pub(crate) [u8; 32]);

impl core::fmt::Debug for SupporterPepper {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SupporterPepper(..)")
    }
}

/// Read the pepper. `Ok(None)` when unset.
///
/// # Errors
///
/// A value that is set but is not 64 hex characters.
pub(crate) fn load_pepper() -> anyhow::Result<Option<SupporterPepper>> {
    match std::env::var(PEPPER_VAR) {
        Ok(v) if !v.trim().is_empty() => crate::login_handle::parse_hex32(v.trim())
            .map(|k| Some(SupporterPepper(k)))
            .ok_or_else(|| anyhow::anyhow!("{PEPPER_VAR} must be 64 hex characters (32 bytes)")),
        _ => Ok(None),
    }
}

/// Log the startup state.
pub(crate) fn log_startup(issuer: Option<&AnonTokenIssuer>) {
    match issuer {
        Some(i) if i.supporter_pepper().is_some() => {
            tracing::info!("supporter codes enabled");
        },
        Some(_) => tracing::info!(knob = PEPPER_VAR, "supporter codes disabled (pepper unset)"),
        None => tracing::info!(
            "supporter codes disabled (anonymous tokens are dormant: no master secret)"
        ),
    }
}

/// The stored key of a code.
pub(crate) fn code_hash(pepper: &SupporterPepper, code: &SupporterCode) -> [u8; 32] {
    lakko_crypto::hash::blake3_keyed(&pepper.0, code.as_bytes())
}

// ---------------------------------------------------------------------------
// Mint command
// ---------------------------------------------------------------------------

/// `lakko-svc-auth supporter-codes mint --count N`: store N new code hashes
/// and print the codes, one per line in display form, to stdout. Nothing
/// else goes to stdout; errors go to stderr through the caller.
///
/// # Errors
///
/// A bad command line, no pepper, no database, or a store failure.
pub(crate) async fn run_cli(args: &[String]) -> anyhow::Result<()> {
    let count = parse_mint_args(args)?;
    let pepper = load_pepper()?
        .ok_or_else(|| anyhow::anyhow!("{PEPPER_VAR} must be set to mint supporter codes"))?;
    let db_url = std::env::var("LAKKO_AUTH_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .context("LAKKO_AUTH_DATABASE_URL or DATABASE_URL env var must be set")?;
    let pool = sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(1)
        .connect(&db_url)
        .await
        .context("connect to MariaDB")?;
    crate::MIGRATIONS
        .run(&pool)
        .await
        .context("run sqlx migrations")?;
    let codes = mint(&pool, &pepper, count).await?;
    let mut out = std::io::stdout().lock();
    for code in &codes {
        writeln!(out, "{}", code.to_display()).context("write a code to stdout")?;
    }
    out.flush().context("flush stdout")?;
    pool.close().await;
    Ok(())
}

/// `mint --count N` -> N.
fn parse_mint_args(args: &[String]) -> anyhow::Result<u32> {
    let usage =
        || anyhow::anyhow!("usage: lakko-svc-auth supporter-codes mint --count N (1..={MAX_MINT})");
    match args {
        [cmd, flag, n] if cmd == "mint" && flag == "--count" => n
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=MAX_MINT).contains(n))
            .ok_or_else(usage),
        _ => Err(usage()),
    }
}

/// Store `count` fresh code hashes and return the codes.
pub(crate) async fn mint(
    pool: &MySqlPool,
    pepper: &SupporterPepper,
    count: u32,
) -> anyhow::Result<Vec<SupporterCode>> {
    let mut codes = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    while codes.len() < usize::try_from(count).unwrap_or(0) {
        let code = SupporterCode::generate();
        let res = sqlx::query("INSERT INTO supporter_codes (code_hash) VALUES (?)")
            .bind(&code_hash(pepper, &code)[..])
            .execute(pool)
            .await;
        match res {
            Ok(_) => codes.push(code),
            // 80 random bits colliding: draw another.
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {},
            Err(e) => return Err(e).context("store a supporter code"),
        }
    }
    Ok(codes)
}

// ---------------------------------------------------------------------------
// Redeem route
// ---------------------------------------------------------------------------

/// Why a redeem was refused (see the module table).
#[derive(Debug)]
pub(crate) enum RedeemError {
    Disabled,
    BadBody,
    Malformed,
    BadBlindedElement,
    EpochNotIssuable { current_epoch: u64 },
    Limit(LimitError),
    Unknown,
    Mismatch,
    Expired,
    Internal,
}

impl IntoResponse for RedeemError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::Disabled => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({ "error": "supporter-codes-disabled" }),
            ),
            Self::BadBody => (StatusCode::BAD_REQUEST, json!({ "error": "bad-request" })),
            Self::Malformed => (
                StatusCode::BAD_REQUEST,
                json!({ "error": "supporter-code-malformed" }),
            ),
            Self::BadBlindedElement => (
                StatusCode::BAD_REQUEST,
                json!({ "error": "bad-blinded-element" }),
            ),
            Self::EpochNotIssuable { current_epoch } => (
                StatusCode::BAD_REQUEST,
                json!({ "error": "epoch-not-issuable", "current_epoch": current_epoch }),
            ),
            Self::Limit(e) => return e.into_response(),
            Self::Unknown => (
                StatusCode::NOT_FOUND,
                json!({ "error": "supporter-code-unknown" }),
            ),
            Self::Mismatch => (
                StatusCode::CONFLICT,
                json!({ "error": "supporter-code-mismatch" }),
            ),
            Self::Expired => (
                StatusCode::GONE,
                json!({ "error": "supporter-code-expired" }),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "internal" }),
            ),
        };
        (status, Json(body)).into_response()
    }
}

/// `POST /auth/supporter-code/redeem` body.
#[derive(Deserialize)]
struct RedeemReq {
    /// The code as typed (forgiving parse).
    code: String,
    /// The `beta-place` epoch the client built its request for.
    epoch: u64,
    /// Base64url (no padding) of the 32-byte blinded element.
    blinded_element: String,
}

/// What the first redeem stored.
type Stored = (
    Option<u64>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);

/// `POST /auth/supporter-code/redeem` - anonymous. Order: shape, address
/// bucket, lookup, per-code counter, then the evaluation under the row lock.
pub(crate) async fn redeem(State(state): State<AppState>, req: Request<Body>) -> Response {
    match redeem_inner(&state, req).await {
        Ok(r) => r,
        Err(e) => {
            tracing::info!(outcome = ?e, "supporter code redeem refused");
            e.into_response()
        },
    }
}

async fn redeem_inner(state: &AppState, req: Request<Body>) -> Result<Response, RedeemError> {
    let issuer = state.anon_tokens.as_ref().ok_or(RedeemError::Disabled)?;
    let pepper = issuer.supporter_pepper().ok_or(RedeemError::Disabled)?;

    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|_| RedeemError::BadBody)?;
    let body: RedeemReq = serde_json::from_slice(&bytes).map_err(|_| RedeemError::BadBody)?;

    // Shape first: a malformed request costs no bucket and no counter.
    let code = SupporterCode::parse(&body.code).map_err(|_| RedeemError::Malformed)?;
    let blinded: [u8; BLINDED_ELEMENT_LEN] = URL_SAFE_NO_PAD
        .decode(&body.blinded_element)
        .ok()
        .and_then(|b| <[u8; BLINDED_ELEMENT_LEN]>::try_from(b.as_slice()).ok())
        .ok_or(RedeemError::BadBlindedElement)?;
    validate_blinded_batch(&[blinded]).map_err(|_| RedeemError::BadBlindedElement)?;
    let current = issuer.server.current_epoch_for(TokenContext::BetaPlace);
    if body.epoch != current {
        return Err(RedeemError::EpochNotIssuable {
            current_epoch: current,
        });
    }

    crate::limits::charge_source(&state.limits, Category::SupporterCode, &parts.headers, peer)
        .map_err(RedeemError::Limit)?;

    let hash = code_hash(pepper, &code);
    let known: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM supporter_codes WHERE code_hash = ?")
            .bind(&hash[..])
            .fetch_optional(&state.db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "supporter code lookup failed");
                RedeemError::Internal
            })?;
    if known.is_none() {
        return Err(RedeemError::Unknown);
    }
    crate::limits::charge_name(&state.db, &state.limits, Scope::SupporterCode, &hash)
        .await
        .map_err(RedeemError::Limit)?;

    let (evaluation, proof) = evaluate_once(&state.db, issuer, &hash, current, &blinded).await?;
    tracing::info!(outcome = "issued", "supporter code redeemed");
    Ok(Json(json!({
        "context": TokenContext::BetaPlace.name(),
        "epoch": current,
        "evaluation": URL_SAFE_NO_PAD.encode(evaluation),
        "proof": URL_SAFE_NO_PAD.encode(proof),
    }))
    .into_response())
}

/// Under the code row's lock: evaluate and store on the first redeem, return
/// the stored answer on a repeat of it, refuse anything else.
async fn evaluate_once(
    pool: &MySqlPool,
    issuer: &AnonTokenIssuer,
    hash: &[u8; 32],
    current: u64,
    blinded: &[u8; BLINDED_ELEMENT_LEN],
) -> Result<(Vec<u8>, Vec<u8>), RedeemError> {
    let internal = |e: sqlx::Error| {
        tracing::error!(error = %e, "supporter code store failed");
        RedeemError::Internal
    };
    let mut tx = pool.begin().await.map_err(internal)?;
    let row: Option<Stored> = sqlx::query_as(
        "SELECT epoch, blinded_element, evaluation, proof FROM supporter_codes \
         WHERE code_hash = ? FOR UPDATE",
    )
    .bind(&hash[..])
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;
    let Some((epoch, stored_blinded, evaluation, proof)) = row else {
        return Err(RedeemError::Unknown);
    };
    match epoch {
        None => {
            let key = IssuerKey::derive(&issuer.server.master, TokenContext::BetaPlace, current)
                .map_err(|_| RedeemError::Internal)?;
            let issuance = key
                .issue(&[*blinded])
                .map_err(|_| RedeemError::BadBlindedElement)?;
            let evaluation = issuance
                .evaluations
                .first()
                .ok_or(RedeemError::Internal)?
                .to_vec();
            let proof = issuance.proof.to_vec();
            sqlx::query(
                "UPDATE supporter_codes SET epoch = ?, blinded_element = ?, evaluation = ?, \
                 proof = ? WHERE code_hash = ?",
            )
            .bind(current)
            .bind(&blinded[..])
            .bind(&evaluation[..])
            .bind(&proof[..])
            .bind(&hash[..])
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
            tx.commit().await.map_err(internal)?;
            Ok((evaluation, proof))
        },
        Some(e) if e == current => {
            if stored_blinded.as_deref() != Some(&blinded[..]) {
                return Err(RedeemError::Mismatch);
            }
            match (evaluation, proof) {
                (Some(ev), Some(pr)) => Ok((ev, pr)),
                _ => Err(RedeemError::Internal),
            }
        },
        // Redeemed in an earlier window: the place was held then. A stored
        // epoch AHEAD of the clock (a clock moved back) is refused the same
        // way rather than evaluated a second time.
        Some(_) => Err(RedeemError::Expired),
    }
}

// ---------------------------------------------------------------------------
// The spend at the bind
// ---------------------------------------------------------------------------

/// A `beta-place` token that verified on a bind request, to be spent in the
/// bind's transaction if the cap refuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SupporterPlace {
    pub(crate) spend_id: SpendId,
    pub(crate) epoch: u64,
}

/// The `beta-place` token carried by a bind, if it verifies now. Anything
/// else (no header, a malformed or invalid token, tokens dormant) is `None`:
/// the bind then answers the cap's ordinary refusal. Pure: the spent-set is
/// checked in the bind's transaction.
pub(crate) fn place_from_headers(
    issuer: Option<&Arc<AnonTokenIssuer>>,
    headers: &HeaderMap,
) -> Option<SupporterPlace> {
    let issuer = issuer?;
    let value = headers.get(TOKEN_HEADER)?.to_str().ok()?;
    let bytes = decode_header_value(value).ok()?;
    let verified = verify_token(
        &issuer.server.master,
        &bytes,
        TokenContext::BetaPlace,
        lakko_anon_token_server::now_unix_secs(),
        issuer.server.window_secs,
    )
    .ok()?;
    Some(SupporterPlace {
        spend_id: verified.spend_id,
        epoch: verified.epoch,
    })
}

#[cfg(test)]
mod tests;
