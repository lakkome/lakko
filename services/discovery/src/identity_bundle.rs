// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Public identity-bundle directory.
//!
//! The discovery service already maps `username → account_id`
//! via [`crate::registry`]. To send a
//! `lakko-contacts::ContactRequest`, the sender ALSO needs the
//! recipient's:
//!
//!   * **Ed25519 identity public key** — pins the
//!     `from_pubkey` field on ContactRequest signature
//!     verification.
//!   * **X25519 public key** — the HPKE-Base sealed envelope's
//!     recipient key for the optional intro message inside the
//!     ContactRequest.
//!
//! Both are public; putting them in a queryable directory leaks
//! no secret. One row per `account_id`; uploaded at signup,
//! replaceable via UPSERT on key rotation (Phase 1.16d3c+).
//!
//! ## Authorisation
//!
//! `POST /discovery/identity` is session-token-gated (Phase 1.5b)
//! and the handler requires the body's `account_id` to equal the
//! token's. It is counted per account by the write limiter
//! (`rate_limit::write_middleware`); since ADR-0054 S3 it carries
//! no proof of work.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::key_reset::RouteError;
use crate::AppState;

/// `account_id` length in bytes (UUIDv7 raw, matches the rest of
/// the service).
const ACCOUNT_ID_LEN: usize = 16;
/// Ed25519 public key length (RFC 8032 §5.1).
const SIGNATURE_PUBKEY_LEN: usize = 32;
/// X25519 public key length (RFC 7748 §5).
const X25519_PUBKEY_LEN: usize = 32;
/// Maximum supersession-proof length the server will store (G-4, ADR-0016).
/// The current `lakko-transparency` proof is 145 bytes; the cap leaves a
/// little headroom and matches the `VARBINARY(150)` column. The server caps
/// the length only to bound storage — it never parses the bytes (invariant
/// #2); validation is the receiving peer's job.
const MAX_SUPERSESSION_PROOF_LEN: usize = 150;

/// Maximum leaf-authorisation length discovery will store (G-30 #2,
/// ADR-0019). The widest leaf_auth is a 145-byte supersession proof (a
/// genesis self-sig is only 64 bytes); the cap leaves headroom and matches
/// the `VARBINARY(200)` column (and the transparency-side `log_entries`
/// column). Discovery caps the length only to bound storage — it never
/// parses the bytes (invariant #2); the leaf authorisation is verified
/// operator-side in `services/transparency` and by the client rotation chain.
const MAX_LEAF_AUTH_LEN: usize = 200;

/// Maximum genesis-attestation length discovery will store (ADR-0036). The
/// `GenesisAttestation` is a 153-byte fixed-layout Ed25519 blob; the cap
/// leaves headroom and matches the `VARBINARY(256)` column (and the
/// transparency-side `log_entries.genesis_attest` column). Discovery caps the
/// length only to bound storage — it never parses the bytes (invariant #2);
/// the attestation is verified operator-side in `services/transparency` and by
/// the client on a first observation.
const MAX_GENESIS_ATTEST_LEN: usize = 256;

/// A fetched `identity_bundles` row: `(signature_pubkey, x25519_pubkey,
/// supersession_proof?, key_generation, key_reset_attest?)` — the proof
/// column is nullable (G-4, ADR-0016); the generation and the nullable
/// key-reset attestation are ADR-0073's.
type IdentityBundleRow = (Vec<u8>, Vec<u8>, Option<Vec<u8>>, u64, Option<Vec<u8>>);

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

/// Decode the optional G-4 supersession proof (ADR-0016). `None` (the field
/// absent) stays `None`; the upsert then PRESERVES any already-stored proof
/// (via `COALESCE`), so a non-rotation re-upload does not strip a still-valid
/// proof. A present value is base64-decoded and length-capped to
/// [`MAX_SUPERSESSION_PROOF_LEN`]
/// here; the bytes are VERIFIED later, inside the upload's transaction and
/// against the key the row holds (`proof_is_valid`, ADR-0074 section 3.5).
/// The receiving peer still verifies for itself (`verify_supersession`).
fn decode_optional_proof(b64: Option<&str>) -> Result<Option<Vec<u8>>, (StatusCode, String)> {
    let Some(b64) = b64 else {
        return Ok(None);
    };
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid supersession_proof base64: {e}"),
        )
    })?;
    if bytes.len() > MAX_SUPERSESSION_PROOF_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("supersession_proof exceeds {MAX_SUPERSESSION_PROOF_LEN} bytes"),
        ));
    }
    Ok(Some(bytes))
}

/// Decode the optional G-30 #2 leaf authorisation (ADR-0019). Mirrors
/// [`decode_optional_proof`] exactly: `None` (the field absent) stays `None`
/// and the upsert PRESERVES any already-stored value (via `COALESCE`), so a
/// non-rotation re-upload does not strip a still-valid leaf authorisation. A
/// present value is base64-decoded and length-capped to [`MAX_LEAF_AUTH_LEN`]
/// — discovery does NOT parse or verify the bytes (invariant #2); the leaf
/// authorisation is verified operator-side in `services/transparency` and by
/// the client rotation chain.
fn decode_optional_leaf_auth(b64: Option<&str>) -> Result<Option<Vec<u8>>, (StatusCode, String)> {
    let Some(b64) = b64 else {
        return Ok(None);
    };
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid leaf_auth base64: {e}"),
        )
    })?;
    if bytes.len() > MAX_LEAF_AUTH_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("leaf_auth exceeds {MAX_LEAF_AUTH_LEN} bytes"),
        ));
    }
    Ok(Some(bytes))
}

/// Decode the optional ADR-0036 genesis attestation. Mirrors
/// [`decode_optional_leaf_auth`] exactly: `None` (the field absent) stays
/// `None` and the upsert PRESERVES any already-stored value (via `COALESCE`),
/// so a non-rotation re-upload does not strip a still-valid attestation before
/// it has been forwarded to the transparency log. A present value is
/// base64-decoded and length-capped to [`MAX_GENESIS_ATTEST_LEN`] — discovery
/// does NOT parse or verify the bytes (invariant #2); the attestation is
/// verified operator-side in `services/transparency` and by the client on a
/// first observation.
fn decode_optional_genesis_attest(
    b64: Option<&str>,
) -> Result<Option<Vec<u8>>, (StatusCode, String)> {
    let Some(b64) = b64 else {
        return Ok(None);
    };
    let bytes = B64.decode(b64).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid genesis_attest base64: {e}"),
        )
    })?;
    if bytes.len() > MAX_GENESIS_ATTEST_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("genesis_attest exceeds {MAX_GENESIS_ATTEST_LEN} bytes"),
        ));
    }
    Ok(Some(bytes))
}

// ============================================================================
// POST /discovery/identity   — upsert
// ============================================================================

