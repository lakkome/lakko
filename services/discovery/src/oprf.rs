// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! VOPRF (RFC 9497) server side.
//!
//! Single static key `K` lives in the singleton `oprf_server_setup`
//! table; generated on first start, read-only afterwards. The
//! `/discovery/oprf` endpoint accepts a serialised
//! [`voprf::BlindedElement`], applies `K`, and returns the serialised
//! [`voprf::EvaluationElement`]. The client finalises locally — the
//! server never sees plaintext usernames. `/discovery/oprf/batch`
//! evaluates the whole term set of a name being published in one request
//! (ADR-0087 section 4, ADR-0060, amendment of 2026-11-04).
//!
//! Ciphersuite: Ristretto255-SHA512 (RFC 9497 §3.2.1). Picked to match
//! the OPAQUE-PAKE choice in `services/auth` (Ristretto255 + SHA-512
//! KSF) so a future shared crypto crate can reuse both code paths.

use anyhow::{anyhow, Context, Result};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_anon_token::TokenContext;
use lakko_anon_token_server::TokenRejection;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;
use voprf::{BlindedElement, OprfServer};

/// Lakko VOPRF ciphersuite — Ristretto255-SHA512.
pub(crate) type LakkoOprfCs = voprf::Ristretto255;

/// Server-static OPRF state (the secret scalar `K`). Used to serve
/// every `/discovery/oprf` evaluation. Wrapped in `Arc` in `AppState`
/// so cloning the state for axum extractors is cheap.
pub(crate) type LakkoOprfServer = OprfServer<LakkoOprfCs>;

/// Load the server-static OPRF setup from the DB; on first start,
/// generate fresh state and persist it.
///
/// Singleton-row pattern — the table has a `CHECK(id=1)` constraint,
/// so it physically cannot hold more than one row.
///
/// # Errors
///
/// Any DB error or VOPRF deserialisation failure propagates via
/// `anyhow::Error`. Fail-fast critical path — a service with a broken
/// OPRF setup must not start.
pub(crate) async fn load_or_init_server_setup(pool: &MySqlPool) -> Result<LakkoOprfServer> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT state_bytes FROM oprf_server_setup WHERE id = 1")
            .fetch_optional(pool)
            .await
            .context("query oprf_server_setup")?;

    if let Some((bytes,)) = row {
        let server = OprfServer::<LakkoOprfCs>::new_from_seed(&bytes, b"lakko/oprf/v1")
            .map_err(|e| anyhow!("rebuild OprfServer from stored seed: {e:?}"))?;
        tracing::info!("loaded existing OPRF server setup from oprf_server_setup");
        return Ok(server);
    }

    // First run — generate a fresh 32-byte seed and persist it. We
    // deliberately store the SEED (not the derived scalar) because
    // voprf's `OprfServer::new_from_seed` is the documented stable
    // entry point; the derived scalar is an internal representation.
    let mut seed = [0u8; 32];
    rand::Rng::fill(&mut OsRng, &mut seed[..]);
    let server = OprfServer::<LakkoOprfCs>::new_from_seed(&seed, b"lakko/oprf/v1")
        .map_err(|e| anyhow!("derive OprfServer from fresh seed: {e:?}"))?;

    sqlx::query("INSERT INTO oprf_server_setup (id, state_bytes) VALUES (1, ?)")
        .bind(&seed[..])
        .execute(pool)
        .await
        .context("insert fresh OPRF server seed")?;

    tracing::info!("generated and persisted fresh OPRF server setup (first run)");
    Ok(server)
}

// ============================================================================
// /discovery/oprf
// ============================================================================

#[derive(Deserialize)]
pub(crate) struct OprfRequest {
    /// Base64-encoded serialised `BlindedElement<Ristretto255-SHA512>`
    /// produced by the client's `OprfClient::blind(...)`.
    pub(crate) blinded: String,
}

#[derive(Serialize)]
pub(crate) struct OprfResponse {
    /// Base64-encoded serialised `EvaluationElement<Ristretto255-SHA512>`.
    /// Client passes it back into `state.finalize(input, &eval)` to
    /// recover the OPRF output.
    pub(crate) evaluated: String,
}

/// `POST /discovery/oprf` — apply the server's static OPRF key to a
/// blinded element and return the evaluation. Server learns nothing
/// about the underlying input.
pub(crate) async fn evaluate(
    State(state): State<crate::AppState>,
    Json(payload): Json<OprfRequest>,
) -> Result<Json<OprfResponse>, (StatusCode, String)> {
    let blinded_bytes = B64.decode(&payload.blinded).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid blinded base64: {e}"),
        )
    })?;
    let blinded = BlindedElement::<LakkoOprfCs>::deserialize(&blinded_bytes).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid BlindedElement: {e:?}"),
        )
    })?;

    let evaluation = state.oprf.blind_evaluate(&blinded);
    let bytes = evaluation.serialize();

    Ok(Json(OprfResponse {
        evaluated: B64.encode(bytes),
    }))
}

// ============================================================================
// /discovery/oprf/batch
// ============================================================================

/// The most blinded elements one `POST /discovery/oprf/batch` evaluates:
/// `lakko_contacts::MAX_HANDLE_TERMS` (65: up to 64 name search terms plus the
/// record's own `@name`) plus one, because a rename (`/discovery/handle/replace`)
/// evaluates the old `@name` as well as the new one and its terms. Publishing a
/// group or channel `@name` record (ADR-0087 section 4) or a person's public
/// record in Everyone (ADR-0060) evaluates at most that set; anything larger is
/// refused before any evaluation.
pub(crate) const MAX_OPRF_BATCH: usize = lakko_contacts::MAX_HANDLE_TERMS + 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OprfBatchRequest {
    /// Base64-encoded serialised `BlindedElement`s, 1..=[`MAX_OPRF_BATCH`].
    pub(crate) blinded: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct OprfBatchResponse {
    /// Base64-encoded serialised `EvaluationElement`s, in request order.
    pub(crate) evaluated: Vec<String>,
}

fn batch_refusal(code: &'static str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": code })),
    )
        .into_response()
}

