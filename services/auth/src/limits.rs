// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Server-side attempt limits for the pre-session routes (ADR-0054 S4).
//!
//! Proof of work used to gate every anonymous auth route. It priced honest
//! phones without a JIT far above an attacker's hardware, so it now survives
//! only on `/signup/start`; every other route is limited by counters the
//! server keeps:
//!
//! - **Per name** ([`charge_name`]): a fixed-window counter keyed on what the
//!   request names — the login handle it tries to log into (ADR-0054 S5), the
//!   contact hash it asks to send a code to, the account a device request targets. The
//!   counter is charged BEFORE the handler looks anything up, and it exists
//!   for a name with no account exactly as for one with an account, so a
//!   refusal says nothing about existence (same status, same body, same
//!   queries on both paths). Stored in `auth_name_limits` as a BLAKE3 of the
//!   name; a store error refuses the request (fail closed, `503`).
//! - **Per source address** ([`ip_gate`]): an in-memory token bucket per
//!   (route category, address). Addresses never reach the database. The
//!   address is what the fronting nginx wrote (`X-Real-IP`, else the
//!   rightmost `X-Forwarded-For` entry), else the socket peer; a request with
//!   none of them is charged to one shared bucket rather than admitted
//!   unmetered.
//!
//! A limit reached answers `429` with `{"error": <code>, "retry_after_secs":
//! N}` and a `Retry-After: N` header — never `401`, so no client reads it as
//! a bad credential or a lost session. The codes are one per route family so
//! the clients can map each to its copy (`docs/ux/anti-abuse.md` §2):
//! [`CODE_TOO_MANY_ATTEMPTS`], [`CODE_RECOVERY_CODE_SEND_LIMITED`],
//! [`CODE_DEVICE_REQUEST_LIMITED`], [`CODE_SIGNUP_RATE_LIMITED`].
//!
//! Every default below is **chosen, not measured**; each has an env knob of
//! the form `N/SECS` (at most `N` per `SECS` seconds).
//!
//! No limiter here fails open: the name counters fail closed on a store
//! error (the guessing and code-sending routes are exactly where an
//! unmetered window would be spent by an attacker), and the address buckets
//! have no store to fail.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderValue, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use lakko_crypto::hash::blake3_derive_key_from_material;
use serde_json::json;
use sqlx::MySqlPool;

/// Body code for a guessing route (login, password reset, TOTP recovery, the
/// word vault, a recovery-code verify, device authorize) whose name counter
/// or address bucket is empty.
pub(crate) const CODE_TOO_MANY_ATTEMPTS: &str = "too-many-attempts";

/// Body code for a refused one-time-code send (email / SMS).
pub(crate) const CODE_RECOVERY_CODE_SEND_LIMITED: &str = "recovery-code-send-limited";

/// Body code for a refused `/device/request` (the "Is this you?" prompt).
pub(crate) const CODE_DEVICE_REQUEST_LIMITED: &str = "device-request-limited";

/// Body code for a refused `/signup/start` or `/pow/challenge`.
pub(crate) const CODE_SIGNUP_RATE_LIMITED: &str = "signup-rate-limited";

/// Body code when the name-counter store cannot be read (`503`).
pub(crate) const CODE_LIMITER_UNAVAILABLE: &str = "rate-limiter-unavailable";

/// Domain separator for the stored name-counter key.
const NAME_KEY_DOMAIN: &str = "lakko/auth/name-limit/v1";

/// Saturation ceiling for the stored hit count (the column is `INT UNSIGNED`).
const MAX_HITS: u32 = 1_000_000_000;

/// How often the pruner sweeps expired counters and idle buckets.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// At most `limit` events per `window_secs` seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    /// Events admitted per window (and the address bucket's capacity).
    pub(crate) limit: u32,
    /// Window length in seconds.
    pub(crate) window_secs: u32,
}

impl Window {
    pub(crate) const fn new(limit: u32, window_secs: u32) -> Self {
        Self { limit, window_secs }
    }

    /// Parse `N/SECS`, both positive.
    fn parse(raw: &str) -> Option<Self> {
        let (n, secs) = raw.trim().split_once('/')?;
        let limit: u32 = n.trim().parse().ok()?;
        let window_secs: u32 = secs.trim().parse().ok()?;
        (limit > 0 && window_secs > 0).then_some(Self { limit, window_secs })
    }
}

