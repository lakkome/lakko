// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The verifier side of a spend, for an axum service: read the
//! `x-lakko-anon-token` header, verify the token for the route's context,
//! record it in the spent-set, and refuse with a stable code otherwise.
//!
//! Two entry points over one core ([`TokenVerifier::spend_header`]):
//!
//! - [`require_token`], a middleware for a route whose token is spent before
//!   the handler runs ([`TokenVerifier::gate`] gives it its state);
//! - [`TokenVerifier::spend_header`] called from inside a handler, for a route
//!   that validates its body first so a malformed request costs no token.
//!
//! # Refusals
//!
//! | Status | Body `{"error": ...}` | When |
//! |---|---|---|
//! | 503 | `anon-tokens-disabled` | no master secret configured on this service |
//! | 401 | `anon-token-required` | no header |
//! | 401 | `anon-token-malformed` | not base64url of a 106-byte token, unknown version or context byte |
//! | 401 | `anon-token-wrong-context` | a valid token drawn for another action |
//! | 401 | `anon-token-expired` | epoch neither the current nor the previous one |
//! | 401 | `anon-token-invalid` | the output does not verify under the master secret |
//! | 401 | `anon-token-spent` | this token was already spent at this service |
//! | 500 | `internal` | the spent-set store failed (fail closed) |
//!
//! A 401 here never means a lost session: the routes that spend a token
//! carry no session. The disabled check comes first, so a service without
//! the master secret answers 503 to every token route whatever the request
//! carries - it never degrades to accepting requests without a token.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use lakko_anon_token::{
    decode_header_value, verify_token, AnonTokenError, TokenContext, VerifiedToken, TOKEN_HEADER,
};
use sqlx::MySqlPool;

use crate::{now_unix_secs, spawn_spent_pruner, spend, AnonTokenServerConfig, SpendError};

/// Why a spend was refused. Each variant maps to one status and one stable
/// machine-readable code (see the module table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenRejection {
    /// No master secret on this service (503 `anon-tokens-disabled`).
    Disabled,
    /// No `x-lakko-anon-token` header (401 `anon-token-required`).
    Missing,
    /// The header is not a well-formed token (401 `anon-token-malformed`).
    Malformed,
    /// A token for another context (401 `anon-token-wrong-context`).
    WrongContext,
    /// The token's epoch is outside the accepted window (401
    /// `anon-token-expired`).
    Expired,
    /// The token does not verify (401 `anon-token-invalid`).
    Invalid,
    /// Already spent at this service (401 `anon-token-spent`).
    Spent,
    /// The spent-set store failed; nothing was honoured (500 `internal`).
    Internal,
}

impl TokenRejection {
    /// The HTTP status.
    #[must_use]
    pub const fn status(self) -> StatusCode {
        match self {
            Self::Disabled => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Missing
            | Self::Malformed
            | Self::WrongContext
            | Self::Expired
            | Self::Invalid
            | Self::Spent => StatusCode::UNAUTHORIZED,
        }
    }

    /// The stable code carried as `{"error": code}`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Disabled => "anon-tokens-disabled",
            Self::Missing => "anon-token-required",
            Self::Malformed => "anon-token-malformed",
            Self::WrongContext => "anon-token-wrong-context",
            Self::Expired => "anon-token-expired",
            Self::Invalid => "anon-token-invalid",
            Self::Spent => "anon-token-spent",
            Self::Internal => "internal",
        }
    }

    const fn from_verify(e: AnonTokenError) -> Self {
        match e {
            AnonTokenError::Malformed
            | AnonTokenError::UnknownVersion
            | AnonTokenError::UnknownContext => Self::Malformed,
            AnonTokenError::WrongContext => Self::WrongContext,
            AnonTokenError::EpochOutOfWindow => Self::Expired,
            AnonTokenError::InvalidToken => Self::Invalid,
            // A zero window is refused at configuration time and the rest are
            // issuance-side errors or unreachable VOPRF failures.
            _ => Self::Internal,
        }
    }
}

impl IntoResponse for TokenRejection {
    fn into_response(self) -> Response {
        (
            self.status(),
            Json(serde_json::json!({ "error": self.code() })),
        )
            .into_response()
    }
}

/// A service's token verifier: its configuration (or none, when dormant) and
/// the pool holding its spent-set. Cheap to clone.
#[derive(Debug, Clone)]
pub struct TokenVerifier {
    config: Option<Arc<AnonTokenServerConfig>>,
    pool: MySqlPool,
}

impl TokenVerifier {
    /// Build over `pool` (the service's own database, which must carry the
    /// `anon_token_spent` table) and `config` (`None` = dormant: every token
    /// route answers 503).
    #[must_use]
    pub fn new(pool: MySqlPool, config: Option<AnonTokenServerConfig>) -> Self {
        Self {
            config: config.map(Arc::new),
            pool,
        }
    }

