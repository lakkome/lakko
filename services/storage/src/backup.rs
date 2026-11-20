// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Per-account backup-blob endpoints (Phase 2a1b).
//!
//! Sits alongside the content-addressed blob proxy in
//! [`super::blob`]. Three operations behind the
//! `auth::require_session` middleware:
//!
//! - **`PUT /backup`** — owner uploads / overwrites their backup
//!   blob. Body is the wire bytes from `lakko-backup::seal()`.
//!   Server treats the payload as opaque — it can be any bytes
//!   up to the [`MAX_BACKUP_BYTES`] ceiling. The
//!   account-id-the-blob-belongs-to comes from the verified
//!   session token, NOT from the URL or body, so an attacker
//!   with a valid token for account A cannot overwrite account
//!   B's backup.
//! - **`GET /backup`** — owner fetches their current backup blob.
//!   404 when none exists (a fresh account that has never
//!   uploaded).
//! - **`DELETE /backup`** — owner removes their backup blob. The
//!   server has no way to tell "user deleted on purpose" from
//!   "user lost their phrase and gave up" — both end at the
//!   same filesystem state. 204 on success, 404 when none
//!   exists (idempotent).
//!
//! ## On-disk layout
//!
//! Backup blobs live at `<LAKKO_BLOB_DIR>/backup/<account_id_hex>`.
//! Distinct from the content-addressed blob layout
//! (`<LAKKO_BLOB_DIR>/<aa>/<bb>/<full_hash>`) so an operator
//! browsing the disk can tell them apart at a glance, and so a
//! filesystem-level audit can scope a per-account sweep cleanly.
//!
//! ## Why per-account, not content-addressed
//!
//! The content-addressed model works for ciphertext blobs
//! because **anyone who knows the hash can read them, and reading
//! gives them only the ciphertext**. For backup blobs we want
//! "only the account owner can read or write." A
//! content-addressed scheme would force the client to remember
//! the hash, which defeats the "fresh device, only recovery
//! phrase" recovery use case.
//!
//! Per-account naming + session-token gate trades one privacy
//! property (server learns the hash of every blob as the
//! filename) for the recovery ergonomics. Storage is encrypted
//! by the client; the server filename leak is acceptable for the
//! threat model.
//!
//! ## Overwrite atomicity
//!
//! `PUT /backup` is atomic via the same `tempfile + rename`
//! pattern the content-addressed path uses. A crashed upload
//! leaves the existing backup blob intact (if any) rather than
//! truncating it. The temp file lives in the same parent so the
//! `rename(2)` is filesystem-atomic.

use crate::auth::AuthenticatedAccount;
use crate::stream::ReaderStream;
use crate::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures::StreamExt as _;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt as _;

/// Per-blob ceiling for the backup payload. 4 MiB is generous
/// — a typical Phase 2a1 payload (identity + ~hundred-contact
/// address book + a dozen MLS snapshots) lands well under 100
/// KiB. We pick a high ceiling so the codec can grow as 2a3
/// brings DeviceGroup-side snapshot publishing online without
/// another deploy.
pub(crate) const MAX_BACKUP_BYTES: usize = 4 * 1024 * 1024;

// ============================================================================
// PUT /backup
// ============================================================================

/// `PUT /backup` — overwrite the calling account's backup blob.
///
/// The body is opaque to the server — it's the wire bytes from
/// `lakko-backup::seal()`. The server only enforces:
///   - body ≤ [`MAX_BACKUP_BYTES`]
///   - atomic rename so a crashed upload doesn't corrupt the
///     existing backup
///   - per-account isolation via the session-token middleware
pub(crate) async fn put_backup(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<AuthenticatedAccount>,
    body: Body,
) -> Result<Response, (StatusCode, String)> {
    let parent = backup_parent(&state.blob_root);
    let target = backup_path(&state.blob_root, &authed.0);
    // The free-space guard (`crate::disk`), with the largest backup as the
    // incoming size: the body length is not known before it is read.
    if !crate::disk::check(
        &state.blob_root,
        MAX_BACKUP_BYTES as u64,
        state.limits.min_free_bytes,
    )
    .await
    {
        return Err((StatusCode::INSUFFICIENT_STORAGE, "storage-full".to_string()));
    }

    tokio::fs::create_dir_all(&parent).await.map_err(|e| {
        tracing::error!(error = %e, dir = %parent.display(), "create backup parent");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal io error".to_string(),
        )
    })?;

    let temp = make_tempfile_in(&parent).await?;
    let bytes_written = stream_body_into_tempfile(body, &temp).await?;

    let already_existed = tokio::fs::metadata(&target).await.is_ok();
    persist_tempfile(temp, target.clone()).await?;

    tracing::info!(
        bytes = bytes_written,
        first_upload = !already_existed,
        "backup blob written",
    );

    let status = if already_existed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, "").into_response())
}

