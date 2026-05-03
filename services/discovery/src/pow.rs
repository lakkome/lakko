// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Equix proof-of-work for anti-spam.
//!
//! Mirrored verbatim from `services/auth/src/pow.rs`. Phase 2 should
//! DRY this into a shared `lakko-pow` workspace crate; for now the
//! duplication is intentional — the two services have isolated
//! databases (per the per-service DB rule) and therefore independent
//! `pow_challenges` tables.
//!
//! Phase 1.5 gates the expensive discovery endpoints
//! (`/discovery/oprf`, `/discovery/register`, `/discovery/lookup`)
//! with an Equix PoW. The client:
//!
//!   1. Calls `POST /pow/challenge` to obtain a fresh `seed` (16 random
//!      bytes), an `effort_bits` knob, and a TTL.
//!   2. Picks a `nonce` (16 bytes) and runs `equix::solve(seed || nonce)`.
//!      For ~5% of seeds Equix fails to find a solution; in that case
//!      the client increments the nonce and retries.
//!   3. Once a solution is found, computes
//!      `SHA-256(seed || nonce || solution)` and checks it has at least
//!      `effort_bits` leading zero bits. If not, increments the nonce
//!      and retries — this is what makes the PoW *adjustable*.
//!   4. Submits `(seed, nonce, solution)` alongside the actual request
//!      (e.g. `/signup/start`). The handler verifies and consumes the
//!      challenge before doing any expensive crypto.
//!
//! Equix verify is fast (microseconds). The seed row is deleted on the
//! first verification attempt — successful or failed — so a stolen
//! proof cannot be replayed. A failed attempt costs the client a fresh
//! `/pow/challenge` round-trip.
//!
//! Effort knob: env var `LAKKO_DISCOVERY_POW_EFFORT_BITS` (default 8;
//! 0 in tests).
//!
//! Two further anti-spam layers are planned:
//!
//!   * **Privacy Pass (RFC 9576)** — anonymous attestation tokens via
//!     App Attest / Play Integrity. Deferred until we have client-side
//!     attestation infrastructure.
//!   * **IP rate limit with rotating salts** — Phase 2 hardening.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::MySqlPool;

/// Seed length (random bytes the server hands out per challenge).
const SEED_LEN: usize = 16;
/// Client-chosen nonce length.
const NONCE_LEN: usize = 16;
/// Equix solution length (8 × u16 packed, fixed by the algorithm).
const SOLUTION_LEN: usize = equix::Solution::NUM_BYTES;

/// Challenge TTL. Has to be long enough for slow clients (mobile +
/// background) to solve at the configured difficulty, but short enough
/// to bound DB growth.
const CHALLENGE_TTL_SECS: i64 = 300;

/// Period between pruner runs.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

/// Default effort if `LAKKO_POW_EFFORT_BITS` is unset.
///
/// Each extra bit doubles the expected client cost. At 8 bits the
/// expected number of solve attempts is 256, ~5–15s of CPU on a typical
/// 2024+ phone with the JIT compiler enabled. Set to `0` for tests so
/// the first nonce satisfies the difficulty trivially.
const DEFAULT_EFFORT_BITS: u8 = 8;

/// Read the configured effort from the env var. Each service has its
/// own knob (auth uses `LAKKO_POW_EFFORT_BITS`, discovery uses
/// `LAKKO_DISCOVERY_POW_EFFORT_BITS`). Tests override to 0.
fn configured_effort_bits() -> u8 {
    std::env::var("LAKKO_DISCOVERY_POW_EFFORT_BITS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_EFFORT_BITS)
}

// ============================================================================
// /pow/challenge
// ============================================================================

#[derive(Serialize)]
pub(crate) struct ChallengeResponse {
    /// Base64-encoded 16-byte seed.
    pub(crate) seed: String,
    /// Number of leading zero bits required on
    /// `SHA-256(seed || nonce || solution)`.
    pub(crate) effort_bits: u8,
    /// Seconds until the challenge expires.
    pub(crate) expires_in: u32,
}

/// `POST /pow/challenge` — issue a fresh PoW challenge.
///
/// The endpoint itself is cheap (one INSERT) but is still rate-limited
/// per-IP at the nginx layer in production (Phase 2 hardening). For now
/// it has no anti-abuse on its own — relying on the front-end nginx
/// `limit_req` zone.
pub(crate) async fn issue_challenge(
    State(state): State<crate::AppState>,
) -> Result<Json<ChallengeResponse>, (StatusCode, String)> {
    let mut seed = [0u8; SEED_LEN];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let effort_bits = configured_effort_bits();

    sqlx::query(
        "INSERT INTO pow_challenges (seed, effort_bits, expires_at) \
         VALUES (?, ?, NOW(3) + INTERVAL ? SECOND)",
    )
    .bind(&seed[..])
    .bind(effort_bits)
    .bind(CHALLENGE_TTL_SECS)
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "insert pow_challenges");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal db error".to_string(),
        )
    })?;

    // Cast is safe: TTL constant is well within u32::MAX.
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let expires_in = CHALLENGE_TTL_SECS as u32;

    Ok(Json(ChallengeResponse {
        seed: B64.encode(seed),
        effort_bits,
        expires_in,
    }))
}