/// What a name counter counts. The discriminant is stored in
/// `auth_name_limits.scope`; never renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Scope {
    /// `/login/start`, keyed on the login handle (ADR-0054 S5).
    LoginPassword = 1,
    /// `/recovery/login/start`, keyed on the login handle.
    LoginRecovery = 2,
    /// `/password/reset/start`, keyed on the login handle.
    PasswordReset = 3,
    /// `/totp/recover`, keyed on the login handle.
    TotpRecover = 4,
    /// `/vault/recover/finish`, keyed on the login handle.
    VaultGuess = 5,
    /// `/recovery-contact/recover/finish`, keyed on login handle + kind.
    RecoveryCodeGuess = 6,
    /// Any one-time-code send, keyed on the contact hash.
    CodeSendContact = 7,
    /// `/recovery-contact/enroll/start`, keyed on the signed-in account.
    CodeSendAccount = 8,
    /// `/device/request`, keyed on the target account id.
    DeviceRequestTarget = 9,
    /// `/device/authorize/start`, keyed on the signed-in account.
    DeviceGrantMint = 10,
}

impl Scope {
    const ALL: [Self; 10] = [
        Self::LoginPassword,
        Self::LoginRecovery,
        Self::PasswordReset,
        Self::TotpRecover,
        Self::VaultGuess,
        Self::RecoveryCodeGuess,
        Self::CodeSendContact,
        Self::CodeSendAccount,
        Self::DeviceRequestTarget,
        Self::DeviceGrantMint,
    ];

    const fn index(self) -> usize {
        self as usize - 1
    }

    const fn env_var(self) -> &'static str {
        match self {
            Self::LoginPassword => "LAKKO_AUTH_LIMIT_LOGIN",
            Self::LoginRecovery => "LAKKO_AUTH_LIMIT_RECOVERY_LOGIN",
            Self::PasswordReset => "LAKKO_AUTH_LIMIT_PASSWORD_RESET",
            Self::TotpRecover => "LAKKO_AUTH_LIMIT_TOTP_RECOVER",
            Self::VaultGuess => "LAKKO_AUTH_LIMIT_VAULT_GUESS",
            Self::RecoveryCodeGuess => "LAKKO_AUTH_LIMIT_RECOVERY_CODE_GUESS",
            Self::CodeSendContact => "LAKKO_AUTH_LIMIT_CODE_SEND_CONTACT",
            Self::CodeSendAccount => "LAKKO_AUTH_LIMIT_CODE_SEND_ACCOUNT",
            Self::DeviceRequestTarget => "LAKKO_AUTH_LIMIT_DEVICE_REQUEST",
            Self::DeviceGrantMint => "LAKKO_AUTH_LIMIT_DEVICE_GRANT",
        }
    }

    /// Defaults — chosen, not measured. The TOTP and vault values are the
    /// per-row lockouts they replace (5 in 15 minutes).
    const fn default_window(self) -> Window {
        match self {
            Self::LoginPassword
            | Self::LoginRecovery
            | Self::PasswordReset
            | Self::DeviceGrantMint => Window::new(10, 900),
            Self::TotpRecover | Self::VaultGuess | Self::RecoveryCodeGuess => Window::new(5, 900),
            Self::CodeSendContact => Window::new(3, 900),
            Self::CodeSendAccount => Window::new(5, 3600),
            Self::DeviceRequestTarget => Window::new(5, 600),
        }
    }

    const fn refusal_code(self) -> &'static str {
        match self {
            Self::LoginPassword
            | Self::LoginRecovery
            | Self::PasswordReset
            | Self::TotpRecover
            | Self::VaultGuess
            | Self::RecoveryCodeGuess
            | Self::DeviceGrantMint => CODE_TOO_MANY_ATTEMPTS,
            Self::CodeSendContact | Self::CodeSendAccount => CODE_RECOVERY_CODE_SEND_LIMITED,
            Self::DeviceRequestTarget => CODE_DEVICE_REQUEST_LIMITED,
        }
    }
}

/// A family of routes sharing one per-address bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Category {
    /// Login, recovery login, password reset, TOTP recovery, the word vault,
    /// a recovery-code verify, device authorize start / redeem.
    Attempt,
    /// One-time-code sends (recover start, enroll start).
    CodeSend,
    /// `/device/request`.
    DeviceRequest,
    /// `/signup/start`.
    Signup,
    /// `/pow/challenge` (used only by signup).
    Challenge,
}

