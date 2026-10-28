// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Content-addressed blob routes (ADR-0065).
//!
//! Blobs are stored at `<root>/<hex[0:2]>/<hex[2:4]>/<hex>`; the two 256-way
//! fan-out levels keep the per-directory file count bounded. Every request
//! is answered from the index ([`crate::index`]): a file with no row does not
//! exist for a client.
//!
//! # Routes
//!
//! | Route | Credential | Success |
//! |---|---|---|
//! | `POST /blob/pass/upload` | one `blob-upload` anonymous token (`x-lakko-anon-token`) | 200 pass |
//! | `POST /blob/pass/read` | one `blob-read` anonymous token | 200 pass |
//! | `PUT /blob/{hex}` | upload pass (`x-lakko-blob-pass`), `x-lakko-blob-delete-key`, `Content-Length` | 201 new / 200 existed |
//! | `GET /blob/{hex}` | read pass | 200 streamed |
//! | `HEAD /blob/{hex}` | read pass | 200, `Content-Length` |
//! | `POST /blob/keep` | read pass | 204 |
//! | `DELETE /blob/{hex}` | `x-lakko-storage-proof` under the row's delete key | 204 |
//!
//! Every refusal is `{"error": code}` (HEAD: status only). Codes are listed
//! on [`ApiError`]. The order of checks on a pass route is: the pass secret
//! is configured, the pass verifies, the request is well formed, the budget
//! is charged, then the index is consulted - so a request without a pass
//! learns nothing about which blobs exist, and a probe costs budget whether
//! the blob exists or not.
//!
//! # Access
//!
//! A download, a keep and a repeated upload are accesses: each raises the
//! row's last access day to today, written at most once per blob per day. A
//! `HEAD` is not an access. The daily sweep ([`crate::sweep`]) deletes a blob
//! whose last access is more than the lifetime (365 days by default) back.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use futures::StreamExt as _;
use lakko_anon_token::TokenContext;
use lakko_anon_token_server::now_unix_secs;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;

use crate::index::{today, BlobId, BlobRow, IndexError, UploadOutcome};
use crate::pass::{Pass, PassError, PassKind, PASS_HEADER};
use crate::AppState;

/// SHA-256 hex length: 32 bytes x 2 hex chars.
const HEX_LEN: usize = 64;

/// The upload header carrying the blob's delete public key: base64url
/// without padding of 32 bytes (43 characters).
pub(crate) const DELETE_KEY_HEADER: &str = "x-lakko-blob-delete-key";

/// Most ids one `POST /blob/keep` may carry. Chosen, not measured: a
/// conversation's pinned and saved files fit in one request.
pub(crate) const MAX_KEEP_IDS: usize = 256;

/// Body ceiling of `POST /blob/keep`: 256 ids of 66 JSON bytes plus slack.
pub(crate) const MAX_KEEP_BODY: usize = 32 * 1024;

/// A coded refusal. Status and `{"error": code}`:
///
/// | Status | Code | When |
/// |---|---|---|
/// | 503 | `blob-pass-disabled` | `LAKKO_STORAGE_PASS_SECRET` unset: no pass can be issued or checked |
/// | 401 | `blob-pass-required` | no `x-lakko-blob-pass` header |
/// | 401 | `blob-pass-invalid` | not a pass, a wrong tag, or a pass of the other kind |
/// | 401 | `blob-pass-expired` | a genuine pass past its hour |
/// | 429 | `blob-upload-budget-exhausted` | the upload pass has no files or bytes left for this upload |
/// | 429 | `blob-read-budget-exhausted` | the read pass has no downloads or keeps left |
/// | 400 | `blob-id-malformed` | the path is not 64 lower-case hex |
/// | 400 | `blob-delete-key-required` / `blob-delete-key-invalid` | upload without a usable delete key |
/// | 411 | `blob-length-required` | upload without `Content-Length` |
/// | 413 | `blob-too-large` | `Content-Length` above the 50 MiB ceiling |
/// | 400 | `blob-hash-mismatch` | the body does not hash to the path |
/// | 400 | `blob-length-mismatch` | the body is not `Content-Length` bytes |
/// | 400 | `keep-malformed` | the keep body is not `{"ids": [hex, ...]}` with 1..=256 ids |
/// | 404 | `blob-not-found` | no such blob (never stored, deleted, or expired) |
/// | 403 | `blob-delete-refused` | any delete refusal (no row, no key, bad or stale proof) |
/// | 503 | `blob-node-unavailable` | the row names another storage node (forwarding is not built) |
/// | 500 | `internal` | index or file failure |
///
/// Token refusals on the pass routes are the shared gate's codes
/// (`anon-token-*`, `anon-tokens-disabled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiError {
    PassDisabled,
    PassRequired,
    PassInvalid,
    PassExpired,
    UploadBudget,
    ReadBudget,
    IdMalformed,
    DeleteKeyRequired,
    DeleteKeyInvalid,
    LengthRequired,
    TooLarge,
    HashMismatch,
    LengthMismatch,
    KeepMalformed,
    NotFound,
    DeleteRefused,
    NodeUnavailable,
    Internal,
}

