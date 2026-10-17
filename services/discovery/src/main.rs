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
//! ## ADR-0054 S3 — anonymous tokens instead of proof of work
//!
//! No route here asks for a proof of work any more. The anonymous
//! actions that were rationed by one spend an anonymous token, carried
//! as `x-lakko-anon-token` and checked by `lakko_anon_token_server`'s
//! gate before the handler runs:
//!
//! - `POST /discovery/oprf` — context `discovery-lookup` (the OPRF
//!   evaluation is the enumeration step; `lookup` and `channel/lookup`
//!   follow it and keep only the per-IP limiter);
//! - `POST /discovery/channel/register` — context `discovery-register`;
//! - the three `POST .../{blob_id}/consume` routes — context
//!   `invite-consume`.
//!
//! The session-gated writes carry no proof of work either; they are
//! counted per account (`rate_limit::write_middleware`).
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
//! - `LAKKO_ANON_TOKEN_MASTER_SECRET` — 64 hex characters, the SAME value
//!   `services/auth` issues under. Unset: every token route answers
//!   503 `anon-tokens-disabled` (loud startup warning). Malformed:
//!   startup fails.
//! - `LAKKO_ANON_TOKEN_WINDOW_SECS` — the token epoch window (default
//!   86400); must match auth.
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
//! - `LAKKO_DISCOVERY_RATE_LIMIT_PER_MINUTE` / `_BURST` — per-IP bucket for
//!   requests without a valid session token on the OPRF / lookup endpoints
//!   (default 60 / 10).
//! - `LAKKO_DISCOVERY_ACCOUNT_RATE_LIMIT_PER_MINUTE` / `_BURST` — per-account
//!   bucket for requests with one (default 60 / 10); the session-gated writes
//!   get a separate bucket of the same size.
//! - `LAKKO_DISCOVERY_AUTHENTICATED_IP_RATE_LIMIT_PER_MINUTE` / `_BURST` —
//!   per-IP ceiling for requests with one (default 600 / 100).
//! - `LAKKO_DISCOVERY_USERNAME_CERT_SEED_HEX` — dev / e2e only: a fixed
//!   64-hex seed for the username-certificate key (ADR-0060), so a test can
//!   pin its verifying key. Unset in production: the key is generated on
//!   first boot and persisted. Set: a loud startup warning.
//! - `LAKKO_DISCOVERY_ATTEST_KEY` — base64 of auth's 32-byte Ed25519 attest
//!   verifying key (ADR-0073 S3), pinned at deploy time; a key-reset
//!   attestation on `POST /discovery/identity` is verified under it. Unset:
//!   an attested upload answers 503 `key-reset-disabled` (loud startup
//!   warning), and since no unattested key change is accepted any more, a
//!   published key can then change only through a supersession proof.
//!   Malformed: startup fails.
//! - `LAKKO_DISCOVERY_RENAME_INTERVAL_SECS` — minimum seconds between two
//!   renames of one account (ADR-0076 D2; default 2592000 = 30 days, chosen
//!   not measured; 0 = no minimum).
//! - `LAKKO_DISCOVERY_RETIRED_NAME_HOLD_SECS` — seconds a name an account
//!   renamed away from stays unclaimable before the hourly pruner frees it
//!   (ADR-0076 D1; default 2592000 = 30 days, chosen not measured).
//! - `RUST_LOG` — `tracing-subscriber` env-filter spec.
//!
//! ## Endpoints
//!
//! - `GET    /health`              — liveness, 200 OK
//! - `GET    /ready`               — readiness (DB ping), 200 / 503
//! - `GET    /version`
//! - `POST   /discovery/oprf`      — apply server static `K` to a blinded element (token)
//! - `POST   /discovery/register`  — bind an account's FIRST OPRF-hashed username
//!   (`409 username-taken` / `409 username-already-set`, ADR-0076)
//! - `POST   /discovery/username/replace` — rename: retire the current name and bind another
//!   (ADR-0076; `409 no-username` / `username-taken`, `429 rename-too-soon`)
//! - `POST   /discovery/lookup`    — resolve OPRF-hashed username → account_id (Everyone only, ADR-0060)
//! - `PUT    /discovery/visibility`  — enter / leave Everyone, publish name terms (ADR-0060)
//! - `POST   /discovery/name/lookup` — accounts in Everyone with one name term (ADR-0060)
//! - `GET    /discovery/username-certificate` — this account's username certificate (ADR-0060)
//! - `GET    /.well-known/lakko-username-cert-key` — the certificate verifying key (ADR-0060)
//! - `POST   /discovery/identity`           — upsert public identity bundle (Phase 1.16d3a)
//! - `GET    /discovery/identity/{acct}`    — fetch public identity bundle (Phase 1.16d3a)
//! - `POST   /discovery/keypackage`         — upsert one device's MLS KeyPackage (ADR-0050)
//! - `DELETE /discovery/keypackage/device/{dev}` — remove one device's KeyPackage (ADR-0050)
//! - `GET    /discovery/keypackage/{acct}`  — fetch every device's KeyPackage (ADR-0050)
//! - `POST   /discovery/invite`             — upload InviteCapability blob (Phase 1.18d1)
//! - `GET    /discovery/invite/{blob_id}`   — fetch InviteCapability blob (Phase 1.18d1)
//! - `POST   /discovery/invite/{blob_id}/consume` — atomic max_uses bump (Phase 1.18d2a, token)
//! - `DELETE /discovery/invite/{blob_id}`           — owner-signed revocation (Phase 1.18d2b)
//! - `POST   /discovery/call-invite`             — upload CallInviteCapability blob (Phase 3c3-server)
//! - `GET    /discovery/call-invite/{blob_id}`   — fetch CallInviteCapability blob (Phase 3c3-server)
//! - `POST   /discovery/call-invite/{blob_id}/consume` — atomic max_uses bump (Phase 3c3-server, token)
//! - `DELETE /discovery/call-invite/{blob_id}`         — owner-signed revocation (Phase 3c3-revoke)
//! - `POST   /discovery/group-invite`             — upload GroupInviteCapability blob (ADR-0028, G-3)
//! - `GET    /discovery/group-invite/{blob_id}`   — fetch GroupInviteCapability blob (ADR-0028, G-3)
//! - `POST   /discovery/group-invite/{blob_id}/consume` — atomic per-joiner consume (ADR-0028, G-3, token)
//! - `POST   /discovery/channel/register`     — bind an OPRF-hashed channel name (G4-5, token)
//! - `POST   /discovery/channel/lookup`       — resolve an OPRF-hashed channel name (G4-5)

