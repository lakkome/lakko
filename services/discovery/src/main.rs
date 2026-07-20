// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Discovery service for Lakko.
//!
//! Phase 1.5 — username table only. Resolves `@username` to
//! `account_id` via OPRF-blinded queries (RFC 9497, Ristretto255-SHA512
//! ciphersuite). The server never stores plaintext usernames or sees
//! them in queries — only blinded curve points and OPRF outputs. See
//! `oprf.rs` for the protocol details and `registry.rs` for the
//! storage layer.
//!
//! KeyPackage bundle delivery has landed (see `keypackage.rs` —
//! `POST /discovery/keypackage` upload + `GET
//! /discovery/keypackage/{account_id}` fetch). Key Transparency
//! log inclusion proofs remain deferred to Phase 2 (no
//! transparency service is deployed to anchor identities).
//!
//! ## Phase 1.5b — auth-token gating
//!
//! `services/auth` issues HMAC-SHA256 bearer tokens on every
//! `*/finish` endpoint; this service verifies them on every
//! mutating write via `auth::require_session` middleware. Token
//! payload carries the authenticated `account_id`; handlers that
//! bind state to a specific account additionally check the body's
//! `account_id` matches what the token claims (anti-impersonation
//! when one account tries to register state under another's id).
//!
//! Read paths (`/discovery/oprf`, `/discovery/lookup`,
//! `/discovery/identity/{}`, `/discovery/keypackage/{}`,
//! `/discovery/invite/{}`) stay open — they take no account-bound
//! input and a leaked token would not let the attacker do
//! anything they couldn't do by knowing the public account_id or
//! blob_id anyway.
//!
//! ## Configuration
//!
//! Loaded from `/etc/lakko/lakko.env` via systemd `EnvironmentFile=`
//! in production; from `.env` via `dotenvy` in debug builds.
//!
//! - `LAKKO_BIND` — `host:port` to listen on (default `127.0.0.1:8084`)
//! - `LAKKO_DISCOVERY_DATABASE_URL` (or `DATABASE_URL` fallback) —
//!   sqlx-style MariaDB DSN; the database must be exclusive to this
//!   service per the per-service DB isolation rule.
//! - `LAKKO_DISCOVERY_POW_EFFORT_BITS` — PoW difficulty (default 8;
//!   0 in tests).
//! - `LAKKO_SESSION_TOKEN_SECRET` — base64-encoded HMAC-SHA256 key
//!   shared with `services/auth`. **Required** — middleware
//!   fails fast at startup if missing or shorter than 32 bytes
//!   after decode.
//! - `LAKKO_TRANSPARENCY_URL` — base URL of `services/transparency`
//!   (e.g. `http://127.0.0.1:8087`). **Optional** — when unset the
//!   KT-log S2 append integration is OFF (identity-key changes are not
//!   logged). When set, a best-effort POST is made to the transparency
//!   append endpoint on every identity-key CHANGE (fail-open — never
//!   fails the discovery upload).
//! - `LAKKO_TRANSPARENCY_APPEND_SECRET` — base64-encoded shared service
//!   secret (the SAME value `services/transparency` holds). Required to
//!   ARM the append integration alongside `LAKKO_TRANSPARENCY_URL`;
//!   presented as `Authorization: Bearer <secret-b64>`.
//! - `LAKKO_DISCOVERY_TRANSPARENCY_RECONCILE_SECS` — KT-log S2.5
//!   reconciliation-sweep interval (seconds, default 300). The background
//!   reconciler eventually appends every account's CURRENT published
//!   identity to the transparency log, closing the S2 best-effort
//!   fail-open gap. Spawned ONLY when the transparency integration is
//!   armed (`LAKKO_TRANSPARENCY_URL` set); `0` disables the sweep.
//! - `RUST_LOG` — `tracing-subscriber` env-filter spec.
//!
//! ## Endpoints
//!
//! - `GET    /health`              — liveness, 200 OK
//! - `GET    /ready`               — readiness (DB ping), 200 / 503
//! - `GET    /version`
//! - `POST   /pow/challenge`       — Equix PoW challenge (1.1e-style)
//! - `POST   /discovery/oprf`      — apply server static `K` to a blinded element
//! - `POST   /discovery/register`  — bind OPRF-hashed username to account_id
//! - `POST   /discovery/lookup`    — resolve OPRF-hashed username → account_id
//! - `POST   /discovery/identity`           — upsert public identity bundle (Phase 1.16d3a)
//! - `GET    /discovery/identity/{acct}`    — fetch public identity bundle (Phase 1.16d3a)
//! - `POST   /discovery/keypackage`         — upsert public MLS KeyPackage (Phase 1.16e1)
//! - `GET    /discovery/keypackage/{acct}`  — fetch public MLS KeyPackage (Phase 1.16e1)
//! - `POST   /discovery/invite`             — upload InviteCapability blob (Phase 1.18d1)
//! - `GET    /discovery/invite/{blob_id}`   — fetch InviteCapability blob (Phase 1.18d1)
//! - `POST   /discovery/invite/{blob_id}/consume` — atomic max_uses bump (Phase 1.18d2a)
//! - `DELETE /discovery/invite/{blob_id}`           — owner-signed revocation (Phase 1.18d2b)
//! - `POST   /discovery/call-invite`             — upload CallInviteCapability blob (Phase 3c3-server)
//! - `GET    /discovery/call-invite/{blob_id}`   — fetch CallInviteCapability blob (Phase 3c3-server)
//! - `POST   /discovery/call-invite/{blob_id}/consume` — atomic max_uses bump (Phase 3c3-server)
//! - `DELETE /discovery/call-invite/{blob_id}`         — owner-signed revocation (Phase 3c3-revoke)
//! - `POST   /discovery/group-invite`             — upload GroupInviteCapability blob (ADR-0028, G-3)
//! - `GET    /discovery/group-invite/{blob_id}`   — fetch GroupInviteCapability blob (ADR-0028, G-3)
//! - `POST   /discovery/group-invite/{blob_id}/consume` — atomic per-joiner consume (ADR-0028, G-3)

