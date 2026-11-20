// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Vault routes (ADR-0064 section 1).
//!
//! A vault is one conversation's or channel's saved state: a head and
//! numbered chunks, each sealed by the client under a key the server never
//! sees. No request names an account: the vault id is a client derivation,
//! a claim spends one anonymous `vault-claim` token, and every other request
//! is signed with the vault's own write key (`lakko_vault::proof`, header
//! `x-lakko-storage-proof`). No session token is read on these routes.
//!
//! # Routes
//!
//! | Route | Credential | Success |
//! |---|---|---|
//! | `PUT /vault/{id}` | one `vault-claim` token (`x-lakko-anon-token`); body `{"write_pk": <base64url, 32 bytes>}` | 201 claimed / 200 already claimed under this key |
//! | `PUT /vault/{id}/head`, `PUT /vault/{id}/chunk/{n}` | proof under the write key over the body | 201 |
//! | `GET /vault/{id}/head`, `GET /vault/{id}/chunk/{n}` | proof over an empty body | 200 |
//! | `DELETE /vault/{id}` | proof over an empty body | 204 |
//!
//! `{id}` is 64 lower-case hex; `{n}` is a decimal chunk number without a
//! sign or leading zero. The proof signs the service path
//! (`/vault/<id>/head`), never a reverse-proxy prefix.
//!
//! # Object bodies
//!
//! A write's body, and a read's response body, is
//!
//! ```text
//! version(u64 BE) || sealed object
//! ```
//!
//! The version rides in the signed body, so a replayed write cannot be sent
//! again at another version. It is accepted only at the object's current
//! version plus one (a new object starts at 1); anything else is `409
//! vault-version-conflict`, and the writer reads, merges and writes again.
//! The sealed object is opaque here; only its length is checked
//! (`lakko_vault::seal::MIN_OBJECT_BYTES` to `MAX_OBJECT_BYTES`). A chunk
//! number may be at most the vault's chunk count: chunk `count` adds a
//! chunk, a larger number is `409 vault-chunk-out-of-range`.
//!
//! # Refusals
//!
//! Every refusal is `{"error": code}`.
//!
//! | Status | Code | When |
//! |---|---|---|
//! | 400 | `vault-id-malformed` | the id is not 64 lower-case hex |
//! | 400 | `vault-chunk-malformed` | the chunk number is not a canonical decimal `u32` |
//! | 400 | `vault-claim-malformed` | the claim body is not `{"write_pk": ...}` with a usable Ed25519 key |
//! | 400 | `vault-body-malformed` | the body is not a non-zero version and an object of a valid size |
//! | 401 | `vault-proof-refused` | a head, chunk or delete request without a valid proof, **whether the vault is claimed or not** |
//! | 404 | `vault-object-not-found` | the object has no committed version |
//! | 409 | `vault-claimed` | a claim of a vault claimed under another key |
//! | 409 | `vault-version-conflict` | the version is not current + 1, or another write of it is in progress |
//! | 409 | `vault-chunk-out-of-range` | a chunk number above the chunk count, or a new chunk past the chunk limit |
//! | 413 | `vault-full` | the write would take the vault past its byte limit |
//! | 429 | `vault-write-rate` | the vault used its writes for this hour |
//! | 503 | `vault-node-unavailable` | the vault lives on another storage node (forwarding is not built) |
//! | 500 | `internal` | index or file failure |
//!
//! A claim's token refusals are the shared gate's codes (`anon-token-*`,
//! `anon-tokens-disabled`). A body above the route ceiling is refused by the
//! framework with a plain 413 before any check.
//!
//! The order on a signed route: the path parses (400), the proof verifies
//! against the vault's write key (401, the same answer for a vault that does
//! not exist), the vault is on this node (503), then the body and the index.
//! So a request without the write key learns nothing about a vault beyond
//! what a malformed path tells it.
//!
//! # Limits
//!
//! Per vault, all **chosen, not measured** (no traffic exists to measure):
//! [`DEFAULT_MAX_BYTES`] (64 MiB, counting writes in progress),
//! [`DEFAULT_MAX_CHUNKS`] (256), [`DEFAULT_WRITES_PER_HOUR`] (240
//! reservations per clock hour; a refused write costs none), and
//! [`RESERVATION_MS`] (60 s, after which another writer may take a version
//! whose writer never finished). The first three can be changed by
//! `LAKKO_STORAGE_VAULT_MAX_BYTES`, `LAKKO_STORAGE_VAULT_MAX_CHUNKS` and
//! `LAKKO_STORAGE_VAULT_WRITES_PER_HOUR`.
//!
//! # Files
//!
//! Objects are files under `<blob root>/.lakko-vaults/<id[0:2]>/<id>/`,
//! written once: `claim` (the 32-byte write key), `head.v<n>.<token>` and
//! `chunk.<k>.v<n>.<token>`, where `<token>` is the 16-byte reservation token
//! in hex. The token keeps a writer whose reservation timed out from
//! replacing the file of the writer that took it over; no name is ever
//! overwritten. A write stores its file, then commits the version in the
//! index, then deletes the version it replaced. A reservation whose writer
//! never committed is taken over by the next writer after
//! [`RESERVATION_MS`]; its file, if one was stored, is left as an orphan.
//! Nothing here lists directories: rebuilding a lost index from the files is
//! not built.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_anon_token::TokenContext;
use lakko_vault::seal::{MAX_OBJECT_BYTES, MIN_OBJECT_BYTES};
use serde::Deserialize;
use tokio::io::AsyncWriteExt as _;

