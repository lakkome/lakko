// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The moderator list this service serves (ADR-0089 section 3).
//!
//! The offline root signs the list; the operator places it at
//! `LAKKO_MODERATION_LIST_PATH` and pins the root's public key in
//! `LAKKO_MODERATION_ROOT_KEY` (base64url, 32 bytes). The list is verified
//! once at startup; a list that does not verify refuses startup, so the
//! service never serves bytes a client would refuse. A new list takes a
//! restart.

use std::path::Path;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_moderation::ModeratorList;

use crate::{error, AppState};

/// Env var: path of the signed moderator list.
pub const LIST_PATH_VAR: &str = "LAKKO_MODERATION_LIST_PATH";
/// Env var: the root public key, base64url without padding.
pub const ROOT_KEY_VAR: &str = "LAKKO_MODERATION_ROOT_KEY";

/// The list as loaded: its bytes, served verbatim, and its decoded form.
#[derive(Debug)]
pub struct LoadedList {
    /// The signed bytes.
    pub bytes: Vec<u8>,
    /// The verified list.
    pub list: ModeratorList,
}

/// Why the list could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ListError {
    /// One of the two variables is set and the other is not.
    #[error("set both {LIST_PATH_VAR} and {ROOT_KEY_VAR}, or neither")]
    HalfConfigured,
    /// The root key is not 32 bytes of base64url.
    #[error("{ROOT_KEY_VAR} is not a base64url 32-byte key")]
    BadRootKey,
    /// The file could not be read.
    #[error("cannot read the moderator list: {0}")]
    Read(#[from] std::io::Error),
    /// The list does not verify under the root key.
    #[error("the moderator list does not verify: {0}")]
    Verify(lakko_moderation::ModerationError),
}

/// Read and verify the list from the environment. `Ok(None)` when neither
/// variable is set: reporting is then not available on this server.
///
/// # Errors
///
/// [`ListError`].
pub fn load_from_env() -> Result<Option<LoadedList>, ListError> {
    let path = std::env::var(LIST_PATH_VAR)
        .ok()
        .filter(|v| !v.trim().is_empty());
    let root = std::env::var(ROOT_KEY_VAR)
        .ok()
        .filter(|v| !v.trim().is_empty());
    match (path, root) {
        (None, None) => Ok(None),
        (Some(path), Some(root)) => load(Path::new(path.trim()), root.trim()).map(Some),
        _ => Err(ListError::HalfConfigured),
    }
}

/// Read and verify the list at `path` under `root_b64`.
///
/// # Errors
///
/// [`ListError`].
pub fn load(path: &Path, root_b64: &str) -> Result<LoadedList, ListError> {
    let root = parse_root_key(root_b64)?;
    let bytes = std::fs::read(path)?;
    from_bytes(bytes, &root)
}

/// Verify `bytes` under `root`.
///
/// # Errors
///
/// [`ListError::Verify`].
pub fn from_bytes(bytes: Vec<u8>, root: &[u8; 32]) -> Result<LoadedList, ListError> {
    let list = ModeratorList::verify(&bytes, root).map_err(ListError::Verify)?;
    Ok(LoadedList { bytes, list })
}

/// Parse a base64url (no padding) 32-byte key.
///
/// # Errors
///
/// [`ListError::BadRootKey`].
pub fn parse_root_key(b64: &str) -> Result<[u8; 32], ListError> {
    URL_SAFE_NO_PAD
        .decode(b64)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
        .ok_or(ListError::BadRootKey)
}

/// `GET /moderation/moderators`.
pub async fn moderators(State(state): State<AppState>) -> Response {
    let Some(list) = &state.list else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "reporting-unavailable");
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        list.bytes.clone(),
    )
        .into_response()
}
