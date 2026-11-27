// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! HTTP handlers for the transparency log.
//!
//! Five endpoints:
//!
//! - `POST /transparency/append`           — service-secret-authed leaf append.
//! - `GET  /transparency/root`             — anon current signed root.
//! - `GET  /transparency/proof/:idx`       — anon inclusion proof.
//! - `GET  /transparency/latest/:username_hash_b64` — anon latest entry.
//! - `GET  /transparency/operator-key`     — anon operator pubkey to pin.
//!
//! ## How the proof response carries what `verify_inclusion` needs
//!
//! The crate's
//! [`verify_inclusion`](lakko_transparency::verify_inclusion)`(entry,
//! proof, signed_root)` takes:
//!
//! - the [`LogEntry`](lakko_transparency::LogEntry) — reconstructed by
//!   the client from `leaf_b64` (the entry's `canonical_bytes()`),
//! - the [`InclusionProof`](lakko_transparency::InclusionProof) —
//!   `{ leaf_index, audit_path }`, carried as `idx` + `audit_path`,
//! - the [`SignedRoot`](lakko_transparency::SignedRoot) — fetched
//!   separately from `/transparency/root`; the proof response carries
//!   `log_size` so a client can sanity-check the proof was built
//!   against the same tree size the root commits to.
//!
//! So `GET /transparency/proof/:idx` returns exactly
//! `{ idx, leaf_b64, audit_path, log_size }` and the client pairs it
//! with the separately-fetched root.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use base64::engine::general_purpose::{
    STANDARD as B64, STANDARD_NO_PAD as B64_NP, URL_SAFE as B64_URL, URL_SAFE_NO_PAD as B64_URL_NP,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::metrics::AppendOutcome;
use crate::AppState;

/// Maximum identity-pubkey length we accept on append. Ed25519 is 32
/// bytes; we cap at the column width (`VARBINARY(64)`) so an oversized
/// blob is rejected at ingress rather than at the DB.
const MAX_PUBKEY_LEN: usize = 64;

// ----------------------------------------------------------------
// POST /transparency/append
// ----------------------------------------------------------------

/// `POST /transparency/append` request body.
///
/// The caller does NOT supply the version — the transparency service
/// assigns it monotonically per `username_hash` under the append lock
/// (KT-log S2 decision B).
#[derive(Debug, Deserialize)]
pub(crate) struct AppendReq {
    /// BLAKE3 account-key hash, base64-standard (32 bytes after decode).
    /// This is the opaque `username_hash` field — for the discovery
    /// integration it carries
    /// `BLAKE3("lakko/transparency/account/v1" || account_id)`, but the
    /// service treats it as 32 opaque bytes and never learns the
    /// account_id-to-username mapping.
    pub username_hash_b64: String,
    /// Identity public key, base64-standard (1..=64 bytes after decode;
    /// Ed25519 is 32).
    pub identity_pubkey_b64: String,
    /// ADR-0017 fresh-start marker — a no-supersession-proof owner
    /// reset. Defaults to `false` (a normal proven publish / rotation).
    #[serde(default)]
    pub is_reset: bool,
    /// G-30 #2 (ADR-0019): the client leaf authorisation, base64-standard.
    /// A genesis / fresh-start SELF-signature (64 bytes, signed by the
    /// leaf's own key over the `(account_key, pubkey)` binding) for a first
    /// entry or an `is_reset` mint, OR a normal-rotation supersession proof
    /// (145 bytes, signed by the previous key). When present the operator
    /// verifies it and REFUSES an invalid one; whether ABSENCE is refused is
    /// governed by `LAKKO_TRANSPARENCY_REQUIRE_LEAF_AUTH`. Opaque otherwise.
    #[serde(default)]
    pub leaf_auth_b64: Option<String>,
    /// ADR-0036 S4: the auth-attested genesis attestation, base64-standard
    /// (153 bytes, minted by `services/auth` on `POST /auth/genesis/attest`,
    /// binding the OPAQUE-authenticated account to the genesis pubkey). Stored
    /// opaque next to `leaf_auth`; for a genesis/reset leaf the store verifies
    /// it against the pinned auth key when
    /// `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` is on (an absent / invalid one
    /// is then refused — the anti-squatting gate). Ignored for rotations.
    #[serde(default)]
    pub genesis_attest_b64: Option<String>,
    /// ADR-0101 section 10: standard base64 of the 214-byte reset seal of a
    /// reset leaf (`is_reset`), stored as a sidecar; ignored otherwise.
    #[serde(default)]
    pub reset_seal_b64: Option<String>,
}