/// Upload (or replace) the identity bundle for the claimed
/// `account_id`. Session-gated and counted per account (see the
/// file-level docstring).
#[derive(Deserialize)]
pub(crate) struct UploadRequest {
    /// Base64 of the raw 16-byte UUIDv7.
    pub(crate) account_id: String,
    /// Base64 of the 32-byte Ed25519 public key.
    pub(crate) signature_pubkey: String,
    /// Base64 of the 32-byte X25519 public key (HPKE init_key).
    pub(crate) x25519_pubkey: String,
    /// G-4 (ADR-0016): base64 of an optional old-key-signed supersession
    /// proof bridging this rotation. Absent for a normal (non-rotation)
    /// upload, which PRESERVES any stored proof. Since ADR-0074 the server
    /// VERIFIES it (`proof_is_valid`): on a key change it must be signed by
    /// the key the directory holds and authorise the uploaded key; on a
    /// re-upload it must be the proof of the rotation that produced the
    /// uploaded key. A public artefact, so verifying it reads nothing
    /// private (invariant #2). Sent together with `key_reset_attest` it is a
    /// proven rotation at a new generation (ADR-0074 section 3.5).
    #[serde(default)]
    pub(crate) supersession_proof: Option<String>,
    /// G-30 #2 (ADR-0019): base64 of an optional client leaf authorisation
    /// for this upload — a genesis / fresh-start self-signature or a
    /// normal-rotation supersession proof. Absent for a non-rotation
    /// re-upload, which PRESERVES any stored value (the upsert uses
    /// `COALESCE`). Discovery stores it opaque and never parses it
    /// (invariant #2); it is forwarded verbatim to the transparency append.
    #[serde(default)]
    pub(crate) leaf_auth: Option<String>,
    /// ADR-0036: base64 of an optional auth-attested genesis attestation, sent
    /// on a first publish (or an ADR-0017 reset) alongside the `leaf_auth`
    /// self-signature. Absent for a rotation or a non-rotation re-upload,
    /// which PRESERVES any stored value (the upsert uses `COALESCE`).
    /// Discovery stores it opaque and never parses it (invariant #2); it is
    /// forwarded verbatim to the transparency append as `genesis_attest_b64`.
    #[serde(default)]
    pub(crate) genesis_attest: Option<String>,
    /// ADR-0073 (S3): base64 (standard alphabet, like every sibling field —
    /// auth hands it out URL-safe, the client re-encodes as it does for
    /// `genesis_attest`) of the 177-byte key-reset attestation minted by
    /// `POST /account/keys/reset/finish`. Required for a key CHANGE that
    /// carries no supersession proof; discovery VERIFIES it (the one
    /// attestation it parses: signature under the pinned attest key, the
    /// session account's `account_ref`, the uploaded key, a generation
    /// strictly above the row's) and then stores it opaque. On a same-key
    /// re-upload it is ignored. Absent on a routine upload.
    #[serde(default)]
    pub(crate) key_reset_attest: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct UploadResponse {
    pub(crate) ok: bool,
}

/// `POST /discovery/identity` — upsert.
///
/// Replaces an existing row atomically via MariaDB
/// `INSERT … ON DUPLICATE KEY UPDATE`. The
/// `updated_at` column auto-bumps via the table's
/// `ON UPDATE CURRENT_TIMESTAMP` clause.
///
/// ADR-0073 section 4.1 — the change rule: a key CHANGE is accepted only with
/// a supersession proof (ADR-0016; verified since ADR-0074, `400
/// {"error":"supersession-proof-invalid"}` otherwise) or a verified key-reset
/// attestation of a strictly greater generation, or both (a proven rotation,
/// ADR-0074 section 3.5); an unattested, unproven change is `409
/// {"error":"identity-change-needs-attestation"}`. The session token's
/// generation must not be below the row's (`401
/// {"error":"key-generation-superseded"}`). An attested change deletes every
/// KeyPackage row of the account in the same transaction (section 5).
pub(crate) async fn upload(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    axum::Extension(generation): axum::Extension<crate::auth::SessionKeyGeneration>,
    Json(payload): Json<UploadRequest>,
) -> Result<Json<UploadResponse>, RouteError> {
    let account_id = decode_fixed::<ACCOUNT_ID_LEN>(&payload.account_id, "account_id")?;
    let signature_pubkey =
        decode_fixed::<SIGNATURE_PUBKEY_LEN>(&payload.signature_pubkey, "signature_pubkey")?;
    let x25519_pubkey = decode_fixed::<X25519_PUBKEY_LEN>(&payload.x25519_pubkey, "x25519_pubkey")?;

    // G-4 (ADR-0016): decode the optional supersession proof (opaque to the
    // server). `None` PRESERVES any stored proof (the `COALESCE` below), so a
    // routine non-rotation re-upload (login refresh, KeyPackage rotation)
    // does not strip a still-valid proof before peers have pulled it; a new
    // rotation supplies a fresh proof that replaces it, and the proof
    // self-expires via its own 14-day window regardless.
    let supersession_proof = decode_optional_proof(payload.supersession_proof.as_deref())?;

    // G-30 #2 (ADR-0019): decode the optional leaf authorisation (opaque to
    // discovery). Mirrors `supersession_proof`: `None` PRESERVES any stored
    // value (the `COALESCE` in the upsert), so a routine non-rotation
    // re-upload does not strip a still-valid leaf authorisation before it has
    // been forwarded to the transparency log. Discovery NEVER parses these
    // bytes (invariant #2); verification is the transparency service's job
    // and, definitively, the client rotation chain's.
    let leaf_auth = decode_optional_leaf_auth(payload.leaf_auth.as_deref())?;

    // ADR-0036: decode the optional genesis attestation (opaque to
    // discovery). Mirrors `leaf_auth`: `None` PRESERVES any stored value (the
    // `COALESCE` in the upsert), so a routine non-rotation re-upload does not
    // strip a still-valid attestation before it has been forwarded to the
    // transparency log. Discovery NEVER parses these bytes (invariant #2);
    // verification is the transparency service's job and, definitively, the
    // client's on a first observation.
    let genesis_attest = decode_optional_genesis_attest(payload.genesis_attest.as_deref())?;

    // Phase 1.5b anti-impersonation: only the account_id's owner
    // can upload its identity bundle. Same check shape as
    // `registry::register`.
    if account_id != authed.0 {
        tracing::warn!(
            body_account_id = %B64.encode(account_id),
            token_account_id = %B64.encode(authed.0),
            "identity_bundle upload: body account_id does not match token's"
        );
        return Err(RouteError::Text(
            StatusCode::FORBIDDEN,
            "account_id mismatch between body and session token".to_string(),
        ));
    }

    // ADR-0074 section 3.5: the log leaf of a proven change is authorised by
    // the proof itself (ADR-0019). A client that sends the proof and no
    // separate `leaf_auth` gets the proof as the leaf authorisation; the
    // proof is verified in the transaction before anything is stored.
    let leaf_auth = leaf_auth.or_else(|| supersession_proof.clone());

    // ADR-0073: the one attestation discovery parses. Verified here — the
    // signature under the pinned key, the session account's `account_ref`,
    // the uploaded key — and compared against the row's generation inside
    // the transaction below.
    let key_reset_attest = crate::key_reset::verify_upload_attestation(
        payload.key_reset_attest.as_deref(),
        state.key_reset_attest_key.as_ref(),
        &account_id,
        &signature_pubkey,
    )?;

    // ADR-0073 4.1 change rule + the generation floor, under the ADR-0017
    // cool-down kept as defence in depth on attested changes (see
    // `apply_identity_upload_gated`).
    let decision = apply_identity_upload_gated(
        &state.db,
        state.fresh_identity_cooldown_secs,
        &account_id,
        &signature_pubkey,
        &x25519_pubkey,
        Evidence {
            supersession_proof: supersession_proof.as_deref(),
            key_reset_attest: key_reset_attest
                .as_ref()
                .map(|(claims, bytes)| (claims.key_generation, bytes.as_slice())),
        },
        leaf_auth.as_deref(),
        genesis_attest.as_deref(),
        generation.0,
    )
    .await?;

    tracing::info!(
        account_id = %B64.encode(account_id),
        "identity bundle upserted",
    );

    // KT-log S2 (decision E) — best-effort transparency-log append on a
    // KEY CHANGE only. The identity bundle is ALREADY published above; the
    // append is fail-open: a transparency-side failure NEVER fails this
    // upload (a missed append is closed by the S2.5 reconciliation sweep).
    // When `LAKKO_TRANSPARENCY_URL` is unset the client is `None` and the
    // integration is off (logged once at startup).
    //
    // KT-log S2.5: on a CONFIRMED append, best-effort advance the
    // `logged_pubkey` marker to the just-logged key, so the reconciler
    // skips this account. A failed marker UPDATE is fine (the reconciler
    // heals it) — warn + proceed; NEVER fail the upload because of it.
    if let (LogAppendDecision::Append { is_reset }, Some(client)) =
        (decision, state.transparency.as_ref())
    {
        // G-30 #2 (ADR-0019): forward the leaf authorisation supplied with
        // this upload. An `Append` only fires on a key CHANGE, and a key
        // change always carries a fresh leaf authorisation; a missing value
        // is governed by the transparency-side presence policy. Opaque
        // pass-through — discovery never parses these bytes (invariant #2).
        // A missed append here is closed by the S2.5 reconciler, which reads
        // the persisted `leaf_auth` from the row.
        //
        // ADR-0036: forward the genesis attestation alongside it (opaque,
        // `genesis_attest_b64`). Present only for a genesis / reset upload; a
        // rotation carries `None` and the transparency append gate consults it
        // only on the genesis/reset arm anyway. Same fail-open + S2.5-backfill
        // posture as `leaf_auth`.
        let confirmed = client
            .append_key_change(
                &account_id,
                &signature_pubkey,
                is_reset,
                leaf_auth.as_deref(),
                genesis_attest.as_deref(),
            )
            .await;
        if confirmed {
            mark_logged_pubkey(&state.db, &account_id, &signature_pubkey).await;
        }
    }

    Ok(Json(UploadResponse { ok: true }))
}

/// KT-log S2.5 — best-effort advance of the `logged_pubkey` marker after a
/// CONFIRMED transparency append. Records the just-logged key so the
/// reconciler skips this account on its next sweep. A failure here NEVER
/// fails the upload: the reconciler will heal a stale marker. We `warn!`
/// and return.
async fn mark_logged_pubkey(db: &sqlx::MySqlPool, account_id: &[u8], signature_pubkey: &[u8]) {
    let result = sqlx::query("UPDATE identity_bundles SET logged_pubkey = ? WHERE account_id = ?")
        .bind(signature_pubkey)
        .bind(account_id)
        .execute(db)
        .await;
    if let Err(e) = result {
        tracing::warn!(
            account_id = %B64.encode(account_id),
            error = %e,
            "KT-log S2.5: failed to advance logged_pubkey after a confirmed append \
             — the reconciler will heal it",
        );
    }
}

/// Map a sqlx error from the change-rule transaction to a 500.
fn change_db_err(e: sqlx::Error) -> RouteError {
    tracing::error!(error = %e, "identity_bundles change-rule tx");
    RouteError::Text(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal db error".to_string(),
    )
}

/// MariaDB's "deadlock found" error number.
const ER_LOCK_DEADLOCK: u16 = 1213;
/// MariaDB's "lock wait timeout" error number.
const ER_LOCK_WAIT_TIMEOUT: u16 = 1205;

/// `true` when a sqlx error is one a fresh attempt may not meet: a unique-key
/// violation (a concurrent FIRST publish of the same account won the INSERT
/// race, so the row now exists and the retry takes the UPDATE path), or a
/// deadlock / lock-wait timeout (the `SELECT ... FOR UPDATE` on an absent row
/// holds a gap lock, so two concurrent first publishes of DIFFERENT accounts
/// can deadlock on their INSERTs — MariaDB rolls one back and the retry
/// succeeds). Anything else is a real failure.
fn is_lost_race(e: &sqlx::Error) -> bool {
    let Some(db) = e.as_database_error() else {
        return false;
    };
    if db.is_unique_violation() {
        return true;
    }
    db.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
        .is_some_and(|m| matches!(m.number(), ER_LOCK_DEADLOCK | ER_LOCK_WAIT_TIMEOUT))
}

/// Why one attempt of the upsert did not commit.
enum AttemptError {
    /// A database failure; [`is_lost_race`] decides whether to retry it.
    Db(sqlx::Error),
    /// A refusal the client must see (the JSON error codes of
    /// [`crate::key_reset`]). Never retried.
    Refused(RouteError),
}

impl From<sqlx::Error> for AttemptError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

/// What an upload carries to justify a key CHANGE (ADR-0073 section 4.1,
/// ADR-0074 section 3.5). Both are the decoded bytes. The proof is verified
/// inside the transaction, against the key the row holds under its lock
/// ([`proof_is_valid`]); the attestation has already been verified by
/// [`crate::key_reset::verify_upload_attestation`] and is carried with its
/// generation.
#[derive(Clone, Copy)]
pub(crate) struct Evidence<'a> {
    /// G-4 (ADR-0016): a supersession proof, the previous key signing the
    /// uploaded one.
    pub(crate) supersession_proof: Option<&'a [u8]>,
    /// ADR-0073: `(key_generation, wire bytes)` of a verified key-reset
    /// attestation naming this account and the uploaded key.
    pub(crate) key_reset_attest: Option<(u64, &'a [u8])>,
}

impl Evidence<'_> {
    /// The rule's view of what the upload carries.
    const fn kind(self) -> EvidenceKind {
        match (self.supersession_proof, self.key_reset_attest) {
            // ADR-0074 section 3.5: both together are a proven rotation at a
            // new generation, the account's own "change my keys".
            (Some(_), Some((generation, _))) => EvidenceKind::ProvenRotation { generation },
            (Some(_), None) => EvidenceKind::Proof,
            (None, Some((generation, _))) => EvidenceKind::Attested { generation },
            (None, None) => EvidenceKind::None,
        }
    }
}