#![forbid(unsafe_code)]

mod account;
pub(crate) mod auth;
mod call_invite;
mod channel_names;
mod group_invite;
mod identity_bundle;
mod invite;
mod keypackage;
mod oprf;
mod pow;
mod rate_limit;
mod registry;
mod transparency_log;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use lakko_auth_token::TokenSecret;
use serde_json::json;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tokio::signal;
use tower_http::trace::TraceLayer;

/// Default bind address if `LAKKO_BIND` is not set. Distinct from
/// auth (8081), ds (8082), storage (8083).
const DEFAULT_BIND: &str = "127.0.0.1:8084";

/// Migrations directory (relative to crate root). `sqlx::migrate!`
/// reads it at compile time and embeds the SQL in the binary.
const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Per-request body limit. Discovery endpoints carry small JSON
/// (PoW proof + a couple of base64 fields) so 64 KB is plenty.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Shared application state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: MySqlPool,
    /// Server-static OPRF state — loaded (or generated on first run)
    /// at bootstrap. `Arc` so cloning the state for axum is free.
    pub(crate) oprf: Arc<oprf::LakkoOprfServer>,
    /// Phase 1.5b: shared HMAC secret for session-token verification.
    /// Same value as `services/auth`'s `state.session_token_secret`;
    /// the auth-token crate's `verify` consumes this on every
    /// protected request via the `auth::require_session` middleware.
    pub(crate) session_token_secret: Arc<TokenSecret>,
    /// G-discovery-ratelimit: per-IP token-bucket
    /// limiter for the anonymous OPRF + lookup
    /// endpoints. Defense in depth on top of the
    /// existing PoW gate.
    pub(crate) rate_limiter: Arc<rate_limit::RateLimiter>,
    /// ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (ii))
    /// — the per-account cool-down (seconds) between UNPROVEN
    /// AccountIdentity changes to a NEW key, blunting attacker churn on the
    /// lost-everything path. `0` disables the gate. From
    /// `LAKKO_DISCOVERY_FRESH_IDENTITY_COOLDOWN_SECS` (default 3600).
    pub(crate) fresh_identity_cooldown_secs: i64,
    /// KT-log S2 — the transparency-log append client. `Some` when
    /// `LAKKO_TRANSPARENCY_URL` is set; `None` when unset (the log
    /// integration is OFF — `identity_bundle::upload` skips the append
    /// entirely). A best-effort POST on every identity-key CHANGE; never
    /// fails the discovery upload (decision E, fail-open).
    pub(crate) transparency: Option<transparency_log::TransparencyClient>,
}