#![forbid(unsafe_code)]

mod account;
pub(crate) mod auth;
mod call_invite;
mod channel_names;
mod group_invite;
mod identity_bundle;
mod invite;
mod key_reset;
mod keypackage;
mod ledger_id;
mod locate;
mod oprf;
mod rate_limit;
mod registry;
mod transparency_log;
mod visibility;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use lakko_anon_token::TokenContext;
use lakko_anon_token_server::{require_token, AnonTokenServerConfig, TokenVerifier};
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
/// (a couple of base64 fields) so 64 KB is plenty.
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
    /// G-discovery-ratelimit: token-bucket limits for the
    /// anonymous OPRF + lookup endpoints — per account plus a
    /// per-IP ceiling with a session token, per IP without one.
    /// Plus the per-account bucket of the session-gated writes.
    pub(crate) rate_limits: Arc<rate_limit::DiscoveryLimits>,
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
    /// ADR-0054 S3 — the anonymous-token verifier and this service's
    /// spent-set (`anon_token_spent`). Dormant (every token route 503)
    /// when `LAKKO_ANON_TOKEN_MASTER_SECRET` is unset.
    pub(crate) anon_tokens: TokenVerifier,
    /// ADR-0060 — the dedicated Ed25519 key username certificates are signed
    /// with (`username_cert_signing_key`, or the dev override
    /// `LAKKO_DISCOVERY_USERNAME_CERT_SEED_HEX`).
    pub(crate) username_cert_key: Arc<lakko_crypto::signing::SigningKey>,
    /// ADR-0073 S3 — the auth attest VERIFYING key pinned from
    /// `LAKKO_DISCOVERY_ATTEST_KEY`, which a key-reset attestation on an
    /// identity upload is verified under. `None`: an attested upload answers
    /// `503 key-reset-disabled`, so no published key can change without a
    /// supersession proof.
    pub(crate) key_reset_attest_key: Option<lakko_crypto::signing::VerifyingKey>,
    /// ADR-0076 — how often an account may rename and how long a retired
    /// name stays unclaimable (both 30 days by default; chosen).
    pub(crate) name_policy: registry::NamePolicy,
    /// ADR-0080 — the city database `GET /discovery/locate` reads. Knows
    /// nothing when `LAKKO_DISCOVERY_CITY_DB` is unset or unreadable.
    pub(crate) city_lookup: Arc<dyn locate::CityLookup>,
}