/// `POST /transparency/append` response body.
#[derive(Debug, Serialize)]
pub(crate) struct AppendRes {
    /// The 0-based leaf index the entry was appended at.
    pub idx: u64,
    /// The server-assigned monotonic per-`username_hash` version
    /// (`>= 1`).
    pub version: u64,
}

/// Append a leaf to the log. Service-secret-authed (the
/// `service_auth::require_service_secret` middleware gates the route —
/// KT-log S2). Validates the decoded shapes, hands the
/// `(username_hash, identity_pubkey, is_reset)` to the store which
/// ASSIGNS the version under its lock, and replies with the assigned
/// `(idx, version)`.
///
/// **Trust model (G-30 #2, ADR-0019).** This service sees only the 32-byte
/// `username_hash`, never the `account_id`, so it CANNOT verify the
/// `account_id ↔ username_hash` binding. But it DOES now verify that the ACCOUNT
/// authorised the `(account_key → pubkey)` binding: a NEW binding must carry a
/// `leaf_auth` — a genesis / fresh-start self-signature (signed by the leaf's
/// own key) or a normal-rotation supersession proof (signed by the previous
/// key) — which the store verifies under its append lock; an invalid one is
/// REFUSED (403). When `LAKKO_TRANSPARENCY_REQUIRE_LEAF_AUTH` is set, an ABSENT
/// `leaf_auth` is refused too. This is the operator-side defence-in-depth reject
/// — a leaked append secret can no longer pollute the log with a fabricated
/// (keyless) leaf. The CLIENT rotation chain (`verify_supersession_chain`)
/// stays the real integrity gate (the server is untrusted). Genesis remains
/// TOFU (a self-sig does not PREVENT first-entry squatting — that residual is
/// detected via CONIKS self-monitoring + the ADR-0016 fail-closed peer gate).
pub(crate) async fn append(
    state: State<AppState>,
    req: Json<AppendReq>,
) -> Result<Json<AppendRes>, (StatusCode, &'static str)> {
    append_checked(state, req)
        .await
        .map_err(|(outcome, refusal)| {
            crate::metrics::append(outcome);
            refusal
        })
}

