// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI exposure of `lakko-pow`.
//!
//! Mirror of `lakko-ffi-wasm/src/pow_solver.rs`, adapted to the
//! UniFFI 0.28 surface (which does not support free associated
//! functions on `Object`-typed classes — only `&self` methods and
//! `#[uniffi::constructor]`s).
//!
//! Exposes top-level [`pow_solve`] and [`pow_solve_chunk`] (the
//! cancellable slice: at most `max_attempts` nonces, `None` when none
//! produced a proof) and a `PowProof`
//! handle with three base64 getters. The function takes the
//! base64 seed and `effort_bits` the server returned from
//! `/pow/challenge`, runs the solver, and produces a [`PowProof`]
//! ready to drop into the `pow` object every expensive auth
//! endpoint expects.
//!
//! On the Kotlin side this surfaces as a top-level function
//! `powSolve(seedB64, effortBits)` and a `PowProof` class with
//! `seedB64()`, `nonceB64()`, `solutionB64()` accessors.
//!
//! ## Blocking — call from a worker thread
//!
//! `pow_solve` is **blocking**. At server-default `effort_bits = 8`
//! it typically completes in 100–500 ms on a 2024+ phone; at
//! higher difficulties it can take seconds. On Android the caller
//! MUST drive it from `Dispatchers.Default` (CPU-bound) — never
//! from the main thread. On iOS use a `Task.detached(priority:
//! .userInitiated)`.

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_pow::{
    solve, solve_chunk, PowError, PowProof as InnerProof, NONCE_LEN, SEED_LEN, SOLUTION_LEN,
};

use crate::FfiError;

impl From<PowError> for FfiError {
    fn from(e: PowError) -> Self {
        Self::Pow(e.to_string())
    }
}

/// Solve the Equix PoW for the given challenge.
///
/// Blocking. See the module-level doc on threading.
///
/// # Errors
///
/// * `seed_b64` does not decode as base64 → [`FfiError::Pow`]
///   with a "invalid seed base64" message.
/// * Decoded seed has the wrong byte length (must be 16) →
///   [`FfiError::Pow`] with a "seed must be 16 bytes" message.
/// * Solver budget exhausted (extremely unlikely at default
///   server effort, possible at `effort_bits ≥ 14` — caller
///   should retry with a fresh challenge).
#[uniffi::export]
pub fn pow_solve(seed_b64: String, effort_bits: u8) -> Result<Arc<PowProof>, FfiError> {
    let seed = decode_seed(&seed_b64)?;
    let proof: InnerProof = solve(&seed, effort_bits)?;
    Ok(Arc::new(PowProof::from_inner(proof)))
}

/// One cancellable slice of a solve: at most `max_attempts` nonces, then
/// `None` if none of them produced a proof.
///
/// For a caller that must be able to stop the CPU work (signup cancelled):
/// call it in a loop on a worker thread and check the cancel flag between
/// chunks. Each call samples fresh nonces, so the loop is the same search
/// as one [`pow_solve`]. Blocking for the chunk's duration; at the
/// interpreter's per-attempt cost (iOS) a chunk of a few attempts keeps the
/// cancel latency short.
///
/// # Errors
///
/// [`FfiError::Pow`] for a seed that is not base64 or not 16 bytes. Running
/// out of attempts is not an error: it is `None`.
#[uniffi::export]
pub fn pow_solve_chunk(
    seed_b64: String,
    effort_bits: u8,
    max_attempts: u32,
) -> Result<Option<Arc<PowProof>>, FfiError> {
    let seed = decode_seed(&seed_b64)?;
    Ok(solve_chunk(&seed, effort_bits, max_attempts).map(|p| Arc::new(PowProof::from_inner(p))))
}

fn decode_seed(seed_b64: &str) -> Result<[u8; SEED_LEN], FfiError> {
    let seed_bytes = B64
        .decode(seed_b64)
        .map_err(|e| FfiError::Pow(format!("invalid seed base64: {e}")))?;
    seed_bytes
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Pow("seed must be 16 bytes".to_string()))
}