// ============================================================================
// GET /backup
// ============================================================================

/// `GET /backup` — fetch the calling account's backup blob.
pub(crate) async fn get_backup(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<AuthenticatedAccount>,
) -> Result<Response, (StatusCode, String)> {
    let path = backup_path(&state.blob_root, &authed.0);

    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err((
                StatusCode::NOT_FOUND,
                "no backup for this account".to_string(),
            ));
        },
        Err(e) => {
            tracing::error!(error = %e, path = %path.display(), "open backup");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal io error".to_string(),
            ));
        },
    };
    let metadata = file.metadata().await.map_err(|e| {
        tracing::error!(error = %e, "stat backup");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal io error".to_string(),
        )
    })?;

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mut response = body.into_response();
    response
        .headers_mut()
        .insert(axum::http::header::CONTENT_LENGTH, metadata.len().into());
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/octet-stream"),
    );
    Ok(response)
}

// ============================================================================
// DELETE /backup
// ============================================================================

/// `DELETE /backup` — remove the calling account's backup blob.
/// Idempotent — a missing blob still returns 204 No Content.
pub(crate) async fn delete_backup(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<AuthenticatedAccount>,
) -> Result<StatusCode, (StatusCode, String)> {
    match remove_backup(&state.blob_root, &authed.0).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e) => {
            tracing::error!(error = %e, "delete backup");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal io error".to_string(),
            ))
        },
    }
}

/// Remove the backup blob of `account_id`, if there is one. Idempotent: a
/// missing blob is success. Shared by `DELETE /backup` and the internal
/// purge route auth calls after deleting an account (`crate::internal`).
///
/// # Errors
///
/// An I/O error other than "not found".
pub(crate) async fn remove_backup(root: &Path, account_id: &[u8; 16]) -> std::io::Result<()> {
    match tokio::fs::remove_file(backup_path(root, account_id)).await {
        Ok(()) => {
            tracing::info!("backup blob deleted");
            Ok(())
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Parent directory for all per-account backup blobs:
/// `<root>/backup/`.
fn backup_parent(root: &Path) -> PathBuf {
    root.join("backup")
}

/// Filesystem path for the backup blob of `account_id`:
/// `<root>/backup/<account_id_hex>`.
fn backup_path(root: &Path, account_id: &[u8; 16]) -> PathBuf {
    backup_parent(root).join(hex_encode(account_id))
}

/// Lowercase-hex encode a fixed-size byte array — the only
/// caller is the backup-path helper, but extracted so the
/// log-line emitter shares the same exact format.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // `write!` into a String is infallible per the
        // `std::fmt::Write` impl; the `expect` documents that.
        write!(out, "{b:02x}").expect("write to String never fails");
    }
    out
}

/// Create a tempfile in `parent` that the `rename(2)` step will
/// atomically swap into place. The handle is kept open so the
/// caller can stream into it; `parent`'s mount must match the
/// final target's so the rename stays in-filesystem.
async fn make_tempfile_in(parent: &Path) -> Result<tempfile::NamedTempFile, (StatusCode, String)> {
    let parent = parent.to_path_buf();
    tokio::task::spawn_blocking(move || tempfile::NamedTempFile::new_in(parent))
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "spawn_blocking for tempfile");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal io error".to_string(),
            )
        })?
        .map_err(|e| {
            tracing::error!(error = %e, "create tempfile");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal io error".to_string(),
            )
        })
}

/// Stream the request body into the tempfile, enforcing the
/// `MAX_BACKUP_BYTES` ceiling chunk-by-chunk so an oversize
/// upload aborts early without flushing the whole tail to disk.
/// Returns the byte count written.
async fn stream_body_into_tempfile(
    body: Body,
    temp: &tempfile::NamedTempFile,
) -> Result<usize, (StatusCode, String)> {
    let mut stream = body.into_data_stream();
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(temp.path())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "open tempfile for write");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal io error".to_string(),
            )
        })?;

    let mut total: usize = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            tracing::warn!(error = %e, "request body stream");
            (StatusCode::BAD_REQUEST, "body read error".to_string())
        })?;
        total = total.saturating_add(chunk.len());
        if total > MAX_BACKUP_BYTES {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("backup blob exceeds {MAX_BACKUP_BYTES}-byte ceiling"),
            ));
        }
        file.write_all(&chunk).await.map_err(|e| {
            tracing::error!(error = %e, "write tempfile chunk");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal io error".to_string(),
            )
        })?;
    }
    file.flush().await.map_err(|e| {
        tracing::error!(error = %e, "flush tempfile");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal io error".to_string(),
        )
    })?;
    Ok(total)
}