use crate::index::{
    today, ClaimOutcome, CommitOutcome, IndexError, ObjectAddr, ReservationToken, ReserveOutcome,
    VaultId, VaultLimits, VaultRow, WriteRequest,
};
use crate::AppState;

/// The vault directory under the blob root. A dot directory, so the blob
/// import never takes it for a blob shard.
pub(crate) const VAULT_DIR: &str = ".lakko-vaults";

/// Bytes of the version prefix of an object body.
pub(crate) const VERSION_LEN: usize = 8;

/// Body ceiling of a head or chunk write.
pub(crate) const MAX_OBJECT_BODY: usize = VERSION_LEN + MAX_OBJECT_BYTES;

/// Body ceiling of a claim: a small JSON object.
pub(crate) const MAX_CLAIM_BODY: usize = 1024;

/// Default byte limit per vault (64 MiB). Chosen, not measured.
pub(crate) const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Default chunk limit per vault. Chosen, not measured.
pub(crate) const DEFAULT_MAX_CHUNKS: u32 = 256;

/// Default write reservations per vault per clock hour. Chosen, not
/// measured: an owner device writes a channel head before each commit and
/// after each commit it applies.
pub(crate) const DEFAULT_WRITES_PER_HOUR: u32 = 240;

/// How long a reservation holds its version (60 s). Chosen, not measured:
/// a write of a 4 MiB object to a local disk.
pub(crate) const RESERVATION_MS: u64 = 60_000;

/// The limits a service starts with when nothing overrides them.
pub(crate) const DEFAULT_LIMITS: VaultLimits = VaultLimits {
    max_bytes: DEFAULT_MAX_BYTES,
    max_chunks: DEFAULT_MAX_CHUNKS,
    max_writes_per_hour: DEFAULT_WRITES_PER_HOUR,
    reservation_ms: RESERVATION_MS,
};

/// A coded refusal; see the module table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VaultError {
    IdMalformed,
    ChunkMalformed,
    ClaimMalformed,
    BodyMalformed,
    ProofRefused,
    NotFound,
    Claimed,
    VersionConflict,
    ChunkOutOfRange,
    Full,
    WriteRate,
    NodeUnavailable,
    Internal,
}

impl VaultError {
    pub(crate) const fn status(self) -> StatusCode {
        match self {
            Self::IdMalformed
            | Self::ChunkMalformed
            | Self::ClaimMalformed
            | Self::BodyMalformed => StatusCode::BAD_REQUEST,
            Self::ProofRefused => StatusCode::UNAUTHORIZED,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Claimed | Self::VersionConflict | Self::ChunkOutOfRange => StatusCode::CONFLICT,
            Self::Full => StatusCode::PAYLOAD_TOO_LARGE,
            Self::WriteRate => StatusCode::TOO_MANY_REQUESTS,
            Self::NodeUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::IdMalformed => "vault-id-malformed",
            Self::ChunkMalformed => "vault-chunk-malformed",
            Self::ClaimMalformed => "vault-claim-malformed",
            Self::BodyMalformed => "vault-body-malformed",
            Self::ProofRefused => "vault-proof-refused",
            Self::NotFound => "vault-object-not-found",
            Self::Claimed => "vault-claimed",
            Self::VersionConflict => "vault-version-conflict",
            Self::ChunkOutOfRange => "vault-chunk-out-of-range",
            Self::Full => "vault-full",
            Self::WriteRate => "vault-write-rate",
            Self::NodeUnavailable => "vault-node-unavailable",
            Self::Internal => "internal",
        }
    }
}

