// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Content-addressable blob handlers.
//!
//! Blobs are stored at `<root>/<hex[0:2]>/<hex[2:4]>/<hex>`. The two
//! 256-way fan-out levels keep the per-directory file count bounded
//! (~1.5M blobs spread across 65k leaf directories ≈ 23 files/dir
//! before any single directory hits the typical ext4 readdir slowdown
//! threshold).
//!
//! On PUT the body is streamed through a SHA-256 hasher into a
//! tempfile in the same parent directory; if the final hash matches
//! the URL parameter the tempfile is atomically renamed into place.
//! Otherwise the tempfile is dropped (cleaned up by `tempfile::NamedTempFile`).
//! Re-uploading an already-present blob is idempotent (same content
//! by definition of content addressing).
//!
//! GET / HEAD just check filesystem existence and serve via tokio's
//! async file IO — `axum::body::Body::from_stream` keeps memory flat
//! regardless of blob size.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::StreamExt as _;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;

use crate::AppState;

/// SHA-256 hex length: 32 bytes × 2 hex chars.
const HEX_LEN: usize = 64;

/// Validate that `hex` is exactly 64 lowercase hex characters. Rejects
/// uppercase too, so the URL has a single canonical form (avoids
/// path-traversal-via-case-insensitive-filesystem and double-stored
/// blobs that differ only in casing).
fn validate_hex(hex: &str) -> Result<(), (StatusCode, String)> {
    if hex.len() != HEX_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("blob hash must be {HEX_LEN} hex chars"),
        ));
    }
    if !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err((
            StatusCode::BAD_REQUEST,
            "blob hash must be lowercase hex [0-9a-f]".to_string(),
        ));
    }
    Ok(())
}

/// Resolve `<root>/<hex[0:2]>/<hex[2:4]>/<hex>`. Caller must have
/// validated `hex` first.
fn blob_path(root: &Path, hex: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    p.push(&hex[0..2]);
    p.push(&hex[2..4]);
    p.push(hex);
    p
}

/// Parent directory of the blob path (where the tempfile is created so
/// the final rename stays on the same filesystem).
fn blob_parent(root: &Path, hex: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    p.push(&hex[0..2]);
    p.push(&hex[2..4]);
    p
}

/// Probe that the blob root is writable by creating and deleting a
/// uniquely-named temp file. Used by the `/ready` probe and by
/// `main()` at startup.
pub(crate) async fn probe_writable(root: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(root).await?;
    let probe = tempfile::Builder::new()
        .prefix(".lakko-storage-probe-")
        .tempfile_in(root)?;
    drop(probe);
    Ok(())
}

// ============================================================================
// PUT /blob/{hex}
// ============================================================================

/// Create a tempfile in `parent` so the final rename stays on the
/// same filesystem (rename is atomic only within one mount). Runs the
/// blocking call on the dedicated tokio pool.
async fn make_tempfile_in(
    parent: PathBuf,
) -> Result<tempfile::NamedTempFile, (StatusCode, String)> {
    let parent_for_log = parent.clone();
    tokio::task::spawn_blocking(move || {
        tempfile::Builder::new()
            .prefix(".lakko-blob-")
            .tempfile_in(&parent)
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "spawn_blocking for tempfile");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal error".to_string(),
        )
    })?
    .map_err(|e| {
        tracing::error!(error = %e, dir = %parent_for_log.display(), "create tempfile");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal io error".to_string(),
        )
    })
}

/// Stream the request body through a SHA-256 hasher into the tempfile.
/// Returns the lowercase-hex digest of what was actually written.
async fn stream_body_into_tempfile(
    body: Body,
    temp: &tempfile::NamedTempFile,
) -> Result<String, (StatusCode, String)> {
    let mut hasher = Sha256::new();
    let mut writer = tokio::fs::File::from_std(
        temp.as_file()
            .try_clone()
            .map_err(|e| internal_io("clone temp fd", &e))?,
    );

    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            tracing::warn!(error = %e, "client body stream error");
            (StatusCode::BAD_REQUEST, format!("body stream error: {e}"))
        })?;
        hasher.update(&chunk);
        writer
            .write_all(&chunk)
            .await
            .map_err(|e| internal_io("write blob chunk", &e))?;
    }
    writer
        .flush()
        .await
        .map_err(|e| internal_io("flush blob", &e))?;
    writer
        .sync_data()
        .await
        .map_err(|e| internal_io("fsync blob", &e))?;

    Ok(hex::encode(hasher.finalize()))
}

/// Atomically publish the tempfile at `target`. Overwrites any existing
/// file — safe because content addressing guarantees identical bytes.
async fn persist_tempfile(
    temp: tempfile::NamedTempFile,
    target: PathBuf,
) -> Result<(), (StatusCode, String)> {
    let target_for_log = target.clone();
    tokio::task::spawn_blocking(move || temp.persist(&target))
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "spawn_blocking for persist");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error".to_string(),
            )
        })?
        .map_err(|e| {
            tracing::error!(
                error = %e.error,
                target = %target_for_log.display(),
                "persist blob tempfile",
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal io error".to_string(),
            )
        })?;
    Ok(())
}

