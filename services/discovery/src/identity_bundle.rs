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
//! ## Known Phase-1 trust gap
//!
//! `POST /discovery/identity` currently TRUSTS the client's
//! claim of `account_id`, same as `POST /discovery/register`.
//! A PoW token is required, but no proof of ownership beyond
//! that. Phase 1.5b will gate this endpoint on an auth-issued
//! session token; until then, the squatting risk is identical
//! to `/discovery/register` and recoverable by table reset
//! before beta launch.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

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

/// A fetched `identity_bundles` row: `(signature_pubkey, x25519_pubkey,
/// supersession_proof?)` — the proof column is nullable (G-4, ADR-0016).
type IdentityBundleRow = (Vec<u8>, Vec<u8>, Option<Vec<u8>>);

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
/// — the server does NOT parse or verify the bytes (invariant #2); that is the
/// receiving peer's job via `verify_supersession`.
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

// ============================================================================
// POST /discovery/identity   — upsert
// ============================================================================

/// Upload (or replace) the identity bundle for the claimed
/// `account_id`. PoW-gated, claim-trusting (see file-level
/// docstring for the Phase-1 caveat).
#[derive(Deserialize)]
pub(crate) struct UploadRequest {
    pub(crate) pow: crate::pow::PowProof,
    /// Base64 of the raw 16-byte UUIDv7.
    pub(crate) account_id: String,
    /// Base64 of the 32-byte Ed25519 public key.
    pub(crate) signature_pubkey: String,
    /// Base64 of the 32-byte X25519 public key (HPKE init_key).
    pub(crate) x25519_pubkey: String,
    /// G-4 (ADR-0016): base64 of an optional old-key-signed supersession
    /// proof bridging this rotation. Absent for a normal (non-rotation)
    /// upload, which clears any previously stored proof. The server stores
    /// it opaque and never parses it (invariant #2).
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
pub(crate) async fn upload(
    State(state): State<AppState>,
    axum::Extension(authed): axum::Extension<crate::auth::AuthenticatedAccount>,
    Json(payload): Json<UploadRequest>,
) -> Result<Json<UploadResponse>, (StatusCode, String)> {
    crate::pow::verify_proof(&state.db, &payload.pow).await?;

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

    // Phase 1.5b anti-impersonation: only the account_id's owner
    // can upload its identity bundle. Same check shape as
    // `registry::register`.
    if account_id != authed.0 {
        tracing::warn!(
            body_account_id = %B64.encode(account_id),
            token_account_id = %B64.encode(authed.0),
            "identity_bundle upload: body account_id does not match token's"
        );
        return Err((
            StatusCode::FORBIDDEN,
            "account_id mismatch between body and session token".to_string(),
        ));
    }

    // ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (ii)) —
    // the upsert is gated by a per-account cool-down on UNPROVEN key changes
    // (see `apply_identity_upload_gated`); a fresh-publish flood gets 429.
    let decision = apply_identity_upload_gated(
        &state.db,
        state.fresh_identity_cooldown_secs,
        &account_id,
        &signature_pubkey,
        &x25519_pubkey,
        supersession_proof.as_deref(),
        leaf_auth.as_deref(),
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
        let confirmed = client
            .append_key_change(
                &account_id,
                &signature_pubkey,
                is_reset,
                leaf_auth.as_deref(),
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

/// Map a sqlx error from the cool-down transaction to a 500.
fn cooldown_db_err(e: sqlx::Error) -> (StatusCode, String) {
    tracing::error!(error = %e, "identity_bundles cool-down tx");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal db error".to_string(),
    )
}

/// `true` when a sqlx error is a unique-key (duplicate) violation — used to
/// detect a concurrent first-publish INSERT race.
fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.is_unique_violation())
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
    /// `is_reset` flag is the unproven-new-key marker (decision F ==
    /// `bump_marker`): `false` for a first publish, a proven rotation, or
    /// a heal-back to a previously-seen key; `true` only for an unproven
    /// change to a brand-new key (the lost-everything reset).
    Append {
        /// ADR-0017 fresh-start marker (== `cooldown_outcome`'s
        /// `bump_marker`).
        is_reset: bool,
    },
}

/// One attempt's outcome (see [`apply_identity_upload_gated`]).
enum ApplyAttempt {
    /// The upsert committed; carries whether the key changed (and thus
    /// whether the transparency log must be appended).
    Done(LogAppendDecision),
    /// A concurrent FIRST publish for the same account won the INSERT race;
    /// the row now exists, so the caller should retry into the UPDATE path.
    RetryRace,
}

/// Apply the identity-bundle upsert under the ADR-0017 (ii) fresh-identity
/// cool-down, retrying the (rare) concurrent-first-publish INSERT race so the
/// loser takes the cool-down-checked UPDATE path instead of surfacing a 500.
/// Contention on a single account is rare (only the account's own concurrent
/// devices), so a small bounded retry suffices.
///
/// On success returns the [`LogAppendDecision`] (KT-log S2 decision D): the
/// caller appends a transparency-log leaf iff the published key CHANGED. The
/// gated-429 case still returns `Err`.
async fn apply_identity_upload_gated(
    db: &sqlx::MySqlPool,
    cooldown_secs: i64,
    account_id: &[u8],
    signature_pubkey: &[u8],
    x25519_pubkey: &[u8],
    supersession_proof: Option<&[u8]>,
    leaf_auth: Option<&[u8]>,
) -> Result<LogAppendDecision, (StatusCode, String)> {
    const MAX_ATTEMPTS: u32 = 3;
    for _ in 0..MAX_ATTEMPTS {
        match try_apply_identity_once(
            db,
            cooldown_secs,
            account_id,
            signature_pubkey,
            x25519_pubkey,
            supersession_proof,
            leaf_auth,
        )
        .await?
        {
            ApplyAttempt::Done(decision) => return Ok(decision),
            ApplyAttempt::RetryRace => {},
        }
    }
    // Exhausted retries — extreme single-account contention. Transient, so the
    // client can safely retry.
    Err((
        StatusCode::SERVICE_UNAVAILABLE,
        "identity upsert contended — retry".to_string(),
    ))
}

/// A single attempt of the gated upsert. Read-compare-conditional-write under
/// a row lock so the check is race-free. NEVER gates: a no-change re-upload, a proven rotation (a
/// supersession proof — sanctioned, ADR-0016), or a HEAL back to the
/// immediately-previous key (the owner's warm-login re-publish after an
/// attacker replaced their key — so the cool-down can never lock a
/// legitimate owner out of restoring their identity). The gated case is a
/// CHANGE to a brand-new key with NO proof while still cooling down → 429.
///
/// **Trust model (lakko-security 3A (ii)):** the server treats the *presence*
/// of a supersession proof as "proven rotation, not gated" — it does NOT and
/// MUST NOT verify the proof bytes (invariant #2 — the server stays opaque).
/// So a compromised-ACCESS attacker can attach an arbitrary present-but-invalid
/// proof to skip the cool-down. This does NOT defeat the feature: the
/// load-bearing integrity gate is the *client-side* fail-closed verify (a peer
/// fetching a changed pin with an unverifiable proof REFUSES, ADR-0016), which
/// this bypass cannot touch. The cool-down is defence-in-depth against
/// directory churn / squatting / row growth, not the integrity primitive.
/// (The only "free" unproven move is healing to `previous`, so the reachable
/// identity set within one window is at most 2 keys — a flip-flop, never N-key
/// churn.)
async fn try_apply_identity_once(
    db: &sqlx::MySqlPool,
    cooldown_secs: i64,
    account_id: &[u8],
    signature_pubkey: &[u8],
    x25519_pubkey: &[u8],
    supersession_proof: Option<&[u8]>,
    leaf_auth: Option<&[u8]>,
) -> Result<ApplyAttempt, (StatusCode, String)> {
    let mut tx = db.begin().await.map_err(cooldown_db_err)?;

    // Row lock + the three facts we need: the current key, the
    // immediately-previous key, and whether the last unproven NEW-key
    // change is still within the cool-down window (computed server-side to
    // avoid any Rust/DB clock skew; a `0`-second window never matches,
    // since the elapsed seconds are always `>= 0`).
    let existing: Option<(Vec<u8>, Option<Vec<u8>>, i64)> = sqlx::query_as(
        "SELECT signature_pubkey, previous_signature_pubkey, \
            CAST( \
                last_unproven_key_change_at IS NOT NULL \
                AND TIMESTAMPDIFF(SECOND, last_unproven_key_change_at, NOW(3)) < ? \
            AS SIGNED) \
         FROM identity_bundles WHERE account_id = ? FOR UPDATE",
    )
    .bind(cooldown_secs)
    .bind(account_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(cooldown_db_err)?;

    let decision: LogAppendDecision = match existing {
        None => {
            // First publish for this account — nothing to gate.
            let inserted = sqlx::query(
                "INSERT INTO identity_bundles \
                    (account_id, signature_pubkey, x25519_pubkey, supersession_proof, leaf_auth) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(account_id)
            .bind(signature_pubkey)
            .bind(x25519_pubkey)
            .bind(supersession_proof)
            .bind(leaf_auth)
            .execute(&mut *tx)
            .await;
            match inserted {
                // A first publish is a key change from "nothing" to a key,
                // and is never a fresh-start reset → Append{is_reset:false}
                // (decision D). Routed through the same pure mapping as the
                // existing-row case for a single source of truth.
                Ok(_) => log_append_decision(true, false),
                Err(e) if is_unique_violation(&e) => {
                    // A concurrent first publish won the INSERT race — the row
                    // now exists; abandon this tx and retry into the UPDATE
                    // path (which runs the cool-down check).
                    let _ = tx.rollback().await;
                    return Ok(ApplyAttempt::RetryRace);
                },
                Err(e) => return Err(cooldown_db_err(e)),
            }
        },
        Some((current, previous, within_cooldown)) => {
            let outcome = cooldown_outcome(
                &current,
                previous.as_deref(),
                within_cooldown != 0,
                signature_pubkey,
                supersession_proof.is_some(),
            );

            let CooldownOutcome::Apply {
                previous_becomes_current,
                bump_marker,
            } = outcome
            else {
                // Gated: a CHANGE to a brand-new key, no proof, not a heal,
                // still cooling down.
                tx.rollback().await.map_err(cooldown_db_err)?;
                tracing::warn!(
                    account_id = %B64.encode(account_id),
                    "identity fresh-publish refused — cool-down active",
                );
                return Err((
                    StatusCode::TOO_MANY_REQUESTS,
                    "identity changed too recently — try again later".to_string(),
                ));
            };

            // On any key change, the OLD current becomes `previous`;
            // otherwise keep the stored `previous`. Bump the cool-down
            // marker ONLY for an unproven change to a new key (the churn
            // surface); heals + proven rotations leave it untouched.
            let new_previous: Option<&[u8]> = if previous_becomes_current {
                Some(current.as_slice())
            } else {
                previous.as_deref()
            };
            let bump_marker_i: i64 = i64::from(bump_marker);

            sqlx::query(
                "UPDATE identity_bundles SET \
                    signature_pubkey            = ?, \
                    x25519_pubkey               = ?, \
                    supersession_proof          = COALESCE(?, supersession_proof), \
                    leaf_auth                   = COALESCE(?, leaf_auth), \
                    previous_signature_pubkey   = ?, \
                    last_unproven_key_change_at = IF(?, NOW(3), last_unproven_key_change_at) \
                 WHERE account_id = ?",
            )
            .bind(signature_pubkey)
            .bind(x25519_pubkey)
            .bind(supersession_proof)
            .bind(leaf_auth)
            .bind(new_previous)
            .bind(bump_marker_i)
            .bind(account_id)
            .execute(&mut *tx)
            .await
            .map_err(cooldown_db_err)?;

            // Decision D mapping for the existing-row case (pure, so it is
            // unit-testable — see `log_append_decision`).
            log_append_decision(previous_becomes_current, bump_marker)
        },
    };

    tx.commit().await.map_err(cooldown_db_err)?;
    Ok(ApplyAttempt::Done(decision))
}

/// The ADR-0017 (ii) cool-down decision for an upload against an existing
/// row. `Reject` = a CHANGE to a brand-new key, with no proof, that is not
/// a heal-back, while still cooling down. `Apply` carries the two writes the
/// caller must perform.
#[derive(Debug, PartialEq, Eq)]
enum CooldownOutcome {
    /// Refuse — 429 (still cooling down on a new-key churn).
    Reject,
    /// Proceed with the upsert.
    Apply {
        /// The OLD current key becomes `previous` (true on any key change).
        previous_becomes_current: bool,
        /// Bump `last_unproven_key_change_at` to now (true only for an
        /// unproven change to a brand-new key — the churn surface).
        bump_marker: bool,
    },
}

/// Pure KT-log S2 (decision D) mapping for the EXISTING-row case: given
/// the cool-down outcome's two booleans, decide whether discovery must
/// append a transparency-log leaf.
///
/// - `key_changed` (== `cooldown_outcome`'s `previous_becomes_current`) is
///   exactly "the published key changed". `false` → [`LogAppendDecision::Skip`]
///   (a no-change re-upload — no append).
/// - On a change, `unproven_new_key` (== `bump_marker`) is exactly the
///   ADR-0017 fresh-start reset marker (decision F == `is_reset`). A proven
///   rotation or a heal-back to a previously-seen key changes the key but is
///   NOT a reset → `Append { is_reset: false }`.
const fn log_append_decision(key_changed: bool, unproven_new_key: bool) -> LogAppendDecision {
    if key_changed {
        LogAppendDecision::Append {
            is_reset: unproven_new_key,
        }
    } else {
        LogAppendDecision::Skip
    }
}

/// Pure decision matrix for the fresh-identity cool-down (no I/O so it is
/// unit-testable). NEVER rejects a no-change re-upload, a proven rotation,
/// or a HEAL back to the immediately-previous key.
fn cooldown_outcome(
    current: &[u8],
    previous: Option<&[u8]>,
    within_cooldown: bool,
    new_sig: &[u8],
    has_proof: bool,
) -> CooldownOutcome {
    let key_changed = current != new_sig;
    let is_heal = previous == Some(new_sig);
    let unproven_new_key = key_changed && !has_proof && !is_heal;

    if unproven_new_key && within_cooldown {
        CooldownOutcome::Reject
    } else {
        CooldownOutcome::Apply {
            previous_becomes_current: key_changed,
            bump_marker: unproven_new_key,
        }
    }
}

#[cfg(test)]
mod cooldown_tests {
    use super::{cooldown_outcome, CooldownOutcome};

    const A: &[u8] = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const B: &[u8] = b"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
    const C: &[u8] = b"CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";

    #[test]
    fn no_change_never_gated_and_never_bumps() {
        // Re-upload of the SAME key, even within the cool-down → Apply, no
        // previous swap, no marker bump.
        assert_eq!(
            cooldown_outcome(A, Some(B), true, A, false),
            CooldownOutcome::Apply {
                previous_becomes_current: false,
                bump_marker: false,
            }
        );
    }

    #[test]
    fn proven_rotation_never_gated_and_never_bumps() {
        // Key change WITH a proof, even within the cool-down → Apply, swap
        // previous, but do NOT bump the marker (proven rotations are not
        // rate-limited).
        assert_eq!(
            cooldown_outcome(A, None, true, B, true),
            CooldownOutcome::Apply {
                previous_becomes_current: true,
                bump_marker: false,
            }
        );
    }

    #[test]
    fn first_unproven_change_allowed_and_bumps() {
        // Unproven change to a new key while NOT cooling down → Apply +
        // bump the marker (arms the cool-down for the next one).
        assert_eq!(
            cooldown_outcome(A, None, false, B, false),
            CooldownOutcome::Apply {
                previous_becomes_current: true,
                bump_marker: true,
            }
        );
    }

    #[test]
    fn second_unproven_new_key_within_cooldown_rejected() {
        // The churn case: a DIFFERENT brand-new key, no proof, still
        // cooling down → Reject.
        assert_eq!(
            cooldown_outcome(B, Some(A), true, C, false),
            CooldownOutcome::Reject
        );
    }

    #[test]
    fn heal_back_to_previous_allowed_within_cooldown() {
        // The owner's warm-login re-publish after an attacker replaced the
        // key: current=B (attacker), previous=A (owner), new=A. Even within
        // the cool-down this is a HEAL → Apply, never Reject.
        assert_eq!(
            cooldown_outcome(B, Some(A), true, A, false),
            CooldownOutcome::Apply {
                previous_becomes_current: true,
                bump_marker: false,
            }
        );
    }

    #[test]
    fn unproven_new_key_outside_cooldown_allowed() {
        // A new key, no proof, but the cool-down has elapsed → Apply + bump.
        assert_eq!(
            cooldown_outcome(B, Some(A), false, C, false),
            CooldownOutcome::Apply {
                previous_becomes_current: true,
                bump_marker: true,
            }
        );
    }
}

#[cfg(test)]
mod log_append_decision_tests {
    use super::{cooldown_outcome, log_append_decision, CooldownOutcome, LogAppendDecision};

    const A: &[u8] = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const B: &[u8] = b"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
    const C: &[u8] = b"CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";

    /// Fold a cool-down `Apply` outcome through the KT-log S2 (decision D)
    /// mapping, the way `try_apply_identity_once` does for an existing row.
    /// A gated `Reject` can never reach the mapping (it returns 429 before),
    /// so this helper only handles the `Apply` arm.
    fn decide(
        current: &[u8],
        previous: Option<&[u8]>,
        within_cooldown: bool,
        new_sig: &[u8],
        has_proof: bool,
    ) -> LogAppendDecision {
        match cooldown_outcome(current, previous, within_cooldown, new_sig, has_proof) {
            CooldownOutcome::Apply {
                previous_becomes_current,
                bump_marker,
            } => log_append_decision(previous_becomes_current, bump_marker),
            CooldownOutcome::Reject => panic!("test fed a gated case to the decision mapping"),
        }
    }

    #[test]
    fn first_publish_appends_non_reset() {
        // The `None` (first-publish) branch in `try_apply_identity_once`
        // routes through `log_append_decision(true, false)` — a key change
        // from nothing, never a reset.
        assert_eq!(
            log_append_decision(true, false),
            LogAppendDecision::Append { is_reset: false }
        );
    }

    #[test]
    fn same_key_reupload_skips() {
        // No key change → Skip (no transparency append).
        assert_eq!(decide(A, Some(B), false, A, false), LogAppendDecision::Skip);
        // Even within the cool-down, a same-key re-upload still Skips.
        assert_eq!(decide(A, Some(B), true, A, false), LogAppendDecision::Skip);
    }

    #[test]
    fn proven_rotation_appends_non_reset() {
        // Key change WITH a supersession proof → Append, NOT a reset.
        assert_eq!(
            decide(A, None, false, B, true),
            LogAppendDecision::Append { is_reset: false }
        );
    }

    #[test]
    fn unproven_new_key_appends_reset() {
        // Key change to a brand-new key, no proof, cool-down elapsed → the
        // lost-everything case → Append{is_reset:true}.
        assert_eq!(
            decide(B, Some(A), false, C, false),
            LogAppendDecision::Append { is_reset: true }
        );
    }

    #[test]
    fn heal_back_appends_non_reset() {
        // A heal IS a key change (back to a previously-seen key) → it must be
        // LOGGED as a real identity version, but it is NOT a fresh-start
        // reset → Append{is_reset:false}.
        assert_eq!(
            decide(B, Some(A), true, A, false),
            LogAppendDecision::Append { is_reset: false }
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
        "SELECT signature_pubkey, x25519_pubkey, supersession_proof FROM identity_bundles \
         WHERE account_id = ?",
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

    let (sig, x25519, proof) = row.ok_or((
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
            decode_optional_leaf_auth(Some(&proof)).unwrap().unwrap().len(),
            145
        );

        // Boundary: exactly the cap is accepted.
        let at_cap = B64.encode([0u8; MAX_LEAF_AUTH_LEN]);
        assert_eq!(
            decode_optional_leaf_auth(Some(&at_cap)).unwrap().unwrap().len(),
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
}