    /// Whether a master secret is configured.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.config.is_some()
    }

    /// Log the startup state for `service`, loudly when dormant.
    pub fn log_startup(&self, service: &str) {
        if let Some(c) = &self.config {
            tracing::info!(
                service,
                window_secs = c.window_secs,
                "anonymous-token verification enabled"
            );
        } else {
            tracing::warn!(
                service,
                "anonymous-token verification DISABLED: LAKKO_ANON_TOKEN_MASTER_SECRET is not \
                 set; every route that spends a token answers 503 anon-tokens-disabled"
            );
        }
    }

    /// Spawn the hourly spent-set pruner when enabled.
    #[must_use]
    pub fn spawn_pruner(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.config
            .as_ref()
            .map(|c| spawn_spent_pruner(self.pool.clone(), c.window_secs))
    }

    /// The middleware state for routes spending a `context` token.
    #[must_use]
    pub fn gate(&self, context: TokenContext) -> TokenGate {
        TokenGate {
            verifier: self.clone(),
            context,
        }
    }

    /// Verify the token in `headers` for `context` at `now_unix_secs` and
    /// record it spent. `Ok` means the caller may do the work the token pays
    /// for; the spend is already durable, so a later failure of that work
    /// does not refund it.
    ///
    /// # Errors
    ///
    /// A [`TokenRejection`]; see the module table.
    pub async fn spend_header_at(
        &self,
        headers: &HeaderMap,
        context: TokenContext,
        now_unix_secs: u64,
    ) -> Result<VerifiedToken, TokenRejection> {
        let Some(config) = &self.config else {
            return Err(TokenRejection::Disabled);
        };
        let value = headers
            .get(TOKEN_HEADER)
            .ok_or(TokenRejection::Missing)?
            .to_str()
            .map_err(|_| TokenRejection::Malformed)?;
        let bytes = decode_header_value(value).map_err(|_| TokenRejection::Malformed)?;
        let verified = verify_token(
            &config.master,
            &bytes,
            context,
            now_unix_secs,
            config.window_secs,
        )
        .map_err(TokenRejection::from_verify)?;
        match spend(
            &self.pool,
            verified.context,
            &verified.spend_id,
            verified.epoch,
        )
        .await
        {
            Ok(()) => Ok(verified),
            Err(SpendError::AlreadySpent) => Err(TokenRejection::Spent),
            Err(SpendError::Db(e)) => {
                tracing::error!(error = %e, "anon-token spent-set store error; refusing");
                Err(TokenRejection::Internal)
            },
        }
    }

    /// [`Self::spend_header_at`] by the system clock.
    ///
    /// # Errors
    ///
    /// A [`TokenRejection`]; see the module table.
    pub async fn spend_header(
        &self,
        headers: &HeaderMap,
        context: TokenContext,
    ) -> Result<VerifiedToken, TokenRejection> {
        self.spend_header_at(headers, context, now_unix_secs())
            .await
    }
}

/// Middleware state: one verifier bound to one context.
#[derive(Debug, Clone)]
pub struct TokenGate {
    verifier: TokenVerifier,
    context: TokenContext,
}

impl TokenGate {
    /// The context this gate spends.
    #[must_use]
    pub const fn context(&self) -> TokenContext {
        self.context
    }
}

/// Middleware: spend one token of the gate's context before the handler
/// runs; answer the [`TokenRejection`] otherwise. Wire with
/// `route_layer(axum::middleware::from_fn_with_state(verifier.gate(ctx), require_token))`.
pub async fn require_token(State(gate): State<TokenGate>, req: Request, next: Next) -> Response {
    match gate
        .verifier
        .spend_header(req.headers(), gate.context)
        .await
    {
        Ok(_) => next.run(req).await,
        Err(r) => r.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rejection_has_its_documented_status_and_code() {
        let table = [
            (TokenRejection::Disabled, 503, "anon-tokens-disabled"),
            (TokenRejection::Missing, 401, "anon-token-required"),
            (TokenRejection::Malformed, 401, "anon-token-malformed"),
            (
                TokenRejection::WrongContext,
                401,
                "anon-token-wrong-context",
            ),
            (TokenRejection::Expired, 401, "anon-token-expired"),
            (TokenRejection::Invalid, 401, "anon-token-invalid"),
            (TokenRejection::Spent, 401, "anon-token-spent"),
            (TokenRejection::Internal, 500, "internal"),
        ];
        for (r, status, code) in table {
            assert_eq!(r.status().as_u16(), status, "{r:?}");
            assert_eq!(r.code(), code, "{r:?}");
        }
    }

    #[test]
    fn verifier_errors_map_to_the_client_facing_rejections() {
        assert_eq!(
            TokenRejection::from_verify(AnonTokenError::UnknownVersion),
            TokenRejection::Malformed
        );
        assert_eq!(
            TokenRejection::from_verify(AnonTokenError::UnknownContext),
            TokenRejection::Malformed
        );
        assert_eq!(
            TokenRejection::from_verify(AnonTokenError::EpochOutOfWindow),
            TokenRejection::Expired
        );
        assert_eq!(
            TokenRejection::from_verify(AnonTokenError::InvalidToken),
            TokenRejection::Invalid
        );
        assert_eq!(
            TokenRejection::from_verify(AnonTokenError::Internal),
            TokenRejection::Internal
        );
    }
}
