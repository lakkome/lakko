// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Service-local Equix proof-of-work shim.
//!
//! Since ADR-0054 S4 exactly one auth route carries an Equix proof of work:
//! `/signup/start`, deliberately heavy, once per account (every account is a
//! source of anonymous-token quota, so creating one must cost something).
//! Every other route is limited by server-side counters (`src/limits.rs`). The
//! server-side issue / verify / consume / prune logic + tests now live once in
//! the shared [`lakko_pow_server`] crate (G-29 dedup — this was duplicated
//! near-verbatim across `auth` / `discovery` / `push-gw`, a drift risk where a
//! security fix to one copy could miss the others). This module keeps only the
//! thin axum handler that needs the auth `AppState`, and re-exports the shared
//! types so existing `crate::pow::*` call sites are unchanged.
//!
//! Effort knob: env var `LAKKO_SIGNUP_POW_EFFORT_BITS` (default
//! [`lakko_pow_server::DEFAULT_EFFORT_BITS`] = 8, the previous
//! `LAKKO_POW_EFFORT_BITS` default; to be set by measurement on the slowest
//! supported phone, ADR-0054 open question 1; 0 in tests). The effort is
//! stored with each challenge, so the verifier never trusts the client for it.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

pub(crate) use lakko_pow_server::{
    spawn_pruner, verify_proof, ChallengeResponse, PowError, PowProof,
};

/// Effort env var for the signup proof of work.
const EFFORT_VAR: &str = "LAKKO_SIGNUP_POW_EFFORT_BITS";

/// `POST /pow/challenge` — issue a fresh PoW challenge (for `/signup/start`).
///
/// The endpoint itself is cheap (one INSERT); it sits behind the per-address
/// `Challenge` bucket (`src/limits.rs`).
pub(crate) async fn issue_challenge(
    State(state): State<crate::AppState>,
) -> Result<Json<ChallengeResponse>, (StatusCode, String)> {
    let effort = lakko_pow_server::configured_effort_bits(EFFORT_VAR);
    lakko_pow_server::issue_challenge(&state.db, effort)
        .await
        .map(Json)
        .map_err(Into::into)
}
