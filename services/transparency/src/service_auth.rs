// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Shared-service-secret authentication for the append endpoint
//! (KT-log S2).
//!
//! `POST /transparency/append` is NOT a user-facing endpoint — only the
//! discovery service calls it, once per identity-key CHANGE. So the gate
//! is a shared service secret, not a per-user bearer session.
//!
//! The secret is configured via `LAKKO_TRANSPARENCY_APPEND_SECRET`
//! (base64, decoding to `>= 16` bytes). The caller presents it as an
//! `Authorization: Bearer <secret-b64>` header. We base64-decode the
//! presented value and compare it against the configured secret in
//! **constant time** (via [`subtle::ConstantTimeEq`]) so a timing side
//! channel cannot be used to recover the secret byte-by-byte.
//!
//! When the secret is unset the endpoint fails LOUD with **503** — there
//! is no anonymous-append fallback (a transparency log that anyone can
//! append to is worthless as a trust anchor).
//!
//! The read paths (`/root`, `/proof`, `/operator-key`) stay anonymous —
//! they serve only public artefacts and take no account-bound input.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use subtle::ConstantTimeEq as _;

use crate::AppState;

/// Minimum decoded length of the append secret, in bytes. A shorter
/// secret has too little entropy to resist guessing; we reject it at
/// startup (see [`load_append_secret`]).
pub(crate) const MIN_APPEND_SECRET_LEN: usize = 16;

/// Load and validate the append secret from
/// `LAKKO_TRANSPARENCY_APPEND_SECRET`.
///
/// Returns:
///
/// - `Ok(Some(secret))` when the env var is set and decodes to a
///   `>= MIN_APPEND_SECRET_LEN`-byte value,
/// - `Ok(None)` when the env var is unset (the append endpoint then
///   serves 503 — see [`require_service_secret`]),
/// - `Err(..)` when the env var is set but malformed (not base64, or
///   too short) — a misconfiguration we fail fast on rather than boot
///   into a state where every append is silently rejected.
///
/// # Errors
///
/// Returns an error string if the configured value is not valid base64
/// or decodes to fewer than [`MIN_APPEND_SECRET_LEN`] bytes.
pub(crate) fn load_append_secret() -> Result<Option<Arc<Vec<u8>>>, String> {
    let Ok(raw) = std::env::var("LAKKO_TRANSPARENCY_APPEND_SECRET") else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let bytes = B64
        .decode(raw.as_bytes())
        .map_err(|e| format!("LAKKO_TRANSPARENCY_APPEND_SECRET is not valid base64: {e}"))?;
    if bytes.len() < MIN_APPEND_SECRET_LEN {
        return Err(format!(
            "LAKKO_TRANSPARENCY_APPEND_SECRET decodes to {} bytes; need >= {MIN_APPEND_SECRET_LEN}",
            bytes.len()
        ));
    }
    Ok(Some(Arc::new(bytes)))
}