impl ApiError {
    pub(crate) const fn status(self) -> StatusCode {
        match self {
            Self::PassDisabled | Self::NodeUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::PassRequired | Self::PassInvalid | Self::PassExpired => StatusCode::UNAUTHORIZED,
            Self::UploadBudget | Self::ReadBudget => StatusCode::TOO_MANY_REQUESTS,
            Self::IdMalformed
            | Self::DeleteKeyRequired
            | Self::DeleteKeyInvalid
            | Self::HashMismatch
            | Self::LengthMismatch
            | Self::KeepMalformed => StatusCode::BAD_REQUEST,
            Self::LengthRequired => StatusCode::LENGTH_REQUIRED,
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::DeleteRefused => StatusCode::FORBIDDEN,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::PassDisabled => "blob-pass-disabled",
            Self::PassRequired => "blob-pass-required",
            Self::PassInvalid => "blob-pass-invalid",
            Self::PassExpired => "blob-pass-expired",
            Self::UploadBudget => "blob-upload-budget-exhausted",
            Self::ReadBudget => "blob-read-budget-exhausted",
            Self::IdMalformed => "blob-id-malformed",
            Self::DeleteKeyRequired => "blob-delete-key-required",
            Self::DeleteKeyInvalid => "blob-delete-key-invalid",
            Self::LengthRequired => "blob-length-required",
            Self::TooLarge => "blob-too-large",
            Self::HashMismatch => "blob-hash-mismatch",
            Self::LengthMismatch => "blob-length-mismatch",
            Self::KeepMalformed => "keep-malformed",
            Self::NotFound => "blob-not-found",
            Self::DeleteRefused => "blob-delete-refused",
            Self::NodeUnavailable => "blob-node-unavailable",
            Self::Internal => "internal",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status(),
            Json(serde_json::json!({ "error": self.code() })),
        )
            .into_response()
    }
}

impl From<IndexError> for ApiError {
    fn from(e: IndexError) -> Self {
        tracing::error!(error = %e, "blob index failure");
        Self::Internal
    }
}

/// Parse `hex` as a blob id: exactly 64 lower-case hex characters, one
/// canonical form (no case-insensitive-filesystem double storage, no path
/// traversal).
pub(crate) fn parse_id(hex: &str) -> Result<BlobId, ApiError> {
    if hex.len() != HEX_LEN || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(ApiError::IdMalformed);
    }
    let v = hex::decode(hex).map_err(|_| ApiError::IdMalformed)?;
    BlobId::try_from(v.as_slice()).map_err(|_| ApiError::IdMalformed)
}

/// Resolve `<root>/<hex[0:2]>/<hex[2:4]>/<hex>`.
pub(crate) fn blob_path(root: &Path, id: &BlobId) -> PathBuf {
    let hex = hex::encode(id);
    let mut p = root.to_path_buf();
    p.push(&hex[0..2]);
    p.push(&hex[2..4]);
    p.push(&hex);
    p
}