impl IntoResponse for VaultError {
    fn into_response(self) -> Response {
        (
            self.status(),
            Json(serde_json::json!({ "error": self.code() })),
        )
            .into_response()
    }
}

impl From<IndexError> for VaultError {
    fn from(e: IndexError) -> Self {
        tracing::error!(error = %e, "vault index failure");
        Self::Internal
    }
}

fn internal_io(ctx: &'static str, e: &std::io::Error) -> VaultError {
    tracing::error!(error = %e, "{ctx}");
    VaultError::Internal
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Parse a vault id: exactly 64 lower-case hex characters.
pub(crate) fn parse_vault_id(hex: &str) -> Result<VaultId, VaultError> {
    if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(VaultError::IdMalformed);
    }
    let v = hex::decode(hex).map_err(|_| VaultError::IdMalformed)?;
    VaultId::try_from(v.as_slice()).map_err(|_| VaultError::IdMalformed)
}

/// Parse a chunk number: 1..=10 ASCII digits, no leading zero (the single
/// digit `0` aside), fits a `u32`. One number has one spelling, so it has
/// one signed path.
pub(crate) fn parse_chunk_no(s: &str) -> Result<u32, VaultError> {
    let d = s.as_bytes();
    let canonical = !d.is_empty()
        && d.len() <= 10
        && d.iter().all(u8::is_ascii_digit)
        && (d.len() == 1 || d.first() != Some(&b'0'));
    if !canonical {
        return Err(VaultError::ChunkMalformed);
    }
    s.parse::<u32>().map_err(|_| VaultError::ChunkMalformed)
}

/// `<blob root>/.lakko-vaults/<id[0:2]>/<id>`.
pub(crate) fn vault_dir(root: &Path, id: &VaultId) -> PathBuf {
    let hex = hex::encode(id);
    let mut p = root.join(VAULT_DIR);
    p.push(&hex[0..2]);
    p.push(&hex);
    p
}

/// The file name of one committed or reserved object version.
pub(crate) fn object_file_name(addr: ObjectAddr, version: u64, token: &ReservationToken) -> String {
    let token = hex::encode(token);
    match addr {
        ObjectAddr::Head => format!("head.v{version}.{token}"),
        ObjectAddr::Chunk(n) => format!("chunk.{n}.v{version}.{token}"),
    }
}

/// The vault row, once the request's proof verifies under its write key.
/// A missing vault, a missing header, a malformed or stale proof and a
/// wrong signature are one answer.
async fn authorise(
    state: &AppState,
    id: &VaultId,
    method: &str,
    path: &str,
    body: &[u8],
    headers: &HeaderMap,
) -> Result<VaultRow, VaultError> {
    let row = state
        .vaults
        .vault(id)
        .await?
        .ok_or(VaultError::ProofRefused)?;
    let proof = headers
        .get(lakko_vault::proof::PROOF_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .ok_or(VaultError::ProofRefused)?;
    lakko_vault::proof::verify(&row.write_pk, method, path, body, proof, now_ms())
        .map_err(|_| VaultError::ProofRefused)?;
    if row.node != *state.node {
        return Err(VaultError::NodeUnavailable);
    }
    Ok(row)
}

// ============================================================================
// PUT /vault/{id} - claim
// ============================================================================

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimBody {
    write_pk: String,
}

fn parse_claim(body: &[u8]) -> Result<[u8; 32], VaultError> {
    let parsed: ClaimBody = serde_json::from_slice(body).map_err(|_| VaultError::ClaimMalformed)?;
    if parsed.write_pk.len() != 43 {
        return Err(VaultError::ClaimMalformed);
    }
    let raw = URL_SAFE_NO_PAD
        .decode(&parsed.write_pk)
        .map_err(|_| VaultError::ClaimMalformed)?;
    let key = <[u8; 32]>::try_from(raw.as_slice()).map_err(|_| VaultError::ClaimMalformed)?;
    if !lakko_vault::proof::is_usable_public_key(&key) {
        return Err(VaultError::ClaimMalformed);
    }
    Ok(key)
}

/// Store the claim object (the write key) in a new vault directory.
async fn write_claim_file(dir: PathBuf, key: [u8; 32]) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let mut tmp = tempfile::Builder::new().prefix(".tmp-").tempfile_in(&dir)?;
        std::io::Write::write_all(tmp.as_file_mut(), &key)?;
        tmp.as_file().sync_data()?;
        tmp.persist(dir.join("claim")).map_err(|e| e.error)?;
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)?
}