/// Middleware gating `POST /transparency/append` on the shared service
/// secret. Apply via
/// `.route_layer(axum::middleware::from_fn_with_state(state.clone(), require_service_secret))`.
///
/// Failure modes and the status they surface:
///
/// - secret unset on the server → **503** (loud — no anonymous-append
///   fallback),
/// - missing `Authorization` header / no `Bearer ` prefix → **401**,
/// - presented value not base64 → **401**,
/// - decoded value does not match the configured secret (constant-time)
///   → **401**.
///
/// The failure cause is logged at `warn`/`error` for operator
/// diagnostics; the caller sees only the bare status.
pub(crate) async fn require_service_secret(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(secret) = state.append_secret.as_ref() else {
        tracing::error!(
            "append rejected: LAKKO_TRANSPARENCY_APPEND_SECRET unset — \
             the append endpoint is disabled (no anonymous-append fallback)"
        );
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };

    let header_value = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| {
            tracing::warn!("append rejected: missing Authorization header");
            StatusCode::UNAUTHORIZED
        })?;

    let presented_b64 = header_value.strip_prefix("Bearer ").ok_or_else(|| {
        tracing::warn!("append rejected: Authorization header missing 'Bearer ' prefix");
        StatusCode::UNAUTHORIZED
    })?;

    let presented = B64.decode(presented_b64.as_bytes()).map_err(|_| {
        tracing::warn!("append rejected: bearer value not valid base64");
        StatusCode::UNAUTHORIZED
    })?;

    // Constant-time compare. `ConstantTimeEq::ct_eq` already runs in time
    // independent of WHERE the bytes differ; the length comparison short-
    // circuits, but the length of a base64 secret is not itself the
    // secret, so leaking equal-vs-unequal length does not help an
    // attacker recover the secret bytes.
    let matches: bool = secret.as_slice().ct_eq(presented.as_slice()).into();
    if !matches {
        tracing::warn!("append rejected: service secret mismatch");
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::post;
    use axum::Router;
    use lakko_transparency::InMemoryLog;
    use sqlx::mysql::MySqlPoolOptions;
    use tower::ServiceExt as _;

    use crate::store::LogStore;

    /// Build an `AppState` whose `db`/`store` are inert (the
    /// service-secret middleware never touches them) and whose
    /// `append_secret` is `secret`. The pool is created lazily so no DB
    /// connection is attempted.
    fn state_with_secret(secret: Option<Arc<Vec<u8>>>) -> AppState {
        let db = MySqlPoolOptions::new().connect_lazy("mysql://unused:unused@127.0.0.1:1/unused");
        // `connect_lazy` only fails on a malformed URL; the URL above is
        // well-formed, so unwrap is safe in this test helper.
        let db = db.expect("lazy pool from a well-formed URL");
        let log = InMemoryLog::new(lakko_crypto::signing::SigningKey::generate());
        AppState {
            db,
            store: Arc::new(LogStore::from_log(log)),
            append_secret: secret,
            require_leaf_auth: false,
            genesis_attest_key: None,
            require_genesis_attest: false,
            witness: Arc::new(Vec::new()),
        }
    }

    /// A router that gates a trivial 200 handler behind the
    /// service-secret middleware — exactly the shape `build_app` wires
    /// for the append route.
    fn gated_app(state: AppState) -> Router {
        async fn ok() -> StatusCode {
            StatusCode::OK
        }
        Router::new()
            .route("/gated", post(ok))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_service_secret,
            ))
            .with_state(state)
    }

    async fn call(app: Router, auth: Option<&str>) -> StatusCode {
        let mut builder = Request::builder().method("POST").uri("/gated");
        if let Some(a) = auth {
            builder = builder.header(header::AUTHORIZATION, a);
        }
        let req = builder.body(Body::empty()).expect("build request");
        app.oneshot(req).await.expect("oneshot").status()
    }

    #[tokio::test]
    async fn correct_secret_is_accepted() {
        let secret = Arc::new(vec![0x11u8; 32]);
        let app = gated_app(state_with_secret(Some(Arc::clone(&secret))));
        let bearer = format!("Bearer {}", B64.encode(secret.as_slice()));
        assert_eq!(call(app, Some(&bearer)).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn wrong_secret_is_rejected_401() {
        let secret = Arc::new(vec![0x11u8; 32]);
        let app = gated_app(state_with_secret(Some(secret)));
        let bad = format!("Bearer {}", B64.encode([0x22u8; 32]));
        assert_eq!(call(app, Some(&bad)).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn absent_header_is_rejected_401() {
        let secret = Arc::new(vec![0x11u8; 32]);
        let app = gated_app(state_with_secret(Some(secret)));
        assert_eq!(call(app, None).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn missing_bearer_prefix_is_rejected_401() {
        let secret = Arc::new(vec![0x11u8; 32]);
        let app = gated_app(state_with_secret(Some(Arc::clone(&secret))));
        // Raw secret without the `Bearer ` prefix.
        let raw = B64.encode(secret.as_slice());
        assert_eq!(call(app, Some(&raw)).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unset_secret_serves_503() {
        // Secret unset on the server -> loud 503, even with a plausible
        // bearer presented (no anonymous-append fallback).
        let app = gated_app(state_with_secret(None));
        let bearer = format!("Bearer {}", B64.encode([0x11u8; 32]));
        assert_eq!(
            call(app, Some(&bearer)).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn load_append_secret_rejects_short_and_accepts_long() {
        // Helper that drives `load_append_secret` against a temporary env
        // value. Tests run in-process so we set/clear the var inline; the
        // transparency tests are single-threaded per this module's scope,
        // but to be safe we only assert the decode/length logic by calling
        // the inner validation through the public path with the env set.
        // Too short (< 16 bytes) -> Err.
        std::env::set_var("LAKKO_TRANSPARENCY_APPEND_SECRET", B64.encode([0u8; 8]));
        assert!(super::load_append_secret().is_err());

        // >= 16 bytes -> Ok(Some).
        std::env::set_var("LAKKO_TRANSPARENCY_APPEND_SECRET", B64.encode([0u8; 16]));
        let loaded = super::load_append_secret().expect("ok");
        assert_eq!(loaded.map(|s| s.len()), Some(16));

        // Not base64 -> Err.
        std::env::set_var("LAKKO_TRANSPARENCY_APPEND_SECRET", "not base64 !!!");
        assert!(super::load_append_secret().is_err());

        std::env::remove_var("LAKKO_TRANSPARENCY_APPEND_SECRET");
    }
}
