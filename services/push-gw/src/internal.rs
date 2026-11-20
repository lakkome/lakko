// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The internal listener (ADR-0054 decision 1).
//!
//! `/push/notify-group` is not on the public router. It is served on a second
//! listener, bound to `LAKKO_PUSH_GW_INTERNAL_BIND` (default
//! [`DEFAULT_INTERNAL_BIND`], loopback), which nginx does not proxy, and every
//! request must present `x-lakko-internal-secret` equal to
//! `LAKKO_PUSH_GW_INTERNAL_SECRET`. The only caller is the DS, which forwards a
//! wake after a signed publish; the DS holds the same value as
//! `LAKKO_DS_PUSH_GW_SECRET`.
//!
//! The secret is base64 of at least [`MIN_SECRET_LEN`] bytes (the transparency
//! append-secret pattern). The presented header is base64-decoded and compared
//! in constant time. Unset: the internal listener is not started at all and a
//! loud warning is logged - no group wake is delivered, and there is no public
//! fallback.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use subtle::ConstantTimeEq as _;
use tower_http::trace::TraceLayer;

use crate::AppState;

/// Default internal bind: loopback, the port after gif-proxy's 8091 in the
/// service port plan (`infra/README.md`).
pub(crate) const DEFAULT_INTERNAL_BIND: &str = "127.0.0.1:8092";

/// The header the shared secret is presented in.
pub(crate) const SECRET_HEADER: &str = "x-lakko-internal-secret";

/// Minimum decoded length of the shared secret.
pub(crate) const MIN_SECRET_LEN: usize = 16;

/// Body limit on the internal listener: the DS sends a sealed wake (312
/// base64url characters) and a message id.
const MAX_INTERNAL_BODY_BYTES: usize = 4 * 1024;

/// Load and validate `LAKKO_PUSH_GW_INTERNAL_SECRET`.
///
/// `Ok(None)` when unset or empty (the internal listener then stays off),
/// `Ok(Some)` for base64 of at least [`MIN_SECRET_LEN`] bytes.
///
/// # Errors
///
/// A message naming the variable when it is set but not base64, or too
/// short - a misconfiguration the service refuses to start with rather than
/// boot into a state where every wake is refused.
pub(crate) fn load_internal_secret() -> Result<Option<Arc<Vec<u8>>>, String> {
    parse_secret(
        std::env::var("LAKKO_PUSH_GW_INTERNAL_SECRET")
            .ok()
            .as_deref(),
    )
}

fn parse_secret(raw: Option<&str>) -> Result<Option<Arc<Vec<u8>>>, String> {
    let Some(raw) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let bytes = B64
        .decode(raw.as_bytes())
        .map_err(|_| "LAKKO_PUSH_GW_INTERNAL_SECRET is not valid base64".to_string())?;
    if bytes.len() < MIN_SECRET_LEN {
        return Err(format!(
            "LAKKO_PUSH_GW_INTERNAL_SECRET decodes to {} bytes; need >= {MIN_SECRET_LEN}",
            bytes.len()
        ));
    }
    Ok(Some(Arc::new(bytes)))
}

/// The internal router: `/push/notify-group` behind the secret gate, and
/// nothing else.
pub(crate) fn build_internal_app(state: AppState) -> Router {
    Router::new()
        .route("/push/notify-group", post(crate::notify::notify_group))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_internal_secret,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_INTERNAL_BODY_BYTES,
        ))
        // Requests by route template, method and status class (ADR-0096).
        .layer(lakko_metrics::red_layer("push-gw"))
        .layer(TraceLayer::new_for_http().make_span_with(lakko_log_hygiene::request_span))
        .with_state(state)
}

/// Refuse any request that does not present the configured secret.
///
/// - no secret configured → **503** (the listener is not started in that
///   case, so this is defence in depth),
/// - header missing, not base64, or not equal (constant time) → **401**.
///
/// The cause is logged for the operator; the caller sees only the status.
pub(crate) async fn require_internal_secret(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(secret) = state.internal_secret.as_ref() else {
        tracing::error!("internal request refused: LAKKO_PUSH_GW_INTERNAL_SECRET unset");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let Some(presented) = request
        .headers()
        .get(SECRET_HEADER)
        .and_then(|v| v.to_str().ok())
    else {
        tracing::warn!("internal request refused: missing secret header");
        return Err(StatusCode::UNAUTHORIZED);
    };
    let Ok(presented) = B64.decode(presented.trim().as_bytes()) else {
        tracing::warn!("internal request refused: secret header not base64");
        return Err(StatusCode::UNAUTHORIZED);
    };
    // `ct_eq` runs in time independent of where the bytes differ; a length
    // mismatch returns early, and the length of a secret is not the secret.
    let matches: bool = secret.as_slice().ct_eq(presented.as_slice()).into();
    if !matches {
        tracing::warn!("internal request refused: secret mismatch");
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    const SECRET_B64: &str = "c2hhcmVkLXdha2Utc2VjcmV0LWZvci10ZXN0cy0zMmI=";

    fn state(secret: Option<&str>) -> AppState {
        crate::tests::state_without_db(secret.map(|s| Arc::new(B64.decode(s).unwrap())))
    }

    async fn post_notify(app: Router, secret: Option<&str>) -> StatusCode {
        let mut req = Request::builder()
            .method("POST")
            .uri("/push/notify-group")
            .header("content-type", "application/json");
        if let Some(s) = secret {
            req = req.header(SECRET_HEADER, s);
        }
        app.oneshot(req.body(Body::from("{}")).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn a_missing_secret_is_401() {
        let app = build_internal_app(state(Some(SECRET_B64)));
        assert_eq!(post_notify(app, None).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_wrong_secret_is_401() {
        let other = B64.encode([7u8; 32]);
        for bad in [other.as_str(), "not base64 !!", "", "c2hvcnQ="] {
            let app = build_internal_app(state(Some(SECRET_B64)));
            assert_eq!(
                post_notify(app, Some(bad)).await,
                StatusCode::UNAUTHORIZED,
                "accepted {bad:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_right_secret_reaches_the_handler() {
        // The body `{}` is refused by the handler's own extractor (422), which
        // is the proof the gate let the request through without a database.
        let app = build_internal_app(state(Some(SECRET_B64)));
        assert_eq!(
            post_notify(app, Some(SECRET_B64)).await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn no_configured_secret_is_503() {
        let app = build_internal_app(state(None));
        assert_eq!(
            post_notify(app, Some(SECRET_B64)).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn the_internal_listener_serves_nothing_else() {
        for path in [
            "/push/subscribe",
            "/push/register",
            "/push/notify",
            "/pow/challenge",
            "/health",
        ] {
            let app = build_internal_app(state(Some(SECRET_B64)));
            let resp = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header(SECRET_HEADER, SECRET_B64)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[test]
    fn the_secret_must_be_base64_of_sixteen_bytes_or_more() {
        assert!(parse_secret(None).unwrap().is_none());
        assert!(parse_secret(Some("  ")).unwrap().is_none());
        assert!(parse_secret(Some("not base64 !!")).is_err());
        assert!(parse_secret(Some(&B64.encode([1u8; 15]))).is_err());
        assert_eq!(
            parse_secret(Some(&B64.encode([1u8; 16])))
                .unwrap()
                .unwrap()
                .len(),
            16
        );
    }
}
