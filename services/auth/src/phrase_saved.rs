// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Saving a recovery phrase without changing the keys (ADR-0075 section 3,
//! amended 2026-11-01).
//!
//! A rotation exists to outrun a copy of the email vault. A phrase that was
//! never sealed in one has no earlier copy, so its owner keeps the keys: the
//! client shows the words the device already holds and, after the three-word
//! check, calls [`mark`].
//!
//! - `GET /account/phrase` (bearer) -> `{saved, rotation_needed}`. Not gated on
//!   the recovery-email pepper: a server that sends no mail answers it too.
//! - `POST /account/phrase/saved` (bearer + fresh-auth grant) -> `204`, or
//!   `409 phrase-save-needs-rotation` when the current phrase was ever in a
//!   vault. Idempotent: the notice is written once, by
//!   [`crate::account_notices::mark_phrase_saved`].

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::Extension;
use serde::{Deserialize, Serialize};

use crate::session::now_ms;
use crate::webauthn::{ApiError, AuthenticatedAccount};
use crate::AppState;

const CODE_NEEDS_ROTATION: &str = "phrase-save-needs-rotation";

/// Answer of `GET /account/phrase`.
#[derive(Serialize)]
pub(crate) struct PhraseRes {
    /// The owner saved a recovery phrase.
    pub(crate) saved: bool,
    /// Saving one takes a key rotation: the current phrase was ever sealed in
    /// an email vault.
    pub(crate) rotation_needed: bool,
}

/// Body of `POST /account/phrase/saved`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MarkReq {
    /// The proof of ADR-0073 section 17.1.
    #[serde(default)]
    pub(crate) fresh_grant: Option<String>,
}

/// `GET /account/phrase`.
///
/// # Errors
///
/// [`ApiError::Db`].
pub(crate) async fn status(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
) -> Result<Json<PhraseRes>, ApiError> {
    let (saved, in_vault): (bool, bool) =
        sqlx::query_as("SELECT phrase_saved, phrase_in_vault FROM accounts WHERE id = ?")
            .bind(&auth.0[..])
            .fetch_one(&state.db)
            .await
            .map_err(ApiError::Db)?;
    Ok(Json(PhraseRes {
        saved,
        rotation_needed: in_vault,
    }))
}

/// `POST /account/phrase/saved`.
///
/// # Errors
///
/// [`ApiError::FreshAuthRequired`], [`ApiError::Db`], and `409
/// phrase-save-needs-rotation`.
pub(crate) async fn mark(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedAccount>,
    Json(req): Json<MarkReq>,
) -> Result<StatusCode, ApiError> {
    let account_id = auth.0;
    crate::session::require_fresh_grant(&state, &account_id, req.fresh_grant.as_deref()).await?;

    let mut tx = state.db.begin().await.map_err(ApiError::Db)?;
    let (in_vault,): (bool,) =
        sqlx::query_as("SELECT phrase_in_vault FROM accounts WHERE id = ? FOR UPDATE")
            .bind(&account_id[..])
            .fetch_one(&mut *tx)
            .await
            .map_err(ApiError::Db)?;
    if in_vault {
        return Err(ApiError::Coded(StatusCode::CONFLICT, CODE_NEEDS_ROTATION));
    }
    crate::account_notices::mark_phrase_saved(&mut tx, &account_id, now_ms())
        .await
        .map_err(ApiError::Db)?;
    tx.commit().await.map_err(ApiError::Db)?;
    tracing::info!("recovery phrase saved without a key change");
    Ok(StatusCode::NO_CONTENT)
}