impl Category {
    const ALL: [Self; 5] = [
        Self::Attempt,
        Self::CodeSend,
        Self::DeviceRequest,
        Self::Signup,
        Self::Challenge,
    ];

    const fn index(self) -> usize {
        self as usize
    }

    const fn env_var(self) -> &'static str {
        match self {
            Self::Attempt => "LAKKO_AUTH_IP_LIMIT_ATTEMPT",
            Self::CodeSend => "LAKKO_AUTH_IP_LIMIT_CODE_SEND",
            Self::DeviceRequest => "LAKKO_AUTH_IP_LIMIT_DEVICE_REQUEST",
            Self::Signup => "LAKKO_AUTH_IP_LIMIT_SIGNUP",
            Self::Challenge => "LAKKO_AUTH_IP_LIMIT_POW_CHALLENGE",
        }
    }

    /// Defaults — chosen, not measured. Generous enough for a household or
    /// an office behind one address; an attacker rotating addresses is what
    /// the name counters are for.
    const fn default_window(self) -> Window {
        match self {
            Self::Attempt => Window::new(30, 60),
            Self::CodeSend | Self::DeviceRequest | Self::Signup => Window::new(10, 3600),
            Self::Challenge => Window::new(30, 3600),
        }
    }

    const fn refusal_code(self) -> &'static str {
        match self {
            Self::Attempt => CODE_TOO_MANY_ATTEMPTS,
            Self::CodeSend => CODE_RECOVERY_CODE_SEND_LIMITED,
            Self::DeviceRequest => CODE_DEVICE_REQUEST_LIMITED,
            Self::Signup | Self::Challenge => CODE_SIGNUP_RATE_LIMITED,
        }
    }
}

/// Every limit the service enforces, plus the in-memory address buckets.
#[derive(Debug)]
pub(crate) struct Limits {
    names: [Window; Scope::ALL.len()],
    ips: [Window; Category::ALL.len()],
    buckets: Mutex<HashMap<(Category, IpAddr), Bucket>>,
}

impl Limits {
    /// The documented defaults (tests use this directly).
    #[cfg(test)]
    pub(crate) fn defaults() -> Self {
        Self::with_windows(
            Scope::ALL.map(Scope::default_window),
            Category::ALL.map(Category::default_window),
        )
    }

    fn with_windows(names: [Window; Scope::ALL.len()], ips: [Window; Category::ALL.len()]) -> Self {
        Self {
            names,
            ips,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Read every knob; an unset knob takes its default, a malformed one
    /// fails startup.
    ///
    /// # Errors
    ///
    /// Returns an error naming the first knob that is set but is not `N/SECS`
    /// with both numbers positive.
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        fn read(var: &str, default: Window) -> anyhow::Result<Window> {
            match std::env::var(var) {
                Ok(v) if !v.trim().is_empty() => Window::parse(&v)
                    .ok_or_else(|| anyhow::anyhow!("{var} must be N/SECS with both positive")),
                _ => Ok(default),
            }
        }
        let mut names = Scope::ALL.map(Scope::default_window);
        for scope in Scope::ALL {
            names[scope.index()] = read(scope.env_var(), scope.default_window())?;
        }
        let mut ips = Category::ALL.map(Category::default_window);
        for cat in Category::ALL {
            ips[cat.index()] = read(cat.env_var(), cat.default_window())?;
        }
        Ok(Self::with_windows(names, ips))
    }

    /// Override one name window (tests).
    #[cfg(test)]
    pub(crate) fn with_name_window(mut self, scope: Scope, window: Window) -> Self {
        self.names[scope.index()] = window;
        self
    }

    /// Override one address window (tests).
    #[cfg(test)]
    pub(crate) fn with_ip_window(mut self, cat: Category, window: Window) -> Self {
        self.ips[cat.index()] = window;
        self
    }

    /// The window for a name scope.
    pub(crate) const fn name_window(&self, scope: Scope) -> Window {
        self.names[scope.index()]
    }

    /// Log the effective limits once at startup.
    pub(crate) fn log_startup(&self) {
        for scope in Scope::ALL {
            let w = self.name_window(scope);
            tracing::info!(
                knob = scope.env_var(),
                limit = w.limit,
                window_secs = w.window_secs,
                "auth name limit"
            );
        }
        for cat in Category::ALL {
            let w = self.ips[cat.index()];
            tracing::info!(
                knob = cat.env_var(),
                limit = w.limit,
                window_secs = w.window_secs,
                "auth address limit"
            );
        }
    }

    /// Charge one request from `ip` to `cat`'s bucket.
    fn charge_ip_at(&self, cat: Category, ip: IpAddr, now: Instant) -> Result<(), u64> {
        let w = self.ips[cat.index()];
        let capacity = f64::from(w.limit);
        let rate = capacity / f64::from(w.window_secs);
        let mut buckets = match self.buckets.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let bucket = buckets.entry((cat, ip)).or_insert(Bucket {
            tokens: capacity,
            last: now,
        });
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = rate.mul_add(elapsed, bucket.tokens).min(capacity);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let wait = ((1.0 - bucket.tokens) / rate).ceil();
            // `wait` is bounded by one window (u32 seconds), so the cast
            // cannot truncate a meaningful value.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let secs = wait.max(1.0) as u64;
            Err(secs)
        }
    }