/// Platform-facing PoW proof. Three base64 fields ready to inline
/// into the `pow` object on every expensive auth endpoint
/// (`/signup/start`, `/signup/finish`, `/login/start`,
/// `/recovery/login/start`).
///
/// Each getter returns a fresh `String` copy — cheap (the strings
/// are tiny, ~24 chars each) and means the caller cannot
/// accidentally mutate cached state.
#[derive(uniffi::Object)]
pub struct PowProof {
    seed: [u8; SEED_LEN],
    nonce: [u8; NONCE_LEN],
    solution: [u8; SOLUTION_LEN],
}

impl PowProof {
    fn from_inner(p: InnerProof) -> Self {
        Self {
            seed: p.seed,
            nonce: p.nonce,
            solution: p.solution,
        }
    }
}

// Manual `Debug` so tests can use `expect_err` and panic-format the
// result. Bytes here are not secret — the seed is public from the
// challenge endpoint, and the nonce / solution are about to be
// sent in cleartext to the server. UniFFI does not auto-derive
// `Debug` on `Object` types, hence the manual impl.
impl core::fmt::Debug for PowProof {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PowProof")
            .field("seed_b64", &self.seed_b64())
            .field("nonce_b64", &self.nonce_b64())
            .field("solution_b64", &self.solution_b64())
            .finish()
    }
}

#[uniffi::export]
impl PowProof {
    /// Base64-encoded seed (echoes the seed from the
    /// `/pow/challenge` response — surfaced here so callers can
    /// build the `pow` object from the proof alone).
    pub fn seed_b64(&self) -> String {
        B64.encode(self.seed)
    }

    /// Base64-encoded nonce — 16 bytes.
    pub fn nonce_b64(&self) -> String {
        B64.encode(self.nonce)
    }

    /// Base64-encoded Equix solution — 16 bytes.
    pub fn solution_b64(&self) -> String {
        B64.encode(self.solution)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Happy path with `effort_bits = 0` — returns instantly and
    /// the proof's seed echoes the input.
    #[test]
    fn solve_zero_effort_returns_proof_with_input_seed() {
        let seed = [0x44u8; SEED_LEN];
        let seed_b64 = B64.encode(seed);
        let proof = pow_solve(seed_b64.clone(), 0).expect("solve");
        assert_eq!(proof.seed_b64(), seed_b64);
        let nonce = B64.decode(proof.nonce_b64()).expect("decode nonce");
        assert_eq!(nonce.len(), NONCE_LEN);
        let solution = B64.decode(proof.solution_b64()).expect("decode solution");
        assert_eq!(solution.len(), SOLUTION_LEN);
    }

    #[test]
    fn solve_chunk_loop_finds_a_proof_and_an_empty_chunk_is_none() {
        let seed_b64 = B64.encode([0x45u8; SEED_LEN]);
        assert!(pow_solve_chunk(seed_b64.clone(), 0, 0).unwrap().is_none());
        let mut found = None;
        for _ in 0..10_000 {
            found = pow_solve_chunk(seed_b64.clone(), 4, 2).unwrap();
            if found.is_some() {
                break;
            }
        }
        let proof = found.expect("proof");
        let seed: [u8; SEED_LEN] = B64.decode(proof.seed_b64()).unwrap().try_into().unwrap();
        let nonce = B64.decode(proof.nonce_b64()).unwrap();
        let solution: [u8; SOLUTION_LEN] = B64
            .decode(proof.solution_b64())
            .unwrap()
            .try_into()
            .unwrap();
        assert!(lakko_pow::meets_effort(&seed, &nonce, &solution, 4));
        assert!(matches!(
            pow_solve_chunk("x!".to_string(), 0, 1),
            Err(FfiError::Pow(_))
        ));
    }

    #[test]
    fn solve_rejects_bad_seed_base64() {
        let err = pow_solve("not!!base64!!".to_string(), 0).expect_err("bad base64 must fail");
        assert!(matches!(err, FfiError::Pow(_)));
    }

    #[test]
    fn solve_rejects_wrong_seed_length() {
        let bad_seed = B64.encode([0u8; 8]);
        let err = pow_solve(bad_seed, 0).expect_err("short seed must fail");
        assert!(matches!(err, FfiError::Pow(msg) if msg.contains("16 bytes")));
    }
}