/// `PUT /vault/{id}` - claim a vault for a write key. The body is checked
/// before the token is spent, so a malformed claim costs none; a well-formed
/// claim spends one token whatever the outcome.
pub(crate) async fn claim_vault(
    State(state): State<AppState>,
    AxumPath(hex): AxumPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let id = match parse_vault_id(&hex) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };
    let key = match parse_claim(&body) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    if let Err(rejection) = state
        .tokens
        .spend_header(&headers, TokenContext::VaultClaim)
        .await
    {
        return rejection.into_response();
    }
    let row = VaultRow {
        id,
        write_pk: key,
        node: state.node.to_string(),
        created_day: today(),
        bytes_used: 0,
    };
    match state.vaults.claim(row).await {
        Ok(ClaimOutcome::Created) => {
            if let Err(e) = write_claim_file(vault_dir(&state.blob_root, &id), key).await {
                tracing::error!(error = %e, "store the vault claim object");
                if let Err(e) = state.vaults.unclaim(&id).await {
                    tracing::error!(error = %e, "undo a vault claim");
                }
                return VaultError::Internal.into_response();
            }
            StatusCode::CREATED.into_response()
        },
        Ok(ClaimOutcome::SameKey) => StatusCode::OK.into_response(),
        Ok(ClaimOutcome::OtherKey) => VaultError::Claimed.into_response(),
        Err(e) => VaultError::from(e).into_response(),
    }
}

// ============================================================================
// PUT /vault/{id}/head, PUT /vault/{id}/chunk/{n}
// ============================================================================

/// Split a write body into its version and sealed object.
fn split_body(body: &[u8]) -> Result<(u64, &[u8]), VaultError> {
    let (version, sealed) = body
        .split_first_chunk::<VERSION_LEN>()
        .ok_or(VaultError::BodyMalformed)?;
    let version = u64::from_be_bytes(*version);
    if version == 0 || sealed.len() < MIN_OBJECT_BYTES || sealed.len() > MAX_OBJECT_BYTES {
        return Err(VaultError::BodyMalformed);
    }
    Ok((version, sealed))
}

/// Store `sealed` as a new file named `name` in `dir`, never replacing an
/// existing name.
async fn store_object(dir: PathBuf, name: String, sealed: Bytes) -> std::io::Result<()> {
    tokio::fs::create_dir_all(&dir).await?;
    let tmp = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || {
            tempfile::Builder::new().prefix(".tmp-").tempfile_in(&dir)
        })
        .await
        .map_err(std::io::Error::other)??
    };
    let mut writer = tokio::fs::File::from_std(tmp.as_file().try_clone()?);
    writer.write_all(&sealed).await?;
    writer.flush().await?;
    writer.sync_data().await?;
    let target = dir.join(name);
    tokio::task::spawn_blocking(move || tmp.persist_noclobber(target).map(|_| ()))
        .await
        .map_err(std::io::Error::other)?
        .map_err(|e| e.error)
}

async fn remove_object_file(dir: &Path, name: &str) {
    match tokio::fs::remove_file(dir.join(name)).await {
        Ok(()) => {},
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => tracing::warn!(error = %e, "remove a replaced vault object"),
    }
}