#[tokio::main]
async fn main() -> Result<()> {
    #[cfg(debug_assertions)]
    {
        if let Ok(path) = dotenvy::dotenv() {
            eprintln!("dotenv loaded from {}", path.display());
        }
    }

    init_tracing();

    let bind: SocketAddr = std::env::var("LAKKO_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_string())
        .parse()
        .context("parse LAKKO_BIND as host:port")?;

    // Per-service DB — see CLAUDE.md "Local dev". Falls back to the
    // generic `DATABASE_URL` for single-service runs in isolation.
    let db_url = std::env::var("LAKKO_DISCOVERY_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .context("LAKKO_DISCOVERY_DATABASE_URL or DATABASE_URL env var must be set")?;

    let pool = MySqlPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&db_url)
        .await
        .context("connect to MariaDB")?;

    MIGRATIONS.run(&pool).await.context("run sqlx migrations")?;

    let oprf_server = oprf::load_or_init_server_setup(&pool)
        .await
        .context("load OPRF server setup")?;

    // Phase 1.5b: session-token verifier. Same env var name +
    // shape as `services/auth` — operator-side parity matters so
    // a deploy step can write the same value to /etc/lakko/lakko.env
    // once. Failure here is fail-fast at startup; an unconfigured
    // discovery service would otherwise reject every authenticated
    // write with a cryptic 401.
    let session_token_secret_raw = std::env::var("LAKKO_SESSION_TOKEN_SECRET")
        .context("LAKKO_SESSION_TOKEN_SECRET env var must be set (base64-encoded HMAC-SHA256 key, ≥ 32 bytes after decode)")?;
    let session_token_secret = TokenSecret::from_base64(&session_token_secret_raw)
        .context("parse LAKKO_SESSION_TOKEN_SECRET as base64")?;

    // G-discovery-ratelimit: per-IP token bucket on the
    // anonymous OPRF / lookup endpoints. Defaults match
    // `rate_limit::DEFAULT_*`; overridable via env.
    let rate_per_min: f64 = parse_env_f64(
        "LAKKO_DISCOVERY_RATE_LIMIT_PER_MINUTE",
        rate_limit::DEFAULT_RATE_PER_MINUTE,
    );
    let rate_burst: f64 = parse_env_f64(
        "LAKKO_DISCOVERY_RATE_LIMIT_BURST",
        rate_limit::DEFAULT_BURST,
    );
    let rate_limiter = Arc::new(rate_limit::RateLimiter::new(rate_per_min, rate_burst));
    tracing::info!(rate_per_min, rate_burst, "discovery rate limiter armed",);

    // ADR-0017 (ii) — fresh-identity-publish cool-down (seconds). Default
    // 1 h; 0 disables. A negative value is clamped to 0 (disabled).
    let fresh_identity_cooldown_secs: i64 =
        std::env::var("LAKKO_DISCOVERY_FRESH_IDENTITY_COOLDOWN_SECS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(3600)
            .max(0);
    tracing::info!(
        fresh_identity_cooldown_secs,
        "identity fresh-publish cool-down armed",
    );

    // KT-log S2 — transparency-log append integration. Both vars must be
    // set to arm it: `LAKKO_TRANSPARENCY_URL` (the service base URL) and
    // `LAKKO_TRANSPARENCY_APPEND_SECRET` (the SAME shared secret the
    // transparency service holds). When the URL is unset the integration
    // is OFF (the append is skipped per request) — logged once here.
    let transparency = build_transparency_client();
    if transparency.is_some() {
        tracing::info!("KT-log S2: transparency append integration armed");
    } else {
        tracing::info!(
            "KT-log S2: transparency append integration OFF \
             (LAKKO_TRANSPARENCY_URL unset) — identity-key changes are not logged"
        );
    }

    let state = AppState {
        db: pool.clone(),
        oprf: Arc::new(oprf_server),
        session_token_secret: Arc::new(session_token_secret),
        rate_limiter: Arc::clone(&rate_limiter),
        fresh_identity_cooldown_secs,
        transparency,
    };

    let _pow_pruner = pow::spawn_pruner(pool.clone());
    // Phase 1.18d2c — hourly sweep of expired invite blobs.
    // The `idx_expiry` index from migration 0004 keeps the
    // recurring DELETE cheap; `expiry_ms = 0` (NEVER_EXPIRES)
    // rows are preserved by design — the codec layer surfaces
    // the stalking-risk warning, the storage layer doesn't
    // second-guess.
    let _invite_pruner = invite::spawn_pruner(pool.clone());
    // ADR-0028 (G-3) — hourly sweep of expired group-invite blobs.
    // The `idx_group_invite_expiry` index from migration 0014 keeps
    // the recurring DELETE cheap; `expiry_ms = 0`
    // (GROUP_INVITE_NEVER_EXPIRES) rows are preserved by design, and
    // the FK CASCADE on `group_invite_consumers` reaps the children.
    let _group_invite_pruner = group_invite::spawn_pruner(pool.clone());
    // G-discovery-ratelimit: drop idle per-IP buckets so
    // the limiter's memory ceiling stays bounded.
    let _rate_pruner = rate_limit::spawn_pruner((*rate_limiter).clone());

    // KT-log S2.5 — the transparency-log reconciliation sweep. Spawned
    // ONLY when the transparency integration is armed; best-effort +
    // idempotent, so it never blocks startup or any request path.
    let _transparency_reconciler = maybe_spawn_reconciler(state.transparency.clone(), pool.clone());

    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, "discovery service listening");

    axum::serve(
        listener,
        // ConnectInfo so the rate-limit middleware can
        // read the source IP when X-Forwarded-For is
        // absent (dev / direct binding).
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("axum serve")?;

    pool.close().await;
    tracing::info!("discovery service stopped cleanly");
    Ok(())
}

/// Build the public router. Extracted so tests can drive it directly
/// without binding a TCP listener.
///
/// Phase 1.5b: protected routes (state-mutating writes that bind
/// to a specific account_id) sit behind the `require_session`
/// middleware. Read paths + `/pow/challenge` + `/discovery/oprf`
/// (which takes no account_id and is cheap-by-PoW) stay open.
/// `invite::consume` is also open — anyone holding the URL can
/// already exercise the consume counter, and the joiner doesn't
/// necessarily have an auth token yet (a fresh-install joiner
/// taps a Universal Link before they've signed up).
pub(crate) fn build_app(state: AppState) -> Router {
    use axum::middleware;

    // Protected routes — every write that binds to a specific
    // account_id requires a valid session token. The handler is
    // additionally responsible for checking the body's account_id
    // matches the token's (anti-impersonation).
    let protected = Router::new()
        .route("/discovery/register", post(registry::register))
        // A3 (GDPR erasure) — purge this account's discovery footprint
        // (username binding, identity bundle, KeyPackage, owned invites).
        // Session-token-gated; the account_id comes from the verified token.
        .route(
            "/discovery/account",
            axum::routing::delete(account::delete_account),
        )
        .route("/discovery/identity", post(identity_bundle::upload))
        .route("/discovery/keypackage", post(keypackage::upload))
        .route("/discovery/invite", post(invite::upload))
        // Phase 3c3-server — call-invite blob upload. Session-
        // token-gated like the contact-invite upload above; the
        // handler additionally enforces capability-owner /
        // token-account match (anti-impersonation).
        .route("/discovery/call-invite", post(call_invite::upload))
        // Phase 3c3-revoke — owner-signed call-invite delete.
        // Body carries Ed25519 signature over the canonical
        // bytes per `lakko_contacts::call_invite::
        // call_revoke_canonical_bytes`. Session-token-gated
        // for DoS rate-limiting; the signature is the actual
        // authorisation gate.
        .route(
            "/discovery/call-invite/{blob_id}",
            axum::routing::delete(call_invite::revoke),
        )
        // ADR-0028 (G-3) — group join-ticket blob upload. Session-
        // token-gated as a DoS / logged-in-user rate-limit; there
        // is no owner-account binding on the capability to enforce
        // (the issuer is identified by its Ed25519 pubkey only).
        .route("/discovery/group-invite", post(group_invite::upload))
        // ADR-0028 revoke — issuer-signed group-invite delete. Body carries the
        // Ed25519 signature over the group-invite revoke canonical bytes;
        // session-token-gated for DoS rate-limiting, the signature is the
        // authorisation gate (verified against the stored capability's
        // issuer_signature_pubkey).
        .route(
            "/discovery/group-invite/{blob_id}",
            axum::routing::delete(group_invite::revoke),
        )
        // `DELETE /discovery/invite/{blob_id}` is owner-signed via
        // its body, but we ALSO require a session token: the
        // signature proves "owner of the capability", the token
        // proves "logged-in Lakko user", and the union is strictly
        // stronger than the signature alone (rate-limits a stolen
        // signing seed to one account).
        .route(
            "/discovery/invite/{blob_id}",
            axum::routing::delete(invite::revoke),
        )
        // Phase 1.18d2b-list — owner-signed POST that returns
        // every live invite for the calling account. Body
        // carries the Ed25519 signature; the session-token
        // middleware here is a DoS gate, same role as on
        // `revoke` above.
        .route(
            "/discovery/invite/by-owner",
            post(invite::list_by_owner),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ));

    // G-discovery-ratelimit — per-IP token bucket on the
    // three enumeration-vector endpoints. PoW remains the
    // primary work proof; rate limiting is defense in
    // depth that caps per-IP server CPU regardless of
    // how many PoW tokens an attacker mints.
    let rate_limited = Router::new()
        .route("/discovery/oprf", post(oprf::evaluate))
        .route("/discovery/lookup", post(registry::lookup))
        .route("/discovery/channel/lookup", post(channel_names::lookup))
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state.rate_limiter),
            rate_limit::middleware,
        ));

    // Open routes — no auth required, no rate limit.
    // Endpoints excluded from the limiter above:
    //   * /health, /ready, /version — no compute.
    //   * /pow/challenge — itself the primary anti-spam
    //     layer; rate-limiting it would interfere with
    //     legitimate retries on poor networks.
    //   * /discovery/identity/{id} + keypackage/{id} —
    //     by-id reads, no enumeration vector (the id is
    //     a 32-byte pseudonym).
    //   * /discovery/invite, /discovery/call-invite,
    //     /discovery/group-invite — by-blob-id reads, same shape.
    //   * /discovery/channel/register — write that's
    //     UNIQUE-key-gated server-side.
    let open = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route("/pow/challenge", post(pow::issue_challenge))
        .route(
            "/discovery/identity/{account_id}",
            get(identity_bundle::fetch),
        )
        .route("/discovery/keypackage/{account_id}", get(keypackage::fetch))
        // Phase 1.18d1 — capability-based invite-link blob fetch +
        // consume. Both are unauthenticated by design: a joiner
        // tapping a Universal Link on a fresh install may not be
        // signed up yet.
        .route("/discovery/invite/{blob_id}", get(invite::fetch))
        .route(
            "/discovery/invite/{blob_id}/consume",
            post(invite::consume),
        )
        // Phase 3c3-server — call-invite fetch + consume.
        // Anonymous mirror of the contact-invite open routes
        // above; joiner needs to access the capability before
        // they have an account-bound session.
        .route(
            "/discovery/call-invite/{blob_id}",
            get(call_invite::fetch),
        )
        .route(
            "/discovery/call-invite/{blob_id}/consume",
            post(call_invite::consume),
        )
        // ADR-0028 (G-3) — group join-ticket fetch + consume.
        // Anonymous mirror of the call-invite open routes above; a
        // joiner needs the capability before they have an
        // account-bound session, and the consume dedup tag is
        // self-authenticating (PoW-gated, server-blind).
        .route(
            "/discovery/group-invite/{blob_id}",
            get(group_invite::fetch),
        )
        .route(
            "/discovery/group-invite/{blob_id}/consume",
            post(group_invite::consume),
        )
        // G4-5 — channel @name discovery via OPRF.
        // Register stays on the open path (no
        // enumeration vector — UNIQUE(oprf_hash) gates
        // anti-squatting). Lookup moved to `rate_limited`
        // above.
        .route(
            "/discovery/channel/register",
            post(channel_names::register),
        );

    open.merge(rate_limited)
        .merge(protected)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Build the KT-log S2 transparency-append client from the environment.