// ============================================================================
// PoW proof — attached to expensive endpoints
// ============================================================================

/// PoW proof body. Embedded as the `pow` field in expensive endpoint
/// payloads.
#[derive(Deserialize, Clone)]
pub(crate) struct PowProof {
    /// Base64-encoded 16-byte seed (matches a row in `pow_challenges`).
    pub(crate) seed: String,
    /// Base64-encoded 16-byte client-chosen nonce.
    pub(crate) nonce: String,
    /// Base64-encoded 16-byte Equix solution.
    pub(crate) solution: String,
}

/// PoW verification outcome. Each variant maps to a specific HTTP
/// response so handlers can `?` straight through.
#[derive(Debug)]
pub(crate) enum PowError {
    /// One of the b64 fields is malformed or has the wrong byte length.
    BadEncoding(&'static str),
    /// Seed not in the table or already expired (or already consumed).
    UnknownOrExpired,
    /// SHA-256 prefix has fewer leading zero bits than required.
    InsufficientEffort,
    /// `equix::verify_bytes` rejected the solution.
    InvalidSolution,
    /// DB error (only at the storage layer).
    Db(String),
}

impl From<PowError> for (StatusCode, String) {
    fn from(e: PowError) -> Self {
        match e {
            PowError::BadEncoding(field) => (StatusCode::BAD_REQUEST, format!("pow: bad {field}")),
            PowError::UnknownOrExpired => (
                StatusCode::UNAUTHORIZED,
                "pow: unknown or expired challenge".to_string(),
            ),
            PowError::InsufficientEffort => (
                StatusCode::UNAUTHORIZED,
                "pow: insufficient effort".to_string(),
            ),
            PowError::InvalidSolution => (
                StatusCode::UNAUTHORIZED,
                "pow: invalid solution".to_string(),
            ),
            PowError::Db(s) => (StatusCode::INTERNAL_SERVER_ERROR, s),
        }
    }
}

/// Decoded fixed-size byte arrays from a `PowProof`. Internal helper.
#[derive(Debug)]
struct DecodedProof {
    seed: [u8; SEED_LEN],
    nonce: [u8; NONCE_LEN],
    solution: [u8; SOLUTION_LEN],
}

fn decode_proof(proof: &PowProof) -> Result<DecodedProof, PowError> {
    fn decode_fixed<const N: usize>(b64: &str, name: &'static str) -> Result<[u8; N], PowError> {
        let bytes = B64.decode(b64).map_err(|_| PowError::BadEncoding(name))?;
        if bytes.len() != N {
            return Err(PowError::BadEncoding(name));
        }
        let mut out = [0u8; N];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
    Ok(DecodedProof {
        seed: decode_fixed::<SEED_LEN>(&proof.seed, "seed")?,
        nonce: decode_fixed::<NONCE_LEN>(&proof.nonce, "nonce")?,
        solution: decode_fixed::<SOLUTION_LEN>(&proof.solution, "solution")?,
    })
}

/// Verify the cryptographic part of a proof — Equix solution + effort
/// bits — without touching the DB. Pure function; testable in isolation.
fn verify_crypto(decoded: &DecodedProof, effort_bits: u8) -> Result<(), PowError> {
    // Equix challenge = seed || nonce.
    let mut equix_challenge = [0u8; SEED_LEN + NONCE_LEN];
    equix_challenge[..SEED_LEN].copy_from_slice(&decoded.seed);
    equix_challenge[SEED_LEN..].copy_from_slice(&decoded.nonce);

    equix::verify_bytes(&equix_challenge, &decoded.solution)
        .map_err(|_| PowError::InvalidSolution)?;

    if effort_bits > 0 {
        let mut hasher = Sha256::new();
        hasher.update(decoded.seed);
        hasher.update(decoded.nonce);
        hasher.update(decoded.solution);
        let digest = hasher.finalize();
        if leading_zero_bits(&digest) < u32::from(effort_bits) {
            return Err(PowError::InsufficientEffort);
        }
    }

    Ok(())
}

/// Verify a `PowProof` against the DB-backed challenge table and the
/// Equix math. Consumes the seed row on the first attempt — successful
/// or failed — so the proof is single-use.
///
/// This is the entry point for handlers; called before any expensive
/// crypto / DB work in `signup_start`, `signup_finish`, `login_start`,
/// `recovery_login_start`.
pub(crate) async fn verify_proof(pool: &MySqlPool, proof: &PowProof) -> Result<(), PowError> {
    let decoded = decode_proof(proof)?;

    // Atomically: SELECT FOR UPDATE → DELETE. Same pattern as
    // `take_login_state` in opaque.rs.
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| PowError::Db(e.to_string()))?;

    let row: Option<(u8,)> = sqlx::query_as(
        "SELECT effort_bits FROM pow_challenges \
         WHERE seed = ? AND expires_at > NOW(3) \
         FOR UPDATE",
    )
    .bind(&decoded.seed[..])
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| PowError::Db(e.to_string()))?;