    /// Drop buckets that have refilled completely; returns how many.
    fn prune_idle(&self, now: Instant) -> usize {
        let mut buckets = match self.buckets.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let before = buckets.len();
        let ips = self.ips;
        buckets.retain(|(cat, _), b| {
            let full_after = Duration::from_secs(u64::from(ips[cat.index()].window_secs));
            now.saturating_duration_since(b.last) < full_after
        });
        before - buckets.len()
    }

    /// The longest name window, for the pruner.
    fn max_name_window_secs(&self) -> u32 {
        self.names.iter().map(|w| w.window_secs).max().unwrap_or(1)
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// A request refused by a limiter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LimitError {
    /// A limit is reached: `429`, body code + `Retry-After`.
    Limited {
        /// Stable body code (one per route family).
        code: &'static str,
        /// Seconds until the limit admits a request again (>= 1).
        retry_after_secs: u64,
    },
    /// The counter store failed: `503`, the request is refused.
    Unavailable,
}

impl IntoResponse for LimitError {
    fn into_response(self) -> Response {
        match self {
            Self::Limited {
                code,
                retry_after_secs,
            } => {
                let mut resp = (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(json!({ "error": code, "retry_after_secs": retry_after_secs })),
                )
                    .into_response();
                if let Ok(v) = HeaderValue::from_str(&retry_after_secs.to_string()) {
                    resp.headers_mut().insert(header::RETRY_AFTER, v);
                }
                resp
            },
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": CODE_LIMITER_UNAVAILABLE })),
            )
                .into_response(),
        }
    }
}

// ---------------------------------------------------------------------------
// Name counters
// ---------------------------------------------------------------------------

fn name_key(scope: Scope, name: &[u8]) -> [u8; 32] {
    let mut material = Vec::with_capacity(1 + name.len());
    material.push(scope as u8);
    material.extend_from_slice(name);
    blake3_derive_key_from_material(NAME_KEY_DOMAIN, &material)
}

/// Charge one attempt against `scope` for `name` and refuse once the window's
/// limit is exceeded.
///
/// Call it before the handler looks `name` up, on every path: the counter
/// is keyed on the name alone, so an unknown name is counted and refused
/// exactly like a known one.
///
/// # Errors
///
/// [`LimitError::Limited`] once the window is spent; [`LimitError::Unavailable`]
/// when the store fails (fail closed).
pub(crate) async fn charge_name(
    pool: &MySqlPool,
    limits: &Limits,
    scope: Scope,
    name: &[u8],
) -> Result<(), LimitError> {
    let w = limits.name_window(scope);
    match bump(pool, scope, &name_key(scope, name), w.window_secs).await {
        Ok((hits, _)) if hits <= w.limit => Ok(()),
        Ok((_, retry_after_secs)) => {
            tracing::debug!(scope = ?scope, "auth name limit reached");
            Err(LimitError::Limited {
                code: scope.refusal_code(),
                retry_after_secs,
            })
        },
        Err(e) => {
            tracing::error!(error = %e, scope = ?scope, "auth name limit store failed; refusing");
            Err(LimitError::Unavailable)
        },
    }
}

