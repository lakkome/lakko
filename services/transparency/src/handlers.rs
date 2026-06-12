// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! HTTP handlers for the transparency log.
//!
//! Four endpoints:
//!
//! - `POST /transparency/append`           — bearer-authed leaf append.
//! - `GET  /transparency/root`             — anon current signed root.
//! - `GET  /transparency/proof/:idx`       — anon inclusion proof.
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
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_transparency::LogEntry;
use serde::{Deserialize, Serialize};

use crate::AppState;

/// Maximum identity-pubkey length we accept on append. Ed25519 is 32
/// bytes; we cap at the column width (`VARBINARY(64)`) so an oversized
/// blob is rejected at ingress rather than at the DB.
const MAX_PUBKEY_LEN: usize = 64;

// ----------------------------------------------------------------
// POST /transparency/append
// ----------------------------------------------------------------

/// `POST /transparency/append` request body.
#[derive(Debug, Deserialize)]
pub(crate) struct AppendReq {
    /// BLAKE3 username hash, base64-standard (32 bytes after decode).
    pub username_hash_b64: String,
    /// Identity public key, base64-standard (1..=64 bytes after decode;
    /// Ed25519 is 32).
    pub identity_pubkey_b64: String,
    /// Monotonic per-username version; must be `>= 1`.
    pub version: u64,
    /// ADR-0017 fresh-start marker — a no-supersession-proof owner
    /// reset. Defaults to `false` (a normal proven publish / rotation).
    #[serde(default)]
    pub is_reset: bool,
}

/// `POST /transparency/append` response body.
#[derive(Debug, Serialize)]
pub(crate) struct AppendRes {
    /// The 0-based leaf index the entry was appended at.
    pub idx: u64,
}

/// Append a leaf to the log. Bearer-session-authed (the
/// `require_session` middleware gates the route for S1). Validates the
/// decoded shapes, builds a [`LogEntry`], persists it, and replies with
/// the assigned index.
pub(crate) async fn append(
    State(state): State<AppState>,
    Json(req): Json<AppendReq>,
) -> Result<Json<AppendRes>, (StatusCode, &'static str)> {
    let username_hash_vec = B64.decode(req.username_hash_b64.as_bytes()).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "username_hash_b64 not valid base64",
        )
    })?;
    let username_hash: [u8; 32] = username_hash_vec
        .as_slice()
        .try_into()
        .map_err(|_| (StatusCode::BAD_REQUEST, "username_hash must be 32 bytes"))?;

    let identity_pubkey = B64
        .decode(req.identity_pubkey_b64.as_bytes())
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "identity_pubkey_b64 not valid base64",
            )
        })?;
    if identity_pubkey.is_empty() || identity_pubkey.len() > MAX_PUBKEY_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            "identity_pubkey must be 1..=64 bytes",
        ));
    }

    if req.version < 1 {
        return Err((StatusCode::BAD_REQUEST, "version must be >= 1"));
    }

    let entry = LogEntry {
        username_hash,
        identity_pubkey,
        version: req.version,
    };

    let idx = state
        .store
        .append(&state.db, entry, req.is_reset)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "transparency append failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "append failed")
        })?;

    Ok(Json(AppendRes { idx }))
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
}

/// Return the current signed root. Anonymous. Signs the current tree
/// state with `now` as the timestamp.
pub(crate) async fn root(State(state): State<AppState>) -> Json<RootRes> {
    let now = now_unix_secs();
    let signed = state.store.signed_root(now).await;
    Json(RootRes {
        tree_root_b64: B64.encode(signed.tree_root),
        log_size: signed.log_size,
        timestamp_unix: signed.timestamp_unix,
        signature_b64: B64.encode(signed.signature.to_bytes()),
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
    let (entry, proof) = state.store.proof(idx).await.map_err(|e| {
        tracing::debug!(idx, error = %e, "proof request out of range");
        (StatusCode::NOT_FOUND, "no entry at that index")
    })?;

    // `log_size` reflects the tree the proof was built against — read it
    // from the freshly-signed root so it is consistent with the audit
    // path. (The proof is built against `current_root`'s size.)
    let log_size = state.store.signed_root(now_unix_secs()).await.log_size;

    Ok(Json(ProofRes {
        idx: proof.leaf_index,
        leaf_b64: B64.encode(entry.canonical_bytes()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::LogStore;
    use lakko_crypto::signing;
    use lakko_transparency::{verify_inclusion, InMemoryLog, InclusionProof, SignedRoot};
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
}
