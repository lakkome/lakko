// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Equix proof-of-work client (solver) for the Lakko anti-spam gate.
//!
//! ## Why a separate crate
//!
//! The server (`services/auth/src/pow.rs`) only verifies; the
//! client only solves. Both halves use `equix`, but on different
//! axes:
//!
//!   * **Server** can opt into the `compiler` feature for an
//!     x86_64 JIT verify. It cannot be used on `wasm32` (no
//!     executable memory).
//!   * **Client** must run on host, iOS, Android **and** wasm32.
//!     We disable `equix`'s default features (so no JIT) and
//!     stick to the portable interpreter — same algorithm,
//!     5–10× slower on x86 but the only option in the browser.
//!
//! Bundling this into `services/auth` would force a feature
//! conflict; bundling it into `lakko-ffi-wasm` would couple the
//! UniFFI surface (Swift / Kotlin) to the wasm-bindgen one. A
//! standalone crate consumed from both FFI surfaces is the
//! smallest seam.
//!
//! ## Protocol mirror
//!
//! Verbatim mirror of the server protocol described in
//! `services/auth/src/pow.rs`:
//!
//! 1. `POST /pow/challenge` returns `(seed, effort_bits, expires_in)`.
//! 2. Pick a `nonce` (16 bytes). Run `equix::solve(seed || nonce)`.
//!    For ~5% of seeds Equix fails to find a solution; increment the
//!    nonce and retry.
//! 3. Once a solution is found, check that
//!    `SHA-256(seed || nonce || solution)` has at least
//!    `effort_bits` leading zero bits. If not, increment the nonce
//!    and retry.
//! 4. Submit `(seed, nonce, solution)` alongside the expensive
//!    request.
//!
//! ## API
//!
//! ```no_run
//! use lakko_pow::solve;
//!
//! let seed = [0u8; 16];     // from POST /pow/challenge
//! let effort_bits = 0;      // tests only; production is 8+
//! let proof = solve(&seed, effort_bits).expect("solve");
//! // proof.seed / proof.nonce / proof.solution → base64 → request body
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use rand::RngCore;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Server-issued seed length, in bytes.
pub const SEED_LEN: usize = 16;
/// Client-chosen nonce length, in bytes.
pub const NONCE_LEN: usize = 16;
/// Equix solution length, in bytes. Fixed by the algorithm
/// (8 × `u16` packed).
pub const SOLUTION_LEN: usize = equix::Solution::NUM_BYTES;

/// A complete PoW proof — exactly the bytes the server's
/// `pow_proof` field expects (after base64-encoding each
/// component).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowProof {
    /// 16-byte seed handed out by the server in `/pow/challenge`.
    pub seed: [u8; SEED_LEN],
    /// 16-byte nonce selected by the solver.
    pub nonce: [u8; NONCE_LEN],
    /// 16-byte Equix solution for `seed || nonce`.
    pub solution: [u8; SOLUTION_LEN],
}

/// Solver failure modes.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PowError {
    /// The solver budget (max nonce-attempts before giving up)
    /// was exhausted before a satisfying nonce was found.
    ///
    /// The expected number of attempts at `effort_bits = N` is
    /// `2^N` for the leading-zero gate, multiplied by ~1.05× for
    /// the Equix per-seed failure rate. The default budget
    /// (`solve_with_budget`'s `max_attempts` argument) gives
    /// generous headroom for `effort_bits ≤ 12`. Above that, the
    /// caller should pass a larger budget.
    #[error("PoW budget exhausted (max attempts reached)")]
    BudgetExhausted,
}

/// Default attempt budget for `solve()`. Tuned so that the
/// default server difficulty (`DEFAULT_EFFORT_BITS = 8`,
/// expected ~256 attempts × 1.05 = ~270) finishes with very high
/// probability inside a single `solve()` call.
///
/// At `effort_bits = 12`, the expected count is ~4300 — still
/// inside the budget. Above 14 the caller should bump it
/// explicitly via [`solve_with_budget`].
pub const DEFAULT_MAX_ATTEMPTS: u32 = 1 << 16; // 65 536

/// Run the solver with the default attempt budget.
///
/// Blocking. Typical wall-clock cost on a 2024+ laptop:
///
///   * `effort_bits = 0` — sub-millisecond.
///   * `effort_bits = 8` (server default) — 100–500 ms.
///   * `effort_bits = 12` — 2–8 s.
///
/// Browser callers SHOULD run `solve()` inside a Web Worker so
/// the main thread stays responsive. The function itself does
/// no I/O and is portable to every target.
///
/// # Errors
///
/// Returns [`PowError::BudgetExhausted`] if the default budget
/// is too tight; raise the budget via [`solve_with_budget`].
pub fn solve(seed: &[u8; SEED_LEN], effort_bits: u8) -> Result<PowProof, PowError> {
    solve_with_budget(seed, effort_bits, DEFAULT_MAX_ATTEMPTS)
}