async fn put_object(
    state: &AppState,
    id: &VaultId,
    addr: ObjectAddr,
    path: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Response, VaultError> {
    authorise(state, id, "PUT", path, &body, headers).await?;
    let (version, sealed) = split_body(&body)?;
    let size = u64::try_from(sealed.len()).unwrap_or(u64::MAX);
    let req = WriteRequest {
        addr,
        version,
        size,
        now_ms: now_ms(),
    };
    let token = match state.vaults.reserve(id, req, &state.vault_limits).await? {
        ReserveOutcome::Reserved(t) => t,
        // Deleted between the proof check and the reservation.
        ReserveOutcome::NoVault => return Err(VaultError::ProofRefused),
        ReserveOutcome::VersionConflict => return Err(VaultError::VersionConflict),
        ReserveOutcome::ChunkOutOfRange => return Err(VaultError::ChunkOutOfRange),
        ReserveOutcome::VaultFull => return Err(VaultError::Full),
        ReserveOutcome::RateLimited => return Err(VaultError::WriteRate),
    };
    let dir = vault_dir(&state.blob_root, id);
    let name = object_file_name(addr, version, &token);
    let sealed = body.slice(VERSION_LEN..);
    if let Err(e) = store_object(dir.clone(), name.clone(), sealed).await {
        let failure = internal_io("store a vault object", &e);
        if let Err(e) = state.vaults.release(id, addr, &token).await {
            tracing::error!(error = %e, "release a vault reservation");
        }
        return Err(failure);
    }
    match state.vaults.commit(id, addr, &token, size).await {
        Ok(CommitOutcome::Committed { replaced }) => {
            if let Some((old_version, old_token)) = replaced {
                remove_object_file(&dir, &object_file_name(addr, old_version, &old_token)).await;
            }
            Ok(StatusCode::CREATED.into_response())
        },
        Ok(CommitOutcome::Lost) => {
            remove_object_file(&dir, &name).await;
            Err(VaultError::VersionConflict)
        },
        Err(e) => {
            remove_object_file(&dir, &name).await;
            Err(e.into())
        },
    }
}

/// `PUT /vault/{id}/head` - write the head at `current + 1`.
pub(crate) async fn put_head(
    State(state): State<AppState>,
    AxumPath(hex): AxumPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, VaultError> {
    let id = parse_vault_id(&hex)?;
    let path = format!("/vault/{hex}/head");
    put_object(&state, &id, ObjectAddr::Head, &path, &headers, body).await
}

/// `PUT /vault/{id}/chunk/{n}` - write chunk `n` at `current + 1`.
pub(crate) async fn put_chunk(
    State(state): State<AppState>,
    AxumPath((hex, n)): AxumPath<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, VaultError> {
    let id = parse_vault_id(&hex)?;
    let chunk = parse_chunk_no(&n)?;
    let path = format!("/vault/{hex}/chunk/{n}");
    put_object(&state, &id, ObjectAddr::Chunk(chunk), &path, &headers, body).await
}

// ============================================================================
// GET /vault/{id}/head, GET /vault/{id}/chunk/{n}
// ============================================================================

async fn get_object(
    state: &AppState,
    id: &VaultId,
    addr: ObjectAddr,
    path: &str,
    headers: &HeaderMap,
) -> Result<Response, VaultError> {
    authorise(state, id, "GET", path, &[], headers).await?;
    let dir = vault_dir(&state.blob_root, id);
    // A write that commits between the index read and the file read deletes
    // the file of the version just read; read the row again once.
    let mut attempt = 0;
    let (row, sealed) = loop {
        let row = state
            .vaults
            .object(id, addr)
            .await?
            .ok_or(VaultError::NotFound)?;
        let file = dir.join(object_file_name(addr, row.version, &row.token));
        match tokio::fs::read(&file).await {
            Ok(b) => break (row, b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && attempt == 0 => attempt += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::error!("vault object row without a file");
                return Err(VaultError::Internal);
            },
            Err(e) => return Err(internal_io("read a vault object", &e)),
        }
    };
    let mut out = Vec::with_capacity(VERSION_LEN + sealed.len());
    out.extend_from_slice(&row.version.to_be_bytes());
    out.extend_from_slice(&sealed);
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], out).into_response())
}

/// `GET /vault/{id}/head` - the current head, `version || sealed`.
pub(crate) async fn get_head(
    State(state): State<AppState>,
    AxumPath(hex): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Response, VaultError> {
    let id = parse_vault_id(&hex)?;
    let path = format!("/vault/{hex}/head");
    get_object(&state, &id, ObjectAddr::Head, &path, &headers).await
}

/// `GET /vault/{id}/chunk/{n}` - the current chunk `n`, `version || sealed`.
pub(crate) async fn get_chunk(
    State(state): State<AppState>,
    AxumPath((hex, n)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, VaultError> {
    let id = parse_vault_id(&hex)?;
    let chunk = parse_chunk_no(&n)?;
    let path = format!("/vault/{hex}/chunk/{n}");
    get_object(&state, &id, ObjectAddr::Chunk(chunk), &path, &headers).await
}

// ============================================================================
// DELETE /vault/{id}
// ============================================================================

/// `DELETE /vault/{id}` - remove the vault, its rows first, then its files.
/// The id may be claimed again afterwards.
pub(crate) async fn delete_vault(
    State(state): State<AppState>,
    AxumPath(hex): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Response, VaultError> {
    let id = parse_vault_id(&hex)?;
    let path = format!("/vault/{hex}");
    authorise(&state, &id, "DELETE", &path, &[], &headers).await?;
    state.vaults.delete_vault(&id).await?;
    match tokio::fs::remove_dir_all(vault_dir(&state.blob_root, &id)).await {
        Ok(()) => {},
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => {
            // The rows are gone, so the vault is already unreachable; the
            // files are orphans on disk.
            tracing::warn!(error = %e, "delete vault files after the rows");
        },
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests;
