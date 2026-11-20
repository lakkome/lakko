// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The internal listener: `services/auth` removes a deleted account's backup
//! blob here.
//!
//! When `services/auth` deletes an account it queues a purge
//! (`services/auth/src/purge.rs`) and its worker calls
//! `DELETE /internal/accounts/{account_id_hex}/backup`, which removes
//! `<LAKKO_BLOB_DIR>/backup/<account_id_hex>` exactly as the public
//! `DELETE /backup` does ([`crate::backup::remove_backup`]). Idempotent:
//! `204` also when there was no blob. The backup is the only thing this
//! service keys by account: blobs, index rows and passes name none.
//!
//! The route is served on a second listener, bound to
//! `LAKKO_STORAGE_INTERNAL_BIND` (loopback, never proxied by nginx), and
//! every request must present `x-lakko-internal-secret` equal to
//! `LAKKO_INTERNAL_PURGE_SECRET` (base64 of at least [`MIN_SECRET_LEN`]
//! bytes; the same value auth and discovery hold), compared in constant
//! time. Both unset: no listener. One set without the other: a startup
//! error.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::delete;
use axum::Router;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use std::path::PathBuf;
use subtle::ConstantTimeEq as _;
use tower_http::trace::TraceLayer;

/// The internal listener's address.
pub(crate) const BIND_VAR: &str = "LAKKO_STORAGE_INTERNAL_BIND";
/// The shared secret (one name across auth, discovery and storage).
pub(crate) const SECRET_VAR: &str = "LAKKO_INTERNAL_PURGE_SECRET";
/// The header the secret is presented in.
pub(crate) const SECRET_HEADER: &str = "x-lakko-internal-secret";
/// Minimum decoded length of the secret.
pub(crate) const MIN_SECRET_LEN: usize = 16;

/// The listener address and the decoded secret.
pub(crate) type InternalConfig = (SocketAddr, Arc<Vec<u8>>);

/// What the internal routes need.
#[derive(Clone)]
pub(crate) struct InternalState {
    pub(crate) blob_root: Arc<PathBuf>,
    pub(crate) secret: Arc<Vec<u8>>,
}

/// Read the listener address and the secret from the environment.
///
/// # Errors
///
/// When only one of the two is set, the address does not parse, or the
/// secret is not base64 of at least [`MIN_SECRET_LEN`] bytes.
pub(crate) fn load_config() -> Result<Option<InternalConfig>, String> {
    parse_config(
        std::env::var(BIND_VAR).ok().as_deref(),
        std::env::var(SECRET_VAR).ok().as_deref(),
    )
}

