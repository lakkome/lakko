// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Auth service for Lakko.
//!
//! Phase 1.1a: HTTP server skeleton (axum) + MariaDB pool (sqlx) +
//! liveness/readiness probes + graceful shutdown.
//! Phase 1.1b: OPAQUE-PAKE registration (`/signup/{start,finish}`).
//! Phase 1.1c: OPAQUE-PAKE login (`/login/{start,finish}`).
//! Phase 1.1d: BIP39 mnemonic recovery — a parallel OPAQUE registration
//! kind, bundled into signup and exposed for re-authentication via
//! `/recovery/login/{start,finish}`.
//! Phase 1.1e: Equix proof-of-work anti-spam (`/pow/challenge`). Since
//! ADR-0054 S4 it gates `/signup/start` only; every other pre-session route
//! is limited by server-side counters (`src/limits.rs`).
//!
//! Configuration via environment variables (loaded by systemd from
//! `/etc/lakko/lakko.env`):
//!
//! - `LAKKO_BIND` — `host:port` to listen on (default `127.0.0.1:8081`)
//! - `DATABASE_URL` — sqlx-style MariaDB DSN
//! - `RUST_LOG` — `tracing-subscriber` env-filter spec
//! - `LAKKO_SIGNUP_POW_EFFORT_BITS` — the one proof of work left, on
//!   `/signup/start` (ADR-0054 S4; default 8, to be set by measurement on the
//!   slowest supported phone; 0 in tests)
//! - `LAKKO_AUTH_LIMIT_<SCOPE>` / `LAKKO_AUTH_IP_LIMIT_<CATEGORY>` — the
//!   per-name and per-address attempt limits that replaced proof of work on
//!   every other route, each `N/SECS` (see `src/limits.rs` for the names and
//!   the defaults, which are chosen, not measured)
//! - `LAKKO_AUTH_HANDLE_CHANGE_MIN_INTERVAL_SECS` — minimum seconds between
//!   two binds of one account that change its login handle (default 60;
//!   restoring the handle held just before is exempt; 0 = no minimum)
//! - `LAKKO_SESSION_TOKEN_SECRET` — base64-encoded HMAC-SHA256 key
//!   (≥ 32 bytes after decode) shared with `lakko-svc-discovery` for
//!   Phase 1.5b session-token issuance + verification. **Required**
//!   at startup; missing / short secrets fail-fast.
//! - `LAKKO_SESSION_TOKEN_TTL_DAYS` — issued-token validity window
//!   in days (default 30). Matches the rough OPAQUE export-key
//!   lifetime documented in the plan.
//! - `LAKKO_WEBAUTHN_RP_ID`     — WebAuthn relying-party identifier
//!   (eTLD+1, e.g. `lakko.me`). Optional. Unset = passkey routes
//!   return 503.
//! - `LAKKO_WEBAUTHN_RP_NAME`   — human label shown by the
//!   authenticator UI. Default: `Lakko`.
//! - `LAKKO_WEBAUTHN_RP_ORIGIN` — full origin including scheme,
//!   e.g. `https://lakko.me`. Required when `RP_ID` is set.
//! - `LAKKO_AUTH_GUEST_ACCOUNT_AGE_SECS` — an account younger than this counts
//!   as fresh/guest for the ADR-0033 call-admission throttle (default 3600).
//! - `LAKKO_AUTH_CALL_ADMISSION_WINDOW_SECS` — the throttle's sliding window
//!   (default 60).
//! - `LAKKO_AUTH_CALL_ADMISSION_BURST_LIMIT` — max distinct fresh accounts per
//!   `call_id` per window before throttling (default 8; **0 = disabled**).
//! - `LAKKO_AUTH_CALL_THROTTLE_SECS` — how long a call stays throttled once
//!   tripped (default 300).
//! - `LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT` — max backup-head mints per account per
//!   window before `429` (ADR-0037 F5b; **default 0 = disabled**; set a positive
//!   value, e.g. 60, to arm it — fails OPEN on a store error).
//! - `LAKKO_BACKUP_HEAD_MINT_RATE_WINDOW_SECS` — the mint rate-limit's fixed
//!   window (default 60; only consulted once a positive limit is set).
//! - `LAKKO_ANON_TOKEN_MASTER_SECRET` — 64 hex characters shared with every
//!   service that verifies anonymous tokens (ADR-0054). Optional; unset =
//!   `/auth/tokens*` return 503 (logged as a warning at startup).
//! - `LAKKO_AUTH_HANDLE_PEPPER` — 64 hex characters keying the stored login
//!   handles (ADR-0054 S5). Optional; unset = every route that names an
//!   account by login handle (login, recovery login, password reset, TOTP
//!   recovery, the word vault, recovery-contact recovery, and
//!   `/auth/login-handle`) answers 503 `login-by-name-disabled`, logged as a
//!   warning at startup. Losing it unbinds every handle.
//! - `LAKKO_ANON_TOKEN_WINDOW_SECS` — anonymous-token epoch window (default
//!   86400; must match every verifier).
//! - `LAKKO_ANON_TOKEN_QUOTA_<CONTEXT>` — tokens per account per epoch for one
//!   context (see `src/anon_tokens.rs` for the names and defaults).
//!
//! Endpoints:
//!
//! - `GET  /health`                  — liveness (no DB), 200 OK
//! - `GET  /ready`                   — readiness (pings DB), 200 OK or 503
//! - `GET  /version`                 — `{"service", "version", "git_sha"}`
//! - `POST /pow/challenge`           — issue Equix PoW challenge (Phase 1.1e)
//! - `POST /signup/start`            — OPAQUE registration step 1 (Phase 1.1b/d/e)
//! - `POST /signup/finish`           — OPAQUE registration step 2 (Phase 1.1b/d/e)
//! - `POST /login/start`             — OPAQUE login step 1, password (Phase 1.1c/e)
//! - `POST /login/finish`            — OPAQUE login step 2, password (Phase 1.1c)
//! - `POST /recovery/login/start`    — OPAQUE login step 1, BIP39 (Phase 1.1d/e)
//! - `POST /recovery/login/finish`   — OPAQUE login step 2, BIP39 (Phase 1.1d)
//! - `POST /password/reset/start`    — replace password via mnemonic, step 1 (Phase 1.16c3c3a)
//! - `POST /password/reset/finish`   — replace password via mnemonic, step 2 (Phase 1.16c3c3a)
//! - `POST /webauthn/register/start` — passkey enrolment, step 1 (Phase 2a2a, bearer-auth)
//! - `POST /webauthn/register/finish`— passkey enrolment, step 2 (Phase 2a2a, bearer-auth)
//! - `GET  /webauthn/credentials`    — list this account's passkeys (Phase 2a2a, bearer-auth)
//! - `DELETE /webauthn/credentials/{id}` — remove a passkey (Phase 2a2a, bearer-auth)
//! - `POST /webauthn/auth/start`     — passkey login, step 1 (Phase 2a2b, anonymous)
//! - `POST /webauthn/auth/finish`    — passkey login, step 2 (Phase 2a2b, anonymous)
//! - `POST /call/jwt`                — issue SFU-bound JWT (Phase 3b4 forward, bearer-auth)
//! - `GET  /.well-known/jwks.json`   — publish call-JWT public key (Phase 3b4 forward, anonymous)
//! - `POST /auth/tokens`             — issue anonymous tokens against a quota (ADR-0054, bearer-auth)
//! - `GET  /auth/tokens/key/{context}/{epoch}` — anonymous-token public key (ADR-0054, anonymous)
//! - `PUT  /auth/login-handle`       — bind the caller's login handle (ADR-0054 S5, bearer-auth;
//!   `429 handle-bind-limited` per account, per address, and within the minimum interval
//!   between handle changes)
//! - `DELETE /auth/login-handle`     — remove it (ADR-0054 S5, bearer-auth)

