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
//! - `LAKKO_SESSION_TOKEN_TTL_SECS` — access-token validity window in
//!   seconds (default 900, ADR-0073 section 3.1; chosen, not measured). It
//!   is the revocation latency at every verifier but auth after a key reset.
//! - `LAKKO_REFRESH_TOKEN_TTL_DAYS` — refresh-credential validity window in
//!   days (default 30); slides on every `POST /auth/session/refresh`.
//! - `LAKKO_FRESH_GRANT_TTL_SECS` — validity window in seconds of the
//!   fresh-auth grant a password sign-in, a passkey sign-in or a signup
//!   returns (default 600; chosen, not measured). Reset account keys, delete
//!   account, add a device and add a passkey each require a live one (`403
//!   fresh-auth-required` otherwise), with one exception: deleting an account
//!   that has a passkey is authorised by the ADR-0014 passkey action grant
//!   alone, and the fresh-auth grant is not read.
//! - `LAKKO_WEBAUTHN_RP_ID`     — WebAuthn relying-party identifier
//!   (eTLD+1, e.g. `lakko.me`). Optional. Unset = passkey routes
//!   return 503.
//! - `LAKKO_WEBAUTHN_RP_NAME`   — human label shown by the
//!   authenticator UI. Default: `Lakko`.
//! - `LAKKO_WEBAUTHN_RP_ORIGIN` — full origin including scheme,
//!   e.g. `https://lakko.me`. Required when `RP_ID` is set.
//! - `LAKKO_AUTH_ROOM_ADMISSION_LIMIT` — call admission tokens one SFU room
//!   may be issued per window (ADR-0078 section 2; default 100, **0 =
//!   disabled**; in memory, no account involved; see `src/room_cap.rs`).
//! - `LAKKO_AUTH_ROOM_ADMISSION_WINDOW_SECS` — that cap's sliding window
//!   (default 60).
//! - `LAKKO_AUTH_YOUNG_ACCOUNT_AGE_SECS` — an account younger than this draws
//!   the young `call-join` quota at token issuance (default 86400; see
//!   `src/anon_tokens.rs`).
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
//!   recovery, and `/auth/login-handle`) answers 503 `login-by-name-disabled`, logged as a
//!   warning at startup. Losing it unbinds every handle.
//! - `LAKKO_AUTH_EMAIL_PEPPER` — 64 hex characters keying the stored
//!   recovery-email value (ADR-0075). Optional; unset (or without
//!   `LAKKO_VAULT_ENC_KEY` / `LAKKO_AUTH_HANDLE_PEPPER`) = the enrol, remove
//!   and recover routes answer 503 `recovery-email-disabled` and
//!   `GET /recovery-email/available` answers `{"available": false}`. Losing
//!   it turns every account's recovery email off.
//! - `LAKKO_AUTH_SMTP_HOST` / `_PORT` / `_TLS` (`starttls` | `implicit`) /
//!   `_USERNAME` / `_PASSWORD` / `_FROM` / `_HELO` — the relay the recovery
//!   code is mailed through (`src/mail.rs`). Host unset = the log-only
//!   sender: nothing is delivered, and neither the address nor the code is
//!   logged.
//! - `LAKKO_AUTH_LIMIT_EMAIL_LETTER_HANDLE` / `_EMAIL_LETTER_ACCOUNT` /
//!   `_EMAIL_VERIFY` / `_EMAIL_VAULT_OPEN` / `_EMAIL_VAULT_STORE` and `LAKKO_AUTH_IP_LIMIT_EMAIL` —
//!   the recovery email's limits, each `N/SECS` (defaults chosen, not
//!   measured; see `src/limits.rs`).
//! - `LAKKO_ANON_CRED_MASTER_SECRET` — 64 hex characters shared with
//!   `lakko-svc-channels` only (ADR-0092 section 4.4; a master of its own,
//!   not the anonymous-token one). Optional; unset = `/auth/anon-credential*`
//!   return 503 `anon-credential-disabled`.
//! - `LAKKO_AUTH_LIMIT_ANON_CREDENTIAL` — per-account window of the two
//!   credential routes, `N/SECS` (default 30/86400; chosen, not measured).
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
//! - `POST /webauthn/register/start` — passkey enrolment, step 1 (Phase 2a2a, bearer-auth
//!   AND `fresh_grant` in the body, else `403 fresh-auth-required`; ADR-0073, 2026-10-06)
//! - `POST /webauthn/register/finish`— passkey enrolment, step 2 (Phase 2a2a, bearer-auth
//!   AND `fresh_grant` in the body, the same gate)
//! - `GET  /webauthn/credentials`    — list this account's passkeys (Phase 2a2a, bearer-auth)
//! - `DELETE /webauthn/credentials/{id}` — remove a passkey (Phase 2a2a, bearer-auth)
//! - `POST /webauthn/auth/start`     — passkey login, step 1 (Phase 2a2b, anonymous)
//! - `POST /webauthn/auth/finish`    — passkey login, step 2 (Phase 2a2b, anonymous)
//! - `POST /call/jwt`                — issue SFU-bound JWT + TURN credential (anonymous: spends
//!   one `call-join` token, ADR-0078 section 2; `429 room-admission-limit` past the room cap)
//! - `GET  /.well-known/jwks.json`   — publish call-JWT public key (Phase 3b4 forward, anonymous)
//! - `POST /auth/tokens`             — issue anonymous tokens against a quota (ADR-0054, bearer-auth)
//! - `GET  /auth/tokens/key/{context}/{epoch}` — anonymous-token public key (ADR-0054, anonymous)
//! - `PUT  /auth/anon-credential/commitment` — register the channel-poll credential commitment
//!   (ADR-0092 section 4.2, bearer-auth; `409 commitment-already-registered` for another one)
//! - `POST /auth/anon-credential`    — issue the current period's channel-poll credential
//!   (ADR-0092 section 4.2, bearer-auth)
//! - `GET  /auth/anon-credential/key/{period}` — that credential's public key (anonymous)
//! - `PUT  /auth/login-handle`       — bind the caller's login handle (ADR-0054 S5, bearer-auth;
//!   `429 handle-bind-limited` per account, per address, and within the minimum interval
//!   between handle changes)
//!   Since ADR-0076 a bind that CHANGES the handle needs `fresh_grant` in the body (`403
//!   fresh-auth-required`), and with `previous_handle` + `recovery_email {address,
//!   vault_salt?, sealed_entropy?}` it moves the recovery email to the new handle instead of
//!   dropping it (`403 address-mismatch`, `409 handle-mismatch`).
//! - `POST /auth/login-handle/check` — `{state: "same" | "other" | "none"}` for the handle a
//!   device holds against its account's bound one (ADR-0076, bearer-auth)
//! - `DELETE /auth/login-handle`     — remove it (ADR-0054 S5, bearer-auth)
//! - `POST /auth/session/refresh`    — renew the session pair with a refresh credential
//!   (ADR-0073 section 3.2, anonymous, per-address limited)
//! - `POST /account/keys/reset/start` / `/finish` — reset the account keys: bump the key
//!   generation, replace the recovery credential, delete passkeys / vaults / grants /
//!   requests, mint the key-reset attestation, issue a fresh pair (ADR-0073 section 2,
//!   bearer-auth AND a `fresh_grant` from a password or passkey sign-in made just
//!   now, else `403 fresh-auth-required`; amended 2026-10-04 and 2026-10-06, when
//!   `DELETE /account` (header `x-lakko-fresh-grant`, for an account without a passkey;
//!   an account with one needs the passkey action grant instead), `POST /device/authorize/start`
//!   and `POST /device/requests/{id}/{pake,result}` (body `fresh_grant`) gained the
//!   same gate)
//! - `POST /auth/session/logout`     — raise the session floor of the caller's own device, so
//!   its access token and refresh credentials stop at auth at once (ADR-0074 section 2.1,
//!   bearer-auth, no body)
//! - `POST /auth/session/bind-device` — exchange a pair that names no device for one naming
//!   the caller's (ADR-0074 section 0.4, bearer-auth, body `device_id_b64` + `refresh_token`)
//! - `POST /account/devices/revoke`  — remove another device: raise its session floor so its
//!   credentials answer `401 device-revoked` (ADR-0074 section 2.1, bearer-auth AND a
//!   `fresh_grant` AND a `statement_b64` signed by the account's recorded identity key, else
//!   `403 fresh-auth-required` / `403 account-key-required`)
//! - `GET  /recovery-email/available` — whether this server can mail a recovery code
//!   (ADR-0075, anonymous) -> `{available}`
//! - `POST /recovery-email/enrol/start` / `/finish` — add or replace the account's recovery
//!   email with a mailed code (ADR-0075 section 6, bearer-auth; the start also needs a
//!   `fresh_grant` and the account's own bound `handle`; the finish optionally stores the
//!   44-byte email vault)
//! - `GET  /recovery-email`          — `{available, enabled, vault, phrase_saved, totp}` for the
//!   session's account (bearer-auth)
//! - `DELETE /recovery-email`        — remove the recovery email and the vault (bearer-auth +
//!   `fresh_grant`)
//! - `PUT  /recovery-email/vault`    — store an email vault where the account has none, for the
//!   address it already confirmed (ADR-0075 amended 2026-10-13; bearer-auth, NO fresh-auth
//!   grant; body `handle`, `address`, `vault_salt`, `sealed_entropy`; `403 address-mismatch`,
//!   `409 no-recovery-email` / `phrase-saved` / `vault-exists`)
//! - `DELETE /recovery-email/vault`  — the account becomes phrase-protected: delete the vault,
//!   mark the account, write the notice (bearer-auth + `fresh_grant`)
//! - `GET  /account/notices?after=`  — what happened to the account, for its own devices
//! - `GET  /account/phrase`           — `{saved, rotation_needed}`: whether saving a phrase takes a key rotation
//! - `POST /account/phrase/saved`     — the owner saved a phrase that was never in a vault (fresh-auth grant)
//!   (ADR-0075 section 9, bearer-auth; kept 60 days)
//! - `POST /recovery-email/recover/start` — mail a code if the handle and address belong
//!   together; one answer for a match and a miss (ADR-0075 section 7, anonymous)
//! - `POST /recovery-email/recover/verify` — the code -> `{ticket, totp_required}` or the
//!   uniform `401 code-refused` (anonymous)
//! - `POST /recovery-email/recover/totp` — the authenticator code, when the account has one
//!   (anonymous, under the ticket)
//! - `POST /recovery-email/recover/open` — the email vault (`mode: "vault"`, ticket spent) or
//!   `mode: "password"` (anonymous, under the ticket)
//! - `POST /recovery-email/recover/password/start` / `/finish` — choose a new password under
//!   the ticket (password mode): captures the recall anchor, replaces the envelope, lands a
//!   session pair and mints NO fresh-auth grant (anonymous)
//!
//! Since ADR-0075 the recall (`POST /account/recall/{status,start,finish}`) also has a phrase
//! arm: `status` takes an optional `handle` and answers `account_id_b64` and `phrase_arm`,
//! and `start` takes `proof: "phrase"` (a login against the anchored RECOVERY envelope),
//! refused for an account that had an email vault when the anchor was captured. A key
//! rotation (`/account/keys/rotate/finish`) takes `phrase_saved: true` for "Save a recovery
//! phrase".
//!
//! Since ADR-0074 every session-landing route also takes an optional `device_id_b64` (the
//! device the pair names), and `POST /auth/genesis/attest` records the account's identity
//! key on its first call and attests only that key afterwards (`409 identity-key-mismatch`).

