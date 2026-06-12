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
/// **Trust assumption (lakko-security KT-log review, finding 2).** This
/// service sees only the 32-byte `username_hash`, never the `account_id`, so
/// it CANNOT verify the `account_id ↔ username_hash` binding. Anyone holding
/// `LAKKO_TRANSPARENCY_APPEND_SECRET` (i.e. the discovery service, or a leaked
/// server-to-server secret) can therefore append a leaf for ANY account-key
/// with ANY pubkey, and it will carry a valid inclusion proof under the
/// operator-signed root. The log's integrity is thus bounded by "discovery +
/// the append secret are honest". This is NOT a silent key substitution: a
/// forged leaf still has to pass the client's fail-closed ADR-0016
/// supersession gate (which runs FIRST and refuses a changed key with no
/// old-key-signed proof), so the worst a forged append achieves is
/// log-poisoning / a confusing `mismatch` surfaced to the user — not
/// auto-trust. Closing this fully (so the service/auditors verify the binding
/// WITHOUT trusting discovery) needs a client-supplied signature by the
/// account-identity key over its own `(username_hash, pubkey, version)` —
/// tracked as future work (G-30).
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

    let result = state
        .store
        .append(&state.db, username_hash, identity_pubkey, req.is_reset)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "transparency append failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "append failed")
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
            .append(&pool, alice, vec![0x01; 32], false)
            .await
            .expect("append alice v1");
        store
            .append(&pool, alice, vec![0x02; 32], false)
            .await
            .expect("append alice v2");

        let state = crate::AppState {
            db: pool.clone(),
            store,
            append_secret: None,
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