/// G-discovery-ratelimit: token buckets on the anonymous OPRF / lookup
/// endpoints. Defaults are `rate_limit::DEFAULT_*`; each is overridable via env.
fn rate_limits_from_env(session_token_secret: Arc<TokenSecret>) -> rate_limit::DiscoveryLimits {
    let env_limit = |rate_var: &'static str, burst_var: &'static str, rate: f64, burst: f64| {
        rate_limit::Limit {
            per_minute: parse_env_f64(rate_var, rate),
            burst: parse_env_f64(burst_var, burst),
        }
    };
    let anonymous_ip_limit = env_limit(
        "LAKKO_DISCOVERY_RATE_LIMIT_PER_MINUTE",
        "LAKKO_DISCOVERY_RATE_LIMIT_BURST",
        rate_limit::DEFAULT_RATE_PER_MINUTE,
        rate_limit::DEFAULT_BURST,
    );
    let account_limit = env_limit(
        "LAKKO_DISCOVERY_ACCOUNT_RATE_LIMIT_PER_MINUTE",
        "LAKKO_DISCOVERY_ACCOUNT_RATE_LIMIT_BURST",
        rate_limit::DEFAULT_ACCOUNT_RATE_PER_MINUTE,
        rate_limit::DEFAULT_ACCOUNT_BURST,
    );
    let authenticated_ip_limit = env_limit(
        "LAKKO_DISCOVERY_AUTHENTICATED_IP_RATE_LIMIT_PER_MINUTE",
        "LAKKO_DISCOVERY_AUTHENTICATED_IP_RATE_LIMIT_BURST",
        rate_limit::DEFAULT_AUTHENTICATED_IP_RATE_PER_MINUTE,
        rate_limit::DEFAULT_AUTHENTICATED_IP_BURST,
    );
    tracing::info!(
        ?anonymous_ip_limit,
        ?account_limit,
        ?authenticated_ip_limit,
        "discovery rate limits armed"
    );
    rate_limit::DiscoveryLimits::new(
        anonymous_ip_limit,
        account_limit,
        authenticated_ip_limit,
        session_token_secret,
    )
}