/// `PUT /blob/{hex}` — upload a blob. The body is streamed through a
/// SHA-256 hasher; if the resulting digest matches `hex` the blob is
/// atomically committed. Otherwise the tempfile is discarded and the
/// caller gets 400 Bad Request.
///
/// Idempotent: re-uploading the same hash overwrites with byte-identical
/// content (which is a no-op by the meaning of content addressing).
/// Returns:
///
/// - **201 Created** — first upload of this hash
/// - **200 OK**      — re-upload (already existed)
/// - **400 Bad Request** — hash mismatch or malformed hex
/// - **413 Payload Too Large** — > `MAX_BLOB_BYTES` (handled by the
///   `DefaultBodyLimit` layer)
/// - **500 Internal** — IO error
pub(crate) async fn put_blob(
    State(state): State<AppState>,
    AxumPath(hex): AxumPath<String>,
    body: Body,
) -> Result<Response, (StatusCode, String)> {
    validate_hex(&hex)?;

    let target = blob_path(&state.blob_root, &hex);
    let parent = blob_parent(&state.blob_root, &hex);
    let already_exists = tokio::fs::metadata(&target).await.is_ok();

    tokio::fs::create_dir_all(&parent).await.map_err(|e| {
        tracing::error!(error = %e, dir = %parent.display(), "create blob parent");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal io error".to_string(),
        )
    })?;

    let temp = make_tempfile_in(parent).await?;
    let computed_hex = stream_body_into_tempfile(body, &temp).await?;

    if computed_hex != hex {
        tracing::info!(
            claimed = %hex,
            computed = %computed_hex,
            "PUT /blob: hash mismatch — discarding upload",
        );
        // `temp` drops here, removing the partial file.
        return Err((
            StatusCode::BAD_REQUEST,
            "blob content does not match URL hash".to_string(),
        ));
    }

    persist_tempfile(temp, target).await?;

    let status = if already_exists {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, "").into_response())
}

// ============================================================================
// GET /blob/{hex}
// ============================================================================

/// `GET /blob/{hex}` — fetch a blob. Streamed via tokio file IO + a
/// `ReaderStream`, so memory stays flat regardless of size.
pub(crate) async fn get_blob(
    State(state): State<AppState>,
    AxumPath(hex): AxumPath<String>,
) -> Result<Response, (StatusCode, String)> {
    validate_hex(&hex)?;

    let path = blob_path(&state.blob_root, &hex);

    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err((StatusCode::NOT_FOUND, "blob not found".to_string()));
        },
        Err(e) => {
            tracing::error!(error = %e, path = %path.display(), "open blob");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal io error".to_string(),
            ));
        },
    };
    let metadata = file
        .metadata()
        .await
        .map_err(|e| internal_io("stat blob", &e))?;

    let stream = tokio_util_compat::ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        header::HeaderValue::from(metadata.len()),
    );
    Ok(response)
}

// ============================================================================
// HEAD /blob/{hex}
// ============================================================================

/// `HEAD /blob/{hex}` — return 200 if the blob exists, 404 otherwise.
/// `Content-Length` is set so callers can size their downloads.
pub(crate) async fn head_blob(
    State(state): State<AppState>,
    AxumPath(hex): AxumPath<String>,
) -> Result<Response, (StatusCode, String)> {
    validate_hex(&hex)?;

    let path = blob_path(&state.blob_root, &hex);
    match tokio::fs::metadata(&path).await {
        Ok(meta) => {
            let mut response = Response::new(Body::empty());
            response.headers_mut().insert(
                header::CONTENT_LENGTH,
                header::HeaderValue::from(meta.len()),
            );
            Ok(response)
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err((StatusCode::NOT_FOUND, String::new()))
        },
        Err(e) => {
            tracing::error!(error = %e, path = %path.display(), "head blob");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal io error".to_string(),
            ))
        },
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn internal_io(ctx: &str, e: &std::io::Error) -> (StatusCode, String) {
    tracing::error!(error = %e, "{ctx}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal io error".to_string(),
    )
}

/// Tiny shim for `tokio_util::io::ReaderStream` so we don't pull the
/// whole `tokio-util` crate. `tokio` does not export this on its own,
/// so we hand-roll a minimal `Stream<Item = Result<Bytes, io::Error>>`
/// over an `AsyncRead`. 8 KB chunks balance syscall count and memory.
mod tokio_util_compat {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use bytes::Bytes;
    use futures::Stream;
    use tokio::io::{AsyncRead, ReadBuf};

    const CHUNK: usize = 8 * 1024;

    pub(super) struct ReaderStream<R> {
        reader: R,
        buf: Vec<u8>,
    }

