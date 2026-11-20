// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The moderation service: report intake and receipts (ADR-0098 section 2).
//!
//! ## Endpoints
//!
//! - `GET  /moderation/moderators` — the root-signed moderator list, raw
//!   bytes (`application/octet-stream`). A client verifies it under its
//!   pinned root key before sealing a report to it.
//! - `POST /moderation/report` — a report as `lakko_moderation::report`
//!   composes it (raw bytes). No session; one anonymous token of context
//!   `report` in `x-lakko-anon-token`, spent only after the body is checked,
//!   so a malformed report costs none. Answers `201 {"receipt_id": ...}`.
//! - `GET  /moderation/report/{receipt_id}` — `{"status": "received" |
//!   "action" | "none"}`. Says neither what action nor against whom.
//! - `GET /moderation/log/operator-key`, `/moderation/log/root`,
//!   `/moderation/log/entry/{index}`, `/moderation/log/consistency` — the
//!   moderation log ([`log_routes`], ADR-0098 section 5).
//! - `GET /moderation/snapshot/{period}/bucket/{prefix}` — one bucket of a
//!   suspension snapshot with its proofs ([`log_routes`], [`snapshots`],
//!   ADR-0098 step 4). The service learns the 16-bit prefix a client asks
//!   for, nothing more.
//! - `GET /health`, `/ready`, `/version`.
//!
//! The report, log and snapshot routes are behind a per-network limit ([`ip_limit`]).
//! Moderator statements come in on a separate internal listener
//! ([`internal`]); the moderators' panel is a third listener ([`panel`]).
//!
//! ## What the service learns
//!
//! The clear part of a report: target, reason, conversation kind, evidence
//! count. The sealed part stays sealed; this service never opens it and
//! holds no moderator key. A report row keeps the day it arrived, not the
//! time, and names no account, address or token.
//!
//! ## Dormant
//!
//! With no moderator list configured, `/moderation/moderators` and
//! `/moderation/report` answer `503 {"error": "reporting-unavailable"}`,
//! the state the client shows as "Reporting isn't available on this
//! server" (UX `reporting-and-moderation.md` 2.3).

#![forbid(unsafe_code)]

pub mod internal;
pub mod ip_limit;
pub mod list;
pub mod log;
pub mod log_routes;
pub mod log_store;
pub mod panel;
pub mod panel_routes;
pub mod panel_view;
pub mod reports;
pub mod snapshots;
pub mod store;
pub mod witness_client;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use lakko_anon_token_server::TokenVerifier;
use serde_json::json;
use tower_http::trace::TraceLayer;

/// Largest report body accepted. The sealed content is bounded at 256 KiB
/// by `lakko_moderation::seal`, plus at most 64 key wraps and the header;
/// this leaves room for both. A report over the core's bounds is refused by
/// the decoder anyway.
pub const MAX_REPORT_BYTES: usize = 300 * 1024;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    /// Where reports are kept.
    pub store: Arc<dyn store::ReportStore>,
    /// The current moderator list, or `None` when reporting is not
    /// configured on this server.
    pub list: Option<Arc<list::LoadedList>>,
    /// The anonymous-token verifier (context `report`).
    pub tokens: TokenVerifier,
    /// The per-network limit of the report and log routes.
    pub limits: Arc<ip_limit::IpLimiter>,
    /// The moderation log.
    pub log: Arc<log::ModerationLog>,
    /// The suspension snapshots held.
    pub snapshots: Arc<snapshots::Snapshots>,
}

/// Build the router. Extracted so tests can drive it directly.
pub fn build_app(state: AppState) -> Router {
    let limited = Router::new()
        .route("/moderation/moderators", get(list::moderators))
        .route("/moderation/report", post(reports::submit))
        .route("/moderation/report/{receipt_id}", get(reports::status))
        .route(
            "/moderation/log/operator-key",
            get(log_routes::operator_key),
        )
        .route("/moderation/log/root", get(log_routes::root))
        .route("/moderation/log/entry/{index}", get(log_routes::entry))
        .route("/moderation/log/consistency", get(log_routes::consistency))
        .route(
            "/moderation/snapshot/{period}/bucket/{prefix}",
            get(log_routes::snapshot_bucket),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.limits.clone(),
            ip_limit::middleware,
        ));
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .merge(limited)
        .layer(DefaultBodyLimit::max(MAX_REPORT_BYTES))
        // Requests by route template, method and status class (ADR-0096).
        .layer(lakko_metrics::red_layer("moderation"))
        .layer(TraceLayer::new_for_http().make_span_with(lakko_log_hygiene::request_span))
        .with_state(state)
}

/// A JSON error body `{"error": code}`.
pub(crate) fn error(status: StatusCode, code: &'static str) -> axum::response::Response {
    (status, Json(json!({ "error": code }))).into_response()
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    if state.store.ping().await {
        (StatusCode::OK, "ready\n").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready\n").into_response()
    }
}

async fn version() -> impl IntoResponse {
    Json(json!({
        "service": "lakko-svc-moderation",
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": option_env!("LAKKO_GIT_SHA").unwrap_or("unknown"),
    }))
}

#[cfg(test)]
mod log_tests;
#[cfg(test)]
mod panel_tests;
#[cfg(test)]
mod snapshot_tests;
#[cfg(test)]
mod store_tests;
#[cfg(test)]
mod tests;