///
/// Returns `Some` only when BOTH `LAKKO_TRANSPARENCY_URL` and
/// `LAKKO_TRANSPARENCY_APPEND_SECRET` are set and non-empty; otherwise
/// `None` (the integration is off — the append is skipped per request).
/// A missing append secret while the URL is set is a misconfiguration we
/// surface with a `warn!` and treat as off, rather than appending without
/// auth (which the transparency side would reject with 401 anyway).
fn build_transparency_client() -> Option<transparency_log::TransparencyClient> {
    let url = std::env::var("LAKKO_TRANSPARENCY_URL")
        .ok()
        .filter(|s| !s.is_empty())?;
    let Some(secret) = std::env::var("LAKKO_TRANSPARENCY_APPEND_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        tracing::warn!(
            "LAKKO_TRANSPARENCY_URL is set but LAKKO_TRANSPARENCY_APPEND_SECRET is not — \
             KT-log S2 append integration stays OFF (an unauthenticated append would be \
             rejected with 401)"
        );
        return None;
    };
    match transparency_log::TransparencyClient::new(&url, &secret) {
        Ok(client) => Some(client),
        Err(e) => {
            tracing::warn!(error = %e, "failed to build transparency append client — KT-log S2 OFF");
            None
        },
    }
}

/// KT-log S2.5 — spawn the transparency reconciliation sweep when both the
/// integration is armed (`transparency` is `Some`) AND the interval is
/// non-zero. Returns the task handle to hold (so the task lives for the
/// process), or `None` when the integration is off or the sweep is disabled
/// (`LAKKO_DISCOVERY_TRANSPARENCY_RECONCILE_SECS=0`). Logs the decision once.
fn maybe_spawn_reconciler(
    transparency: Option<transparency_log::TransparencyClient>,
    pool: MySqlPool,
) -> Option<tokio::task::JoinHandle<()>> {
    let client = transparency?;
    let Some(interval) = reconcile_interval() else {
        tracing::info!(
            "KT-log S2.5: transparency reconciler DISABLED \
             (LAKKO_DISCOVERY_TRANSPARENCY_RECONCILE_SECS=0)"
        );
        return None;
    };
    tracing::info!(
        interval_secs = interval.as_secs(),
        "KT-log S2.5: transparency reconciler armed",
    );
    Some(transparency_log::spawn_transparency_reconciler(
        pool, client, interval,
    ))
}