/// Probe that the blob root is writable by creating and deleting a
/// uniquely-named temp file. Used by `/ready` and at startup.
pub(crate) async fn probe_writable(root: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(root).await?;
    let probe = tempfile::Builder::new()
        .prefix(".lakko-storage-probe-")
        .tempfile_in(root)?;
    drop(probe);
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Check the pass of `kind` on a request and charge `(a, b)` to it.
fn take_pass(
    state: &AppState,
    headers: &HeaderMap,
    kind: PassKind,
    a: u64,
    b: u64,
) -> Result<Pass, ApiError> {
    let secret = state.pass_secret.as_ref().ok_or(ApiError::PassDisabled)?;
    let value = headers
        .get(PASS_HEADER)
        .ok_or(ApiError::PassRequired)?
        .to_str()
        .map_err(|_| ApiError::PassInvalid)?;
    let now = now_unix_secs();
    let pass = secret.verify(value, kind, now).map_err(|e| match e {
        PassError::Invalid => ApiError::PassInvalid,
        PassError::Expired => ApiError::PassExpired,
    })?;
    charge(state, &pass, a, b, now)?;
    Ok(pass)
}

fn charge(state: &AppState, pass: &Pass, a: u64, b: u64, now: u64) -> Result<(), ApiError> {
    state
        .ledger
        .charge(pass, a, b, &state.limits, now)
        .map_err(|()| match pass.kind {
            PassKind::Upload => ApiError::UploadBudget,
            PassKind::Read => ApiError::ReadBudget,
        })
}

// ============================================================================
// POST /blob/pass/{upload,read}
// ============================================================================

async fn issue_pass(state: &AppState, headers: &HeaderMap, kind: PassKind) -> Response {
    // The secret is checked before the token so a service that cannot issue
    // a pass never takes a token for one.
    let Some(secret) = state.pass_secret.as_ref() else {
        return ApiError::PassDisabled.into_response();
    };
    let context = match kind {
        PassKind::Upload => TokenContext::BlobUpload,
        PassKind::Read => TokenContext::BlobRead,
    };
    if let Err(rejection) = state.tokens.spend_header(headers, context).await {
        return rejection.into_response();
    }
    let expires_at = now_unix_secs().saturating_add(state.limits.ttl_secs);
    let (_, value) = secret.issue(kind, expires_at);
    let budget = match kind {
        PassKind::Upload => serde_json::json!({
            "files": state.limits.upload_files,
            "bytes": state.limits.upload_bytes,
        }),
        PassKind::Read => serde_json::json!({
            "downloads": state.limits.read_downloads,
            "keeps": state.limits.read_keeps,
        }),
    };
    Json(serde_json::json!({
        "pass": value,
        "kind": kind.name(),
        "expires_at_unix_secs": expires_at,
        "budget": budget,
    }))
    .into_response()
}

/// `POST /blob/pass/upload` - spend one `blob-upload` token for an upload
/// pass.
pub(crate) async fn pass_upload(State(state): State<AppState>, headers: HeaderMap) -> Response {
    issue_pass(&state, &headers, PassKind::Upload).await
}

/// `POST /blob/pass/read` - spend one `blob-read` token for a read pass.
pub(crate) async fn pass_read(State(state): State<AppState>, headers: HeaderMap) -> Response {
    issue_pass(&state, &headers, PassKind::Read).await
}

// ============================================================================
// PUT /blob/{hex}
// ============================================================================

fn delete_key(headers: &HeaderMap) -> Result<[u8; 32], ApiError> {
    let value = headers
        .get(DELETE_KEY_HEADER)
        .ok_or(ApiError::DeleteKeyRequired)?
        .to_str()
        .map_err(|_| ApiError::DeleteKeyInvalid)?;
    if value.len() != 43 {
        return Err(ApiError::DeleteKeyInvalid);
    }
    let raw = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ApiError::DeleteKeyInvalid)?;
    let key = <[u8; 32]>::try_from(raw.as_slice()).map_err(|_| ApiError::DeleteKeyInvalid)?;
    if !lakko_vault::proof::is_usable_public_key(&key) {
        return Err(ApiError::DeleteKeyInvalid);
    }
    Ok(key)
}

fn content_length(headers: &HeaderMap) -> Result<u64, ApiError> {
    headers
        .get(header::CONTENT_LENGTH)
        .ok_or(ApiError::LengthRequired)?
        .to_str()
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(ApiError::LengthRequired)
}