#[tokio::main]
#[allow(clippy::too_many_lines)] // flat startup wiring reads best whole
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

    // ADR-0060 — the username-certificate signing key.
    let username_cert_key = visibility::load_or_init_cert_key(&pool)
        .await
        .context("load username certificate signing key")?;

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

    // ADR-0073 S3 — the pinned auth attest key. A malformed value refuses
    // startup; an unset one is loud, since it leaves every key change that is
    // not a supersession proof unreachable.
    let key_reset_attest_key = key_reset::attest_key_from_lookup(|name| std::env::var(name).ok())
        .map_err(anyhow::Error::msg)
        .context("key-reset attest key")?;
    if key_reset_attest_key.is_some() {
        tracing::info!("ADR-0073: key-reset attest key pinned; attested identity resets accepted");
    } else {
        tracing::warn!(
            "{} unset - an identity upload carrying a key-reset attestation answers 503 \
             key-reset-disabled, and no published key can change without a supersession proof",
            key_reset::ATTEST_KEY_ENV
        );
    }

    let session_token_secret = Arc::new(session_token_secret);
    let rate_limits = Arc::new(rate_limits_from_env(Arc::clone(&session_token_secret)));

    // ADR-0054 S3 — anonymous tokens. A malformed secret or window refuses
    // startup; an unset secret leaves the token routes answering 503.
    let anon_tokens = TokenVerifier::new(
        pool.clone(),
        AnonTokenServerConfig::from_env().context("anonymous-token configuration")?,
    );
    anon_tokens.log_startup("discovery");

    let name_policy = load_name_policy()?;

    let state = AppState {
        db: pool.clone(),
        oprf: Arc::new(oprf_server),
        session_token_secret,
        rate_limits: Arc::clone(&rate_limits),
        fresh_identity_cooldown_secs,
        transparency,
        anon_tokens,
        username_cert_key: Arc::new(username_cert_key),
        key_reset_attest_key,
        name_policy,
        city_lookup: locate::city_lookup_from_env(),
    };

    let _spent_pruner = state.anon_tokens.spawn_pruner();
    // Phase 1.18d2c — hourly sweep of expired invite blobs.
    // The `idx_expiry` index from migration 0004 keeps the
    // recurring DELETE cheap; `expiry_ms = 0` (NEVER_EXPIRES)
    // rows are preserved by design — the codec layer surfaces
    // the stalking-risk warning, the storage layer doesn't
    // second-guess.
    let _invite_pruner = invite::spawn_pruner(pool.clone());
    // ADR-0032 guest lifecycle — what an account nobody named published here
    // goes a day after it was published.
    let _unclaimed_pruner = account::spawn_unclaimed_pruner(
        pool.clone(),
        account::unclaimed_ttl_from_env()
            .context("load LAKKO_DISCOVERY_UNCLAIMED_ACCOUNT_TTL_SECS")?,
    );
    // ADR-0028 (G-3) — hourly sweep of expired group-invite blobs.
    // The `idx_group_invite_expiry` index from migration 0014 keeps
    // the recurring DELETE cheap; `expiry_ms = 0`
    // (GROUP_INVITE_NEVER_EXPIRES) rows are preserved by design, and
    // the FK CASCADE on `group_invite_consumers` reaps the children.
    let _group_invite_pruner = group_invite::spawn_pruner(pool.clone());
    // G-discovery-ratelimit: drop idle buckets so the
    // limiters' memory ceiling stays bounded.
    // ADR-0074: with it, the tombstones of deleted device ids (30 days).
    // ADR-0076: and the usernames retired longer ago than the hold.
    let _pruners =
        spawn_bucket_and_tombstone_pruners(&rate_limits, &pool, name_policy.retired_hold_secs);

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
        // read the source IP when nginx's X-Real-IP /
        // X-Forwarded-For are absent (dev / direct binding).
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("axum serve")?;

    pool.close().await;
    tracing::info!("discovery service stopped cleanly");
    Ok(())
}