#![forbid(unsafe_code)]

mod anon_tokens;
mod backup_head;
mod call_jwt;
mod device_authorize;
mod device_request;
mod genesis_attest;
mod limits;
mod login_handle;
mod opaque;
mod otp_sender;
mod pow;
mod recovery_contact;
mod throttle;
mod totp;
mod turn;
mod webauthn;
mod webauthn_action;
mod word_vault;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{delete, get, post, put};
use axum::Router;
use opaque_ke::ServerSetup;
use serde_json::json;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tokio::signal;
use tower_http::trace::TraceLayer;
use webauthn_rs::Webauthn;

use crate::opaque::LakkoCipherSuite;
use lakko_auth_token::TokenSecret;

/// Default bind address if `LAKKO_BIND` is not set.
const DEFAULT_BIND: &str = "127.0.0.1:8081";

/// Default session-token TTL — 30 days. Matches the rough OPAQUE
/// export-key lifetime documented in the plan. Override via
/// `LAKKO_SESSION_TOKEN_TTL_DAYS`.
const DEFAULT_SESSION_TOKEN_TTL_DAYS: u64 = 30;

/// Migrations directory (relative to crate root). `sqlx::migrate!` reads it
/// at compile time and embeds the SQL in the binary.
const MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Shared application state.
///
/// Every field must be `Clone`-able (primitive or `Arc<...>`); axum
/// requires `Clone + Send + Sync` for the `State<AppState>` extractor.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: MySqlPool,
    /// Server-static OPAQUE setup — one per service instance, loaded (or
    /// generated on first run) at bootstrap. Wrapped in `Arc` to avoid
    /// cloning the inner scalar / point material on every request.
    pub(crate) opaque_setup: Arc<ServerSetup<LakkoCipherSuite>>,
    /// Shared HMAC secret for issuing session tokens (Phase 1.5b).
    /// Loaded once at startup from `LAKKO_SESSION_TOKEN_SECRET`; the
    /// same value MUST be present in `lakko-svc-discovery`'s env so
    /// the verifier middleware accepts what we issued.
    pub(crate) session_token_secret: Arc<TokenSecret>,
    /// Issued-token TTL in milliseconds. Convertible from
    /// `LAKKO_SESSION_TOKEN_TTL_DAYS` × 86_400_000.
    pub(crate) session_token_ttl_ms: i64,
    /// Phase 2a2 — WebAuthn relying-party config. `None` when the
    /// service is started without `LAKKO_WEBAUTHN_RP_ID`; the four
    /// `/webauthn/*` routes then return 503 instead of registering
    /// passkeys with an undefined RP identity.
    pub(crate) webauthn: Option<Arc<Webauthn>>,
    /// G-13 (ADR-0014) — WebAuthn action binding. The low-level
    /// `WebauthnCore` configured from the SAME RP env vars as
    /// `webauthn` above, used ONLY by the `/auth/action/{start,finish}`
    /// endpoints to issue + verify per-action challenges over the
    /// vendored fork. `None` (routes return 503) under the same
    /// condition as `webauthn` — `LAKKO_WEBAUTHN_RP_ID` unset.
    pub(crate) webauthn_action_core: Option<Arc<webauthn_rs_core::WebauthnCore>>,
    /// Phase 3b4 forward — Ed25519 keypair the service uses
    /// to sign call JWTs (consumed by `services/sfu` per
    /// ADR-0008). Loaded (or generated on first run) at
    /// startup via `call_jwt::load_or_init_call_signing_key`.
    /// Public key is published at GET `/.well-known/jwks.json`.
    /// Crypto + wire shape lives in the shared `lakko-call-jwt`
    /// crate so client FFI uses the same types.
    pub(crate) call_jwt_signing: Arc<lakko_call_jwt::CallSigningKey>,
    /// G-13 (ADR-0015) — dedicated Ed25519 key for signing device-removal
    /// attestations on `/auth/action/finish`. Separate from
    /// `call_jwt_signing` (key separation). The public half is published at
    /// `GET /.well-known/device-attest-jwks.json`.
    pub(crate) attest_key: Arc<lakko_call_jwt::CallSigningKey>,
    /// G3-{1,2}-turn step 1 — short-lived TURN-credential
    /// minter. `None` when `LAKKO_TURN_URIS` /
    /// `LAKKO_TURN_SHARED_SECRET` are unset; the
    /// `POST /call/turn-credential` route returns 503 in
    /// that case and clients fall back to STUN-only ICE.
    pub(crate) turn: Option<Arc<turn::TurnConfig>>,
    /// ADR-0018 factor (c) — server-static AEAD key encrypting TOTP
    /// secrets at rest (`totp_secrets.secret_enc`). Loaded from
    /// `LAKKO_TOTP_ENC_KEY` (base64, exactly 32 bytes after decode);
    /// `None` when unset, in which case the three `/totp/*` routes
    /// return 503 — loud refusal, no silent fallback.
    pub(crate) totp_enc_key: Option<Arc<[u8; 32]>>,
    /// ADR-0049 — word-vault body-encryption key (`LAKKO_VAULT_ENC_KEY`).
    /// `None` disables the `/vault/*` routes (503).
    pub(crate) vault_enc_key: Option<Arc<[u8; 32]>>,
    /// ADR-0018 factor (d) — server-static pepper keying the
    /// recovery-contact + OTP-code hashes (`recovery_contacts.contact_hash`,
    /// `otp_challenges.code_hash`). Loaded from
    /// `LAKKO_RECOVERY_CONTACT_PEPPER` (base64, exactly 32 bytes after
    /// decode); `None` when unset, in which case all `/recovery-contact/*`
    /// routes return 503 — loud refusal, no silent fallback. Keying the hash
    /// on this server secret keeps a leaked DB un-enumerable + the contact
    /// hashes non-reversible (invariant #1: only the hash is ever stored).
    pub(crate) recovery_contact_pepper: Option<Arc<[u8; 32]>>,
    /// ADR-0018 factor (d) — one-time-code delivery dispatcher. Routes a
    /// `send(kind, contact, code)` to the email (SMTP) or phone (SMS) adapter
    /// configured at startup, falling back per-kind to a `LogOnlyOtpSender`
    /// when no real adapter is armed (the `services/push-gw` posture). The
    /// contact plaintext is supplied transiently and never persisted/logged.
    pub(crate) otp_sender: Arc<dyn otp_sender::OtpSender>,
    /// ADR-0033 — call-admission anti-spam throttle config. Loaded from the
    /// environment at startup and consulted by `issue_call_jwt` before minting.
    /// `burst_limit == 0` (the default when
    /// `LAKKO_AUTH_CALL_ADMISSION_BURST_LIMIT` is unset / 0) disables it. The
    /// throttle sees only admission bookkeeping (call_id, account_id,
    /// timestamps) and fails OPEN on a store error — no E2EE / invariant impact.
    pub(crate) throttle: throttle::ThrottleConfig,
    /// ADR-0037 F5b — generous, fail-open per-account rate-limit on the
    /// backup-head mint (`POST /auth/backup/head`). Loaded from the environment
    /// at startup and consulted by `backup_head` before the counter bump.
    /// `limit == 0` (`LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT=0`) disables it. Sees
    /// only per-account mint bookkeeping and fails OPEN on a store error — no
    /// E2EE / invariant impact.
    pub(crate) backup_head_mint_rate: backup_head::MintRateConfig,
    /// ADR-0054 decision 2 — anonymous-token issuance (the shared master
    /// secret + epoch window + per-context quotas). `None` when
    /// `LAKKO_ANON_TOKEN_MASTER_SECRET` is unset: `/auth/tokens*` return 503.
    pub(crate) anon_tokens: Option<Arc<anon_tokens::AnonTokenIssuer>>,
    /// ADR-0054 S5 — pepper keying the stored login handles
    /// (`LAKKO_AUTH_HANDLE_PEPPER`, 64 hex). `None` disables every route that
    /// names an account by login handle (503 `login-by-name-disabled`).
    pub(crate) handle_pepper: Option<Arc<[u8; 32]>>,
    /// ADR-0054 S4 — per-name attempt counters (DB-backed, fail closed) and
    /// per-address token buckets (in memory) that replaced proof of work on
    /// every route except `/signup/start`.
    pub(crate) limits: Arc<limits::Limits>,
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    // Load `.env` from the workspace root (or nearest parent) — debug
    // builds only. Release builds skip dotenv entirely; production picks
    // up env vars from the systemd `EnvironmentFile=/etc/lakko/lakko.env`
    // directive instead.
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

    // Per-service database — read `LAKKO_AUTH_DATABASE_URL` first, fall
    // back to the generic `DATABASE_URL`. Each service uses its own DB
    // because sqlx's migration tracking lives in a hardcoded
    // `_sqlx_migrations` table per database.
    let db_url = std::env::var("LAKKO_AUTH_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .context("LAKKO_AUTH_DATABASE_URL or DATABASE_URL env var must be set")?;

    let pool = MySqlPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&db_url)
        .await
        .context("connect to MariaDB")?;

    // Apply embedded migrations on startup. Idempotent.
    MIGRATIONS.run(&pool).await.context("run sqlx migrations")?;

    // Load (or initialize on first run) the server-static OPAQUE setup.
    // Runs AFTER migrations so opaque_server_setup table exists.
    let opaque_setup = opaque::load_or_init_server_setup(&pool)
        .await
        .context("load OPAQUE ServerSetup")?;

    // Phase 1.5b: session-token secret. Required env var; failing
    // fast at startup beats issuing un-verifiable tokens silently.
    let session_token_secret_raw = std::env::var("LAKKO_SESSION_TOKEN_SECRET")
        .context("LAKKO_SESSION_TOKEN_SECRET env var must be set (base64-encoded HMAC-SHA256 key, ≥ 32 bytes after decode)")?;
    let session_token_secret = TokenSecret::from_base64(&session_token_secret_raw)
        .context("parse LAKKO_SESSION_TOKEN_SECRET as base64")?;

    let session_token_ttl_days: u64 = std::env::var("LAKKO_SESSION_TOKEN_TTL_DAYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SESSION_TOKEN_TTL_DAYS);
    // Saturating multiply guards against an env-typo like "9999999"
    // overflowing the i64 millisecond timestamp arithmetic later.
    let session_token_ttl_ms: i64 = i64::try_from(session_token_ttl_days)
        .unwrap_or(i64::MAX / 86_400_000)
        .saturating_mul(86_400_000);

    // Phase 2a2 — optional WebAuthn config. Unset env vars leave the
    // four `/webauthn/*` routes returning 503; the rest of the
    // service (OPAQUE password / recovery flows) is unaffected.
    let webauthn_instance =
        webauthn::load_webauthn().context("load WebAuthn relying-party config")?;
    if webauthn_instance.is_some() {
        tracing::info!("webauthn enabled");
    } else {
        tracing::info!("webauthn disabled (LAKKO_WEBAUTHN_RP_ID not set)");
    }

    // G-13 (ADR-0014) — action-binding core, configured from the same RP
    // env vars. Enabled together with `webauthn` (both gate on
    // `LAKKO_WEBAUTHN_RP_ID`); the `/auth/action/*` routes return 503 when
    // it is `None`.
    let webauthn_action_core =
        webauthn_action::load_action_core().context("load WebAuthn action-binding core")?;

    // Phase 3b4 forward — load (or generate on first run)
    // the Ed25519 call-JWT signing key. Same load_or_init
    // pattern as OPAQUE server setup.
    let call_jwt_signing = call_jwt::load_or_init_call_signing_key(&pool)
        .await
        .context("load call_jwt signing key")?;
    tracing::info!(
        kid = %call_jwt_signing.kid,
        "call-JWT signing key loaded"
    );

    // G-13 (ADR-0015) — dedicated device-removal attestation signing key.
    let attest_key = webauthn_action::load_or_init_attest_key(&pool)
        .await
        .context("load action-attestation signing key")?;
    tracing::info!(kid = %attest_key.kid, "action-attestation signing key loaded");

    // G3-{1,2}-turn step 1 — TURN-credential config. Loud-
    // fail on malformed env vars (typo in base64 / scheme);
    // soft-disable when both env vars are simply unset.
    let turn_config = turn::TurnConfig::from_env().context("load TURN config")?;
    if turn_config.is_some() {
        tracing::info!("turn-credential endpoint enabled");
    } else {
        tracing::info!("turn-credential endpoint disabled (LAKKO_TURN_URIS / LAKKO_TURN_SHARED_SECRET not set)");
    }

    // ADR-0018 factor (c) — TOTP secret-encryption key. Loud-fail on a
    // malformed value (bad base64 / wrong length); soft-disable when the
    // env var is simply unset (the /totp/* routes then return 503).
    let totp_enc_key = totp::load_totp_enc_key().context("load LAKKO_TOTP_ENC_KEY")?;
    if totp_enc_key.is_some() {
        tracing::info!("totp endpoints enabled");
    } else {
        tracing::info!("totp endpoints disabled (LAKKO_TOTP_ENC_KEY not set)");
    }

    // ADR-0049 — word-vault body-encryption key. Loud-fail on a malformed
    // value; soft-disable when unset (the /vault/* routes then return 503).
    let vault_enc_key = word_vault::load_vault_enc_key().context("load LAKKO_VAULT_ENC_KEY")?;
    if vault_enc_key.is_some() {
        tracing::info!("word-vault endpoints enabled");
    } else {
        tracing::info!("word-vault endpoints disabled (LAKKO_VAULT_ENC_KEY not set)");
    }

    // ADR-0018 factor (d) — recovery-contact pepper. Loud-fail on a malformed
    // value (bad base64 / wrong length); soft-disable when unset (the
    // /recovery-contact/* routes then return 503).
    let recovery_contact_pepper = recovery_contact::load_recovery_contact_pepper()
        .context("load LAKKO_RECOVERY_CONTACT_PEPPER")?;
    if recovery_contact_pepper.is_some() {
        tracing::info!("recovery-contact endpoints enabled");
    } else {
        tracing::info!(
            "recovery-contact endpoints disabled (LAKKO_RECOVERY_CONTACT_PEPPER not set)"
        );
    }

    // ADR-0018 factor (d) — build the OTP delivery dispatcher (per-kind real
    // adapter where configured, LogOnly fallback otherwise). Logs which real
    // senders armed.
    let otp_sender: Arc<dyn otp_sender::OtpSender> = Arc::new(otp_sender::build_otp_sender());

    // ADR-0033 — call-admission anti-spam throttle. Loaded from the environment;
    // disabled by default (burst_limit 0 / unset).
    let throttle_config = throttle::ThrottleConfig::from_env();
    if throttle_config.enabled() {
        tracing::info!(
            burst_limit = throttle_config.burst_limit,
            window_secs = throttle_config.window_secs,
            guest_age_secs = throttle_config.guest_age_secs,
            throttle_secs = throttle_config.throttle_secs,
            "call-admission throttle enabled"
        );
    } else {
        tracing::info!(
            "call-admission throttle disabled (LAKKO_AUTH_CALL_ADMISSION_BURST_LIMIT unset / 0)"
        );
    }

    // ADR-0037 F5b — generous, fail-open per-account backup-head mint
    // rate-limit. Loaded from the environment; enabled by default at a generous
    // cap (`LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT=0` disables it).
    let backup_head_mint_rate = backup_head::MintRateConfig::from_env();
    if backup_head_mint_rate.enabled() {
        tracing::info!(
            limit = backup_head_mint_rate.limit,
            window_secs = backup_head_mint_rate.window_secs,
            "backup-head mint rate-limit enabled"
        );
    } else {
        tracing::info!(
            "backup-head mint rate-limit disabled (LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT=0)"
        );
    }

    // ADR-0054 decision 2 — anonymous-token issuance. Loud-fail on a malformed
    // master secret / window / quota; dormant (with a warning) when unset.
    let anon_tokens = anon_tokens::load_anon_tokens().context("load anonymous-token config")?;
    anon_tokens::log_startup(anon_tokens.as_ref());

    // ADR-0054 S5 — the login-handle pepper. Loud-fail on a malformed value;
    // dormant (with a warning) when unset.
    let handle_pepper =
        login_handle::load_handle_pepper().context("load LAKKO_AUTH_HANDLE_PEPPER")?;
    login_handle::log_startup(handle_pepper.as_ref());
    // ADR-0054 S4 — attempt limits. Loud-fail on a malformed knob.
    let limits = Arc::new(
        limits::Limits::from_env(handle_pepper.as_ref()).context("load auth attempt limits")?,
    );
    limits.log_startup();

    let state = AppState {
        db: pool.clone(),
        opaque_setup: Arc::new(opaque_setup),
        session_token_secret: Arc::new(session_token_secret),
        session_token_ttl_ms,
        webauthn: webauthn_instance,
        webauthn_action_core,
        call_jwt_signing: Arc::new(call_jwt_signing),
        attest_key: Arc::new(attest_key),
        turn: turn_config.map(Arc::new),
        totp_enc_key: totp_enc_key.map(Arc::new),
        vault_enc_key: vault_enc_key.map(Arc::new),
        recovery_contact_pepper: recovery_contact_pepper.map(Arc::new),
        otp_sender,
        throttle: throttle_config,
        backup_head_mint_rate,
        anon_tokens: anon_tokens.clone().map(Arc::new),
        handle_pepper: handle_pepper.map(Arc::new),
        limits: limits.clone(),
    };

    // Background tasks: periodically delete expired rows from the
    // `opaque_login_states`, `pow_challenges`, and `webauthn_states`
    // tables. Aborted when the service stops.
    let _login_pruner = opaque::spawn_login_state_pruner(pool.clone());
    let _pow_pruner = pow::spawn_pruner(pool.clone());
    // ADR-0054 S4 — expired name counters + refilled address buckets.
    let _limits_pruner = limits::spawn_pruner(pool.clone(), limits);
    let _webauthn_pruner = webauthn::spawn_state_pruner(pool.clone());
    // ADR-0033 — prune expired call-admission events + throttled-call rows.
    let _throttle_pruner = throttle::spawn_pruner(pool.clone(), throttle_config);
    // ADR-0054 — drop issuance-quota rows older than the previous epoch.
    let _anon_token_pruner = anon_tokens.as_ref().map(|i| {
        lakko_anon_token_server::spawn_issuance_pruner(pool.clone(), i.server.window_secs)
    });

    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!(addr = %bind, "auth service listening");

    // Connect info feeds the address limiter when no nginx header is present
    // (a direct `cargo run`).
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("axum serve")?;

    pool.close().await;
    tracing::info!("auth service stopped cleanly");
    Ok(())
}