fn internal_io(ctx: &'static str, e: &std::io::Error) -> ApiError {
    tracing::error!(error = %e, "{ctx}");
    ApiError::Internal
}

/// Stream `body` through SHA-256 into `temp`, stopping at more than
/// `declared` bytes. Returns the digest and the byte count.
async fn stream_into(
    body: Body,
    temp: &tempfile::NamedTempFile,
    declared: u64,
) -> Result<([u8; 32], u64), ApiError> {
    let mut hasher = Sha256::new();
    let mut writer = tokio::fs::File::from_std(
        temp.as_file()
            .try_clone()
            .map_err(|e| internal_io("clone temp fd", &e))?,
    );
    let mut written: u64 = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ApiError::LengthMismatch)?;
        written = written.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if written > declared {
            return Err(ApiError::LengthMismatch);
        }
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
    Ok((hasher.finalize().into(), written))
}

/// `PUT /blob/{hex}` - store a blob. Charged one file and `Content-Length`
/// bytes against the upload pass before the body is read; the charge is not
/// refunded when the upload then fails. The body must hash to the path and
/// be exactly `Content-Length` bytes. A first upload writes the row with the
/// delete key; a repeated upload is an access and keeps the first key.
pub(crate) async fn put_blob(
    State(state): State<AppState>,
    AxumPath(hex): AxumPath<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    // Pass shape and expiry first, so nothing below is reachable without one;
    // the charge follows the request checks, which cost the client nothing.
    take_pass(&state, &headers, PassKind::Upload, 0, 0)?;
    let id = parse_id(&hex)?;
    let key = delete_key(&headers)?;
    let declared = content_length(&headers)?;
    if declared > crate::MAX_BLOB_BYTES as u64 {
        return Err(ApiError::TooLarge);
    }
    take_pass(&state, &headers, PassKind::Upload, 1, declared)?;

    let target = blob_path(&state.blob_root, &id);
    let parent = target
        .parent()
        .map(Path::to_path_buf)
        .ok_or(ApiError::Internal)?;
    tokio::fs::create_dir_all(&parent)
        .await
        .map_err(|e| internal_io("create blob parent", &e))?;
    let temp = tokio::task::spawn_blocking(move || {
        tempfile::Builder::new()
            .prefix(".lakko-blob-")
            .tempfile_in(&parent)
    })
    .await
    .map_err(|_| ApiError::Internal)?
    .map_err(|e| internal_io("create tempfile", &e))?;

    let (digest, written) = stream_into(body, &temp, declared).await?;
    if written != declared {
        return Err(ApiError::LengthMismatch);
    }
    if digest != id {
        // `temp` drops here, removing the partial file.
        return Err(ApiError::HashMismatch);
    }
    tokio::task::spawn_blocking(move || temp.persist(&target))
        .await
        .map_err(|_| ApiError::Internal)?
        .map_err(|e| internal_io("persist blob", &e.error))?;

    let day = today();
    let outcome = state
        .index
        .record_upload(BlobRow {
            id,
            size: written,
            node: state.node.to_string(),
            created_day: day,
            last_access_day: day,
            delete_pk: Some(key),
        })
        .await?;
    let status = match outcome {
        UploadOutcome::Created => StatusCode::CREATED,
        UploadOutcome::Existed => StatusCode::OK,
    };
    Ok(status.into_response())
}

// ============================================================================
// GET / HEAD /blob/{hex}
// ============================================================================

/// The row of `id` on this node, or the refusal.
async fn local_row(state: &AppState, id: &BlobId) -> Result<BlobRow, ApiError> {
    let row = state.index.get(id).await?.ok_or(ApiError::NotFound)?;
    if row.node != *state.node {
        return Err(ApiError::NodeUnavailable);
    }
    Ok(row)
}

/// A row whose file is gone is removed, so the blob reads as not found from
/// then on rather than failing every time.
async fn file_missing(state: &AppState, id: &BlobId) -> ApiError {
    tracing::warn!("blob row without a file; removing the row");
    match state.index.remove(id).await {
        Ok(_) => ApiError::NotFound,
        Err(e) => ApiError::from(e),
    }
}

