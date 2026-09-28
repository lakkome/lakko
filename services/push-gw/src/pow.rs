// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Service-local Equix proof-of-work shim.
//!
//! Phase 1.6 gated the push-gw endpoints with an Equix PoW. Since ADR-0054
//! decision 1 the wake itself is not a public route (the DS forwards it to
//! the internal listener after a signed publish, see `src/internal.rs`), so
//! the PoW now covers only `/push/register`, `/push/unregister` and the group
//! subscribe / unsubscribe pair - until ADR-0054 S3 replaces it there too. The
//! server-side issue / verify / consume / prune logic + tests now live once in
//! the shared [`lakko_pow_server`] crate (G-29 dedup — this was duplicated
//! near-verbatim across `auth` / `discovery` / `push-gw`, a drift risk where a
//! security fix to one copy could miss the others). This module keeps only the
//! thin axum handler that needs the push-gw `AppState`, and re-exports the
//! shared types so existing `crate::pow::*` call sites are unchanged.
//!
//! Effort knob: env var `LAKKO_PUSH_GW_POW_EFFORT_BITS` (default 8; 0 in
//! tests).

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

pub(crate) use lakko_pow_server::{spawn_pruner, verify_proof, ChallengeResponse, PowProof};

/// Effort env var for the push-gw service.
const EFFORT_VAR: &str = "LAKKO_PUSH_GW_POW_EFFORT_BITS";

/// `POST /pow/challenge` — issue a fresh PoW challenge.
///
/// The endpoint itself is cheap (one INSERT) but is still rate-limited per-IP
/// at the nginx layer in production (Phase 2 hardening).
pub(crate) async fn issue_challenge(
    State(state): State<crate::AppState>,
) -> Result<Json<ChallengeResponse>, (StatusCode, String)> {
    let effort = lakko_pow_server::configured_effort_bits(EFFORT_VAR);
    lakko_pow_server::issue_challenge(&state.db, effort)
        .await
        .map(Json)
        .map_err(Into::into)
}