fn parse_config(
    bind: Option<&str>,
    secret: Option<&str>,
) -> Result<Option<InternalConfig>, String> {
    let set = |v: Option<&str>| {
        v.map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    match (set(bind), set(secret)) {
        (None, None) => Ok(None),
        (Some(bind), Some(secret)) => {
            let addr: SocketAddr = bind
                .parse()
                .map_err(|_| format!("{BIND_VAR} must be host:port"))?;
            let bytes = B64
                .decode(secret.as_bytes())
                .map_err(|_| format!("{SECRET_VAR} is not valid base64"))?;
            if bytes.len() < MIN_SECRET_LEN {
                return Err(format!(
                    "{SECRET_VAR} decodes to {} bytes; need >= {MIN_SECRET_LEN}",
                    bytes.len()
                ));
            }
            Ok(Some((addr, Arc::new(bytes))))
        },
        _ => Err(format!(
            "{BIND_VAR} and {SECRET_VAR} must be set together or not at all"
        )),
    }
}

/// The internal router: the backup purge route behind the secret gate,
/// nothing else.
pub(crate) fn build_internal_app(state: InternalState) -> Router {
    Router::new()
        .route(
            "/internal/accounts/{account_id_hex}/backup",
            delete(purge_backup),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_secret,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(1024))
        .layer(TraceLayer::new_for_http().make_span_with(lakko_log_hygiene::request_span))
        .with_state(state)
}

/// Refuse a request that does not present the configured secret (`401`).
async fn require_secret(
    State(state): State<InternalState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let presented = request
        .headers()
        .get(SECRET_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| B64.decode(v.trim().as_bytes()).ok());
    let Some(presented) = presented else {
        tracing::warn!("internal request refused: secret header missing or not base64");
        return Err(StatusCode::UNAUTHORIZED);
    };
    // A length mismatch returns early; the length of a secret is not the
    // secret.
    let matches: bool = state.secret.as_slice().ct_eq(presented.as_slice()).into();
    if !matches {
        tracing::warn!("internal request refused: secret mismatch");
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

/// Parse 32 hex characters into an account id.
fn parse_account_id(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    hex::decode(text).ok()?.try_into().ok()
}

/// `DELETE /internal/accounts/{account_id_hex}/backup`.
async fn purge_backup(
    State(state): State<InternalState>,
    Path(account_id_hex): Path<String>,
) -> StatusCode {
    let Some(account_id) = parse_account_id(&account_id_hex) else {
        return StatusCode::BAD_REQUEST;
    };
    match crate::backup::remove_backup(&state.blob_root, &account_id).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(e) => {
            tracing::warn!(error = %e, "internal backup purge failed");
            StatusCode::INTERNAL_SERVER_ERROR
        },
    }
}

/// Start the internal listener when configured; warn when not.
///
/// # Errors
///
/// When the address cannot be bound.
pub(crate) async fn spawn(
    blob_root: Arc<PathBuf>,
    config: Option<InternalConfig>,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    let Some((addr, secret)) = config else {
        tracing::warn!(
            "{BIND_VAR} / {SECRET_VAR} unset - the internal listener is OFF: auth cannot remove \
             the backup blob of a deleted account"
        );
        return Ok(None);
    };
    if !addr.ip().is_loopback() {
        tracing::warn!(%addr, "internal listener is not on loopback; keep it off any public interface");
    }
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("bind internal {addr}: {e}"))?;
    tracing::info!(%addr, "storage internal listener up");
    let app = build_internal_app(InternalState { blob_root, secret });
    Ok(Some(tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!(error = %e, "storage internal listener stopped");
        }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt as _;

    const SECRET_B64: &str = "cHVyZ2Utc2VjcmV0LWZvci10ZXN0cy0zMi1ieXRlcyE=";

    #[test]
    fn both_or_neither_must_be_set() {
        assert!(parse_config(None, None).unwrap().is_none());
        assert!(parse_config(Some(" "), Some("")).unwrap().is_none());
        assert!(parse_config(Some("127.0.0.1:8095"), None).is_err());
        assert!(parse_config(None, Some(SECRET_B64)).is_err());
        assert!(parse_config(Some("nope"), Some(SECRET_B64)).is_err());
        assert!(parse_config(Some("127.0.0.1:8095"), Some("c2hvcnQ=")).is_err());
        assert!(parse_config(Some("127.0.0.1:8095"), Some("!!")).is_err());
        let (addr, _) = parse_config(Some("127.0.0.1:8095"), Some(SECRET_B64))
            .unwrap()
            .unwrap();
        assert_eq!(addr.port(), 8095);
    }

    #[test]
    fn an_account_id_is_32_hex_characters() {
        assert_eq!(parse_account_id(&"ab".repeat(16)), Some([0xab; 16]));
        assert_eq!(parse_account_id(&"AB".repeat(16)), Some([0xab; 16]));
        assert!(parse_account_id(&"ab".repeat(15)).is_none());
        assert!(parse_account_id(&"zz".repeat(16)).is_none());
    }

    async fn send(app: &Router, method: Method, path: &str, secret: Option<&str>) -> StatusCode {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(s) = secret {
            req = req.header(SECRET_HEADER, s);
        }
        app.clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn the_internal_delete_removes_the_backup_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let root = Arc::new(dir.path().to_path_buf());
        let app = build_internal_app(InternalState {
            blob_root: root.clone(),
            secret: Arc::new(B64.decode(SECRET_B64).unwrap()),
        });
        let id = [0x4bu8; 16];
        let other = [0x4cu8; 16];
        let backups = root.join("backup");
        std::fs::create_dir_all(&backups).unwrap();
        let file = backups.join(hex::encode(id));
        let other_file = backups.join(hex::encode(other));
        std::fs::write(&file, b"sealed backup").unwrap();
        std::fs::write(&other_file, b"another account").unwrap();
        let path = format!("/internal/accounts/{}/backup", hex::encode(id));

        // No secret, a wrong one, a wrong method or path: nothing removed.
        assert_eq!(
            send(&app, Method::DELETE, &path, None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            send(
                &app,
                Method::DELETE,
                &path,
                Some("d3Jvbmctc2VjcmV0LXdyb25nLXNlY3JldA==")
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            send(&app, Method::DELETE, "/backup", Some(SECRET_B64)).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            send(&app, Method::GET, &path, Some(SECRET_B64)).await,
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert!(file.exists());

        assert_eq!(
            send(
                &app,
                Method::DELETE,
                "/internal/accounts/zz/backup",
                Some(SECRET_B64)
            )
            .await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            send(&app, Method::DELETE, &path, Some(SECRET_B64)).await,
            StatusCode::NO_CONTENT
        );
        assert!(!file.exists());
        assert!(other_file.exists(), "another account's backup was removed");
        // Again: nothing there, still 204.
        assert_eq!(
            send(&app, Method::DELETE, &path, Some(SECRET_B64)).await,
            StatusCode::NO_CONTENT
        );
    }
}