/// The body of [`append`]. A refusal carries its ADR-0096 outcome; the two
/// success outcomes (`appended`, `unchanged`) are counted in the store,
/// where they are decided.
async fn append_checked(
    State(state): State<AppState>,
    Json(req): Json<AppendReq>,
) -> Result<Json<AppendRes>, (AppendOutcome, (StatusCode, &'static str))> {
    let invalid = |r: (StatusCode, &'static str)| (AppendOutcome::Invalid, r);
    let username_hash_vec = B64.decode(req.username_hash_b64.as_bytes()).map_err(|_| {
        invalid((
            StatusCode::BAD_REQUEST,
            "username_hash_b64 not valid base64",
        ))
    })?;
    let username_hash: [u8; 32] = username_hash_vec
        .as_slice()
        .try_into()
        .map_err(|_| invalid((StatusCode::BAD_REQUEST, "username_hash must be 32 bytes")))?;

    let identity_pubkey = B64
        .decode(req.identity_pubkey_b64.as_bytes())
        .map_err(|_| {
            invalid((
                StatusCode::BAD_REQUEST,
                "identity_pubkey_b64 not valid base64",
            ))
        })?;
    if identity_pubkey.is_empty() || identity_pubkey.len() > MAX_PUBKEY_LEN {
        return Err(invalid((
            StatusCode::BAD_REQUEST,
            "identity_pubkey must be 1..=64 bytes",
        )));
    }

    // G-30 #2 (ADR-0019): decode the optional client leaf authorisation.
    let leaf_auth: Option<Vec<u8>> =
        match req.leaf_auth_b64.as_deref() {
            Some(s) => Some(B64.decode(s.as_bytes()).map_err(|_| {
                invalid((StatusCode::BAD_REQUEST, "leaf_auth_b64 not valid base64"))
            })?),
            None => None,
        };
    // Presence policy: when configured to require leaf authorisation, an absent
    // value is the operator-side reject. The store still verifies a PRESENT
    // value regardless of this flag (an invalid auth is always refused).
    if state.require_leaf_auth && leaf_auth.is_none() {
        return Err((
            AppendOutcome::LeafAuthMissing,
            (StatusCode::FORBIDDEN, "leaf authorisation required"),
        ));
    }

    // ADR-0036 S4: decode the optional auth-attested genesis attestation. Its
    // presence policy (`require_genesis_attest`, applied only to genesis/reset
    // leaves) lives in the store — it is the store that knows whether an append
    // is a genesis or a rotation.
    let genesis_attest: Option<Vec<u8>> = match req.genesis_attest_b64.as_deref() {
        Some(s) => Some(B64.decode(s.as_bytes()).map_err(|_| {
            invalid((
                StatusCode::BAD_REQUEST,
                "genesis_attest_b64 not valid base64",
            ))
        })?),
        None => None,
    };

    let reset_seal: Option<Vec<u8>> =
        match req.reset_seal_b64.as_deref() {
            Some(s) => Some(B64.decode(s.as_bytes()).map_err(|_| {
                invalid((StatusCode::BAD_REQUEST, "reset_seal_b64 not valid base64"))
            })?),
            None => None,
        };

    let result = state
        .store
        .append_with_seal(
            &state.db,
            username_hash,
            identity_pubkey,
            req.is_reset,
            leaf_auth.as_deref(),
            crate::store::GenesisAttestGate {
                attestation: genesis_attest.as_deref(),
                key: state.genesis_attest_key.as_ref(),
                required: state.require_genesis_attest,
            },
            reset_seal.as_deref(),
            now_unix_secs_i64(),
        )
        .await
        .map_err(|e| match e {
            crate::store::StoreError::Unauthorised(detail) => {
                tracing::warn!(detail, "transparency append refused: leaf authorisation");
                (
                    AppendOutcome::Refused,
                    (StatusCode::FORBIDDEN, "leaf authorisation rejected"),
                )
            },
            other => {
                tracing::error!(error = %other, "transparency append failed");
                (
                    AppendOutcome::Error,
                    (StatusCode::INTERNAL_SERVER_ERROR, "append failed"),
                )
            },
        })?;

    Ok(Json(AppendRes {
        idx: result.idx,
        version: result.version,
    }))
}

// ----------------------------------------------------------------
// GET /transparency/root
// ----------------------------------------------------------------

/// `GET /transparency/root` response body — the operator-signed root.
///
/// Clients reconstruct the
/// [`SignedRoot`](lakko_transparency::SignedRoot) from these fields and
/// call `verify` against the pinned operator key.
#[derive(Debug, Serialize)]
pub(crate) struct RootRes {
    /// Merkle tree root over the first `log_size` entries, base64.
    pub tree_root_b64: String,
    /// Number of entries the root commits to.
    pub log_size: u64,
    /// Operator-chosen timestamp (seconds since UNIX epoch).
    pub timestamp_unix: u64,
    /// Ed25519 signature over `(tree_root || log_size || timestamp)`,
    /// base64. See `SignedRoot::signing_payload` for the canonical
    /// pre-image.
    pub signature_b64: String,
    /// ADR-0020 + ADR-0039 S3 (K-of-N): the witness co-signature BUNDLE over
    /// `(tree_root, log_size)` — one base64 blob per witness that co-signed the
    /// served root. Empty (the field is OMITTED) when the witness integration is
    /// unconfigured or every witness was unreachable this round. A client that
    /// pins witness keys enforces the K-of-N threshold over this bundle (the
    /// split-view defence) and refuses a root that does not meet it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub witness_cosigs_b64: Vec<String>,
}

/// Return the current signed root + (ADR-0020 / ADR-0039 S3) its witness
/// co-signature BUNDLE. Anonymous. Signs the current tree state with `now` as
/// the timestamp and fans out to every configured witness, attaching the
/// collected K-of-N co-signature bundle.
pub(crate) async fn root(State(state): State<AppState>) -> Json<RootRes> {
    let now = now_unix_secs();
    let (signed, cosigs) = state
        .store
        .root_with_cosig(&state.db, &state.witness, now)
        .await;
    Json(RootRes {
        tree_root_b64: B64.encode(signed.tree_root),
        log_size: signed.log_size,
        timestamp_unix: signed.timestamp_unix,
        signature_b64: B64.encode(signed.signature.to_bytes()),
        witness_cosigs_b64: cosigs.into_iter().map(|c| B64.encode(c)).collect(),
    })
}

// ----------------------------------------------------------------
// GET /transparency/proof/:idx
// ----------------------------------------------------------------

/// `GET /transparency/proof/:idx` response body.
///
/// Carries exactly what
/// [`verify_inclusion`](lakko_transparency::verify_inclusion) needs:
/// the leaf bytes (the `LogEntry` via `canonical_bytes`), the audit
/// path with the leaf index (the `InclusionProof`), and `log_size` so
/// the client can confirm the proof was built against the same tree
/// size the root it pairs with commits to.
#[derive(Debug, Serialize)]
pub(crate) struct ProofRes {
    /// Leaf index (0-based) of the proven entry.
    pub idx: u64,
    /// The entry's `canonical_bytes()`, base64. The client decodes this
    /// to reconstruct the `LogEntry` for `verify_inclusion`.
    pub leaf_b64: String,
    /// Audit path: sibling node-hashes from the leaf up to the root,
    /// each base64. Order is leaf→root, matching the crate's verifier.
    pub audit_path: Vec<String>,
    /// Tree size the proof was generated against.
    pub log_size: u64,
}

/// Return an inclusion proof for `idx`. Anonymous.
pub(crate) async fn proof(
    State(state): State<AppState>,
    Path(idx): Path<u64>,
) -> Result<Json<ProofRes>, (StatusCode, &'static str)> {
    let (leaf, proof) = state.store.leaf_proof(idx).await.map_err(|e| {
        tracing::debug!(idx, error = %e, "proof request out of range");
        (StatusCode::NOT_FOUND, "no entry at that index")
    })?;

    // `log_size` reflects the tree the proof was built against — read it
    // from the freshly-signed root so it is consistent with the audit
    // path. (The proof is built against `current_root`'s size.)
    let log_size = state.store.signed_root(now_unix_secs()).await.log_size;

    Ok(Json(ProofRes {
        idx: proof.leaf_index,
        leaf_b64: B64.encode(leaf),
        audit_path: proof.audit_path.iter().map(|h| B64.encode(h)).collect(),
        log_size,
    }))
}

// ----------------------------------------------------------------
// GET /transparency/operator-key
// ----------------------------------------------------------------

/// `GET /transparency/operator-key` response body.
#[derive(Debug, Serialize)]
pub(crate) struct OperatorKeyRes {
    /// The operator's Ed25519 verifying key, base64 (32 bytes). Clients
    /// pin this out-of-band; it is the trust anchor for every signed
    /// root.
    pub operator_pubkey_b64: String,
}

/// Return the operator verifying key. Anonymous.
pub(crate) async fn operator_key(State(state): State<AppState>) -> Json<OperatorKeyRes> {
    let vk = state.store.operator_verifying_key().await;
    Json(OperatorKeyRes {
        operator_pubkey_b64: B64.encode(vk.to_bytes()),
    })
}

// ----------------------------------------------------------------
// GET /transparency/latest/:username_hash_b64
// ----------------------------------------------------------------

/// `GET /transparency/latest/:username_hash_b64` response body
/// (KT-log S3a).
///
/// Returns the latest (highest-version) entry the log holds for the
/// account-key. A client uses `idx` to fetch the matching inclusion
/// proof from `/transparency/proof/:idx`, `identity_pubkey_b64` +
/// `version` as the exact `(pubkey, version)` to bind in
/// `verify_log_inclusion`, and `is_reset` to distinguish a proven
/// rotation from an ADR-0017 fresh start.
#[derive(Debug, Serialize)]
pub(crate) struct LatestRes {
    /// Leaf index (0-based) of the latest entry (== the `/proof` idx).
    pub idx: u64,
    /// Server-assigned monotonic per-`username_hash` version (`>= 1`).
    pub version: u64,
    /// The account's current identity public key, base64-standard.
    pub identity_pubkey_b64: String,
    /// ADR-0017 fresh-start marker — `true` if the latest entry was a
    /// no-supersession-proof owner reset.
    pub is_reset: bool,
    /// G-30 #2 (ADR-0019): the opaque client leaf authorisation stored with
    /// this entry (a genesis self-sig or a supersession proof), base64, or
    /// absent for a pre-Sprint-2 / unauthenticated row. A client can verify a
    /// genesis self-sig on a first observation against the served pubkey.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leaf_auth_b64: Option<String>,
    /// ADR-0036 S4: the opaque auth-attested genesis attestation stored with
    /// this entry, base64, or absent for a rotation / a pre-ADR-0036 row. A
    /// client verifies it (`verify_genesis_binding`) against the pinned auth key
    /// on a first observation to confirm the account authorised the genesis key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genesis_attest_b64: Option<String>,
    /// ADR-0101 section 10: standard base64 of the reset seal stored with a
    /// reset leaf (absent for an unsealed reset or a non-reset leaf).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_seal_b64: Option<String>,
}

/// Return the latest entry for the account-key carried in the path
/// (base64-encoded, 32 bytes after decode). Anonymous — the path param
/// is the opaque `username_hash`
/// (`BLAKE3("lakko/transparency/account/v1" || account_id)`); the
/// service never learns the account_id-to-username mapping (invariant
/// #1 / #5).
///
/// Accepts the account-key in either standard or URL-safe base64 (with
/// or without padding) so a client can put it in a URL path without
/// escaping `/` or `+`. 404 when no entry for that account-key exists.
pub(crate) async fn latest(
    State(state): State<AppState>,
    Path(username_hash_b64): Path<String>,
) -> Result<Json<LatestRes>, (StatusCode, &'static str)> {
    let username_hash_vec = decode_b64_any(&username_hash_b64).ok_or((
        StatusCode::BAD_REQUEST,
        "username_hash_b64 not valid base64",
    ))?;
    let username_hash: [u8; 32] = username_hash_vec
        .as_slice()
        .try_into()
        .map_err(|_| (StatusCode::BAD_REQUEST, "username_hash must be 32 bytes"))?;

    let entry = state
        .store
        .latest(&state.db, username_hash)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "transparency latest lookup failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "latest lookup failed")
        })?
        .ok_or((StatusCode::NOT_FOUND, "no entry for that account-key"))?;

    Ok(Json(LatestRes {
        idx: entry.idx,
        version: entry.version,
        identity_pubkey_b64: B64.encode(entry.identity_pubkey),
        is_reset: entry.is_reset,
        leaf_auth_b64: entry.leaf_auth.map(|a| B64.encode(a)),
        genesis_attest_b64: entry.genesis_attest.map(|a| B64.encode(a)),
        reset_seal_b64: entry.reset_seal.map(|a| B64.encode(a)),
    }))
}