/// `GET /blob/{hex}` - download a blob, streamed. One download against the
/// read pass; an access.
pub(crate) async fn get_blob(
    State(state): State<AppState>,
    AxumPath(hex): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    take_pass(&state, &headers, PassKind::Read, 0, 0)?;
    let id = parse_id(&hex)?;
    take_pass(&state, &headers, PassKind::Read, 1, 0)?;
    let row = local_row(&state, &id).await?;

    let path = blob_path(&state.blob_root, &id);
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(file_missing(&state, &id).await);
        },
        Err(e) => return Err(internal_io("open blob", &e)),
    };
    let len = file
        .metadata()
        .await
        .map_err(|e| internal_io("stat blob", &e))?
        .len();
    let day = today();
    if row.last_access_day < day {
        state.index.touch(&[id], day).await?;
    }

    let mut response = Response::new(Body::from_stream(crate::stream::ReaderStream::new(file)));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, header::HeaderValue::from(len));
    Ok(response)
}

/// `HEAD /blob/{hex}` - 200 with `Content-Length` when the blob exists. One
/// download against the read pass; not an access.
pub(crate) async fn head_blob(
    State(state): State<AppState>,
    AxumPath(hex): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    take_pass(&state, &headers, PassKind::Read, 0, 0)?;
    let id = parse_id(&hex)?;
    take_pass(&state, &headers, PassKind::Read, 1, 0)?;
    let row = local_row(&state, &id).await?;
    let mut response = Response::new(Body::empty());
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, header::HeaderValue::from(row.size));
    Ok(response)
}

// ============================================================================
// POST /blob/keep
// ============================================================================

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeepBody {
    ids: Vec<String>,
}

/// `POST /blob/keep` with `{"ids": [hex, ...]}` (1..=256 ids) - mark each
/// existing blob accessed today. One keep per id against the read pass.
/// Answers 204 whether the ids exist or not.
pub(crate) async fn keep_blobs(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    take_pass(&state, &headers, PassKind::Read, 0, 0)?;
    let parsed: KeepBody = serde_json::from_slice(&body).map_err(|_| ApiError::KeepMalformed)?;
    if parsed.ids.is_empty() || parsed.ids.len() > MAX_KEEP_IDS {
        return Err(ApiError::KeepMalformed);
    }
    let ids = parsed
        .ids
        .iter()
        .map(|h| parse_id(h).map_err(|_| ApiError::KeepMalformed))
        .collect::<Result<Vec<_>, _>>()?;
    let count = u64::try_from(ids.len()).unwrap_or(u64::MAX);
    take_pass(&state, &headers, PassKind::Read, 0, count)?;
    state.index.touch(&ids, today()).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ============================================================================
// DELETE /blob/{hex}
// ============================================================================

/// `DELETE /blob/{hex}` - delete a blob, row then file. Requires an
/// `x-lakko-storage-proof` over `("DELETE", "/blob/<hex>", empty body)`
/// under the delete key the first upload carried; no pass. Every refusal is
/// the same 403 `blob-delete-refused`: no row, a row with no key, a missing,
/// malformed, stale or wrong proof.
pub(crate) async fn delete_blob(
    State(state): State<AppState>,
    AxumPath(hex): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = parse_id(&hex).map_err(|_| ApiError::DeleteRefused)?;
    let proof = headers
        .get(lakko_vault::proof::PROOF_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::DeleteRefused)?;
    let row = state.index.get(&id).await?.ok_or(ApiError::DeleteRefused)?;
    let key = row.delete_pk.ok_or(ApiError::DeleteRefused)?;
    let path = format!("/blob/{hex}");
    lakko_vault::proof::verify(&key, "DELETE", &path, &[], proof, now_ms())
        .map_err(|_| ApiError::DeleteRefused)?;
    if row.node != *state.node {
        return Err(ApiError::NodeUnavailable);
    }
    state.index.remove(&id).await?;
    match tokio::fs::remove_file(blob_path(&state.blob_root, &id)).await {
        Ok(()) => {},
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => {
            // The row is gone, so the blob is already unreachable; the file
            // is an orphan on disk.
            tracing::warn!(error = %e, "delete blob file after its row");
        },
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests;