/// Run the solver with an explicit attempt budget.
///
/// `max_attempts` is the upper bound on the number of nonces
/// considered. Each attempt either fails inside Equix (~5%) or
/// produces a candidate solution that may or may not satisfy the
/// leading-zero gate.
///
/// Nonces are sampled from a fresh CSPRNG on each call — any two
/// invocations with the same `seed` are independent. This is the
/// expected behaviour: an attacker who replays a prior `(seed,
/// nonce, solution)` is blocked at the server because the seed
/// row is consumed on the first verification attempt.
///
/// # Errors
///
/// [`PowError::BudgetExhausted`] when `max_attempts` is hit
/// without a satisfying nonce.
pub fn solve_with_budget(
    seed: &[u8; SEED_LEN],
    effort_bits: u8,
    max_attempts: u32,
) -> Result<PowProof, PowError> {
    // Equix challenge buffer = seed || nonce. Reused across
    // iterations to avoid 64-byte allocations in the hot loop.
    let mut challenge = [0u8; SEED_LEN + NONCE_LEN];
    challenge[..SEED_LEN].copy_from_slice(seed);

    // Build the Equix runtime once and reuse — `EquiXBuilder::build`
    // does the per-seed HashX program build, which we want exactly
    // once per nonce (per the algorithm). For the loop here we
    // re-instantiate per nonce, since the program input is
    // `seed || nonce` and the nonce changes.
    let mut rng = rand::rngs::OsRng;

    for _ in 0..max_attempts {
        // Fresh random nonce per attempt. Using random nonces
        // (vs. incrementing) means parallel solvers on the same
        // seed don't all chase the same neighbourhood — useful
        // when the future Web Worker pool fans out.
        rng.fill_bytes(&mut challenge[SEED_LEN..]);

        // Equix may legitimately fail to find a solution for
        // ~5% of (seed, nonce) pairs. Treat that as "skip and
        // retry with a different nonce".
        let Ok(solutions) = equix::solve(&challenge) else {
            continue;
        };
        let Some(solution) = solutions.first() else {
            continue;
        };

        let solution_bytes = solution.to_bytes();
        if !meets_effort(seed, &challenge[SEED_LEN..], &solution_bytes, effort_bits) {
            continue;
        }

        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&challenge[SEED_LEN..]);
        return Ok(PowProof {
            seed: *seed,
            nonce,
            solution: solution_bytes,
        });
    }

    Err(PowError::BudgetExhausted)
}

/// Check the leading-zero-bits gate. Pure function, exposed for
/// tests and for callers that want to verify a proof locally
/// before sending it.
///
/// The 1-byte signature on `effort_bits` mirrors the server (it
/// fits in a `u8` because no caller would pick a value above 32 in
/// practice).
#[must_use]
pub fn meets_effort(
    seed: &[u8; SEED_LEN],
    nonce: &[u8],
    solution: &[u8; SOLUTION_LEN],
    effort_bits: u8,
) -> bool {
    if effort_bits == 0 {
        return true;
    }
    let mut hasher = Sha256::new();
    hasher.update(seed);
    hasher.update(nonce);
    hasher.update(solution);
    let digest = hasher.finalize();
    leading_zero_bits(&digest) >= u32::from(effort_bits)
}

/// Count leading zero bits across `bytes`, MSB-first within each
/// byte.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `effort_bits = 0` — every solution passes the gate, so the
    /// solver returns on the first non-failing Equix attempt.
    #[test]
    fn solve_with_zero_effort_returns_quickly() {
        let seed = [0xAAu8; SEED_LEN];
        let proof = solve(&seed, 0).expect("solve");
        assert_eq!(proof.seed, seed);
    }

    /// The proof we produce is verifiable by upstream `equix` —
    /// same call the server makes.
    #[test]
    fn produced_proof_passes_equix_verify() {
        let seed = [0x11u8; SEED_LEN];
        let proof = solve(&seed, 0).expect("solve");

        let mut challenge = [0u8; SEED_LEN + NONCE_LEN];
        challenge[..SEED_LEN].copy_from_slice(&proof.seed);
        challenge[SEED_LEN..].copy_from_slice(&proof.nonce);

        equix::verify_bytes(&challenge, &proof.solution).expect("equix verify");
    }

    /// At low effort the leading-zero gate trivially holds for
    /// some nonces; verify the gate logic.
    #[test]
    fn solve_with_modest_effort_satisfies_gate() {
        let seed = [0x22u8; SEED_LEN];
        // 4 leading zero bits — expected ~16 attempts, very fast.
        let proof = solve(&seed, 4).expect("solve");
        assert!(
            meets_effort(&proof.seed, &proof.nonce, &proof.solution, 4),
            "produced proof must satisfy its own effort gate"
        );
    }

    /// Each call samples fresh nonces; two solves on the same
    /// seed produce different proofs (with overwhelming
    /// probability — 16 random bytes collide once in 2^128).
    #[test]
    fn solves_use_independent_nonces() {
        let seed = [0x33u8; SEED_LEN];
        let a = solve(&seed, 0).expect("solve a");
        let b = solve(&seed, 0).expect("solve b");
        assert_ne!(a.nonce, b.nonce, "nonces must be independent");
    }

    /// Budget = 0 means we never even try — must return
    /// BudgetExhausted, never panic.
    #[test]
    fn zero_budget_returns_exhausted() {
        let seed = [0u8; SEED_LEN];
        let err = solve_with_budget(&seed, 0, 0).expect_err("must fail");
        assert!(matches!(err, PowError::BudgetExhausted));
    }

    /// `meets_effort` is a pure function — small unit tests.
    #[test]
    fn leading_zero_bits_smoke() {
        assert_eq!(leading_zero_bits(&[0xFF]), 0);
        assert_eq!(leading_zero_bits(&[0x7F]), 1);
        assert_eq!(leading_zero_bits(&[0x00, 0x01]), 15);
        assert_eq!(leading_zero_bits(&[0x00, 0x00, 0x80]), 16);
    }
}