// ----------------------------------------------------------------
// POST /transparency/announcement/append (ADR-0101 section 10)
// ----------------------------------------------------------------

/// Body of `POST /transparency/announcement/append` (service-secret gated:
/// discovery only).
#[derive(Debug, Deserialize)]
pub(crate) struct AnnouncementAppendReq {
    /// Standard base64 of the 32-byte `account_username_hash`.
    pub username_hash_b64: String,
    /// Standard base64 of the seal announcement as the account signed it.
    pub announcement_b64: String,
}

/// Answer of `POST /transparency/announcement/append`.
#[derive(Debug, Serialize)]
pub(crate) struct AnnouncementAppendRes {
    /// The leaf index (the existing one for a retry).
    pub idx: u64,
}

/// `POST /transparency/announcement/append`: append (or find) the
/// announcement leaf. `400` malformed, `403` an announcement that does not
/// verify under the account's latest identity key (or no identity leaf),
/// `409` a `seq` not above the logged ones, `500` a store failure.
pub(crate) async fn append_announcement(
    State(state): State<AppState>,
    Json(req): Json<AnnouncementAppendReq>,
) -> Result<Json<AnnouncementAppendRes>, (StatusCode, &'static str)> {
    let username_hash: [u8; 32] = B64
        .decode(req.username_hash_b64.as_bytes())
        .ok()
        .and_then(|v| <[u8; 32]>::try_from(v.as_slice()).ok())
        .ok_or((
            StatusCode::BAD_REQUEST,
            "username_hash must be 32 bytes, base64",
        ))?;
    let announcement = B64
        .decode(req.announcement_b64.as_bytes())
        .ok()
        .filter(|a| a.len() <= lakko_attest::SEAL_ANNOUNCEMENT_MAX_LEN)
        .ok_or((StatusCode::BAD_REQUEST, "announcement_b64 malformed"))?;
    let idx = state
        .store
        .append_announcement(&state.db, username_hash, &announcement)
        .await
        .map_err(|e| match e {
            crate::store::StoreError::Unauthorised(detail) => {
                tracing::warn!(detail, "announcement append refused");
                crate::metrics::append(AppendOutcome::Refused);
                (StatusCode::FORBIDDEN, "announcement rejected")
            },
            crate::store::StoreError::Stale => {
                crate::metrics::append(AppendOutcome::Refused);
                (StatusCode::CONFLICT, "announcement seq not newer")
            },
            other => {
                tracing::error!(error = %other, "announcement append failed");
                crate::metrics::append(AppendOutcome::Error);
                (StatusCode::INTERNAL_SERVER_ERROR, "append failed")
            },
        })?;
    Ok(Json(AnnouncementAppendRes { idx }))
}