/// KT-log S2.5 — the reconciliation-sweep interval from
/// `LAKKO_DISCOVERY_TRANSPARENCY_RECONCILE_SECS`.
///
/// Returns `Some(interval)` for a positive value (default 300 s when
/// unset/unparseable), or `None` when set to `0` — meaning the reconciler
/// is DISABLED and must not be spawned. A negative or unparseable value
/// falls back to the default rather than disabling, so a typo does not
/// silently turn the sweep off.
fn reconcile_interval() -> Option<Duration> {
    /// Default reconciliation cadence (seconds) when the env var is unset.
    const DEFAULT_RECONCILE_SECS: u64 = 300;

    let secs = match std::env::var("LAKKO_DISCOVERY_TRANSPARENCY_RECONCILE_SECS") {
        Ok(raw) if !raw.is_empty() => raw.parse::<u64>().unwrap_or_else(|_| {
            tracing::warn!(
                value = %raw,
                default = DEFAULT_RECONCILE_SECS,
                "could not parse LAKKO_DISCOVERY_TRANSPARENCY_RECONCILE_SECS as u64 \
                 — falling back to the default",
            );
            DEFAULT_RECONCILE_SECS
        }),
        _ => DEFAULT_RECONCILE_SECS,
    };
    (secs != 0).then(|| Duration::from_secs(secs))
}