/// Forget the counter for `name` (tests stand in for a window elapsing).
#[cfg(test)]
pub(crate) async fn reset_name(pool: &MySqlPool, scope: Scope, name: &[u8]) {
    sqlx::query("DELETE FROM auth_name_limits WHERE scope = ? AND key_hash = ?")
        .bind(scope as u8)
        .bind(name_key(scope, name).as_slice())
        .execute(pool)
        .await
        .expect("reset name limit");
}

/// Atomically bump the fixed-window counter; returns `(hits in the current
/// window, seconds until it resets)`.
async fn bump(
    pool: &MySqlPool,
    scope: Scope,
    key: &[u8; 32],
    window_secs: u32,
) -> Result<(u32, u64), sqlx::Error> {
    let window = i64::from(window_secs);
    let mut tx = pool.begin().await?;
    // The `ON DUPLICATE KEY UPDATE` reads the pre-update `window_start` for
    // both conditions (assignments run left to right and `hits` comes first).
    sqlx::query(
        "INSERT INTO auth_name_limits (scope, key_hash, window_start, hits) \
         VALUES (?, ?, NOW(3), 1) \
         ON DUPLICATE KEY UPDATE \
           hits = IF(window_start <= NOW(3) - INTERVAL ? SECOND, 1, LEAST(hits + 1, ?)), \
           window_start = IF(window_start <= NOW(3) - INTERVAL ? SECOND, NOW(3), window_start)",
    )
    .bind(scope as u8)
    .bind(key.as_slice())
    .bind(window)
    .bind(MAX_HITS)
    .bind(window)
    .execute(&mut *tx)
    .await?;
    let (hits, remaining): (u32, i64) = sqlx::query_as(
        "SELECT hits, CAST(? - TIMESTAMPDIFF(SECOND, window_start, NOW(3)) AS SIGNED) \
         FROM auth_name_limits WHERE scope = ? AND key_hash = ?",
    )
    .bind(window)
    .bind(scope as u8)
    .bind(key.as_slice())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((hits, u64::try_from(remaining.max(1)).unwrap_or(1)))
}

// ---------------------------------------------------------------------------
// Address buckets (middleware)
// ---------------------------------------------------------------------------

/// The key requests without any identifiable source share.
const UNIDENTIFIED: IpAddr = IpAddr::V6(Ipv6Addr::UNSPECIFIED);

/// The source address, trusting only what nginx wrote.
///
/// `X-Real-IP` is set by nginx to `$remote_addr`; of `X-Forwarded-For` only
/// the rightmost entry is nginx's (the rest is whatever the client claimed).
/// The service binds loopback in production, so nobody reaches it without
/// passing nginx.
fn source_ip(req: &Request<Body>) -> Option<IpAddr> {
    let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok());
    if let Some(ip) = header("x-real-ip").and_then(|s| s.trim().parse::<IpAddr>().ok()) {
        return Some(ip);
    }
    if let Some(ip) = header("x-forwarded-for")
        .and_then(|s| s.rsplit(',').next())
        .and_then(|last| last.trim().parse::<IpAddr>().ok())
    {
        return Some(ip);
    }
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
}

/// Route middleware: charge the request's source address to `cat`'s bucket,
/// `429` when it is empty. Wire with
/// `post(handler).route_layer(from_fn_with_state((limits, cat), ip_gate))`.
pub(crate) async fn ip_gate(
    State((limits, cat)): State<(Arc<Limits>, Category)>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let ip = source_ip(&req).unwrap_or(UNIDENTIFIED);
    match limits.charge_ip_at(cat, ip, Instant::now()) {
        Ok(()) => next.run(req).await,
        Err(retry_after_secs) => {
            tracing::debug!(category = ?cat, "auth address limit reached");
            LimitError::Limited {
                code: cat.refusal_code(),
                retry_after_secs,
            }
            .into_response()
        },
    }
}

// ---------------------------------------------------------------------------
// Pruner
// ---------------------------------------------------------------------------

