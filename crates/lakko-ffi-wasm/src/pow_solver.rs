// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen exposure of `lakko-pow`.
//!
//! The single exposed entry point is the static method
//! [`PowSolver::solve`]. It takes the base64 seed and `effort_bits`
//! the server returned from `/pow/challenge`, runs the solver
//! (blocking — typically sub-second at server defaults), and
//! produces a [`PowProof`] with three base64 fields ready to drop
//! straight into the `pow` object the auth handlers expect.
//!
//! The blocking-ness is **explicit**: callers in `apps/web` SHOULD
//! invoke `solve` from inside a Web Worker so the main thread
//! stays responsive. The 1.16c3a sprint exposes the solver only —
//! Worker plumbing is part of 1.16c3b together with the HTTP
//! transport.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_pow::{solve, PowError, PowProof as InnerProof, NONCE_LEN, SEED_LEN, SOLUTION_LEN};
use wasm_bindgen::prelude::*;

/// Map a `lakko_pow::PowError` into a `JsError`. Single variant
/// today (`BudgetExhausted`); any future variants should keep the
/// `Display` text terse — it surfaces to the user as a thrown
/// `Error.message`.
fn pow_err(e: PowError) -> JsError {
    JsError::new(&format!("{e}"))
}

/// Static-only namespace class for the PoW solver.
///
/// Exists as a class so the JS surface stays uniform with the rest
/// of `lakko-ffi-wasm` (where every operation hangs off a class).
/// The struct itself carries no state — every method is a static
/// factory.
///
/// JS usage:
///
/// ```js
/// import { PowSolver } from "@lakko/ffi-wasm";
///
/// // 1. Fetch the challenge.
/// const { seed, effort_bits } = await fetch("/pow/challenge", {
///     method: "POST",
/// }).then((r) => r.json());
///
/// // 2. Solve (blocking — call from a Web Worker for non-trivial
/// //    effort_bits values).
/// const proof = PowSolver.solve(seed, effort_bits);
///
/// // 3. Attach to the next expensive request.
/// await fetch("/signup/start", {
///     method: "POST",
///     headers: { "content-type": "application/json" },
///     body: JSON.stringify({
///         pow: {
///             seed: proof.seedB64,
///             nonce: proof.nonceB64,
///             solution: proof.solutionB64,
///         },
///         account_id: "...",
///         password_request: "...",
///     }),
/// });
/// ```
#[wasm_bindgen]
pub struct PowSolver;

#[wasm_bindgen]
impl PowSolver {
    /// Solve the Equix PoW for the given challenge.
    ///
    /// Blocking. At the server-default `effort_bits = 8` this
    /// typically takes 100–500 ms on a 2024+ phone; at higher
    /// difficulties it can take seconds. **Run inside a Web
    /// Worker** to keep the UI responsive.
    ///
    /// # Errors
    ///
    /// * `seedB64` does not decode as base64 → thrown error.
    /// * Decoded seed has the wrong byte length (must be 16) →
    ///   thrown error.
    /// * Solver budget exhausted (extremely unlikely at default
    ///   server effort, possible at `effort_bits ≥ 14` — caller
    ///   should retry with a fresh challenge).
    pub fn solve(seed_b64: &str, effort_bits: u8) -> Result<PowProof, JsError> {
        let seed_bytes = B64
            .decode(seed_b64)
            .map_err(|e| JsError::new(&format!("invalid seed base64: {e}")))?;
        let seed: [u8; SEED_LEN] = seed_bytes
            .as_slice()
            .try_into()
            .map_err(|_| JsError::new("seed must be 16 bytes"))?;

        let proof: InnerProof = solve(&seed, effort_bits).map_err(pow_err)?;
        Ok(PowProof::from_inner(proof))
    }
}

/// JS-facing PoW proof. Three base64 fields ready to inline into
/// the `pow` object on `/signup/start`, `/signup/finish`,
/// `/login/start`, `/recovery/login/start` request bodies.
///
/// Each getter returns a fresh `String` copy — cheap (the strings
/// are tiny, ~24 chars each) and means the caller cannot
/// accidentally mutate cached state.
///
/// `Debug` is derived because the bytes here are not secret —
/// the seed comes from the public challenge endpoint, and the
/// nonce / solution are about to be sent in cleartext to the
/// server in the next request.
#[derive(Debug)]
#[wasm_bindgen]
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

#[wasm_bindgen]
impl PowProof {
    /// Base64-encoded seed (echoes the `seed` from the
    /// `/pow/challenge` response — surfaced here so callers can
    /// build the `pow` object from the proof alone).
    #[wasm_bindgen(getter, js_name = seedB64)]
    pub fn seed_b64(&self) -> String {
        B64.encode(self.seed)
    }

    /// Base64-encoded nonce — 16 bytes.
    #[wasm_bindgen(getter, js_name = nonceB64)]
    pub fn nonce_b64(&self) -> String {
        B64.encode(self.nonce)
    }

    /// Base64-encoded Equix solution — 16 bytes.
    #[wasm_bindgen(getter, js_name = solutionB64)]
    pub fn solution_b64(&self) -> String {
        B64.encode(self.solution)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Host-side tests cover happy paths only — `JsError::new` is a
    // wasm-bindgen-imported function, stubbed under host `cargo
    // test` and panics when called. Error paths (bad base64,
    // wrong seed length, BudgetExhausted) are exercised in
    // `lakko-pow`'s own tests; the wasm-bindgen-test runner picks
    // them up here in 1.16f.

    /// Happy path with `effort_bits = 0` — returns instantly and
    /// the proof's seed echoes the input.
    #[test]
    fn solve_zero_effort_returns_proof_with_input_seed() {
        let seed = [0x44u8; SEED_LEN];
        let seed_b64 = B64.encode(seed);
        let proof = PowSolver::solve(&seed_b64, 0).expect("solve");
        assert_eq!(proof.seed_b64(), seed_b64);
        // Nonce is non-empty and decodes cleanly.
        let nonce = B64.decode(proof.nonce_b64()).expect("decode nonce");
        assert_eq!(nonce.len(), NONCE_LEN);
        let solution = B64.decode(proof.solution_b64()).expect("decode solution");
        assert_eq!(solution.len(), SOLUTION_LEN);
    }
}