#![forbid(unsafe_code)]

mod account_notices;
mod anon_credential;
mod anon_tokens;
mod backup_head;
mod call_jwt;
mod device_authorize;
mod device_request;
mod devices;
#[cfg(test)]
mod devices_tests;
mod genesis_attest;
mod key_reset;
mod key_rotate;
mod limits;
mod login_handle;
mod mail;
mod opaque;
mod passkey_vault;
mod phrase_saved;
mod pow;
mod recall;
#[cfg(test)]
mod recall_tests;
mod recovery_email;
#[cfg(test)]
mod recovery_email_tests;
mod room_cap;
mod session;
mod totp;
mod turn;
mod unclaimed;
mod webauthn;
mod webauthn_action;

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

/// Default access-token TTL — 15 minutes (ADR-0073 section 3.1; chosen, not
/// measured: a first value to measure the test stack's auth load against).
/// Override via `LAKKO_SESSION_TOKEN_TTL_SECS`.
const DEFAULT_SESSION_TOKEN_TTL_SECS: u64 = 900;

/// Default refresh-credential TTL — 30 days, the session lifetime before
/// ADR-0073. Override via `LAKKO_REFRESH_TOKEN_TTL_DAYS`.
const DEFAULT_REFRESH_TOKEN_TTL_DAYS: u64 = 30;

