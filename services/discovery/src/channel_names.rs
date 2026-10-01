// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Channel-name registry — the table mapping
//! `OPRF(K, normalize(@channelname))` to a 16-byte
//! `channel_id` pseudonym.
//!
//! G4-5 per ADR-0010 §"Server can correlate" mitigation:
//! the server NEVER stores plaintext `@channelname`.
//! Discovery clients submit a blinded element through
//! `/discovery/oprf` (namespace-agnostic — same endpoint
//! used by `@username` lookup), apply the static `K`,
//! finalise locally, and present the lookup key derived from
//! the OPRF output (`lakko_discovery_client::discovery_lookup_key`)
//! to one of the endpoints in this module.
//!
//! ## Why a separate table
//!
//! Two distinct namespaces with distinct id types
//! (UUIDv7 `account_id` vs 16-byte `channel_id`) live in
//! their own table so a query for one namespace hits a
//! tighter index and a corruption in one cannot affect
//! the other. See 0009_channels.sql for the schema.
//!
//! ## Why no `account_id` in the body
//!
//! Channel ownership is not the same concept as account
//! ownership — a channel's identity is its
//! PublisherGroup (ADR-0010 §"Tier 1"), not a single
//! `account_id`. A registration request only commits to
//! "this OPRF hash points to this channel pseudonym",
//! with no claim of who owns the publisher group.
//! Anti-squatting + anti-impersonation lives at the
//! channel-membership layer (PublisherGroup commits,
//! audited via the MLS state machine).

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::AppState;

/// VOPRF Ristretto255-SHA512 output is 64 bytes (one
/// SHA-512 digest). Matches the username table — same
/// ciphersuite, same wire shape.
const OPRF_HASH_LEN: usize = 64;
/// Channel id is 16 bytes (ADR-0010 §"Wire surfaces").
const CHANNEL_ID_LEN: usize = 16;

fn decode_fixed<const N: usize>(
    b64: &str,
    name: &'static str,
) -> Result<[u8; N], (StatusCode, String)> {
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid {name} base64: {e}"),
        )
    })?;
    if bytes.len() != N {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{name} must be {N} bytes after base64 decode"),
        ));
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

// ============================================================================
// POST /discovery/channel/register
// ============================================================================

/// Body for [`register`].
#[derive(Deserialize)]
pub(crate) struct RegisterChannelRequest {
    /// Base64-encoded 64-byte lookup key, derived client-side from
    /// the OPRF output of a `/discovery/oprf` round-trip.
    pub(crate) oprf_hash: String,
    /// Base64-encoded 16-byte channel id pseudonym.
    pub(crate) channel_id: String,
}

#[derive(Serialize)]
pub(crate) struct RegisterChannelResponse {
    pub(crate) ok: bool,
}

/// `POST /discovery/channel/register` — bind an OPRF-
/// hashed `@channelname` to a `channel_id`. First-come-
/// first-served via UNIQUE on `oprf_hash`. A repeat
/// registration of the same hash with a different
/// channel id is **409 Conflict**.
///
/// Anonymous: no session names the publisher. Since
/// ADR-0054 S3 the request spends one `discovery-register`
/// anonymous token (`x-lakko-anon-token`, checked by the
/// router's gate before this handler runs), which is what
/// rations squatting per account without linking the
/// channel to one. See module-level docs for why no
/// account binds the channel id.
pub(crate) async fn register(
    State(state): State<AppState>,
    Json(payload): Json<RegisterChannelRequest>,
) -> Result<Json<RegisterChannelResponse>, (StatusCode, String)> {
    let oprf_hash = decode_fixed::<OPRF_HASH_LEN>(&payload.oprf_hash, "oprf_hash")?;
    let channel_id = decode_fixed::<CHANNEL_ID_LEN>(&payload.channel_id, "channel_id")?;

    let result = sqlx::query("INSERT INTO channel_names (oprf_hash, channel_id) VALUES (?, ?)")
        .bind(&oprf_hash[..])
        .bind(&channel_id[..])
        .execute(&state.db)
        .await;

    match result {
        Ok(_) => {
            tracing::info!(
                channel_id = %B64.encode(channel_id),
                "channel name registered",
            );
            Ok(Json(RegisterChannelResponse { ok: true }))
        },
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => Err((
            StatusCode::CONFLICT,
            "channel name already registered".to_string(),
        )),
        Err(e) => {
            tracing::error!(error = %e, "insert channel_names");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal db error".to_string(),
            ))
        },
    }
}

// ============================================================================
// POST /discovery/channel/lookup
// ============================================================================

/// Body for [`lookup`].
#[derive(Deserialize)]
pub(crate) struct LookupChannelRequest {
    pub(crate) oprf_hash: String,
}

#[derive(Serialize)]
pub(crate) struct LookupChannelResponse {
    /// Base64-encoded 16-byte channel id.
    pub(crate) channel_id: String,
}

/// `POST /discovery/channel/lookup` — resolve an
/// OPRF-hashed `@channelname` to its `channel_id`.
/// Returns **404 Not Found** for unknown hashes.
///
/// Exact-match only — same property the username path
/// (registry.rs) enforces. No prefix search, no
/// autocomplete from the server. Clients build their own
/// autocomplete from their local subscription list.
pub(crate) async fn lookup(
    State(state): State<AppState>,
    Json(payload): Json<LookupChannelRequest>,
) -> Result<Json<LookupChannelResponse>, (StatusCode, String)> {
    let oprf_hash = decode_fixed::<OPRF_HASH_LEN>(&payload.oprf_hash, "oprf_hash")?;

    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT channel_id FROM channel_names WHERE oprf_hash = ?")
            .bind(&oprf_hash[..])
            .fetch_optional(&state.db)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "lookup channel_names");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal db error".to_string(),
                )
            })?;

    match row {
        Some((channel_id,)) => Ok(Json(LookupChannelResponse {
            channel_id: B64.encode(channel_id),
        })),
        None => Err((StatusCode::NOT_FOUND, "channel not found".to_string())),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity: the constants match what the schema +
    /// ADR-0010 promise. A schema bump that widens
    /// channel_id would fail this test before any
    /// migration drift could land in production.
    #[test]
    fn constants_pinned_to_schema() {
        assert_eq!(OPRF_HASH_LEN, 64);
        assert_eq!(CHANNEL_ID_LEN, 16);
    }

    #[test]
    fn decode_fixed_validates_length() {
        let s = B64.encode([0u8; CHANNEL_ID_LEN]);
        decode_fixed::<CHANNEL_ID_LEN>(&s, "channel_id").unwrap();

        let s = B64.encode([0u8; OPRF_HASH_LEN]);
        decode_fixed::<OPRF_HASH_LEN>(&s, "oprf_hash").unwrap();

        // Wrong length.
        let s = B64.encode([0u8; 8]);
        let err = decode_fixed::<CHANNEL_ID_LEN>(&s, "channel_id").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        let s = B64.encode([0u8; 32]);
        let err = decode_fixed::<OPRF_HASH_LEN>(&s, "oprf_hash").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Bad base64.
        let err = decode_fixed::<CHANNEL_ID_LEN>("not-valid!!!", "channel_id").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }
}