/// Atomic `rename(2)` of the staged tempfile into its final
/// location. Both paths must share a filesystem — guaranteed by
/// `make_tempfile_in(parent)` since the tempfile lands in the
/// same parent as the target.
async fn persist_tempfile(
    temp: tempfile::NamedTempFile,
    target: PathBuf,
) -> Result<(), (StatusCode, String)> {
    tokio::task::spawn_blocking(move || temp.persist(target))
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "spawn_blocking for tempfile persist");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal io error".to_string(),
            )
        })?
        .map_err(|e| {
            tracing::error!(error = %e, "tempfile persist");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal io error".to_string(),
            )
        })?;
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_app;
    use axum::body::Body as AxumBody;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use std::sync::Arc;
    use tower::ServiceExt as _;

    /// 16-byte test account id. Distinct constant per test would
    /// be ideal but tests are isolated by separate tempdirs so
    /// reuse is safe.
    const TEST_ACCOUNT_ID: [u8; 16] = [0x42u8; 16];

    fn test_app() -> (
        axum::Router,
        tempfile::TempDir,
        lakko_auth_token::TokenSecret,
    ) {
        let (mut state, tmp) = crate::test_support::state();
        let secret = lakko_auth_token::TokenSecret::new(vec![0xCDu8; 32])
            .expect("32 bytes is over MIN_SECRET_LEN");
        state.session_token_secret = Arc::new(secret.clone());
        (build_app(state), tmp, secret)
    }

    /// Issue a fresh bearer token for `account_id`, valid for an
    /// hour ahead of the current clock — well within the
    /// auth-token crate's freshness window for tests.
    fn mint_token(secret: &lakko_auth_token::TokenSecret, account_id: &[u8; 16]) -> String {
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .expect("UNIX millis fit in i64 well past year 200000");
        let expires_at_ms = now_ms + 60 * 60 * 1_000;
        secret
            .issue(&account_id[..], 1, &[0u8; 32], 0, expires_at_ms)
            .expect("issue")
    }

    /// PUT with a valid bearer + body bytes round-trips to GET.
    #[tokio::test]
    async fn put_then_get_round_trips() {
        let (app, _tmp, secret) = test_app();
        let token = mint_token(&secret, &TEST_ACCOUNT_ID);

        let payload = b"opaque backup envelope bytes".to_vec();
        let put_req = Request::builder()
            .method("PUT")
            .uri("/backup")
            .header("authorization", format!("Bearer {token}"))
            .body(AxumBody::from(payload.clone()))
            .unwrap();
        let put_resp = app.clone().oneshot(put_req).await.unwrap();
        assert_eq!(put_resp.status(), StatusCode::CREATED);

        let get_req = Request::builder()
            .method("GET")
            .uri("/backup")
            .header("authorization", format!("Bearer {token}"))
            .body(AxumBody::empty())
            .unwrap();
        let get_resp = app.oneshot(get_req).await.unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let body = get_resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], &payload[..]);
    }

    /// Re-PUT overwrites the prior blob and surfaces as 200 OK
    /// (vs 201 Created for the first upload).
    #[tokio::test]
    async fn re_put_returns_200_and_overwrites() {
        let (app, _tmp, secret) = test_app();
        let token = mint_token(&secret, &TEST_ACCOUNT_ID);

        // First upload.
        let put1 = Request::builder()
            .method("PUT")
            .uri("/backup")
            .header("authorization", format!("Bearer {token}"))
            .body(AxumBody::from(b"v1".to_vec()))
            .unwrap();
        let resp1 = app.clone().oneshot(put1).await.unwrap();
        assert_eq!(resp1.status(), StatusCode::CREATED);

        // Overwrite.
        let put2 = Request::builder()
            .method("PUT")
            .uri("/backup")
            .header("authorization", format!("Bearer {token}"))
            .body(AxumBody::from(b"v2".to_vec()))
            .unwrap();
        let resp2 = app.clone().oneshot(put2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);

        // GET returns the latest.
        let get = Request::builder()
            .method("GET")
            .uri("/backup")
            .header("authorization", format!("Bearer {token}"))
            .body(AxumBody::empty())
            .unwrap();
        let resp_get = app.oneshot(get).await.unwrap();
        let body = resp_get.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"v2");
    }

    /// GET on a never-uploaded account returns 404.
    #[tokio::test]
    async fn get_missing_returns_404() {
        let (app, _tmp, secret) = test_app();
        let token = mint_token(&secret, &TEST_ACCOUNT_ID);
        let req = Request::builder()
            .method("GET")
            .uri("/backup")
            .header("authorization", format!("Bearer {token}"))
            .body(AxumBody::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// DELETE on an existing blob returns 204 and a subsequent GET
    /// 404s.
    #[tokio::test]
    async fn delete_then_get_returns_404() {
        let (app, _tmp, secret) = test_app();
        let token = mint_token(&secret, &TEST_ACCOUNT_ID);
        // Seed.
        let put = Request::builder()
            .method("PUT")
            .uri("/backup")
            .header("authorization", format!("Bearer {token}"))
            .body(AxumBody::from(b"x".to_vec()))
            .unwrap();
        app.clone().oneshot(put).await.unwrap();

        // Delete.
        let del = Request::builder()
            .method("DELETE")
            .uri("/backup")
            .header("authorization", format!("Bearer {token}"))
            .body(AxumBody::empty())
            .unwrap();
        let resp_del = app.clone().oneshot(del).await.unwrap();
        assert_eq!(resp_del.status(), StatusCode::NO_CONTENT);

        // GET is gone.
        let get = Request::builder()
            .method("GET")
            .uri("/backup")
            .header("authorization", format!("Bearer {token}"))
            .body(AxumBody::empty())
            .unwrap();
        let resp_get = app.oneshot(get).await.unwrap();
        assert_eq!(resp_get.status(), StatusCode::NOT_FOUND);
    }

    /// DELETE on a never-uploaded blob is idempotent — still 204.
    #[tokio::test]
    async fn delete_missing_is_idempotent() {
        let (app, _tmp, secret) = test_app();
        let token = mint_token(&secret, &TEST_ACCOUNT_ID);
        let del = Request::builder()
            .method("DELETE")
            .uri("/backup")
            .header("authorization", format!("Bearer {token}"))
            .body(AxumBody::empty())
            .unwrap();
        let resp = app.oneshot(del).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    /// PUT without a session token returns 401.
    #[tokio::test]
    async fn put_without_auth_returns_401() {
        let (app, _tmp, _secret) = test_app();
        let req = Request::builder()
            .method("PUT")
            .uri("/backup")
            .body(AxumBody::from(b"x".to_vec()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// Two different tokens for two different accounts produce
    /// independent backup blobs — overwriting account A's blob
    /// does not affect account B's. This is the load-bearing
    /// per-account-isolation gate.
    #[tokio::test]
    async fn distinct_accounts_have_distinct_backups() {
        let (app, _tmp, secret) = test_app();
        let account_a: [u8; 16] = [0xAA; 16];
        let account_b: [u8; 16] = [0xBB; 16];
        let token_a = mint_token(&secret, &account_a);
        let token_b = mint_token(&secret, &account_b);

        // A uploads.
        let put_a = Request::builder()
            .method("PUT")
            .uri("/backup")
            .header("authorization", format!("Bearer {token_a}"))
            .body(AxumBody::from(b"a-payload".to_vec()))
            .unwrap();
        app.clone().oneshot(put_a).await.unwrap();

        // B uploads with completely different bytes.
        let put_b = Request::builder()
            .method("PUT")
            .uri("/backup")
            .header("authorization", format!("Bearer {token_b}"))
            .body(AxumBody::from(b"b-payload-different".to_vec()))
            .unwrap();
        app.clone().oneshot(put_b).await.unwrap();

        // A reads — must still see their own bytes (B did not
        // smash A's blob).
        let get_a = Request::builder()
            .method("GET")
            .uri("/backup")
            .header("authorization", format!("Bearer {token_a}"))
            .body(AxumBody::empty())
            .unwrap();
        let body_a = app
            .clone()
            .oneshot(get_a)
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(&body_a[..], b"a-payload");

        // B reads — sees B's bytes.
        let get_b = Request::builder()
            .method("GET")
            .uri("/backup")
            .header("authorization", format!("Bearer {token_b}"))
            .body(AxumBody::empty())
            .unwrap();
        let body_b = app
            .oneshot(get_b)
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(&body_b[..], b"b-payload-different");
    }
}