// ----------------------------------------------------------------
// GET /transparency/announcement/{username_hash_b64}/{generation}
// ----------------------------------------------------------------

/// Answer of `GET /transparency/announcement/{username_hash}/{generation}`:
/// the newest announcement leaf of that account and generation.
#[derive(Debug, Serialize)]
pub(crate) struct AnnouncementRes {
    /// The leaf index; its audit path is `GET /transparency/proof/{idx}`.
    pub idx: u64,
    /// The generation.
    pub key_generation: u64,
    /// Its `seq`.
    pub seq: u64,
    /// Standard base64 of the announcement bytes.
    pub announcement_b64: String,
}

/// `GET /transparency/announcement/{username_hash_b64}/{generation}`: open,
/// `404` when the account logged none at that generation. "The newest" rests
/// on this service answering truthfully, kept honest by the witnesses and by
/// each device's `seq` floor (ADR-0101 section 10).
pub(crate) async fn announcement(
    State(state): State<AppState>,
    Path((username_hash_b64, generation)): Path<(String, u64)>,
) -> Result<Json<AnnouncementRes>, (StatusCode, &'static str)> {
    let username_hash: [u8; 32] = decode_b64_any(&username_hash_b64)
        .and_then(|v| <[u8; 32]>::try_from(v.as_slice()).ok())
        .ok_or((StatusCode::BAD_REQUEST, "username_hash must be 32 bytes"))?;
    let entry = state
        .store
        .announcement(&state.db, username_hash, generation)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "announcement lookup failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "announcement lookup failed",
            )
        })?
        .ok_or((StatusCode::NOT_FOUND, "no announcement for that generation"))?;
    Ok(Json(AnnouncementRes {
        idx: entry.idx,
        key_generation: entry.key_generation,
        seq: entry.seq,
        announcement_b64: B64.encode(entry.announcement),
    }))
}