/// `POST /discovery/oprf/batch` — evaluate up to [`MAX_OPRF_BATCH`] blinded
/// elements for one `discovery-register` anonymous token (2026-11-04
/// amendment of ADR-0087 section 4 and ADR-0060): publishing a name costs one
/// request and one token, not one lookup token per search term. Lookups stay
/// on `/discovery/oprf`, one `discovery-lookup` token per evaluation.
///
/// No session: a group or channel name write must not name an account, and a
/// session here would link the account to the publish by time.
///
/// Order: a service without anonymous tokens answers 503 whatever the body
/// (as every token route does); then the body is decoded and every element
/// deserialised — an empty list, more than [`MAX_OPRF_BATCH`] entries or one
/// bad element is a 400 with no token spent and nothing evaluated; then the
/// token is spent; then every element is evaluated. The per-IP bucket sits
/// outside this handler and is charged once per request.
pub(crate) async fn evaluate_batch(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !state.anon_tokens.is_enabled() {
        return TokenRejection::Disabled.into_response();
    }
    let Ok(request) = serde_json::from_slice::<OprfBatchRequest>(&body) else {
        return batch_refusal("oprf-batch-malformed");
    };
    if request.blinded.is_empty() {
        return batch_refusal("oprf-batch-empty");
    }
    if request.blinded.len() > MAX_OPRF_BATCH {
        return batch_refusal("oprf-batch-too-large");
    }
    let mut blinded = Vec::with_capacity(request.blinded.len());
    for b64 in &request.blinded {
        let Ok(bytes) = B64.decode(b64) else {
            return batch_refusal("oprf-batch-bad-element");
        };
        let Ok(element) = BlindedElement::<LakkoOprfCs>::deserialize(&bytes) else {
            return batch_refusal("oprf-batch-bad-element");
        };
        blinded.push(element);
    }
    if let Err(refused) = state
        .anon_tokens
        .spend_header(&headers, TokenContext::DiscoveryRegister)
        .await
    {
        return refused.into_response();
    }
    let evaluated = blinded
        .iter()
        .map(|element| B64.encode(state.oprf.blind_evaluate(element).serialize()))
        .collect();
    Json(OprfBatchResponse { evaluated }).into_response()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use voprf::OprfClient;

    /// Full client-server VOPRF round-trip in memory: blind → server
    /// blind_evaluate → unblind. Confirms the ciphersuite + seed-based
    /// server construction match what RFC 9497 prescribes.
    #[test]
    fn oprf_roundtrip_in_memory() {
        let mut rng = OsRng;
        let seed = [0xABu8; 32];
        let server = OprfServer::<LakkoOprfCs>::new_from_seed(&seed, b"lakko/oprf/v1").unwrap();

        let input = b"@alice";

        // Client: blind.
        let client_blind = OprfClient::<LakkoOprfCs>::blind(input, &mut rng).unwrap();
        let blinded = client_blind.message;

        // Wire: serialise + deserialise (mirrors what the HTTP layer does).
        let blinded_bytes = blinded.serialize();
        let blinded2 = BlindedElement::<LakkoOprfCs>::deserialize(&blinded_bytes).unwrap();

        // Server: blind_evaluate.
        let evaluation = server.blind_evaluate(&blinded2);
        let eval_bytes = evaluation.serialize();
        let evaluation2 =
            voprf::EvaluationElement::<LakkoOprfCs>::deserialize(&eval_bytes).unwrap();

        // Client: finalize.
        let output = client_blind.state.finalize(input, &evaluation2).unwrap();
        assert_eq!(
            output.len(),
            64,
            "Ristretto255-SHA512 OPRF output is 64 bytes"
        );

        // Run the same input again and confirm we get the same output —
        // this is the "P" in OPRF: deterministic for given (K, x).
        let client_blind2 = OprfClient::<LakkoOprfCs>::blind(input, &mut rng).unwrap();
        let evaluation2 = server.blind_evaluate(&client_blind2.message);
        let output2 = client_blind2.state.finalize(input, &evaluation2).unwrap();
        assert_eq!(
            output, output2,
            "OPRF must be deterministic across blinding randomness",
        );
    }

    #[test]
    fn oprf_different_inputs_produce_different_outputs() {
        let mut rng = OsRng;
        let seed = [0xCDu8; 32];
        let server = OprfServer::<LakkoOprfCs>::new_from_seed(&seed, b"lakko/oprf/v1").unwrap();

        let blind_a = OprfClient::<LakkoOprfCs>::blind(b"@alice", &mut rng).unwrap();
        let eval_a = server.blind_evaluate(&blind_a.message);
        let out_a = blind_a.state.finalize(b"@alice", &eval_a).unwrap();

        let blind_b = OprfClient::<LakkoOprfCs>::blind(b"@bob", &mut rng).unwrap();
        let eval_b = server.blind_evaluate(&blind_b.message);
        let out_b = blind_b.state.finalize(b"@bob", &eval_b).unwrap();

        assert_ne!(
            out_a, out_b,
            "different usernames must hash to different OPRF outputs",
        );
    }
}