/// The protected routes of [`build_app`]: every write bound to one account,
/// behind the session middleware and the per-account write limiter.
fn protected_routes(state: &AppState) -> Router<AppState> {
    use axum::middleware;

    // Protected routes — every write that binds to a specific
    // account_id requires a valid session token. The handler is
    // additionally responsible for checking the body's account_id
    // matches the token's (anti-impersonation). Layer order: the
    // session layer is added last so it runs first and hands the
    // write limiter the verified account.
    Router::new()
        .route("/discovery/register", post(registry::register))
        // ADR-0076 — rename: retire the current name, bind another.
        .route("/discovery/username/replace", post(registry::replace))
        // ADR-0060 — enter or leave Everyone (username public flag + name
        // search terms), and fetch this account's username certificate.
        .route(
            "/discovery/visibility",
            axum::routing::put(visibility::put_visibility),
        )
        .route(
            "/discovery/username-certificate",
            get(visibility::get_certificate),
        )
        // A3 (GDPR erasure) — purge this account's discovery footprint
        // (username binding, identity bundle, KeyPackage). Invite rows name no
        // owner (ADR-0078 section 10) and are not touched.
        // Session-token-gated; the account_id comes from the verified token.
        .route(
            "/discovery/account",
            axum::routing::delete(account::delete_account),
        )
        .route("/discovery/identity", post(identity_bundle::upload))
        .route("/discovery/keypackage", post(keypackage::upload))
        // ADR-0050 — remove one device's KeyPackage (device removal). ADR-0074:
        // under the generation floor, tombstones the id, and needs an account
        // statement to delete another device's row.
        .route(
            "/discovery/keypackage/device/{device_id}",
            axum::routing::delete(keypackage::delete),
        )
        .route("/discovery/invite", post(invite::upload))
        // Phase 3c3-server — call-invite blob upload. Session-
        // token-gated like the contact-invite upload above, as a
        // rate-limit gate only; the account is not stored.
        .route("/discovery/call-invite", post(call_invite::upload))
        // Phase 3c3-revoke — call-invite delete signed by the link's
        // revocation key (ADR-0078 section 10) over
        // `lakko_contacts::call_invite::call_revoke_canonical_bytes`.
        // Session-token-gated for DoS rate-limiting; the signature is
        // the actual authorisation gate.
        .route(
            "/discovery/call-invite/{blob_id}",
            axum::routing::delete(call_invite::revoke),
        )
        // ADR-0028 (G-3) — group join-ticket blob upload. Session-
        // token-gated as a DoS / logged-in-user rate-limit; the row
        // stores neither the account nor the issuer key.
        .route("/discovery/group-invite", post(group_invite::upload))
        // ADR-0028 revoke — group-invite delete signed by the link's
        // revocation key over the group-invite revoke canonical bytes;
        // session-token-gated for DoS rate-limiting, the signature is the
        // authorisation gate (verified against the stored revocation_pk).
        .route(
            "/discovery/group-invite/{blob_id}",
            axum::routing::delete(group_invite::revoke),
        )
        // `DELETE /discovery/invite/{blob_id}` is signed by the link's
        // revocation key via its body, and ALSO requires a session token
        // as a rate-limit gate (the account is not recorded).
        .route(
            "/discovery/invite/{blob_id}",
            axum::routing::delete(invite::revoke),
        )
        // ADR-0078 section 10 removed `POST /discovery/invite/by-owner`:
        // no ledger row names its owner, and a client lists its own links
        // from the record it keeps for each link it minted.
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state.rate_limits),
            rate_limit::write_middleware,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_session,
        ))
}

