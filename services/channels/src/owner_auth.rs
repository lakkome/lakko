// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The owner-signed route gate ([ADR-0059] section 1).
//!
//! A route that only a channel's owner may call (store or withdraw the
//! listing, read the subscriber list) carries an
//! `x-lakko-channel-owner-proof` header: a
//! `lakko_channels::owner` proof over the method, the routed path, the query
//! and the body, signed by the owner key registered when the channel was
//! created. The gate checks, in order:
//!
//! 1. the header parses;
//! 2. the channel has an owner key (an unknown channel is refused the same
//!    way);
//! 3. the proof is inside the 120 s window and its signature verifies under
//!    that key;
//! 4. its nonce has not been seen, recording it (so a replay inside the
//!    window is refused). The nonce is recorded only after the signature
//!    verified, so nobody without the key can fill the nonce table.
//!
//! Every refusal is one response, `401 {"code": "owner-proof-required"}`, so
//! a caller learns nothing from which step failed. A store failure is a 500
//! and nothing is honoured (fail closed).
//!
//! ## Co-publishers
//!
//! A route a publisher calls (the subscriber list, publishing a post,
//! delivering an envelope, rotate, rotate/stage, rotate/commit: ADR-0059
//! section 9, closing G-43) goes through [`require_publisher`] instead, which also accepts an
//! `x-lakko-channel-copublisher-proof` signed by a co-publisher key the owner
//! appointed (`PUT /v1/channels/<id>/copublishers/<key>`). The header names
//! its key, so the gate looks up that one key: it must be appointed for the
//! channel in the path, then the same window, signature and nonce steps run
//! against the same nonce table, with the same single refusal. An owner proof
//! is tried first when both headers are present. Every other owner-signed
//! route (the listing, the appointments themselves) stays owner-only.
//!
//! [ADR-0059]: ../../../docs/adr/0059-channel-discover.md

use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use lakko_channels::copublisher::{
    verify_copublisher_proof, CoPublisherProof, COPUBLISHER_PROOF_HEADER_NAME,
};
use lakko_channels::owner::{
    verify_owner_proof, OwnerProof, OwnerRequest, OWNER_PROOF_HEADER_NAME, OWNER_PROOF_WINDOW_MS,
};
use lakko_crypto::signing::VerifyingKey;

use crate::api::{err, unix_ms, AppState, ErrJson};
use crate::metrics::{proof_refused, ProofKind, RefusalReason};
use crate::storage::ChannelId;

/// How long a nonce is remembered: twice the proof window, so a proof can
/// never be replayed after its nonce was pruned (it is outside the window by
/// then).
pub const OWNER_NONCE_RETENTION_MS: u64 = 2 * OWNER_PROOF_WINDOW_MS;

/// The single refusal every failed proof gets.
fn refused() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        ErrJson(err(
            "owner-proof-required",
            "this route needs a valid channel owner proof",
        )),
    )
        .into_response()
}

fn internal() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrJson(err("internal", "unexpected error")),
    )
        .into_response()
}

/// Require a valid owner proof for `channel` over this request.
///
/// # Errors
///
/// The response to return instead of running the handler.
pub async fn require_owner(
    state: &AppState,
    channel: ChannelId,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), Response> {
    let Some(proof) = headers
        .get(OWNER_PROOF_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| OwnerProof::parse(v).ok())
    else {
        proof_refused(ProofKind::Owner, RefusalReason::Malformed);
        return Err(refused());
    };
    let owner = match state.store.owner_pubkey(channel).await {
        Ok(Some(key)) => key,
        Ok(None) => {
            proof_refused(ProofKind::Owner, RefusalReason::UnknownKey);
            return Err(refused());
        },
        Err(e) => {
            tracing::error!(error = %e, "owner key lookup failed");
            return Err(internal());
        },
    };
    let Ok(key) = VerifyingKey::from_bytes(&owner) else {
        proof_refused(ProofKind::Owner, RefusalReason::UnknownKey);
        return Err(refused());
    };
    let req = OwnerRequest {
        method: method.as_str(),
        canonical_path: uri.path(),
        query: uri.query().unwrap_or(""),
        body,
    };
    let now = unix_ms();
    if verify_owner_proof(&proof, &req, &key, now).is_err() {
        proof_refused(ProofKind::Owner, RefusalReason::Invalid);
        return Err(refused());
    }
    consume_nonce(state, ProofKind::Owner, proof.nonce, now).await
}

async fn consume_nonce(
    state: &AppState,
    kind: ProofKind,
    nonce: [u8; lakko_channels::owner::OWNER_PROOF_NONCE_LEN],
    now: u64,
) -> Result<(), Response> {
    match state.store.consume_owner_nonce(nonce, now).await {
        Ok(true) => Ok(()),
        Ok(false) => {
            proof_refused(kind, RefusalReason::Replayed);
            Err(refused())
        },
        Err(e) => {
            tracing::error!(error = %e, "owner nonce store failed; refusing");
            Err(internal())
        },
    }
}

/// Require a valid owner proof, or a valid proof by a co-publisher key the
/// owner appointed for `channel`, over this request.
///
/// # Errors
///
/// The response to return instead of running the handler: the same single
/// `401` as [`require_owner`] for every refusal, a `500` on a store failure.
pub async fn require_publisher(
    state: &AppState,
    channel: ChannelId,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), Response> {
    if headers.contains_key(OWNER_PROOF_HEADER_NAME) {
        return require_owner(state, channel, method, uri, headers, body).await;
    }
    let Some(proof) = headers
        .get(COPUBLISHER_PROOF_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| CoPublisherProof::parse(v).ok())
    else {
        proof_refused(ProofKind::CoPublisher, RefusalReason::Malformed);
        return Err(refused());
    };
    match state
        .store
        .is_copublisher(channel, proof.copublisher_key)
        .await
    {
        Ok(true) => {},
        Ok(false) => {
            proof_refused(ProofKind::CoPublisher, RefusalReason::UnknownKey);
            return Err(refused());
        },
        Err(e) => {
            tracing::error!(error = %e, "co-publisher key lookup failed");
            return Err(internal());
        },
    }
    let req = OwnerRequest {
        method: method.as_str(),
        canonical_path: uri.path(),
        query: uri.query().unwrap_or(""),
        body,
    };
    let now = unix_ms();
    if verify_copublisher_proof(&proof, &req, now).is_err() {
        proof_refused(ProofKind::CoPublisher, RefusalReason::Invalid);
        return Err(refused());
    }
    consume_nonce(state, ProofKind::CoPublisher, proof.proof.nonce, now).await
}