/// Periodically drop name counters whose window has long passed and address
/// buckets that have refilled.
pub(crate) fn spawn_pruner(pool: MySqlPool, limits: Arc<Limits>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let dropped = limits.prune_idle(Instant::now());
            if dropped > 0 {
                tracing::debug!(dropped, "auth address buckets pruned");
            }
            let result = sqlx::query(
                "DELETE FROM auth_name_limits WHERE window_start < NOW(3) - INTERVAL ? SECOND",
            )
            .bind(i64::from(limits.max_name_window_secs()))
            .execute(&pool)
            .await;
            if let Err(e) = result {
                tracing::warn!(error = %e, "auth_name_limits prune failed");
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, last))
    }

    #[test]
    fn window_parse() {
        assert_eq!(Window::parse("10/900"), Some(Window::new(10, 900)));
        assert_eq!(Window::parse(" 3 / 60 "), Some(Window::new(3, 60)));
        assert_eq!(Window::parse("0/60"), None);
        assert_eq!(Window::parse("5/0"), None);
        assert_eq!(Window::parse("5"), None);
        assert_eq!(Window::parse("a/b"), None);
    }

    #[test]
    fn scope_discriminants_are_pinned() {
        // Stored in the database; a renumbering would merge two counters.
        let pinned: Vec<u8> = Scope::ALL.iter().map(|s| *s as u8).collect();
        assert_eq!(pinned, (1..=10).collect::<Vec<u8>>());
        for (i, s) in Scope::ALL.iter().enumerate() {
            assert_eq!(s.index(), i);
        }
        for (i, c) in Category::ALL.iter().enumerate() {
            assert_eq!(c.index(), i);
        }
    }

    #[test]
    fn name_key_separates_scopes_and_names() {
        let a = name_key(Scope::LoginPassword, b"acct");
        assert_eq!(a, name_key(Scope::LoginPassword, b"acct"));
        assert_ne!(a, name_key(Scope::LoginRecovery, b"acct"));
        assert_ne!(a, name_key(Scope::LoginPassword, b"acct2"));
    }

    #[test]
    fn bucket_admits_capacity_then_refuses_with_retry_after() {
        let limits = Limits::defaults().with_ip_window(Category::Signup, Window::new(3, 3600));
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(limits.charge_ip_at(Category::Signup, ip(1), t0).is_ok());
        }
        // One token refills every 1200 s.
        assert_eq!(limits.charge_ip_at(Category::Signup, ip(1), t0), Err(1200));
        // Another address and another category are untouched.
        assert!(limits.charge_ip_at(Category::Signup, ip(2), t0).is_ok());
        assert!(limits.charge_ip_at(Category::Attempt, ip(1), t0).is_ok());
        // After one refill period exactly one more is admitted.
        let t1 = t0 + Duration::from_secs(1200);
        assert!(limits.charge_ip_at(Category::Signup, ip(1), t1).is_ok());
        assert!(limits.charge_ip_at(Category::Signup, ip(1), t1).is_err());
    }

    #[test]
    fn prune_drops_only_refilled_buckets() {
        let limits = Limits::defaults().with_ip_window(Category::Attempt, Window::new(2, 60));
        let t0 = Instant::now();
        assert!(limits.charge_ip_at(Category::Attempt, ip(1), t0).is_ok());
        assert_eq!(limits.prune_idle(t0 + Duration::from_secs(30)), 0);
        assert_eq!(limits.prune_idle(t0 + Duration::from_secs(60)), 1);
    }

    #[test]
    fn source_ip_trusts_real_ip_then_rightmost_forwarded() {
        let req = Request::builder()
            .header("x-forwarded-for", "203.0.113.9, 198.51.100.7")
            .body(Body::empty())
            .unwrap();
        assert_eq!(source_ip(&req), "198.51.100.7".parse().ok());
        let req = Request::builder()
            .header("x-real-ip", "198.51.100.8")
            .header("x-forwarded-for", "203.0.113.9")
            .body(Body::empty())
            .unwrap();
        assert_eq!(source_ip(&req), "198.51.100.8".parse().ok());
        let req = Request::builder().body(Body::empty()).unwrap();
        assert_eq!(source_ip(&req), None);
    }

    #[tokio::test]
    async fn refusal_is_429_with_retry_after_and_code() {
        use http_body_util::BodyExt as _;
        let resp = LimitError::Limited {
            code: CODE_TOO_MANY_ATTEMPTS,
            retry_after_secs: 42,
        }
        .into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(resp.headers()[header::RETRY_AFTER], "42");
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], CODE_TOO_MANY_ATTEMPTS);
        assert_eq!(v["retry_after_secs"], 42);

        let resp = LimitError::Unavailable.into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