/// Build the public router. Extracted so tests can drive it directly without
/// binding a TCP listener.
#[allow(clippy::too_many_lines)] // flat route-registration table reads best whole
fn build_app(state: AppState) -> Router {
    // ADR-0054 S4 — the per-address bucket for one route family, layered on
    // that route alone (the per-name counters run inside the handlers, where
    // the named value is known).
    let ip = |cat: limits::Category| {
        axum::middleware::from_fn_with_state((state.limits.clone(), cat), limits::ip_gate)
    };
    // Phase 2a2 — the four `/webauthn/*` routes are gated by the
    // session-token middleware so only an already-authenticated
    // caller can register / list / delete passkeys. The /auth/*
    // (login) routes that land in 2a2b are anonymous and live in
    // the public router.
    let webauthn_routes = Router::new()
        .route("/webauthn/register/start", post(webauthn::register_start))
        .route("/webauthn/register/finish", post(webauthn::register_finish))
        .route("/webauthn/credentials", get(webauthn::list_credentials))
        .route(
            "/webauthn/credentials/{credential_id_b64}",
            delete(webauthn::delete_credential),
        )
        // A3 (GDPR erasure) — delete the whole account (FK cascade purges all
        // auth state). Bearer-auth + a passkey action grant when the account
        // has a passkey (the discovery + storage tiers are purged by the client
        // via their own bearer-auth'd deletes).
        .route("/account", delete(webauthn::delete_account))
        // A3 (GDPR data-portability) — export the account's auth-tier metadata
        // + auth-factor existence (never the factor secrets, invariant #1).
        .route("/account/export", get(webauthn::export_account))
        // G-13 (ADR-0014) — action-binding ceremony. Bearer-auth: the
        // caller is an already-logged-in account attesting to a specific
        // action with one of its registered passkeys.
        .route(
            "/auth/action/start",
            post(webauthn_action::action_start),
        )
        .route(
            "/auth/action/finish",
            post(webauthn_action::action_finish),
        )
        // ADR-0018 factor (a) — mint a single-use device grant. Bearer-auth:
        // the caller is the ALREADY-authenticated existing device
        // authorizing a fresh device. Per-account + per-address limited. The
        // matching `/device/authorize/redeem` is anonymous (public router).
        .route(
            "/device/authorize/start",
            post(device_authorize::start).route_layer(ip(limits::Category::Attempt)),
        )
        // ADR-0018 factor (c) — TOTP enrollment. Bearer-auth: the caller
        // is an already-logged-in account adding an authenticator factor.
        // The matching `/totp/recover` is anonymous (public router).
        .route("/totp/enroll/start", post(totp::enroll_start))
        .route("/totp/enroll/finish", post(totp::enroll_finish))
        // ADR-0049 — word vault enrollment + status. Bearer-auth: the caller
        // is a signed-in account sealing its own phrase entropy. The matching
        // `/vault/recover/*` are anonymous (public router).
        .route("/vault", get(word_vault::status))
        .route("/vault/enroll", post(word_vault::enroll))
        // ADR-0049 section 3 — the entropy sealed under a passkey's PRF
        // output; opaque per-credential blobs only the account reads.
        .route("/vault/passkey", put(word_vault::passkey_put))
        .route("/vault/passkey/get", post(word_vault::passkey_get))
        // ADR-0049 section 2 (the device link since 2026-09-28) — the
        // signed-in device's side of a camera-less add-device confirmation:
        // see the open request, post its SPAKE2 message once, record the
        // outcome once, or decline.
        .route("/device/requests", get(device_request::list))
        .route(
            "/device/requests/{request_id_b64}",
            delete(device_request::delete),
        )
        .route(
            "/device/requests/{request_id_b64}/pake",
            post(device_request::answer),
        )
        .route(
            "/device/requests/{request_id_b64}/result",
            post(device_request::result),
        )
        // ADR-0054 S5 — bind / remove the login handle the name routes
        // resolve. Bearer-auth: the caller binds its own account. A bind is
        // an existence probe (409), so it is also limited per address (the
        // per-account counter and minimum interval run in the handler).
        .route(
            "/auth/login-handle",
            put(login_handle::put)
                .route_layer(ip(limits::Category::HandleBind))
                .delete(login_handle::delete),
        )
        // ADR-0018 factor (d) — phone / email contact enrollment. Bearer-auth:
        // the caller is an already-logged-in account adding an OTP-recovery
        // contact. The matching `/recovery-contact/recover/*` are anonymous
        // (public router).
        .route(
            "/recovery-contact/enroll/start",
            post(recovery_contact::enroll_start).route_layer(ip(limits::Category::CodeSend)),
        )
        .route(
            "/recovery-contact/enroll/finish",
            post(recovery_contact::enroll_finish),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            webauthn::require_session,
        ));

    // Phase 3b4 forward — `POST /call/jwt` mints a JWT
    // scoped to (account_id, call_id, role). Bearer-auth
    // required (same session-token middleware webauthn
    // routes use). The SFU verifies the JWT against the
    // public key it fetched from `/.well-known/jwks.json`.
    let call_jwt_routes = Router::new()
        .route("/call/jwt", post(call_jwt::issue_call_jwt))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            webauthn::require_session,
        ));

    // G3-{1,2}-turn step 1 — `POST /call/turn-credential`
    // mints a short-lived TURN credential per the coturn
    // `static-auth-secret` REST API. Same bearer-auth
    // middleware as `/call/jwt`. Caller invokes once at
    // call-start; the TURN credential's expiry (default 10
    // minutes) comfortably outlasts ICE gathering on every
    // tested mobile network.
    let turn_routes = Router::new()
        .route("/call/turn-credential", post(turn::issue_turn_credential))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            webauthn::require_session,
        ));

    // G-30 finding 2 (ADR-0036) — `POST /auth/genesis/attest` mints a signed
    // genesis attestation binding the OPAQUE-authenticated account to its
    // client-supplied genesis identity pubkey (anti KT-log first-entry
    // squatting). Bearer-auth ONLY (the same session-token middleware) — NOT
    // the passkey-ceremony path, so it works for OPAQUE-only accounts with no
    // registered passkey.
    let genesis_routes = Router::new()
        .route("/auth/genesis/attest", post(genesis_attest::genesis_attest))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            webauthn::require_session,
        ));

    // G-11 (ADR-0037) — `POST /auth/backup/head` mints a signed backup-head
    // commitment carrying an auth-assigned monotone `backup_version` (anti
    // storage-tier rollback). Bearer-auth ONLY (the same session-token
    // middleware) — holding a valid session token IS proof the caller
    // OPAQUE-authenticated as `account_id`.
    let backup_head_routes = Router::new()
        .route("/auth/backup/head", post(backup_head::backup_head))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            webauthn::require_session,
        ));

    // ADR-0054 decision 2 — `POST /auth/tokens` evaluates a blinded batch of
    // anonymous tokens against the account's per-(context, epoch) quota.
    // Bearer-auth: the session is what the quota is counted against. The
    // matching public-key route is anonymous (public router below).
    let anon_token_routes = Router::new()
        .route("/auth/tokens", post(anon_tokens::issue_tokens))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            webauthn::require_session,
        ));

    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/version", get(version))
        // ADR-0054 S4 — the only proof of work left: one challenge per
        // account creation.
        .route(
            "/pow/challenge",
            post(pow::issue_challenge).route_layer(ip(limits::Category::Challenge)),
        )
        .route(
            "/signup/start",
            post(opaque::signup_start).route_layer(ip(limits::Category::Signup)),
        )
        .route("/signup/finish", post(opaque::signup_finish))
        .route(
            "/login/start",
            post(opaque::login_start).route_layer(ip(limits::Category::Attempt)),
        )
        .route("/login/finish", post(opaque::login_finish))
        .route(
            "/recovery/login/start",
            post(opaque::recovery_login_start).route_layer(ip(limits::Category::Attempt)),
        )
        .route(
            "/recovery/login/finish",
            post(opaque::recovery_login_finish),
        )
        .route(
            "/password/reset/start",
            post(opaque::password_reset_start).route_layer(ip(limits::Category::Attempt)),
        )
        .route(
            "/password/reset/finish",
            post(opaque::password_reset_finish),
        )
        // 2a2b — anonymous passkey login. No bearer-auth: these
        // are the login endpoints, the whole point is to mint
        // the session token.
        .route("/webauthn/auth/start", post(webauthn::auth_start))
        .route("/webauthn/auth/finish", post(webauthn::auth_finish))
        // ADR-0018 factor (a) — redeem a device grant for a session token.
        // ANONYMOUS: the fresh device has no token yet (the whole point is
        // to mint it). Per-address limited; the grant token (held by the
        // fresh device over the rendezvous channel) is the authorization
        // and, since ADR-0056, the whole body: its hash finds the account.
        .route(
            "/device/authorize/redeem",
            post(device_authorize::redeem).route_layer(ip(limits::Category::Attempt)),
        )
        // ADR-0018 factor (c) — recover ACCESS with an authenticator
        // code. ANONYMOUS (the caller lost their device); per-name +
        // per-address limited + replay-protected inside the handler.
        .route(
            "/totp/recover",
            post(totp::recover).route_layer(ip(limits::Category::Attempt)),
        )
        // ADR-0049 — three-word recovery. ANONYMOUS (the caller has nothing
        // yet); enumeration-safe + per-name / per-address limited +
        // destruction-capped inside the handlers. `/start` serves positions
        // + salt + cost, `/finish` compares the guess and releases the
        // sealed entropy.
        .route(
            "/vault/recover/start",
            post(word_vault::recover_start).route_layer(ip(limits::Category::Attempt)),
        )
        // ADR-0049 section 2 (the device link since 2026-09-28) — the fresh
        // device opens a request (anonymous; names the account by login
        // handle since ADR-0056; per-handle + per-address limited; 503
        // without the handle pepper), polls it, and posts its sealed
        // pairing payload once. The request id is a 128-bit secret carried
        // in the body; the payload route is single-shot per request.
        .route(
            "/device/request",
            post(device_request::create).route_layer(ip(limits::Category::DeviceRequest)),
        )
        .route("/device/request/status", post(device_request::status))
        .route("/device/request/payload", post(device_request::payload))
        .route(
            "/vault/recover/finish",
            post(word_vault::recover_finish).route_layer(ip(limits::Category::Attempt)),
        )
        // ADR-0018 factor (d) — recover ACCESS with a phone / email OTP.
        // ANONYMOUS (the caller lost their device); enumeration-safe +
        // per-name / per-address limited inside the handlers.
        // `/recover/start` sends a code (indistinguishably on no-match);
        // `/recover/finish` verifies + mints the session triple.
        .route(
            "/recovery-contact/recover/start",
            post(recovery_contact::recover_start).route_layer(ip(limits::Category::CodeSend)),
        )
        .route(
            "/recovery-contact/recover/finish",
            post(recovery_contact::recover_finish).route_layer(ip(limits::Category::Attempt)),
        )
        // Phase 3b4 forward — anonymous JWKS endpoint. The
        // public key is public; the SFU fetches it at
        // startup + refreshes every 24 h.
        .route("/.well-known/jwks.json", get(call_jwt::jwks_handler))
        .route(
            "/.well-known/device-attest-jwks.json",
            get(webauthn_action::device_attest_jwks_handler),
        )
        .merge(webauthn_routes)
        .merge(call_jwt_routes)
        .merge(turn_routes)
        .merge(genesis_routes)
        .merge(backup_head_routes)
        // ADR-0054 — the anonymous-token public key for (context, epoch).
        // ANONYMOUS: a client checks the issuance proof against this key,
        // fetched independently of the issuance response.
        .route(
            "/auth/tokens/key/{context}/{epoch}",
            get(anon_tokens::public_key),
        )
        .merge(anon_token_routes)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Liveness — service process is alive. Does **not** check the DB; nginx /
/// systemd watchdog should poll this for restart decisions.
async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

/// Readiness — service can serve traffic, including DB-backed endpoints.
/// Pings the connection pool with `SELECT 1`.
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

/// Service version metadata.
async fn version() -> impl IntoResponse {
    Json(json!({
        "service": "lakko-svc-auth",
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": option_env!("LAKKO_GIT_SHA").unwrap_or("unknown"),
    }))
}

/// Initialize tracing-subscriber from `RUST_LOG`. Falls back to `info`.
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

/// Wait for SIGTERM (systemd stop) or SIGINT (Ctrl-C).
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

    /// Build a router suitable for testing endpoints that don't touch the DB.
    /// `/health` and `/version` work without a live connection; `/ready`
    /// requires a real pool and is covered by integration tests.
    fn test_app_no_db() -> Router {
        Router::new()
            .route("/health", get(health))
            .route("/version", get(version))
    }

    #[tokio::test]
    async fn health_returns_200_ok() {
        let app = test_app_no_db();
        let response = app
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
        let app = test_app_no_db();
        let response = app
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
        assert_eq!(parsed["service"], "lakko-svc-auth");
        assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn unknown_path_returns_404() {
        let app = test_app_no_db();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