/// [`Evidence`] without the bytes — the input of the pure change rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EvidenceKind {
    /// Neither a proof nor an attestation.
    None,
    /// A supersession proof alone (ADR-0016): the key changes, the
    /// generation does not.
    Proof,
    /// A verified key-reset attestation of this generation (ADR-0073), with
    /// no proof: a reset.
    Attested {
        /// The generation the attestation moves the account to.
        generation: u64,
    },
    /// A supersession proof AND a verified attestation of this generation
    /// (ADR-0074 section 3.5): a rotation the previous key signed.
    ProvenRotation {
        /// The generation the attestation moves the account to.
        generation: u64,
    },
}

/// `true` when `proof` may ride on this upload (ADR-0074 section 3.5; before
/// it the directory tested for presence only, so a session alone could
/// publish a key every client then refused).
///
/// - `current` is the key the row holds (`None` on a first publish),
///   `previous` the key it held before its last change, `uploaded` the key
///   this upload carries.
/// - **A key change** (`current != uploaded`): the proof must be signed by
///   `current`, authorise exactly `uploaded`, have a validity window the
///   format allows and not be expired at `now_unix_secs`.
/// - **No key change** (a re-upload, or a first publish): the proof is the
///   record of a rotation that already happened, kept for peers. It must
///   open (its signature verifies under the old key it names) and authorise
///   `uploaded`; when the row knows its previous key, the proof's old key
///   must be that one. Its expiry is not judged here: a proof expires while
///   it is stored, and the peer that reads it judges that.
fn proof_is_valid(
    proof: &[u8],
    current: Option<&[u8]>,
    previous: Option<&[u8]>,
    uploaded: &[u8],
    now_unix_secs: i64,
) -> bool {
    let Ok(uploaded) = <[u8; 32]>::try_from(uploaded) else {
        return false;
    };
    match current {
        Some(current) if current != uploaded.as_slice() => {
            let Ok(current) = <[u8; 32]>::try_from(current) else {
                return false;
            };
            lakko_transparency::verify_supersession(proof, &current, now_unix_secs)
                .is_ok_and(|authorised| authorised == uploaded)
        },
        _ => lakko_transparency::SupersessionProof::open(proof).is_ok_and(|claims| {
            claims.new_pubkey == uploaded
                && claims.old_pubkey != uploaded
                && claims.validity_window_ok()
                && previous.map_or(true, |p| p == claims.old_pubkey.as_slice())
        }),
    }
}