/// Build the public router. Extracted so tests can drive it directly
/// without binding a TCP listener.
///
/// Three kinds of route:
///
/// - **protected** — state-mutating writes bound to an account sit behind
///   the `require_session` middleware and the per-account write limiter;
/// - **rate-limited** — the anonymous enumeration vectors, behind the per-IP
///   bucket; of these the OPRF evaluation and the three invite consumes
///   also spend an anonymous token (ADR-0054 S3);
/// - **open** — by-id reads and health, plus the channel-name registration,
///   which spends a `discovery-register` token.
///
/// The consumes stay unauthenticated by design: a fresh-install joiner taps
/// a link before it has a session - but it must hold a token, so it signs
/// up first.
pub(crate) fn build_app(state: AppState) -> Router {
    use axum::middleware;

    let gate = |context: TokenContext| {
        middleware::from_fn_with_state(state.anon_tokens.gate(context), require_token)
    };

    let protected = protected_routes(&state);

    // ADR-0054 S3 — the OPRF evaluation spends a `discovery-lookup` token.
    let oprf = Router::new()
        .route("/discovery/oprf", post(oprf::evaluate))
        .route_layer(gate(TokenContext::DiscoveryLookup));

    // ADR-0054 S3 — the three ledgers' consumes spend an `invite-consume`
    // token. The dedup tag in the body stays server-blind.
    let consumes = Router::new()
        .route(
            "/discovery/invite/{blob_id}/consume",
            post(invite::consume),
        )
        // Phase 3c3-server — call-invite consume.
        .route(
            "/discovery/call-invite/{blob_id}/consume",
            post(call_invite::consume),
        )
        // ADR-0028 (G-3) — group join-ticket consume.
        .route(
            "/discovery/group-invite/{blob_id}/consume",
            post(group_invite::consume),
        )
        .route_layer(gate(TokenContext::InviteConsume));

    // G-discovery-ratelimit — per-IP token bucket on the enumeration-vector
    // endpoints, the outer layer: an address over its bucket is refused
    // before a token is spent or any OPRF compute runs. `lookup` and
    // `channel/lookup` carry no token of their own (their enumeration step
    // is the OPRF evaluation above).
    let rate_limited = Router::new()
        .route("/discovery/lookup", post(registry::lookup))
        // ADR-0060 — find people in Everyone by one name term.
        .route("/discovery/name/lookup", post(visibility::name_lookup))
        .route("/discovery/channel/lookup", post(channel_names::lookup))
        // ADR-0080 — the city of the request's own address, for a device
        // card. Anonymous, stores nothing, logs nothing; per-IP bucket only.
        .route("/discovery/locate", get(locate::locate))
        // The three invite ledgers' open fetch routes. Since the
        // short-link amendment of ADR-0028 (2026-09-27) a ledger id is a
        // random 6-byte handle (2^48), so guessing live ids is an
        // enumeration vector and the per-IP bucket is what bounds it. All
        // stay unauthenticated by design: a joiner tapping a link on a
        // fresh install may not be signed up yet.
        .route("/discovery/invite/{blob_id}", get(invite::fetch))
        .route(
            "/discovery/call-invite/{blob_id}",
            get(call_invite::fetch),
        )
        .route(
            "/discovery/group-invite/{blob_id}",
            get(group_invite::fetch),
        )
        .merge(oprf)
        .merge(consumes)
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state.rate_limits),
            rate_limit::middleware,
        ));

    // G4-5 — channel @name registration: anonymous, spends a
    // `discovery-register` token (ADR-0054 S3). UNIQUE(oprf_hash) gates
    // squatting; the token rations it per account.
    let channel_register = Router::new()
        .route("/discovery/channel/register", post(channel_names::register))
        .route_layer(gate(TokenContext::DiscoveryRegister));

    // Open routes — no auth required, no rate limit.
    //   * /health, /ready, /version — no compute.
    //   * /discovery/identity/{id} + keypackage/{id} —
    //     by-id reads, no enumeration vector (the id is
    //     a 32-byte pseudonym).
    let open = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        .route(
            "/discovery/identity/{account_id}",
            get(identity_bundle::fetch),
        )
        .route("/discovery/keypackage/{account_id}", get(keypackage::fetch))
        // ADR-0060 — the username-certificate verifying key.
        .route(
            "/.well-known/lakko-username-cert-key",
            get(visibility::cert_key),
        )
        .merge(channel_register);

    open.merge(rate_limited)
        .merge(protected)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http().make_span_with(lakko_log_hygiene::request_span))
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

/// Spawn the rate-limit bucket pruner, the ADR-0074 device-tombstone pruner
/// and the ADR-0076 retired-username pruner (hourly: frees the names retired
/// longer ago than `retired_hold_secs`); the handles are held for the life
/// of the process.
fn spawn_bucket_and_tombstone_pruners(
    rate_limits: &Arc<rate_limit::DiscoveryLimits>,
    pool: &sqlx::MySqlPool,
    retired_hold_secs: u64,
) -> [tokio::task::JoinHandle<()>; 3] {
    [
        rate_limit::spawn_pruner(Arc::clone(rate_limits)),
        keypackage::spawn_tombstone_pruner(pool.clone()),
        registry::spawn_retired_pruner(pool.clone(), retired_hold_secs),
    ]
}