    // One-shot consumption regardless of outcome (delete the row even
    // if it's expired, to keep the table tight; the pruner will catch
    // anything we miss).
    sqlx::query("DELETE FROM pow_challenges WHERE seed = ?")
        .bind(&decoded.seed[..])
        .execute(&mut *tx)
        .await
        .map_err(|e| PowError::Db(e.to_string()))?;
    tx.commit().await.map_err(|e| PowError::Db(e.to_string()))?;

    let Some((effort_bits,)) = row else {
        return Err(PowError::UnknownOrExpired);
    };

    verify_crypto(&decoded, effort_bits)
}

/// Count leading zero bits across a byte slice (most-significant byte
/// first, MSB within each byte).
fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut count = 0u32;
    for &b in bytes {
        if b == 0 {
            count += 8;
        } else {
            count += b.leading_zeros();
            return count;
        }
    }
    count
}

/// Spawn a background task that periodically deletes expired challenges
/// from `pow_challenges`. Aborted when the service stops.
pub(crate) fn spawn_pruner(pool: MySqlPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        // The first tick fires immediately — skip it.
        interval.tick().await;
        loop {
            interval.tick().await;
            let result = sqlx::query("DELETE FROM pow_challenges WHERE expires_at < NOW(3)")
                .execute(&pool)
                .await;
            match result {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::debug!(rows = r.rows_affected(), "pruned expired pow_challenges");
                },
                Ok(_) => {},
                Err(e) => tracing::warn!(error = %e, "pow_challenges prune failed"),
            }
        }
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_zero_bits_counts_correctly() {
        assert_eq!(leading_zero_bits(&[0xFF]), 0);
        assert_eq!(leading_zero_bits(&[0x80]), 0);
        assert_eq!(leading_zero_bits(&[0x7F]), 1);
        assert_eq!(leading_zero_bits(&[0x01]), 7);
        assert_eq!(leading_zero_bits(&[0x00, 0x80]), 8);
        assert_eq!(leading_zero_bits(&[0x00, 0x00, 0x40]), 17);
        assert_eq!(leading_zero_bits(&[0x00, 0x00, 0x00, 0x00]), 32);
    }

    /// Solve a real Equix puzzle and verify it round-trips through
    /// `verify_crypto`. Uses effort_bits=0 so any first solution
    /// satisfies the difficulty (keeps the test fast — single Equix
    /// solve, ~5–50ms with the JIT compiler enabled).
    ///
    /// Equix occasionally fails to find a solution for a given
    /// challenge (~5% of seeds); we walk a few seed prefixes until
    /// we hit one that yields a solution.
    #[test]
    fn verify_crypto_accepts_real_solution_zero_effort() {
        let mut decoded = DecodedProof {
            seed: [0u8; SEED_LEN],
            nonce: [0u8; NONCE_LEN],
            solution: [0u8; SOLUTION_LEN],
        };
        let mut found = false;
        for attempt in 0u8..32 {
            decoded.seed[0] = attempt;
            let mut challenge = [0u8; SEED_LEN + NONCE_LEN];
            challenge[..SEED_LEN].copy_from_slice(&decoded.seed);
            challenge[SEED_LEN..].copy_from_slice(&decoded.nonce);
            let Ok(eq) = equix::EquiX::new(&challenge) else {
                continue;
            };
            let solutions = eq.solve();
            if let Some(sol) = solutions.first() {
                decoded.solution = sol.to_bytes();
                found = true;
                break;
            }
        }
        assert!(
            found,
            "Equix should yield a solution within 32 seed attempts",
        );

        verify_crypto(&decoded, 0).expect("real solution must verify at effort=0");
    }

    /// Tampering with the solution bytes must break verification.
    #[test]
    fn verify_crypto_rejects_tampered_solution() {
        let mut decoded = DecodedProof {
            seed: [0u8; SEED_LEN],
            nonce: [0u8; NONCE_LEN],
            solution: [0u8; SOLUTION_LEN],
        };
        for attempt in 0u8..32 {
            decoded.seed[0] = attempt;
            let mut challenge = [0u8; SEED_LEN + NONCE_LEN];
            challenge[..SEED_LEN].copy_from_slice(&decoded.seed);
            challenge[SEED_LEN..].copy_from_slice(&decoded.nonce);
            let Ok(eq) = equix::EquiX::new(&challenge) else {
                continue;
            };
            let solutions = eq.solve();
            if let Some(sol) = solutions.first() {
                decoded.solution = sol.to_bytes();
                break;
            }
        }
        // Flip a byte in the middle of the solution.
        decoded.solution[7] ^= 0xFF;

        let err = verify_crypto(&decoded, 0).unwrap_err();
        assert!(matches!(err, PowError::InvalidSolution));
    }

    /// A solution that's correct Equix-wise but doesn't meet the
    /// effort-bits target must be rejected. We force this by demanding
    /// 64 leading zero bits on the SHA-256 — astronomically unlikely
    /// for any concrete solution we can produce in a unit test.
    #[test]
    fn verify_crypto_rejects_insufficient_effort() {
        let mut decoded = DecodedProof {
            seed: [0u8; SEED_LEN],
            nonce: [0u8; NONCE_LEN],
            solution: [0u8; SOLUTION_LEN],
        };
        for attempt in 0u8..32 {
            decoded.seed[0] = attempt;
            let mut challenge = [0u8; SEED_LEN + NONCE_LEN];
            challenge[..SEED_LEN].copy_from_slice(&decoded.seed);
            challenge[SEED_LEN..].copy_from_slice(&decoded.nonce);
            let Ok(eq) = equix::EquiX::new(&challenge) else {
                continue;
            };
            let solutions = eq.solve();
            if let Some(sol) = solutions.first() {
                decoded.solution = sol.to_bytes();
                break;
            }
        }

        let err = verify_crypto(&decoded, 64).unwrap_err();
        assert!(matches!(err, PowError::InsufficientEffort));
    }

    #[test]
    fn decode_proof_validates_fixed_lengths() {
        let proof = PowProof {
            seed: B64.encode([0u8; 8]), // wrong length
            nonce: B64.encode([0u8; NONCE_LEN]),
            solution: B64.encode([0u8; SOLUTION_LEN]),
        };
        let err = decode_proof(&proof).unwrap_err();
        assert!(matches!(err, PowError::BadEncoding("seed")));

        let proof = PowProof {
            seed: B64.encode([0u8; SEED_LEN]),
            nonce: B64.encode([0u8; SOLUTION_LEN]),
            solution: B64.encode([0u8; 8]), // wrong length
        };
        let err = decode_proof(&proof).unwrap_err();
        assert!(matches!(err, PowError::BadEncoding("solution")));

        // All correct sizes — must succeed (the bytes are zeroes; the
        // crypto check fails later, but decode itself passes).
        let proof = PowProof {
            seed: B64.encode([0u8; SEED_LEN]),
            nonce: B64.encode([0u8; NONCE_LEN]),
            solution: B64.encode([0u8; SOLUTION_LEN]),
        };
        decode_proof(&proof).expect("well-formed lengths must decode");
    }
}