/// Default fresh-auth-grant TTL — 10 minutes (ADR-0073, amended 2026-10-04
/// and 2026-10-06). Chosen, not measured: long enough to read the warning,
/// write down a new 12-word phrase and confirm after signing in with the
/// password or a passkey, or to walk to another device and type its code,
/// short enough that a grant lifted from a device is soon worthless. Override via
/// `LAKKO_FRESH_GRANT_TTL_SECS`.
const DEFAULT_FRESH_GRANT_TTL_SECS: u64 = 600;

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
    /// Access-token TTL in milliseconds (`LAKKO_SESSION_TOKEN_TTL_SECS` x
    /// 1_000).
    pub(crate) session_token_ttl_ms: i64,
    /// Refresh-credential TTL in milliseconds (`LAKKO_REFRESH_TOKEN_TTL_DAYS`
    /// x 86_400_000).
    pub(crate) refresh_token_ttl_ms: i64,
    /// Fresh-auth-grant TTL in milliseconds (`LAKKO_FRESH_GRANT_TTL_SECS`
    /// x 1_000): how long after a password or passkey sign-in the account
    /// keys may be reset, the account deleted or a device added without
    /// signing in again.
    pub(crate) fresh_grant_ttl_ms: i64,
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
    /// `LAKKO_TURN_SHARED_SECRET` are unset; `POST /call/jwt` then
    /// answers `turn: null` and clients fall back to STUN-only ICE.
    pub(crate) turn: Option<Arc<turn::TurnConfig>>,
    /// ADR-0018 factor (c) — server-static AEAD key encrypting TOTP
    /// secrets at rest (`totp_secrets.secret_enc`). Loaded from
    /// `LAKKO_TOTP_ENC_KEY` (base64, exactly 32 bytes after decode);
    /// `None` when unset, in which case the three `/totp/*` routes
    /// return 503 — loud refusal, no silent fallback.
    pub(crate) totp_enc_key: Option<Arc<[u8; 32]>>,
    /// ADR-0072 — the key that wraps the passkey blobs at rest (`LAKKO_VAULT_ENC_KEY`). `None` disables the
    /// `/vault/passkey*` routes (503).
    pub(crate) vault_enc_key: Option<Arc<[u8; 32]>>,
    /// ADR-0078 section 2 — the per-room call admission cap (the rebuilt
    /// ADR-0033 throttle): in memory, keyed on the room id, no account.
    pub(crate) room_cap: Arc<room_cap::RoomAdmissionCap>,
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
    /// ADR-0092 section 4.2 — anonymous-credential issuance for channel
    /// polls. `None` when `LAKKO_ANON_CRED_MASTER_SECRET` is unset:
    /// `/auth/anon-credential*` return 503.
    pub(crate) anon_cred: Option<Arc<anon_credential::AnonCredIssuer>>,
    /// ADR-0054 S5 — pepper keying the stored login handles
    /// (`LAKKO_AUTH_HANDLE_PEPPER`, 64 hex). `None` disables every route that
    /// names an account by login handle (503 `login-by-name-disabled`).
    pub(crate) handle_pepper: Option<Arc<[u8; 32]>>,
    /// ADR-0054 S4 — per-name attempt counters (DB-backed, fail closed) and
    /// per-address token buckets (in memory) that replaced proof of work on
    /// every route except `/signup/start`.
    pub(crate) limits: Arc<limits::Limits>,
    /// ADR-0075 — the recovery email: the pepper keying `email_key` (and the
    /// subkeys derived from it) and the mail sender. `None` when
    /// `LAKKO_AUTH_EMAIL_PEPPER` is unset: the enrol, remove and recover
    /// routes answer 503 `recovery-email-disabled`. The feature also needs
    /// `vault_enc_key` and `handle_pepper`.
    pub(crate) recovery_email: Option<Arc<recovery_email::Config>>,
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

    let session_token_ttl_secs: u64 = std::env::var("LAKKO_SESSION_TOKEN_TTL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SESSION_TOKEN_TTL_SECS);
    // Saturating multiply guards against an env-typo like "9999999999999"
    // overflowing the i64 millisecond timestamp arithmetic later.
    let session_token_ttl_ms: i64 = i64::try_from(session_token_ttl_secs)
        .unwrap_or(i64::MAX / 1_000)
        .saturating_mul(1_000);
    let refresh_token_ttl_days: u64 = std::env::var("LAKKO_REFRESH_TOKEN_TTL_DAYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_REFRESH_TOKEN_TTL_DAYS);
    let refresh_token_ttl_ms: i64 = i64::try_from(refresh_token_ttl_days)
        .unwrap_or(i64::MAX / 86_400_000)
        .saturating_mul(86_400_000);
    let fresh_grant_ttl_secs: u64 = std::env::var("LAKKO_FRESH_GRANT_TTL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_FRESH_GRANT_TTL_SECS);
    let fresh_grant_ttl_ms: i64 = i64::try_from(fresh_grant_ttl_secs)
        .unwrap_or(i64::MAX / 1_000)
        .saturating_mul(1_000);

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
        tracing::info!("TURN credentials issued beside call tokens");
    } else {
        tracing::info!(
            "TURN credentials disabled (LAKKO_TURN_URIS / LAKKO_TURN_SHARED_SECRET not set)"
        );
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

    // ADR-0072 — the passkey-vault at-rest wrapping key. Loud-fail on a
    // malformed value; soft-disable when unset (the /vault/passkey* routes
    // then return 503).
    let vault_enc_key = passkey_vault::load_vault_enc_key().context("load LAKKO_VAULT_ENC_KEY")?;
    if vault_enc_key.is_some() {
        tracing::info!("passkey-vault endpoints enabled");
    } else {
        tracing::info!("passkey-vault endpoints disabled (LAKKO_VAULT_ENC_KEY not set)");
    }

    // ADR-0078 section 2 — the per-room call admission cap. Loud-fail on a
    // malformed knob.
    let room_cap = room_cap::RoomAdmissionCap::from_env().context("load the room admission cap")?;
    room_cap.log_startup();

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
    // ADR-0092 section 4.2 — the channel-poll credential master.
    let anon_cred =
        anon_credential::load_anon_credential().context("load anonymous-credential config")?;
    anon_credential::log_startup(anon_cred.as_ref());

    // ADR-0054 S5 — the login-handle pepper. Loud-fail on a malformed value;
    // dormant (with a warning) when unset.
    let handle_pepper =
        login_handle::load_handle_pepper().context("load LAKKO_AUTH_HANDLE_PEPPER")?;
    let names_enabled = handle_pepper.is_some();
    let unclaimed_ttl_secs =
        unclaimed::ttl_from_env().context("load LAKKO_AUTH_UNCLAIMED_ACCOUNT_TTL_SECS")?;
    login_handle::log_startup(handle_pepper.as_ref());
    // ADR-0054 S4 — attempt limits. Loud-fail on a malformed knob.
    let limits = Arc::new(
        limits::Limits::from_env(handle_pepper.as_ref()).context("load auth attempt limits")?,
    );
    limits.log_startup();

    // ADR-0075 — the recovery email: its pepper and its mail sender. Loud-fail
    // on a malformed pepper or SMTP setting; dormant (with a warning) when the
    // pepper is unset.
    let recovery_email = recovery_email::load().context("load the recovery-email config")?;

    let state = AppState {
        db: pool.clone(),
        opaque_setup: Arc::new(opaque_setup),
        session_token_secret: Arc::new(session_token_secret),
        session_token_ttl_ms,
        refresh_token_ttl_ms,
        fresh_grant_ttl_ms,
        webauthn: webauthn_instance,
        webauthn_action_core,
        call_jwt_signing: Arc::new(call_jwt_signing),
        attest_key: Arc::new(attest_key),
        turn: turn_config.map(Arc::new),
        totp_enc_key: totp_enc_key.map(Arc::new),
        vault_enc_key: vault_enc_key.map(Arc::new),
        room_cap: Arc::new(room_cap),
        backup_head_mint_rate,
        anon_tokens: anon_tokens.clone().map(Arc::new),
        anon_cred: anon_cred.map(Arc::new),
        handle_pepper: handle_pepper.map(Arc::new),
        recovery_email: recovery_email.map(Arc::new),
        limits: limits.clone(),
    };

    recovery_email::log_startup(&state);

    // Background tasks: periodically delete expired rows from the
    // `opaque_login_states`, `pow_challenges`, and `webauthn_states`
    // tables. Aborted when the service stops.
    let _login_pruner = opaque::spawn_login_state_pruner(pool.clone());
    let _pow_pruner = pow::spawn_pruner(pool.clone());
    // ADR-0054 S4 — expired name counters + refilled address buckets.
    let _limits_pruner = limits::spawn_pruner(pool.clone(), limits);
    let _webauthn_pruner = webauthn::spawn_state_pruner(pool.clone());
    let _floor_pruner = session::spawn_floor_pruner(pool.clone(), refresh_token_ttl_ms);
    // ADR-0074 section 0.2 — expired recall anchors and stale recall states.
    let _recall_pruner = recall::spawn_pruner(pool.clone());
    // ADR-0075 — expired mailed-code challenges and tickets; notices past
    // their 60 days.
    let _email_pruner = recovery_email::spawn_pruner(pool.clone());
    let _notice_pruner = account_notices::spawn_pruner(pool.clone());
    // ADR-0032 guest lifecycle — an account nobody named within a day is
    // deleted. Off where sign-in by name is not configured.
    let _unclaimed_pruner =
        unclaimed::spawn_pruner(pool.clone(), names_enabled, unclaimed_ttl_secs);
    // ADR-0078 section 2 — the `call-join` spent-set `POST /call/jwt` writes.
    let _call_join_spent_pruner = anon_tokens.as_ref().map(|i| {
        lakko_anon_token_server::TokenVerifier::new(pool.clone(), Some(i.server.clone()))
            .spawn_pruner()
    });
    // ADR-0054 — drop issuance-quota rows older than the previous epoch.
    // ADR-0092 — drop credential issuance rows older than the previous period.
    let _anon_cred_pruner = anon_credential::spawn_pruner(pool.clone());
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
        // auth state). Bearer-auth + ONE fresh proof (ADR-0073, amended
        // 2026-10-06): the ADR-0014 passkey action grant when the account has
        // a passkey, else a fresh-auth grant in the `x-lakko-fresh-grant`
        // header (the discovery + storage tiers are purged by the client via
        // their own bearer-auth'd deletes).
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
        // ADR-0018 factor (a) — mint a single-use device grant. Bearer-auth
        // + a fresh-auth grant in the body (ADR-0073, amended 2026-10-06):
        // the caller is the ALREADY-authenticated existing device, whose
        // person signed in with the password or a passkey just now,
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
        // ADR-0072 decision 6 — the phrase entropy sealed under a passkey's
        // PRF output; opaque per-credential blobs only the account reads,
        // stores, lists and removes.
        .route(
            "/vault/passkey",
            put(passkey_vault::passkey_put).delete(passkey_vault::passkey_delete),
        )
        .route("/vault/passkey/get", post(passkey_vault::passkey_get))
        .route("/vault/passkeys", get(passkey_vault::passkey_list))
        // ADR-0049 section 2 (the device link since 2026-09-28) — the
        // signed-in device's side of a camera-less add-device confirmation:
        // see the open request, post its SPAKE2 message once, record the
        // outcome once, or decline. Answering and recording the outcome
        // need a fresh-auth grant in the body (ADR-0073, amended 2026-10-06);
        // seeing the request and declining it need the session only.
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
        // ADR-0076 section 2 — how the handle a device holds relates to the
        // one its account is bound to; the session sync binds only on
        // `none`. Counted per account inside.
        .route("/auth/login-handle/check", post(login_handle::check_handle))
        // ADR-0073 — reset the account keys: one transaction that bumps the
        // key generation (signing every other device out), replaces the
        // recovery credential (the OPAQUE registration made from the new
        // phrase's seed), deletes the passkeys, vaults, grants and requests,
        // mints the key-reset attestation and issues the caller a fresh
        // pair. Bearer-auth: the session names the account. Per-account
        // limited. No passkey action grant: this is the lost-everything path,
        // where the passkey may be gone too.
        .route("/account/keys/reset/start", post(key_reset::start))
        .route("/account/keys/reset/finish", post(key_reset::finish))
        // ADR-0074 section 3.4 — rotate the account keys and keep the other
        // devices. Bearer + fresh-auth grant + a supersession proof by the
        // identity key auth has recorded: someone who holds only the
        // password can reset (recallable) but cannot rotate. One
        // transaction: bump the generation, record the new key, replace the
        // recovery credential, delete the passkey vaults (not the passkeys),
        // floor the devices removed with it, mint the attestation.
        .route("/account/keys/rotate/start", post(key_rotate::start))
        .route("/account/keys/rotate/finish", post(key_rotate::finish))
        // ADR-0074 sprint T1 — per-device revocation. `logout` raises the
        // session floor of the caller's own device (no grant: signing out
        // must always work); `bind-device` replaces a pair that names no
        // device with one that names the caller's; `devices/revoke` removes
        // ANOTHER device and needs a fresh-auth grant AND a statement signed
        // by the account's identity key, so the password alone cannot wipe
        // the owner's devices.
        // ADR-0075 sections 6 and 9 — the recovery email of the session's
        // account: add or replace it (a mailed code; the start needs a
        // fresh-auth grant and sends the letter, so it is also limited per
        // address), remove it, read its state, mark the account
        // phrase-protected, and the notices the account's devices show.
        .route(
            "/recovery-email/enrol/start",
            post(recovery_email::enrol_start).route_layer(ip(limits::Category::Email)),
        )
        .route(
            "/recovery-email/enrol/finish",
            post(recovery_email::enrol_finish),
        )
        .route(
            "/recovery-email",
            get(recovery_email::status).delete(recovery_email::remove),
        )
        .route(
            "/recovery-email/vault",
            put(recovery_email::put_vault).delete(recovery_email::remove_vault),
        )
        .route("/account/notices", get(account_notices::list))
        // ADR-0075 section 3, amended 2026-11-01 - saving a phrase that was
        // never sealed in an email vault changes no key.
        .route("/account/phrase", get(phrase_saved::status))
        .route("/account/phrase/saved", post(phrase_saved::mark))
        .route("/auth/session/logout", post(session::logout))
        .route("/auth/session/bind-device", post(session::bind_device))
        .route("/account/devices/revoke", post(devices::revoke))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            webauthn::require_session,
        ));

    // ADR-0078 section 2 — `POST /call/jwt` mints a JWT scoped to
    // (room, random peer id, role) plus a TURN credential. No session: it
    // spends one anonymous `call-join` token, so the issuer never learns who
    // joins which room. The SFU verifies the JWT against the public key it
    // fetched from `/.well-known/jwks.json`.
    let call_jwt_routes = Router::new().route("/call/jwt", post(call_jwt::issue_call_jwt));

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

    // ADR-0092 section 4.2 — the channel-poll credential: register the
    // account's commitment once, then one credential per period. Bearer-auth;
    // the per-period public key is anonymous (public router below).
    let anon_cred_routes = Router::new()
        .route(
            "/auth/anon-credential/commitment",
            put(anon_credential::register_commitment),
        )
        .route(
            "/auth/anon-credential",
            post(anon_credential::issue_credential),
        )
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
        // ADR-0073 section 3.2 — renew the session pair. ANONYMOUS: the
        // refresh credential is the authorisation. Per-address limited; the
        // generation check inside is what signs a reset-away device out.
        .route(
            "/auth/session/refresh",
            post(session::refresh).route_layer(ip(limits::Category::Attempt)),
        )
        // ADR-0074 section 1.2 — the recall: an old device takes the account
        // back after a key reset. ANONYMOUS (its session was superseded by
        // the reset): every request carries an account statement signed by
        // the identity key the account had before the change, checked first,
        // and everything that is not a wrong password is one `401
        // recall-refused`. Per-address limited; the per-anchor attempt
        // budget is charged inside `start`, after the statement.
        .route(
            "/account/recall/status",
            post(recall::status).route_layer(ip(limits::Category::Attempt)),
        )
        .route(
            "/account/recall/start",
            post(recall::start).route_layer(ip(limits::Category::Attempt)),
        )
        .route(
            "/account/recall/finish",
            post(recall::finish).route_layer(ip(limits::Category::Attempt)),
        )
        // ADR-0075 section 7 — getting back in with a mailed code. ANONYMOUS.
        // `start` answers the same whatever was typed and is the route that
        // can send a letter (its own address bucket, and the per-handle
        // letter counter inside); the rest are guessing routes behind the
        // shared attempt bucket, each bound to a challenge or a ticket.
        .route(
            "/recovery-email/available",
            get(recovery_email::available),
        )
        .route(
            "/recovery-email/recover/start",
            post(recovery_email::recover_start).route_layer(ip(limits::Category::Email)),
        )
        .route(
            "/recovery-email/recover/verify",
            post(recovery_email::recover_verify).route_layer(ip(limits::Category::Attempt)),
        )
        .route(
            "/recovery-email/recover/totp",
            post(recovery_email::recover_totp).route_layer(ip(limits::Category::Attempt)),
        )
        .route(
            "/recovery-email/recover/open",
            post(recovery_email::recover_open).route_layer(ip(limits::Category::Attempt)),
        )
        .route(
            "/recovery-email/recover/password/start",
            post(recovery_email::password_start).route_layer(ip(limits::Category::Attempt)),
        )
        .route(
            "/recovery-email/recover/password/finish",
            post(recovery_email::password_finish).route_layer(ip(limits::Category::Attempt)),
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
        // ADR-0092 — the channel-poll credential key of a period. ANONYMOUS:
        // a client checks the issuance proof against this key.
        .route(
            "/auth/anon-credential/key/{period}",
            get(anon_credential::public_key),
        )
        .merge(anon_cred_routes)
        .layer(TraceLayer::new_for_http().make_span_with(lakko_log_hygiene::request_span))
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