/// ADR-0076 — read and log the rename interval and the retired-name hold.
fn load_name_policy() -> Result<registry::NamePolicy> {
    let policy = registry::NamePolicy::from_env().context("username rename policy")?;
    tracing::info!(
        rename_interval_secs = policy.rename_interval_secs,
        retired_hold_secs = policy.retired_hold_secs,
        "username rename policy"
    );
    Ok(policy)
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
mod anon_token_route_tests;

#[cfg(test)]
mod log_capture_tests;

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

    /// The three ledgers' open fetch + consume routes sit behind the per-IP
    /// bucket (the short-link amendment of ADR-0028: a 6-byte id is
    /// guessable, so the rate limit is what bounds enumeration). Builds the
    /// real router over a lazy pool that is never reached: a malformed id is
    /// refused with 400 before any DB work, so the first `burst` requests
    /// from one address answer 400 and the next one 429.
    /// The real state over a lazy pool that is never reached: a route that
    /// touched the database would fail, so a route that answers did not.
    fn lazy_state(limit: rate_limit::Limit, city_lookup: Arc<dyn locate::CityLookup>) -> AppState {
        let secret = Arc::new(TokenSecret::new(vec![7u8; 32]).unwrap());
        let db = MySqlPoolOptions::new()
            .connect_lazy("mysql://nobody:nothing@127.0.0.1:1/none")
            .unwrap();
        AppState {
            anon_tokens: TokenVerifier::new(db.clone(), None),
            db,
            oprf: Arc::new(
                oprf::LakkoOprfServer::new_from_seed(&[1u8; 32], b"lakko/oprf/v1").unwrap(),
            ),
            session_token_secret: Arc::clone(&secret),
            rate_limits: Arc::new(rate_limit::DiscoveryLimits::new(
                limit,
                limit,
                limit,
                Arc::clone(&secret),
            )),
            fresh_identity_cooldown_secs: 0,
            transparency: None,
            username_cert_key: Arc::new(lakko_crypto::signing::SigningKey::from_seed(&[3u8; 32])),
            key_reset_attest_key: None,
            name_policy: registry::NamePolicy::defaults(),
            city_lookup,
        }
    }

    /// The three ledgers' open fetch + consume routes sit behind the per-IP
    /// bucket (the short-link amendment of ADR-0028: a 6-byte id is
    /// guessable, so the rate limit is what bounds enumeration). Builds the
    /// real router over a lazy pool that is never reached: a malformed id is
    /// refused with 400 before any DB work, so the first `burst` requests
    /// from one address answer 400 and the next one 429.
    #[tokio::test]
    async fn invite_fetch_and_consume_routes_are_rate_limited() {
        let limit = rate_limit::Limit {
            per_minute: 60.0,
            burst: 3.0,
        };
        let state = lazy_state(limit, Arc::new(locate::NoCityLookup));
        let app = build_app(state);
        for (method, uri) in [
            ("GET", "/discovery/invite/too-long-for-an-id"),
            ("POST", "/discovery/invite/too-long-for-an-id/consume"),
            ("GET", "/discovery/call-invite/too-long-for-an-id"),
            ("POST", "/discovery/call-invite/too-long-for-an-id/consume"),
            ("GET", "/discovery/group-invite/too-long-for-an-id"),
            ("POST", "/discovery/group-invite/too-long-for-an-id/consume"),
        ] {
            let request = |ip: &str| {
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("x-real-ip", ip)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"joiner_tag":""}"#))
                    .unwrap()
            };
            // A fresh address per route so the buckets do not interfere.
            let ip = format!("192.0.2.{}", uri.len());
            let mut statuses = Vec::new();
            for _ in 0..4 {
                let r = app.clone().oneshot(request(&ip)).await.unwrap();
                statuses.push(r.status());
            }
            assert_ne!(statuses[0], StatusCode::TOO_MANY_REQUESTS, "{method} {uri}");
            assert_eq!(
                statuses[3],
                StatusCode::TOO_MANY_REQUESTS,
                "{method} {uri} must be behind the per-IP bucket: {statuses:?}"
            );
        }
    }

    /// A fake city database: answers for the documentation addresses it was
    /// given and counts nothing.
    struct FakeCities(Vec<(std::net::IpAddr, locate::Place)>);

    impl locate::CityLookup for FakeCities {
        fn locate(&self, ip: std::net::IpAddr) -> Option<locate::Place> {
            self.0
                .iter()
                .find(|(a, _)| *a == ip)
                .map(|(_, p)| p.clone())
        }
    }

    fn almere() -> locate::Place {
        locate::Place {
            city: Some("Almere".to_string()),
            country: Some("NL".to_string()),
        }
    }

    fn locate_app(burst: f64) -> Router {
        let lookup = FakeCities(vec![
            ("203.0.113.7".parse().unwrap(), almere()),
            (
                "2001:db8::1".parse().unwrap(),
                locate::Place {
                    city: None,
                    country: Some("DE".to_string()),
                },
            ),
        ]);
        build_app(lazy_state(
            rate_limit::Limit {
                per_minute: 60.0,
                burst,
            },
            Arc::new(lookup),
        ))
    }

    async fn locate_as(app: &Router, headers: &[(&str, &str)]) -> (StatusCode, String, String) {
        let mut req = Request::builder().method("GET").uri("/discovery/locate");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let r = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = r.status();
        let cache = r
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = r.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap(), cache)
    }

    /// ADR-0080 section 4: no session, no token; the address nginx wrote is
    /// the one located (the same rule the per-IP limiter trusts), and the
    /// answer is never cached. The lazy pool is never reached, so the route
    /// read and wrote no database.
    #[tokio::test]
    async fn locate_answers_for_the_trusted_source_address_without_a_session() {
        let app = locate_app(100.0);
        let (status, body, cache) = locate_as(&app, &[("x-real-ip", "203.0.113.7")]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, r#"{"city":"Almere","country":"NL"}"#);
        assert_eq!(cache, "no-store");
        // The client-supplied leftmost forwarded entry is ignored.
        let (_, body, _) =
            locate_as(&app, &[("x-forwarded-for", "198.51.100.1, 203.0.113.7")]).await;
        assert_eq!(body, r#"{"city":"Almere","country":"NL"}"#);
        let (_, body, _) =
            locate_as(&app, &[("x-forwarded-for", "203.0.113.7, 198.51.100.1")]).await;
        assert_eq!(body, "{}");
        let (_, body, _) = locate_as(&app, &[("x-real-ip", "2001:db8::1")]).await;
        assert_eq!(body, r#"{"country":"DE"}"#);
        let (status, body, _) = locate_as(&app, &[("x-real-ip", "198.51.100.200")]).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "{}"));
    }

    #[tokio::test]
    async fn locate_without_a_database_answers_empty() {
        let app = build_app(lazy_state(
            rate_limit::Limit {
                per_minute: 60.0,
                burst: 10.0,
            },
            Arc::new(locate::NoCityLookup),
        ));
        let (status, body, _) = locate_as(&app, &[("x-real-ip", "203.0.113.7")]).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "{}"));
    }

    #[tokio::test]
    async fn locate_is_behind_the_per_address_bucket() {
        let app = locate_app(2.0);
        let mut statuses = Vec::new();
        for _ in 0..3 {
            statuses.push(locate_as(&app, &[("x-real-ip", "203.0.113.7")]).await.0);
        }
        assert_eq!(
            statuses,
            vec![
                StatusCode::OK,
                StatusCode::OK,
                StatusCode::TOO_MANY_REQUESTS
            ]
        );
        // Another address has its own bucket.
        let (status, _, _) = locate_as(&app, &[("x-real-ip", "198.51.100.9")]).await;
        assert_eq!(status, StatusCode::OK);
    }

    /// ADR-0080 section 4 / ADR-0078 section 11: no log line names the
    /// address or the answer. Captures EVERY event and span field at TRACE
    /// level while the real router (with its request `TraceLayer`) serves a
    /// locate, then searches the output.
    #[tokio::test]
    async fn locate_logs_neither_the_address_nor_the_answer() {
        use std::io::Write;
        use std::sync::Mutex;

        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = Sink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || writer.clone())
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let app = locate_app(100.0);
        let (_, body, _) = locate_as(
            &app,
            &[
                ("x-real-ip", "203.0.113.7"),
                ("x-forwarded-for", "203.0.113.7"),
            ],
        )
        .await;
        drop(guard);
        assert!(body.contains("Almere"), "the fake answered");
        let logged = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert!(!logged.is_empty(), "the request span was captured");
        for secret in ["203.0.113.7", "Almere", "\"NL\"", "country"] {
            assert!(
                !logged.contains(secret),
                "a log line names {secret:?}: {logged}"
            );
        }
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