    impl<R: AsyncRead + Unpin> ReaderStream<R> {
        pub(super) fn new(reader: R) -> Self {
            Self {
                reader,
                buf: vec![0u8; CHUNK],
            }
        }
    }

    impl<R: AsyncRead + Unpin> Stream for ReaderStream<R> {
        type Item = std::io::Result<Bytes>;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            let mut read_buf = ReadBuf::new(&mut this.buf);
            match Pin::new(&mut this.reader).poll_read(cx, &mut read_buf) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
                Poll::Ready(Ok(())) => {
                    let filled = read_buf.filled();
                    if filled.is_empty() {
                        Poll::Ready(None)
                    } else {
                        Poll::Ready(Some(Ok(Bytes::copy_from_slice(filled))))
                    }
                },
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_app;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use std::sync::Arc;
    use tower::ServiceExt as _;

    fn test_app() -> (axum::Router, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState {
            blob_root: Arc::new(tmp.path().to_path_buf()),
        };
        (build_app(state), tmp)
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        hex::encode(h.finalize())
    }

    #[test]
    fn validate_hex_accepts_canonical_lowercase() {
        let s = "0".repeat(64);
        assert!(validate_hex(&s).is_ok());
        let s = "abcdef0123456789".repeat(4);
        assert_eq!(s.len(), 64);
        assert!(validate_hex(&s).is_ok());
    }

    #[test]
    fn validate_hex_rejects_wrong_length_and_case() {
        assert!(validate_hex("").is_err());
        assert!(validate_hex(&"0".repeat(63)).is_err());
        assert!(validate_hex(&"0".repeat(65)).is_err());
        // Uppercase rejected — single canonical form.
        assert!(validate_hex(&"A".repeat(64)).is_err());
        // Path traversal-like bytes rejected.
        assert!(
            validate_hex("../etc/passwd0000000000000000000000000000000000000000000000000000",)
                .is_err()
        );
        // Non-hex chars.
        assert!(validate_hex(&"g".repeat(64)).is_err());
    }

    #[test]
    fn blob_path_is_two_level_sharded() {
        let root = std::path::Path::new("/var/lib/lakko/blobs");
        let hex = "abcdef0123456789".repeat(4);
        let p = blob_path(root, &hex);
        assert_eq!(
            p,
            std::path::PathBuf::from(format!("/var/lib/lakko/blobs/ab/cd/{hex}"))
        );
    }

    #[tokio::test]
    async fn put_then_get_roundtrips() {
        let (app, _tmp) = test_app();
        let payload = b"hello lakko ciphertext";
        let hex = sha256_hex(payload);

        // PUT
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/blob/{hex}"))
                    .body(Body::from(payload.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        // GET
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/blob/{hex}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let got = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&got[..], payload);
    }

    #[tokio::test]
    async fn put_with_wrong_hash_returns_400() {
        let (app, _tmp) = test_app();
        let payload = b"some bytes";
        // Use a different hash on the URL than the body actually hashes to.
        let bogus_hex = "0".repeat(64);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/blob/{bogus_hex}"))
                    .body(Body::from(payload.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn put_with_invalid_hex_returns_400() {
        let (app, _tmp) = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/blob/notvalidhex")
                    .body(Body::from(&b"data"[..]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn re_put_is_idempotent_and_returns_200() {
        let (app, _tmp) = test_app();
        let payload = b"reupload me";
        let hex = sha256_hex(payload);

        // First PUT — 201 Created.
        let r1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/blob/{hex}"))
                    .body(Body::from(payload.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r1.status(), StatusCode::CREATED);

        // Second PUT — 200 OK (already existed).
        let r2 = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/blob/{hex}"))
                    .body(Body::from(payload.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r2.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_unknown_returns_404() {
        let (app, _tmp) = test_app();
        let unknown = "f".repeat(64);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/blob/{unknown}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn head_existing_returns_200_with_content_length() {
        let (app, _tmp) = test_app();
        let payload = b"sized blob";
        let hex = sha256_hex(payload);

        // PUT first.
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/blob/{hex}"))
                    .body(Body::from(payload.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // HEAD.
        let response = app
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri(format!("/blob/{hex}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cl = response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok());
        assert_eq!(cl, Some(payload.len()));
    }

    #[tokio::test]
    async fn head_missing_returns_404() {
        let (app, _tmp) = test_app();
        let unknown = "1".repeat(64);
        let response = app
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri(format!("/blob/{unknown}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn put_creates_two_level_sharded_path_on_disk() {
        let (app, tmp) = test_app();
        let payload = b"layout test";
        let hex = sha256_hex(payload);

        let _ = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/blob/{hex}"))
                    .body(Body::from(payload.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let expected = tmp.path().join(&hex[0..2]).join(&hex[2..4]).join(&hex);
        assert!(
            expected.exists(),
            "blob expected at sharded path {}",
            expected.display(),
        );
    }
}
