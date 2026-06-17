// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! HTTP handlers for the witness service (ADR-0020).

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_crypto::signing::Signature;
use lakko_transparency::{SignedRoot, TreeHash};
use serde::{Deserialize, Serialize};

use crate::store::CosignError;
use crate::AppState;

/// Wire shape of an operator-signed root (matches transparency's `RootRes`).
#[derive(Deserialize)]
pub(crate) struct SignedRootWire {
    pub(crate) tree_root_b64: String,
    pub(crate) log_size: u64,
    pub(crate) timestamp_unix: u64,
    pub(crate) signature_b64: String,
}

/// `POST /witness/cosign` request — the operator submits a newly-signed root +
/// a consistency proof from the witness's last-co-signed size to the new size.
#[derive(Deserialize)]
pub(crate) struct CosignReq {
    pub(crate) new_root: SignedRootWire,
    /// Base64 32-byte audit-path hashes of the RFC 6962 §2.1.2 consistency
    /// proof. Empty for the first co-sign (the witness is at the empty tree).
    #[serde(default)]
    pub(crate) consistency_proof_b64: Vec<String>,
}

/// `POST /witness/cosign` response — the witness co-signature wire blob.
#[derive(Serialize)]
pub(crate) struct CosignRes {
    pub(crate) cosig_b64: String,
}

/// `POST /witness/cosign` — verify + co-sign a new operator root (service-secret
/// gated). The witness co-signs only when the root is operator-signed AND
/// append-only-extends its last-co-signed root; a fork is refused (409).
pub(crate) async fn cosign(
    State(state): State<AppState>,
    Json(req): Json<CosignReq>,
) -> Result<Json<CosignRes>, (StatusCode, &'static str)> {
    let tree_root: TreeHash = B64
        .decode(req.new_root.tree_root_b64.as_bytes())
        .map_err(|_| (StatusCode::BAD_REQUEST, "tree_root_b64 not valid base64"))?
        .as_slice()
        .try_into()
        .map_err(|_| (StatusCode::BAD_REQUEST, "tree_root must be 32 bytes"))?;
    let sig_bytes = B64
        .decode(req.new_root.signature_b64.as_bytes())
        .map_err(|_| (StatusCode::BAD_REQUEST, "signature_b64 not valid base64"))?;
    let signature = Signature::from_bytes(&sig_bytes)
        .map_err(|_| (StatusCode::BAD_REQUEST, "signature must be 64 bytes"))?;
    let new_root = SignedRoot {
        tree_root,
        log_size: req.new_root.log_size,
        timestamp_unix: req.new_root.timestamp_unix,
        signature,
    };

    let mut proof: Vec<TreeHash> = Vec::with_capacity(req.consistency_proof_b64.len());
    for h in &req.consistency_proof_b64 {
        let node: TreeHash = B64
            .decode(h.as_bytes())
            .map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    "consistency proof hash not valid base64",
                )
            })?
            .as_slice()
            .try_into()
            .map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    "consistency proof hash must be 32 bytes",
                )
            })?;
        proof.push(node);
    }

    let cosig = state
        .store
        .cosign(&state.db, &state.operator_pubkey, &new_root, &proof)
        .await
        .map_err(|e| match e {
            CosignError::BadOperatorSignature => {
                (StatusCode::BAD_REQUEST, "operator signature is invalid")
            },
            CosignError::NotConsistent => (
                StatusCode::CONFLICT,
                "root does not extend the witnessed log (fork refused)",
            ),
            CosignError::Rollback { .. } => (StatusCode::CONFLICT, "log_size rollback refused"),
            CosignError::Db(detail) => {
                tracing::error!(detail, "witness cosign storage error");
                (StatusCode::INTERNAL_SERVER_ERROR, "cosign failed")
            },
        })?;

    Ok(Json(CosignRes {
        cosig_b64: B64.encode(cosig),
    }))
}

/// `GET /witness/key` response — the witness's published verifying key.
#[derive(Serialize)]
pub(crate) struct WitnessKeyRes {
    pub(crate) witness_pubkey_b64: String,
}

/// `GET /witness/key` — the witness's Ed25519 verifying key (32 bytes, base64).
/// Clients pin this out-of-band; every co-signature is verified against it.
pub(crate) async fn witness_key(State(state): State<AppState>) -> Json<WitnessKeyRes> {
    Json(WitnessKeyRes {
        witness_pubkey_b64: B64.encode(state.store.witness_verifying_key().to_bytes()),
    })
}