/// Parse a float env var, falling back to `default`
/// on absent/invalid values (logs a warning so the
/// operator notices a typo).
fn parse_env_f64(name: &'static str, default: f64) -> f64 {
    match std::env::var(name) {
        Ok(raw) if !raw.is_empty() => match raw.parse::<f64>() {
            Ok(v) if v > 0.0 => v,
            _ => {
                tracing::warn!(
                    env = name,
                    value = %raw,
                    default,
                    "could not parse env var as positive f64 — falling back to default",
                );
                default
            },
        },
        _ => default,
    }
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.db)
        .await
    {
        Ok(_) => (StatusCode::OK, "ready\n").into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "readiness probe: db ping failed");
            (StatusCode::SERVICE_UNAVAILABLE, "not ready\n").into_response()
        },
    }
}

async fn version() -> impl IntoResponse {
    Json(json!({
        "service": "lakko-svc-discovery",
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": option_env!("LAKKO_GIT_SHA").unwrap_or("unknown"),
    }))
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_level(true)
        .compact()
        .init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) = signal::unix::signal(signal::unix::SignalKind::terminate()) {
            sig.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("received SIGINT, shutting down"),
        () = terminate => tracing::info!("received SIGTERM, shutting down"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    /// Build a router that doesn't touch the DB — for /health,
    /// /version, /not-found probes that work without a live pool.
    fn test_app_no_db() -> Router {
        Router::new()
            .route("/health", get(health))
            .route("/version", get(version))
    }

    #[tokio::test]
    async fn health_returns_200_ok() {
        let response = test_app_no_db()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"ok\n");
    }

    #[tokio::test]
    async fn version_returns_json_with_service_name() {
        let response = test_app_no_db()
            .oneshot(
                Request::builder()
                    .uri("/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["service"], "lakko-svc-discovery");
    }

    /// KT-log S2 (decision E) — when `LAKKO_TRANSPARENCY_URL` is unset the
    /// integration is OFF: `build_transparency_client` returns `None`
    /// cleanly (no panic, no append attempted). This mutates a process-
    /// global env var, so it removes both vars first to isolate from any
    /// ambient `.env`.
    #[test]
    fn transparency_client_off_when_url_unset() {
        // SAFETY of correctness (not memory): single isolated assertion;
        // we clear the URL var so the function takes the `None` path
        // regardless of the host environment.
        std::env::remove_var("LAKKO_TRANSPARENCY_URL");
        assert!(super::build_transparency_client().is_none());
    }
}