// ----------------------------------------------------------------
// helpers
// ----------------------------------------------------------------

/// Current wall-clock time in seconds since the UNIX epoch. Clamps a
/// pre-epoch clock to 0 rather than erroring — the timestamp is an
/// operator-chosen monotonic marker, not a security boundary.
fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `now_unix_secs` as an `i64` for the supersession-proof freshness check
/// (G-30 #2). Saturates rather than wrapping on the (far-future) overflow.
fn now_unix_secs_i64() -> i64 {
    i64::try_from(now_unix_secs()).unwrap_or(i64::MAX)
}

/// Decode a base64 string accepting either alphabet (standard or
/// URL-safe) and either padding mode, returning `None` if none of them
/// parse. The account-key rides in a URL path, where a client may have
/// encoded it URL-safe (to avoid `/` and `+`) or standard; we accept
/// both rather than dictate one and 400 the other.
fn decode_b64_any(s: &str) -> Option<Vec<u8>> {
    B64.decode(s)
        .or_else(|_| B64_NP.decode(s))
        .or_else(|_| B64_URL.decode(s))
        .or_else(|_| B64_URL_NP.decode(s))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LogStore;
    use lakko_crypto::signing;
    use lakko_transparency::{verify_inclusion, InMemoryLog, InclusionProof, LogEntry, SignedRoot};
    use std::sync::Arc;

    /// Reconstruct a `SignedRoot` from a `RootRes` the way a client
    /// would, then prove an entry against it from a `ProofRes`. This
    /// pins that the wire shapes carry exactly what `verify_inclusion`
    /// consumes.
    #[tokio::test]
    async fn proof_response_verifies_against_root_response() {
        let operator = signing::SigningKey::generate();
        let mut log = InMemoryLog::new(operator);
        for i in 0..6 {
            log.append(LogEntry {
                username_hash: lakko_crypto::hash::blake3(format!("u{i}").as_bytes()),
                identity_pubkey: vec![0xAB; 32],
                version: 1,
            });
        }
        let store = Arc::new(LogStore::from_log(log));

        // Build the wire RootRes by hand (mirrors `root` handler).
        let now = 1_700_000_000;
        let signed = store.signed_root(now).await;
        let root_res = RootRes {
            tree_root_b64: B64.encode(signed.tree_root),
            log_size: signed.log_size,
            timestamp_unix: signed.timestamp_unix,
            signature_b64: B64.encode(signed.signature.to_bytes()),
            witness_cosigs_b64: Vec::new(),
        };

        // Build the wire ProofRes for idx=3 (mirrors `proof` handler).
        let (entry, proof) = store.proof(3).await.unwrap();
        let proof_res = ProofRes {
            idx: proof.leaf_index,
            leaf_b64: B64.encode(entry.canonical_bytes()),
            audit_path: proof.audit_path.iter().map(|h| B64.encode(h)).collect(),
            log_size: signed.log_size,
        };

        // --- Now act as the client: decode the wire, reverify. ---
        let tree_root: [u8; 32] = B64
            .decode(&root_res.tree_root_b64)
            .unwrap()
            .try_into()
            .unwrap();
        let sig_bytes = B64.decode(&root_res.signature_b64).unwrap();
        let signature = signing::Signature::from_bytes(&sig_bytes).unwrap();
        let client_root = SignedRoot {
            tree_root,
            log_size: root_res.log_size,
            timestamp_unix: root_res.timestamp_unix,
            signature,
        };
        // The root must verify under the operator pubkey.
        client_root
            .verify(&store.operator_verifying_key().await)
            .expect("root verifies");

        // Reconstruct the entry from leaf bytes. We carry canonical
        // bytes, so the client parses fields back out — but the
        // verifier only needs the SAME LogEntry whose canonical_bytes
        // equals leaf_b64; rebuild it from the known fields here (a real
        // client parses the canonical layout).
        let leaf = B64.decode(&proof_res.leaf_b64).unwrap();
        let rebuilt = store.proof(3).await.unwrap().0;
        assert_eq!(rebuilt.canonical_bytes(), leaf);

        let audit_path: Vec<[u8; 32]> = proof_res
            .audit_path
            .iter()
            .map(|s| B64.decode(s).unwrap().try_into().unwrap())
            .collect();
        let client_proof = InclusionProof {
            leaf_index: proof_res.idx,
            audit_path,
        };

        verify_inclusion(&rebuilt, &client_proof, &client_root).expect("inclusion verifies");
    }

    /// KT-log S3a — `GET /transparency/latest/:username_hash_b64` returns
    /// the latest entry (highest version) for the account-key, and 404s an
    /// unseen one. Drives the full `build_app` router so routing + the
    /// path-param base64 decode + the 404 path are all exercised.
    ///
    /// **No-DB note:** the `latest` handler queries MariaDB (the durable
    /// source of truth), so it cannot be exercised by the in-memory-only
    /// `from_log` store the other handler tests use — there is no shadow
    /// of the DB rows in the `InMemoryLog`. This test is therefore DB-gated
    /// on `LAKKO_TRANSPARENCY_TEST_DATABASE_URL`; it SKIPS (does not fail)
    /// when unset, mirroring the store-side gated tests and
    /// `tests/db_reload_e2e.rs`.
    #[tokio::test]
    async fn latest_endpoint_returns_highest_version_and_404s_unseen() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use http_body_util::BodyExt as _;
        use sqlx::mysql::MySqlPoolOptions;
        use tower::ServiceExt as _;

        let Ok(url) = std::env::var("LAKKO_TRANSPARENCY_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping latest_endpoint_returns_highest_version_and_404s_unseen: \
                 LAKKO_TRANSPARENCY_TEST_DATABASE_URL unset"
            );
            return;
        };

        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::MIGRATIONS.run(&pool).await.expect("run migrations");
        sqlx::query("DELETE FROM log_entries")
            .execute(&pool)
            .await
            .expect("clear log_entries");

        let store = Arc::new(LogStore::from_log(InMemoryLog::new(
            signing::SigningKey::generate(),
        )));
        let alice = lakko_crypto::hash::blake3(b"alice");

        // alice v1 then v2 (a rotation), v2 is_reset=false.
        store
            .append(
                &pool,
                alice,
                vec![0x01; 32],
                false,
                None,
                crate::store::GenesisAttestGate::default(),
                0,
            )
            .await
            .expect("append alice v1");
        store
            .append(
                &pool,
                alice,
                vec![0x02; 32],
                false,
                None,
                crate::store::GenesisAttestGate::default(),
                0,
            )
            .await
            .expect("append alice v2");

        let state = crate::AppState {
            db: pool.clone(),
            store,
            append_secret: None,
            require_leaf_auth: false,
            genesis_attest_key: None,
            require_genesis_attest: false,
            witness: Arc::new(Vec::new()),
        };
        let app = crate::build_app(state);

        // Encode the account-key URL-safe (no `/` in a path) and hit the
        // endpoint.
        let key_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(alice);
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/transparency/latest/{key_b64}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // Latest is v2 at idx 1; the returned pubkey is the v2 one.
        assert_eq!(parsed["idx"], 1);
        assert_eq!(parsed["version"], 2);
        let pk_b64 = parsed["identity_pubkey_b64"].as_str().unwrap();
        assert_eq!(B64.decode(pk_b64).unwrap(), vec![0x02; 32]);
        assert_eq!(parsed["is_reset"], false);

        // An unseen account-key 404s.
        let unseen = lakko_crypto::hash::blake3(b"nobody");
        let unseen_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(unseen);
        let res404 = app
            .oneshot(
                Request::builder()
                    .uri(format!("/transparency/latest/{unseen_b64}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res404.status(), StatusCode::NOT_FOUND);

        sqlx::query("DELETE FROM log_entries")
            .execute(&pool)
            .await
            .expect("clear log_entries");
        pool.close().await;
    }
}