/// The `400 supersession-proof-invalid` refusal.
fn proof_refused(account_id: &[u8]) -> AttemptError {
    tracing::warn!(
        account_id = %B64.encode(account_id),
        "identity upload refused — supersession proof does not verify",
    );
    AttemptError::Refused(crate::key_reset::json_error(
        StatusCode::BAD_REQUEST,
        crate::key_reset::CODE_PROOF_INVALID,
    ))
}

/// Unix seconds, saturating.
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Whether the upsert that just committed CHANGED the published identity
/// key, and therefore whether the discovery service must append a leaf to
/// the transparency log (KT-log S2 decision D).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogAppendDecision {
    /// No key change — a no-change re-upload (e.g. a routine login refresh
    /// or KeyPackage rotation that re-sends the same identity key). Do NOT
    /// append to the transparency log.
    Skip,
    /// A key change happened — append a leaf to the transparency log. The
    /// `is_reset` flag is the KT-log "fresh start" marker: `false` for a
    /// first publish or a proven rotation; `true` for an attested key reset
    /// (ADR-0073, the successor of ADR-0017's unproven change — the only
    /// key change without a supersession proof the directory accepts).
    Append {
        /// ADR-0017 / ADR-0073 fresh-start marker.
        is_reset: bool,
    },
}

/// Apply the identity-bundle upsert under the ADR-0073 change rule, the
/// generation floor and the ADR-0017 (ii) cool-down, retrying a lost race
/// ([`is_lost_race`]: the concurrent-first-publish INSERT race, whose loser
/// then takes the row-locked UPDATE path, and the gap-lock deadlock between
/// first publishes of different accounts) instead of surfacing a 500. Both
/// are rare, so a small bounded retry suffices.
///
/// On success returns the [`LogAppendDecision`] (KT-log S2 decision D): the
/// caller appends a transparency-log leaf iff the published key CHANGED.
/// Every refusal is a JSON error code of [`crate::key_reset`].
#[allow(clippy::too_many_arguments)]
async fn apply_identity_upload_gated(
    db: &sqlx::MySqlPool,
    cooldown_secs: i64,
    account_id: &[u8],
    signature_pubkey: &[u8],
    x25519_pubkey: &[u8],
    evidence: Evidence<'_>,
    leaf_auth: Option<&[u8]>,
    genesis_attest: Option<&[u8]>,
    token_generation: u64,
) -> Result<LogAppendDecision, RouteError> {
    const MAX_ATTEMPTS: u32 = 3;
    for _ in 0..MAX_ATTEMPTS {
        match try_apply_identity_once(
            db,
            cooldown_secs,
            account_id,
            signature_pubkey,
            x25519_pubkey,
            evidence,
            leaf_auth,
            genesis_attest,
            token_generation,
        )
        .await
        {
            Ok(decision) => return Ok(decision),
            Err(AttemptError::Refused(response)) => return Err(response),
            Err(AttemptError::Db(e)) if is_lost_race(&e) => {
                tracing::debug!(error = %e, "identity upsert lost a race — retrying");
            },
            Err(AttemptError::Db(e)) => return Err(change_db_err(e)),
        }
    }
    // Exhausted retries — extreme single-account contention. Transient, so the
    // client can safely retry.
    Err(RouteError::Text(
        StatusCode::SERVICE_UNAVAILABLE,
        "identity upsert contended — retry".to_string(),
    ))
}

/// `(signature_pubkey, key_generation, within_cooldown, previous_signature_pubkey)`
/// of the row an upload meets.
type ExistingRow = (Vec<u8>, u64, i64, Option<Vec<u8>>);

/// A single attempt of the gated upsert. Read-compare-conditional-write under
/// a row lock so the check is race-free. A database error drops the
/// transaction (rollback); a refusal rolls it back explicitly.
///
/// **The change rule (ADR-0073 section 4.1).** A same-key re-upload always
/// applies and changes nothing but the sibling columns. A key CHANGE applies
/// only with a supersession proof (ADR-0016; never gated, never a reset) or a
/// key-reset attestation whose generation is strictly above the row's
/// (stores the attestation and the generation, deletes every KeyPackage row
/// of the account, marks the KT-log leaf `is_reset`); an attested change
/// inside the cool-down window is `429`; a change with neither is `409
/// identity-change-needs-attestation`. The ADR-0017 "heal" back to the
/// previous key — the mechanism of the publish-back — is retired: an old
/// device re-publishing its old key after a reset is refused like any other
/// unattested change.
///
/// **The floor.** The session token's generation must not be below the row's
/// (`401 key-generation-superseded`); a higher one is accepted and raises
/// nothing (the attested upload has not landed yet).
///
/// **A proven rotation (ADR-0074 section 3.5).** A key change carrying a
/// proof AND an attestation is the account changing its own keys while its
/// other devices stay: the proof must verify, the attestation's generation
/// must be strictly above the row's (`409` otherwise), both are stored, the
/// generation is set, the cool-down does not apply and is not started, and
/// the KT-log leaf is NOT a reset. Every KeyPackage row of the account is
/// deleted, as on a reset: each row was published by a device of the old
/// generation, and the directory lists a device again when it publishes
/// with a session of the new one, which a kept device gets only by showing
/// it holds the new key. Device ids are untouched by this (a re-publish is
/// not blocked: the bulk delete writes no tombstone). **The tombstones are
/// kept**, unlike on a reset: a tombstone names a device that was REMOVED,
/// the removal is why the keys are being rotated, and dropping it would let
/// someone who signs in afresh on that device id list it again with a
/// binding under the predecessor key that peers still accept.
///
/// **Trust model.** The proof is verified here ([`proof_is_valid`]): it is a
/// public artefact signed by a key this row already publishes, so verifying
/// it reads nothing private (invariant #2), and it is what stops a session
/// alone from publishing a key that every client then refuses. The
/// load-bearing integrity gate for a PEER stays its own fail-closed verify
/// (ADR-0016): a peer does not trust this check. The attestation is verified
/// too: it is auth's signed artefact, and discovery is the party that
/// enforces "no key change without one".
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn try_apply_identity_once(
    db: &sqlx::MySqlPool,
    cooldown_secs: i64,
    account_id: &[u8],
    signature_pubkey: &[u8],
    x25519_pubkey: &[u8],
    evidence: Evidence<'_>,
    leaf_auth: Option<&[u8]>,
    genesis_attest: Option<&[u8]>,
    token_generation: u64,
) -> Result<LogAppendDecision, AttemptError> {
    let mut tx = db.begin().await?;

    // Row lock + the facts we need: the current key, the stored generation,
    // and whether the last attested change is still within the cool-down
    // window (computed server-side to avoid any Rust/DB clock skew; a
    // `0`-second window never matches, since the elapsed seconds are always
    // `>= 0`).
    let existing: Option<ExistingRow> = sqlx::query_as(
        "SELECT signature_pubkey, key_generation, \
            CAST( \
                last_unproven_key_change_at IS NOT NULL \
                AND TIMESTAMPDIFF(SECOND, last_unproven_key_change_at, NOW(3)) < ? \
            AS SIGNED), \
            previous_signature_pubkey \
         FROM identity_bundles WHERE account_id = ? FOR UPDATE",
    )
    .bind(cooldown_secs)
    .bind(account_id)
    .fetch_optional(&mut *tx)
    .await?;

    let decision: LogAppendDecision = match existing {
        None => {
            // First publish for this account — no floor, no change to judge.
            // An attestation on a first publish (an account that reset before
            // it ever published) still records its generation and clears any
            // KeyPackage rows older devices left.
            if evidence.supersession_proof.is_some_and(|proof| {
                !proof_is_valid(proof, None, None, signature_pubkey, now_unix_secs())
            }) {
                tx.rollback().await?;
                return Err(proof_refused(account_id));
            }
            let attested = evidence.key_reset_attest;
            // A duplicate key here (a concurrent first publish won) and the
            // gap-lock deadlock both surface as a lost race for the caller to
            // retry.
            sqlx::query(
                "INSERT INTO identity_bundles \
                    (account_id, signature_pubkey, x25519_pubkey, supersession_proof, \
                     leaf_auth, genesis_attest, key_generation, key_reset_attest) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(account_id)
            .bind(signature_pubkey)
            .bind(x25519_pubkey)
            .bind(evidence.supersession_proof)
            .bind(leaf_auth)
            .bind(genesis_attest)
            // Genesis is generation 1 (ADR-0073 section 1).
            .bind(attested.map_or(1u64, |(g, _)| g))
            .bind(attested.map(|(_, bytes)| bytes))
            .execute(&mut *tx)
            .await?;
            if attested.is_some() {
                delete_keypackages(&mut tx, account_id).await?;
            }
            // A first publish is a key change from "nothing" to a key, and
            // is never a fresh-start reset → Append{is_reset:false}
            // (decision D).
            LogAppendDecision::Append { is_reset: false }
        },
        Some((current, stored_generation, within_cooldown, previous)) => {
            // ADR-0073: the generation floor, before any change is judged.
            if crate::key_reset::floor_decision(Some(stored_generation), token_generation) {
                tx.rollback().await?;
                tracing::warn!(
                    account_id = %B64.encode(account_id),
                    token_generation,
                    stored_generation,
                    "identity upload refused — session token's key generation superseded",
                );
                return Err(AttemptError::Refused(crate::key_reset::json_error(
                    StatusCode::UNAUTHORIZED,
                    crate::key_reset::CODE_KEY_GENERATION_SUPERSEDED,
                )));
            }

            // ADR-0074 section 3.5: a proof that rides on the upload is
            // verified against the key this row holds, under its lock.
            if evidence.supersession_proof.is_some_and(|proof| {
                !proof_is_valid(
                    proof,
                    Some(&current),
                    previous.as_deref(),
                    signature_pubkey,
                    now_unix_secs(),
                )
            }) {
                tx.rollback().await?;
                return Err(proof_refused(account_id));
            }

            let outcome = change_outcome(
                &current,
                signature_pubkey,
                evidence.kind(),
                stored_generation,
                within_cooldown != 0,
            );

            let (key_changed, attested_generation, proven) = match outcome {
                ChangeOutcome::Apply {
                    key_changed,
                    attested_generation,
                    proven,
                } => (key_changed, attested_generation, proven),
                ChangeOutcome::NeedsAttestation
                | ChangeOutcome::NotNewer
                | ChangeOutcome::CoolingDown => {
                    tx.rollback().await?;
                    let (status, code) = outcome.refusal();
                    tracing::warn!(
                        account_id = %B64.encode(account_id),
                        code,
                        "identity key change refused",
                    );
                    return Err(AttemptError::Refused(crate::key_reset::json_error(
                        status, code,
                    )));
                },
            };

            // On any key change, the OLD current becomes `previous` (kept as
            // an audit column; nothing reads it since the heal was retired).
            // The attestation and the generation are written only when an
            // attested change applies; the cool-down marker moves with it.
            let new_previous: Option<&[u8]> = key_changed.then_some(current.as_slice());
            let attestation_bytes: Option<&[u8]> = attested_generation
                .and(evidence.key_reset_attest)
                .map(|(_, bytes)| bytes);
            // The cool-down marker is the last UNPROVEN change: a rotation
            // the previous key signed does not start it.
            let bump_marker_i: i64 = i64::from(attested_generation.is_some() && !proven);

            sqlx::query(
                "UPDATE identity_bundles SET \
                    signature_pubkey            = ?, \
                    x25519_pubkey               = ?, \
                    supersession_proof          = COALESCE(?, supersession_proof), \
                    leaf_auth                   = COALESCE(?, leaf_auth), \
                    genesis_attest              = COALESCE(?, genesis_attest), \
                    key_generation              = COALESCE(?, key_generation), \
                    key_reset_attest            = COALESCE(?, key_reset_attest), \
                    previous_signature_pubkey   = COALESCE(?, previous_signature_pubkey), \
                    last_unproven_key_change_at = IF(?, NOW(3), last_unproven_key_change_at) \
                 WHERE account_id = ?",
            )
            .bind(signature_pubkey)
            .bind(x25519_pubkey)
            .bind(evidence.supersession_proof)
            .bind(leaf_auth)
            .bind(genesis_attest)
            .bind(attested_generation)
            .bind(attestation_bytes)
            .bind(new_previous)
            .bind(bump_marker_i)
            .bind(account_id)
            .execute(&mut *tx)
            .await?;

            // ADR-0073 section 5: an attested change removes every device of
            // the old generation from the directory, the resetting device's
            // own row included (its binding was signed by the old account
            // key; it re-publishes right after).
            if attested_generation.is_some() {
                if proven {
                    delete_keypackage_rows(&mut tx, account_id).await?;
                } else {
                    delete_keypackages(&mut tx, account_id).await?;
                }
            }

            log_append_decision(key_changed, attested_generation.is_some() && !proven)
        },
    };

    tx.commit().await?;
    Ok(decision)
}

/// Delete every KeyPackage row of `account_id` and KEEP its device
/// tombstones: the proven-rotation arm (ADR-0074 section 3.5; see
/// [`try_apply_identity_once`] for why the tombstones stay).
async fn delete_keypackage_rows(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    account_id: &[u8],
) -> Result<(), sqlx::Error> {
    let deleted = sqlx::query("DELETE FROM keypackages WHERE account_id = ?")
        .bind(account_id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    tracing::info!(
        account_id = %B64.encode(account_id),
        deleted,
        "proven key rotation: KeyPackage rows of the old generation removed, tombstones kept",
    );
    Ok(())
}

/// Delete every KeyPackage row of `account_id` inside the upload's
/// transaction (ADR-0073 section 4.1 / 5), and with them the account's
/// device tombstones (ADR-0074 section 2.2): the generation supersedes every
/// device of the old one, and a device that re-publishes under the new
/// generation holds the new keys.
async fn delete_keypackages(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    account_id: &[u8],
) -> Result<(), sqlx::Error> {
    let deleted = sqlx::query("DELETE FROM keypackages WHERE account_id = ?")
        .bind(account_id)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    sqlx::query("DELETE FROM keypackage_tombstones WHERE account_id = ?")
        .bind(account_id)
        .execute(&mut **tx)
        .await?;
    tracing::info!(
        account_id = %B64.encode(account_id),
        deleted,
        "attested key reset: KeyPackage rows of the old generation removed",
    );
    Ok(())
}

/// The ADR-0073 change-rule decision for an upload against an existing
/// row. `Apply` carries the writes the caller must perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeOutcome {
    /// Proceed with the upsert.
    Apply {
        /// The published key changes (the OLD current becomes `previous`).
        key_changed: bool,
        /// `Some(g)` when an attested change applies: store the attestation,
        /// set `key_generation = g`, delete the KeyPackage rows; and, unless
        /// `proven`, bump the cool-down marker, delete the tombstones and
        /// mark the KT-log leaf `is_reset`.
        attested_generation: Option<u64>,
        /// The change is justified by a supersession proof (alone, or with
        /// an attestation): never a reset, never gated by the cool-down.
        proven: bool,
    },
    /// Refuse — `409`: a key change with neither a proof nor an attestation
    /// (the retired heal lands here too).
    NeedsAttestation,
    /// Refuse — `409`: an attested change whose generation is not strictly
    /// above the row's (a replay, or a reset a later one overtook).
    NotNewer,
    /// Refuse — `429`: an attested change while the cool-down from the last
    /// one is still running (ADR-0017 defence in depth, kept).
    CoolingDown,
}

impl ChangeOutcome {
    /// The status and JSON error code of a refusal; `Apply` has none and
    /// answers the `409 identity-change-needs-attestation` pair only so the
    /// function is total (callers match `Apply` first).
    const fn refusal(self) -> (StatusCode, &'static str) {
        match self {
            Self::Apply { .. } | Self::NeedsAttestation => (
                StatusCode::CONFLICT,
                crate::key_reset::CODE_NEEDS_ATTESTATION,
            ),
            Self::NotNewer => (
                StatusCode::CONFLICT,
                crate::key_reset::CODE_GENERATION_NOT_NEWER,
            ),
            Self::CoolingDown => (
                StatusCode::TOO_MANY_REQUESTS,
                crate::key_reset::CODE_COOLING_DOWN,
            ),
        }
    }
}

/// Pure KT-log S2 (decision D) mapping for the EXISTING-row case: given
/// the change-rule outcome's two facts, decide whether discovery must
/// append a transparency-log leaf.
///
/// - `key_changed` is exactly "the published key changed". `false` →
///   [`LogAppendDecision::Skip`] (a no-change re-upload — no append).
/// - On a change, `attested_reset` is exactly the fresh-start marker
///   (`is_reset`). A proven rotation changes the key but is NOT a reset →
///   `Append { is_reset: false }`.
const fn log_append_decision(key_changed: bool, attested_reset: bool) -> LogAppendDecision {
    if key_changed {
        LogAppendDecision::Append {
            is_reset: attested_reset,
        }
    } else {
        LogAppendDecision::Skip
    }
}

/// Pure decision matrix of the ADR-0073 change rule (no I/O so it is
/// unit-testable). NEVER refuses a no-change re-upload or a proven rotation;
/// an attested change needs a strictly greater generation and an elapsed
/// cool-down; everything else is refused.
fn change_outcome(
    current: &[u8],
    new_sig: &[u8],
    evidence: EvidenceKind,
    stored_generation: u64,
    within_cooldown: bool,
) -> ChangeOutcome {
    if current == new_sig {
        // A routine re-publish of the same key: the attestation, if one
        // rides along (a retry after a crash), has already been applied or
        // is irrelevant; nothing about the generation changes.
        return ChangeOutcome::Apply {
            key_changed: false,
            attested_generation: None,
            proven: false,
        };
    }
    match evidence {
        EvidenceKind::None => ChangeOutcome::NeedsAttestation,
        EvidenceKind::Proof => ChangeOutcome::Apply {
            key_changed: true,
            attested_generation: None,
            proven: true,
        },
        // A rotation at a new generation: the generation rule of a reset,
        // none of its cool-down (the previous key signed this change).
        EvidenceKind::ProvenRotation { generation } => {
            if generation <= stored_generation {
                ChangeOutcome::NotNewer
            } else {
                ChangeOutcome::Apply {
                    key_changed: true,
                    attested_generation: Some(generation),
                    proven: true,
                }
            }
        },
        EvidenceKind::Attested { generation } => {
            if generation <= stored_generation {
                ChangeOutcome::NotNewer
            } else if within_cooldown {
                ChangeOutcome::CoolingDown
            } else {
                ChangeOutcome::Apply {
                    key_changed: true,
                    attested_generation: Some(generation),
                    proven: false,
                }
            }
        },
    }
}

#[cfg(test)]
mod change_rule_tests {
    use super::{change_outcome, ChangeOutcome, EvidenceKind};

    const A: &[u8] = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const B: &[u8] = b"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

    const UNCHANGED: ChangeOutcome = ChangeOutcome::Apply {
        key_changed: false,
        attested_generation: None,
        proven: false,
    };

    #[test]
    fn same_key_always_applies_and_changes_nothing() {
        for evidence in [
            EvidenceKind::None,
            EvidenceKind::Proof,
            EvidenceKind::Attested { generation: 2 },
            EvidenceKind::Attested { generation: 1 },
            EvidenceKind::ProvenRotation { generation: 2 },
        ] {
            assert_eq!(change_outcome(A, A, evidence, 1, true), UNCHANGED);
            assert_eq!(change_outcome(A, A, evidence, 1, false), UNCHANGED);
        }
    }

    #[test]
    fn an_unattested_unproven_change_is_refused_even_outside_the_cooldown() {
        // The retired ADR-0017 heal: no previous-key exception any more.
        assert_eq!(
            change_outcome(A, B, EvidenceKind::None, 1, false),
            ChangeOutcome::NeedsAttestation
        );
        assert_eq!(
            change_outcome(A, B, EvidenceKind::None, 1, true),
            ChangeOutcome::NeedsAttestation
        );
    }

    #[test]
    fn a_proven_rotation_is_never_gated_and_never_a_reset() {
        assert_eq!(
            change_outcome(A, B, EvidenceKind::Proof, 1, true),
            ChangeOutcome::Apply {
                key_changed: true,
                attested_generation: None,
                proven: true,
            }
        );
    }

    #[test]
    fn a_proven_rotation_at_a_new_generation_ignores_the_cooldown_and_needs_a_newer_generation() {
        for within_cooldown in [false, true] {
            assert_eq!(
                change_outcome(
                    A,
                    B,
                    EvidenceKind::ProvenRotation { generation: 2 },
                    1,
                    within_cooldown
                ),
                ChangeOutcome::Apply {
                    key_changed: true,
                    attested_generation: Some(2),
                    proven: true,
                }
            );
            for stored in [2, 3] {
                assert_eq!(
                    change_outcome(
                        A,
                        B,
                        EvidenceKind::ProvenRotation { generation: 2 },
                        stored,
                        within_cooldown
                    ),
                    ChangeOutcome::NotNewer
                );
            }
        }
    }

    #[test]
    fn an_attested_change_needs_a_strictly_greater_generation() {
        assert_eq!(
            change_outcome(A, B, EvidenceKind::Attested { generation: 2 }, 1, false),
            ChangeOutcome::Apply {
                key_changed: true,
                attested_generation: Some(2),
                proven: false,
            }
        );
        assert_eq!(
            change_outcome(A, B, EvidenceKind::Attested { generation: 2 }, 2, false),
            ChangeOutcome::NotNewer
        );
        assert_eq!(
            change_outcome(A, B, EvidenceKind::Attested { generation: 2 }, 3, false),
            ChangeOutcome::NotNewer
        );
    }

    #[test]
    fn the_generation_check_precedes_the_cooldown() {
        assert_eq!(
            change_outcome(A, B, EvidenceKind::Attested { generation: 1 }, 1, true),
            ChangeOutcome::NotNewer
        );
        assert_eq!(
            change_outcome(A, B, EvidenceKind::Attested { generation: 2 }, 1, true),
            ChangeOutcome::CoolingDown
        );
    }
}

#[cfg(test)]
mod log_append_decision_tests {
    use super::{
        change_outcome, log_append_decision, ChangeOutcome, EvidenceKind, LogAppendDecision,
    };

    const A: &[u8] = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const B: &[u8] = b"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

    /// Fold an `Apply` outcome through the KT-log S2 (decision D) mapping,
    /// the way `try_apply_identity_once` does for an existing row.
    fn decide(current: &[u8], new_sig: &[u8], evidence: EvidenceKind) -> LogAppendDecision {
        match change_outcome(current, new_sig, evidence, 1, false) {
            ChangeOutcome::Apply {
                key_changed,
                attested_generation,
                proven,
            } => log_append_decision(key_changed, attested_generation.is_some() && !proven),
            other => panic!("test fed a refused case to the decision mapping: {other:?}"),
        }
    }

    #[test]
    fn first_publish_appends_non_reset() {
        assert_eq!(
            log_append_decision(true, false),
            LogAppendDecision::Append { is_reset: false }
        );
    }

    #[test]
    fn same_key_reupload_skips() {
        assert_eq!(decide(A, A, EvidenceKind::None), LogAppendDecision::Skip);
        assert_eq!(
            decide(A, A, EvidenceKind::Attested { generation: 5 }),
            LogAppendDecision::Skip
        );
    }

    #[test]
    fn proven_rotation_appends_non_reset() {
        assert_eq!(
            decide(A, B, EvidenceKind::Proof),
            LogAppendDecision::Append { is_reset: false }
        );
    }

    #[test]
    fn a_proven_rotation_with_an_attestation_is_not_a_reset_in_the_log() {
        assert_eq!(
            decide(A, B, EvidenceKind::ProvenRotation { generation: 2 }),
            LogAppendDecision::Append { is_reset: false }
        );
    }

    #[test]
    fn attested_reset_appends_reset() {
        assert_eq!(
            decide(A, B, EvidenceKind::Attested { generation: 2 }),
            LogAppendDecision::Append { is_reset: true }
        );
    }
}

// ============================================================================
// GET /discovery/identity/{account_id_b64url}   — fetch
// ============================================================================

/// Public response shape. Keys are base64-encoded with the
/// standard alphabet for parity with the rest of the service's
/// wire format.
#[derive(Serialize)]
pub(crate) struct FetchResponse {
    pub(crate) signature_pubkey: String,
    pub(crate) x25519_pubkey: String,
    /// G-4 (ADR-0016): base64 of the stored supersession proof, when one is
    /// present (i.e. the account rotated recently). Omitted from the JSON
    /// when absent. Opaque server bytes — the peer verifies it locally.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) supersession_proof: Option<String>,
    /// ADR-0073: the key generation the published key belongs to (1 =
    /// genesis; an attested reset stores its strictly greater value), so a
    /// client can compare the published generation with its own token's.
    pub(crate) key_generation: u64,
    /// ADR-0073 R.3: standard base64 of the stored key-reset attestation
    /// (the latest attested reset's), omitted from the JSON when the account
    /// never reset. The stored bytes unparsed: discovery verified them at
    /// upload, and at fetch they are an opaque pass-through like
    /// `supersession_proof` (invariant #2). The peer verifies them locally
    /// against the pinned attest key, the account id and the fetched key;
    /// after a later PROVEN rotation the published key no longer equals the
    /// attested key, and that case carries a supersession proof instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) key_reset_attest: Option<String>,
}

/// `GET /discovery/identity/{account_id_b64url}` — fetch.
///
/// No auth — identity public keys are public by definition. Same
/// rate-limit story as the rest of the discovery service:
/// per-IP `limit_req` at the nginx layer in production.
///
/// Returns **404 Not Found** when the account has no bundle
/// registered. The path encodes `account_id` as
/// **base64url-without-padding** (URL-safe alphabet) — the more
/// common standard alphabet uses `+` and `/`, which axum would
/// reject in a path segment.
pub(crate) async fn fetch(
    State(state): State<AppState>,
    Path(account_id_b64url): Path<String>,
) -> Result<Json<FetchResponse>, (StatusCode, String)> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;

    let bytes = B64URL.decode(&account_id_b64url).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid account_id base64url: {e}"),
        )
    })?;
    if bytes.len() != ACCOUNT_ID_LEN {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("account_id must be {ACCOUNT_ID_LEN} bytes after decode"),
        ));
    }

    let row: Option<IdentityBundleRow> = sqlx::query_as(
        "SELECT signature_pubkey, x25519_pubkey, supersession_proof, key_generation, \
                key_reset_attest \
         FROM identity_bundles WHERE account_id = ?",
    )
    .bind(&bytes[..])
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "fetch identity_bundles");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    let (sig, x25519, proof, key_generation, key_reset_attest) = row.ok_or((
        StatusCode::NOT_FOUND,
        "identity bundle not found".to_string(),
    ))?;

    if sig.len() != SIGNATURE_PUBKEY_LEN || x25519.len() != X25519_PUBKEY_LEN {
        // Schema enforces fixed lengths; a row that doesn't match
        // is corruption, not a client error.
        tracing::error!(
            sig_len = sig.len(),
            x25519_len = x25519.len(),
            "identity_bundles row has unexpected key lengths",
        );
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "stored bundle has unexpected key lengths".to_string(),
        ));
    }

    Ok(Json(FetchResponse {
        signature_pubkey: B64.encode(&sig),
        x25519_pubkey: B64.encode(&x25519),
        // Opaque pass-through — the server re-encodes the stored bytes
        // without interpreting them; the peer verifies the proof locally.
        supersession_proof: proof.map(|p| B64.encode(&p)),
        key_generation,
        // The same opaque pass-through (ADR-0073 R.3).
        key_reset_attest: key_reset_attest.map(|a| B64.encode(&a)),
    }))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_fixed_validates_lengths() {
        // Right length.
        let ok = B64.encode([0u8; 32]);
        let bytes = decode_fixed::<32>(&ok, "x").unwrap();
        assert_eq!(bytes.len(), 32);

        // Too short.
        let short = B64.encode([0u8; 16]);
        let err = decode_fixed::<32>(&short, "x").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Too long.
        let long = B64.encode([0u8; 64]);
        let err = decode_fixed::<32>(&long, "x").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Non-base64.
        let err = decode_fixed::<32>("not-base64!!!", "x").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn decode_optional_proof_handles_absent_present_and_oversized() {
        // Absent -> None (the upsert then PRESERVES any stored proof).
        assert!(decode_optional_proof(None).unwrap().is_none());

        // Present, within the cap (the real proof is 145 bytes) -> decoded.
        let proof = B64.encode([7u8; 145]);
        let decoded = decode_optional_proof(Some(&proof)).unwrap().unwrap();
        assert_eq!(decoded.len(), 145);
        assert!(decoded.iter().all(|&b| b == 7));

        // Boundary: exactly the cap is accepted.
        let at_cap = B64.encode([0u8; MAX_SUPERSESSION_PROOF_LEN]);
        assert_eq!(
            decode_optional_proof(Some(&at_cap)).unwrap().unwrap().len(),
            MAX_SUPERSESSION_PROOF_LEN
        );

        // Over the cap -> 400 (bounds storage; not a crypto check).
        let oversized = B64.encode([0u8; MAX_SUPERSESSION_PROOF_LEN + 1]);
        let err = decode_optional_proof(Some(&oversized)).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Non-base64 -> 400.
        let err = decode_optional_proof(Some("not-base64!!!")).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn decode_optional_leaf_auth_handles_absent_present_and_oversized() {
        // Absent -> None (the upsert then PRESERVES any stored leaf_auth).
        assert!(decode_optional_leaf_auth(None).unwrap().is_none());

        // Present, a 64-byte genesis self-sig -> decoded.
        let genesis = B64.encode([5u8; 64]);
        let decoded = decode_optional_leaf_auth(Some(&genesis)).unwrap().unwrap();
        assert_eq!(decoded.len(), 64);
        assert!(decoded.iter().all(|&b| b == 5));

        // Present, a 145-byte supersession proof (the widest real value) ->
        // decoded.
        let proof = B64.encode([7u8; 145]);
        assert_eq!(
            decode_optional_leaf_auth(Some(&proof))
                .unwrap()
                .unwrap()
                .len(),
            145
        );

        // Boundary: exactly the cap is accepted.
        let at_cap = B64.encode([0u8; MAX_LEAF_AUTH_LEN]);
        assert_eq!(
            decode_optional_leaf_auth(Some(&at_cap))
                .unwrap()
                .unwrap()
                .len(),
            MAX_LEAF_AUTH_LEN
        );

        // Over the cap -> 400 (bounds storage; not a crypto check —
        // discovery never parses leaf_auth, invariant #2).
        let oversized = B64.encode([0u8; MAX_LEAF_AUTH_LEN + 1]);
        let err = decode_optional_leaf_auth(Some(&oversized)).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Non-base64 -> 400.
        let err = decode_optional_leaf_auth(Some("not-base64!!!")).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn decode_optional_genesis_attest_handles_absent_present_and_oversized() {
        // Absent -> None (the upsert then PRESERVES any stored genesis_attest).
        assert!(decode_optional_genesis_attest(None).unwrap().is_none());

        // Present, the 153-byte GenesisAttestation -> decoded.
        let att = B64.encode([9u8; 153]);
        let decoded = decode_optional_genesis_attest(Some(&att)).unwrap().unwrap();
        assert_eq!(decoded.len(), 153);
        assert!(decoded.iter().all(|&b| b == 9));

        // Boundary: exactly the cap is accepted.
        let at_cap = B64.encode([0u8; MAX_GENESIS_ATTEST_LEN]);
        assert_eq!(
            decode_optional_genesis_attest(Some(&at_cap))
                .unwrap()
                .unwrap()
                .len(),
            MAX_GENESIS_ATTEST_LEN
        );

        // Over the cap -> 400 (bounds storage; not a crypto check —
        // discovery never parses genesis_attest, invariant #2).
        let oversized = B64.encode([0u8; MAX_GENESIS_ATTEST_LEN + 1]);
        let err = decode_optional_genesis_attest(Some(&oversized)).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);

        // Non-base64 -> 400.
        let err = decode_optional_genesis_attest(Some("not-base64!!!")).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn fetch_path_validates_base64url() {
        // The fetch handler decodes its `Path<String>` via
        // base64url-without-padding because axum rejects `/` and
        // `+` in path segments. Verify the alphabet round-trips
        // here so the contract surfaces in tests rather than
        // showing up as a 400 in production.
        use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
        let id = [0xABu8; ACCOUNT_ID_LEN];
        let encoded = B64URL.encode(id);
        // Standard-alphabet artefacts (`+`, `/`, `=`) must NOT appear.
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
        let decoded = B64URL.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), ACCOUNT_ID_LEN);
        assert_eq!(decoded.as_slice(), &id[..]);
    }

    /// ADR-0036: the genesis attestation is STORED on a first publish and
    /// PRESERVED across a non-rotation re-upload (the upsert `COALESCE`), then
    /// REPLACED when a fresh value is supplied — exactly `leaf_auth`'s
    /// treatment. Driven through the production `try_apply_identity_once` so it
    /// pins the real INSERT + UPDATE SQL, not a hand-rolled query. DB-gated on
    /// `LAKKO_DISCOVERY_TEST_DATABASE_URL` (skips, does not fail, when unset).
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn genesis_attest_stored_preserved_and_replaced() {
        use sqlx::mysql::MySqlPoolOptions;

        // A generous cool-down: the first publish never sets the marker, so a
        // later attested new key is still not gated (verified below).
        const COOLDOWN: i64 = 3600;
        const NO_EVIDENCE: Evidence<'static> = Evidence {
            supersession_proof: None,
            key_reset_attest: None,
        };

        // Read the stored genesis_attest back (nullable column -> Option).
        async fn stored_attest(pool: &sqlx::MySqlPool, acct: &[u8]) -> Option<Vec<u8>> {
            sqlx::query_scalar("SELECT genesis_attest FROM identity_bundles WHERE account_id = ?")
                .bind(acct)
                .fetch_one(pool)
                .await
                .expect("read genesis_attest")
        }

        let Ok(url) = std::env::var("LAKKO_DISCOVERY_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping genesis_attest_stored_preserved_and_replaced: \
                 LAKKO_DISCOVERY_TEST_DATABASE_URL unset"
            );
            return;
        };

        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .expect("connect test DB");
        crate::anon_token_route_tests::migrate_once(&pool).await;

        // An isolated account so the test never collides with other rows.
        let acct = [0xB0u8; ACCOUNT_ID_LEN];
        let key1 = [0x11u8; SIGNATURE_PUBKEY_LEN];
        let key2 = [0x22u8; SIGNATURE_PUBKEY_LEN];
        let x25519 = [0x33u8; X25519_PUBKEY_LEN];
        let self_sig = [0x44u8; 64]; // a genesis leaf_auth self-sig
        let att1 = [0x55u8; 153]; // the first GenesisAttestation
        let att2 = [0x66u8; 153]; // a replacement attestation

        sqlx::query("DELETE FROM identity_bundles WHERE account_id = ?")
            .bind(&acct[..])
            .execute(&pool)
            .await
            .expect("clear pre-existing row");

        // 1. First publish WITH a genesis attestation -> Append + stored.
        let first = try_apply_identity_once(
            &pool,
            COOLDOWN,
            &acct,
            &key1,
            &x25519,
            NO_EVIDENCE,
            Some(&self_sig),
            Some(&att1),
            1,
        )
        .await
        .unwrap_or_else(|_| panic!("first publish"));
        assert_eq!(first, LogAppendDecision::Append { is_reset: false });
        assert_eq!(
            stored_attest(&pool, &acct).await.as_deref(),
            Some(&att1[..]),
            "genesis_attest stored on first publish"
        );

        // 2. Non-rotation re-upload (SAME key, NO genesis_attest) -> Skip, and
        //    the stored attestation is PRESERVED (COALESCE), exactly like
        //    leaf_auth.
        let again = try_apply_identity_once(
            &pool,
            COOLDOWN,
            &acct,
            &key1,
            &x25519,
            NO_EVIDENCE,
            None,
            None,
            1,
        )
        .await
        .unwrap_or_else(|_| panic!("non-rotation re-upload"));
        assert_eq!(again, LogAppendDecision::Skip);
        assert_eq!(
            stored_attest(&pool, &acct).await.as_deref(),
            Some(&att1[..]),
            "genesis_attest PRESERVED across a non-rotation re-upload (COALESCE)"
        );

        // 3. An attested key reset (ADR-0073: the only unproven key change the
        //    directory accepts) WITH a new genesis attestation -> Append
        //    {is_reset} and the stored attestation is REPLACED. (Not gated:
        //    the first publish never armed the cool-down marker.)
        let reset_att = [0x77u8; 177]; // bytes already verified by the handler
        let reset = try_apply_identity_once(
            &pool,
            COOLDOWN,
            &acct,
            &key2,
            &x25519,
            Evidence {
                supersession_proof: None,
                key_reset_attest: Some((2, &reset_att)),
            },
            Some(&self_sig),
            Some(&att2),
            2,
        )
        .await
        .unwrap_or_else(|_| panic!("reset with a fresh attestation"));
        assert_eq!(reset, LogAppendDecision::Append { is_reset: true });
        assert_eq!(
            stored_attest(&pool, &acct).await.as_deref(),
            Some(&att2[..]),
            "genesis_attest REPLACED when a fresh value is supplied"
        );

        sqlx::query("DELETE FROM identity_bundles WHERE account_id = ?")
            .bind(&acct[..])
            .execute(&pool)
            .await
            .expect("cleanup row");
        pool.close().await;
    }
}
