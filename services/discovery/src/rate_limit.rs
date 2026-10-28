// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

#![allow(unreachable_pub, dead_code, clippy::float_cmp)]

//! Token-bucket rate limiter for discovery's
//! anonymous endpoints — OPRF evaluation, username
//! lookup, channel-name lookup, the invite ledgers'
//! fetch + consume — and for its session-gated writes.
//! Anonymous requests with a valid session token are
//! charged per account plus a per-IP ceiling; requests
//! without one per IP (see [`DiscoveryLimits`]). A
//! session-gated write is charged to its account
//! ([`write_middleware`]).
//!
//! ## Threat model
//!
//! Since ADR-0054 S3 the per-action ration is an
//! anonymous token (the OPRF evaluation, channel-name
//! registration and the invite consumes spend one), and
//! the session-gated writes carry no proof of work: this
//! limiter caps **server CPU spent per source IP** on
//! the OPRF compute (Ristretto255 scalar multiplication)
//! regardless of how many tokens an attacker holds, and
//! caps each account's write rate.
//!
//! ## Bucket math
//!
//! Standard token bucket. Each IP has a `TokenBucket`
//! with a real-valued `tokens` count. On every request
//! the bucket refills `rate_per_sec * elapsed` tokens
//! (capped at `burst`), then tries to consume 1. If
//! `tokens >= 1`, decrement and admit; else reject.
//!
//! The fractional-token model lets the limiter
//! configure smooth rates like "60 requests / minute"
//! (1.0/sec) without integer-discretization artefacts
//! at small `elapsed` values.
//!
//! ## IP extraction
//!
//! Production runs behind system-nginx, which sets
//! `X-Real-IP` to the peer address and appends it to
//! `X-Forwarded-For`. The middleware trusts only what
//! nginx wrote: `X-Real-IP`, else the rightmost
//! `X-Forwarded-For` entry, else
//! `ConnectInfo<SocketAddr>` for direct dev `cargo run`
//! use. The leftmost `X-Forwarded-For` entry is the
//! client's own claim and is never used.
//!
//! ## Address grouping
//!
//! A bucket is keyed by the **network** a request came
//! from, not by its exact address ([`network_key`]): an
//! IPv4 address is its own key, an IPv6 address is keyed
//! by its /64 prefix, and an IPv4-mapped IPv6 address is
//! keyed as the IPv4 address it carries. A single IPv6
//! subscriber is routinely handed a whole /64 (2^64
//! addresses), so keying on the full address would give
//! one attacker an unbounded supply of fresh buckets and
//! make the per-IP ceiling meaningless. The cost is that
//! hosts sharing one /64 share a bucket, which is the
//! same trade an IPv4 NAT already makes.
//!
//! ## Cleanup and the size cap
//!
//! Buckets untouched for `DEFAULT_PRUNE_AFTER` are
//! dropped periodically by a background task spun up at
//! boot. That alone does not bound memory: a flood of
//! requests from fresh sources adds one bucket per
//! request until the next sweep. Each limiter therefore
//! also holds at most [`DEFAULT_MAX_BUCKETS`] buckets.
//! When a new key arrives at the cap, the limiter first
//! drops every **idle** bucket (one that has refilled to
//! its full burst, so forgetting it loses nothing: a
//! fresh bucket is identical), and if that frees nothing
//! it drops the least-recently-touched eighth of the map
//! ([`EVICTION_FRACTION`]), so the O(n) sweep runs at
//! most once per `cap / 8` new keys.
//!
//! Evicting a bucket that is *not* full forgives that
//! source its debt. An attacker can reach that only by
//! pushing its own bucket out of the least-recently-used
//! eighth, which takes a stream of fresh keys, i.e. fresh
//! /64s or IPv4 addresses (or accounts), each of which
//! the same limiter throttles on its own.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use lakko_auth_token::{TokenSecret, ACCOUNT_ID_LEN};
use serde_json::json;
use std::sync::Arc;

/// Default rate: 60 requests per minute per source IP
/// — `1` req/sec sustained. Matches the cadence a
/// human-driven discovery flow needs (search-as-you-
/// type with a half-second debounce + occasional
/// retries) while crushing automated enumeration.
pub const DEFAULT_RATE_PER_MINUTE: f64 = 60.0;

/// Default burst: 10 tokens. A subscriber app pulling
/// every contact's identity bundle on first launch
/// might fire a short burst; the bucket admits up to
/// 10 in a row before the per-minute cap kicks in.
pub const DEFAULT_BURST: f64 = 10.0;

/// Default per-account rate for requests with a valid session token. The same
/// human cadence as the anonymous per-IP default, but charged to the person
/// rather than to everyone sharing their address. Chosen, not measured.
pub const DEFAULT_ACCOUNT_RATE_PER_MINUTE: f64 = 60.0;

/// Default per-account burst. Chosen, not measured.
pub const DEFAULT_ACCOUNT_BURST: f64 = 10.0;

/// Default per-IP ceiling for authenticated requests: ten accounts' worth, so
/// a shared address serves a household or an office while one address
/// cycling through many accounts is still capped. Chosen, not measured.
pub const DEFAULT_AUTHENTICATED_IP_RATE_PER_MINUTE: f64 = 600.0;

/// Default per-IP ceiling burst for authenticated requests. Chosen, not measured.
pub const DEFAULT_AUTHENTICATED_IP_BURST: f64 = 100.0;

/// Idle threshold for the background pruner. Buckets
/// with full tokens AND last-touched older than this
/// are dropped on the next sweep.
pub const DEFAULT_PRUNE_AFTER: Duration = Duration::from_secs(300);

/// Background sweep cadence. Memory between sweeps is
/// bounded by [`DEFAULT_MAX_BUCKETS`], not by this
/// interval.
pub const DEFAULT_PRUNE_INTERVAL: Duration = Duration::from_secs(300);

/// Upper bound on the buckets one limiter holds (the
/// discovery service runs four limiters). A bucket is a
/// key (an address or a 16-byte account id) plus a
/// fractional token count and an `Instant`. Chosen, not
/// derived from a traffic model or a memory measurement:
/// large enough that honest traffic between two sweeps
/// should not reach it, small enough that a flood of
/// fresh sources cannot grow the process without bound.
pub const DEFAULT_MAX_BUCKETS: usize = 100_000;

/// Divisor for the least-recently-used eviction at the
/// cap: when no idle bucket can be dropped, the oldest
/// `len / EVICTION_FRACTION` (at least one) go.
pub const EVICTION_FRACTION: usize = 8;

/// IPv6 prefix length a bucket is keyed on.
pub const IPV6_BUCKET_PREFIX_BITS: u32 = 64;

/// The network a request is charged to: an IPv4 address
/// as itself, an IPv4-mapped IPv6 address as the IPv4
/// address it carries, any other IPv6 address as its
/// /64 prefix (the low 64 bits zeroed).
#[must_use]
pub fn network_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or_else(
            || {
                let mask = u128::MAX << (128 - IPV6_BUCKET_PREFIX_BITS);
                IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask))
            },
            IpAddr::V4,
        ),
    }
}

/// Per-IP token-bucket state.
#[derive(Debug, Clone)]
struct TokenBucket {
    /// Current token count, fractional.
    tokens: f64,
    /// Wall-clock of the last refill (also serves as
    /// "last touched" for the pruner).
    last_refill: Instant,
}

impl TokenBucket {
    fn new(burst: f64, now: Instant) -> Self {
        Self {
            tokens: burst,
            last_refill: now,
        }
    }

    /// Whether the bucket would be full at `now`, i.e.
    /// forgetting it is indistinguishable from keeping it.
    fn is_idle(&self, rate_per_sec: f64, burst: f64, now: Instant) -> bool {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.tokens + rate_per_sec * elapsed >= burst
    }

    /// Refill and try to consume one token. Returns
    /// `true` if admitted, `false` if the bucket is dry.
    fn try_consume(&mut self, rate_per_sec: f64, burst: f64, now: Instant) -> bool {
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        // Refill — capped at burst so a long idle does
        // not produce an unbounded burst credit.
        self.tokens = (self.tokens + rate_per_sec * elapsed).min(burst);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// A token-bucket limiter keyed by `K` (a source IP or an account id).
/// Cheap to clone (the inner state is behind an `Arc<Mutex<...>>`).
#[derive(Debug, Clone)]
pub struct RateLimiter<K = IpAddr> {
    inner: Arc<Mutex<RateLimiterInner<K>>>,
}

#[derive(Debug)]
struct RateLimiterInner<K> {
    /// `rate_per_minute / 60.0` precomputed.
    rate_per_sec: f64,
    /// Maximum tokens per bucket.
    burst: f64,
    /// Most buckets the map may hold.
    max_buckets: usize,
    /// One bucket per key.
    buckets: HashMap<K, TokenBucket>,
}

impl<K: Eq + Hash + Copy> RateLimiterInner<K> {
    /// Make room for one new key: drop every idle bucket
    /// and, if none was idle, the least-recently-touched
    /// `len / EVICTION_FRACTION` (at least one).
    fn make_room(&mut self, now: Instant) {
        let (rate, burst) = (self.rate_per_sec, self.burst);
        self.buckets.retain(|_, b| !b.is_idle(rate, burst, now));
        if self.buckets.len() < self.max_buckets {
            return;
        }
        let evict = (self.buckets.len() / EVICTION_FRACTION).max(1);
        let mut touched: Vec<Instant> = self.buckets.values().map(|b| b.last_refill).collect();
        // `1 <= evict <= len`, so the index is in bounds.
        let (_, cutoff, _) = touched.select_nth_unstable(evict - 1);
        let cutoff = *cutoff;
        let mut removed = 0usize;
        self.buckets.retain(|_, b| {
            if removed < evict && b.last_refill <= cutoff {
                removed += 1;
                false
            } else {
                true
            }
        });
    }
}

impl<K: Eq + Hash + Copy> RateLimiter<K> {
    /// Build with the given rate-per-minute + burst.
    ///
    /// # Panics
    ///
    /// Panics if `rate_per_minute <= 0` or `burst <= 0`
    /// — the limiter would be a permanent reject in
    /// that mode, which is a misconfiguration, not a
    /// runtime state we want to silently propagate.
    #[must_use]
    pub fn new(rate_per_minute: f64, burst: f64) -> Self {
        Self::with_max_buckets(rate_per_minute, burst, DEFAULT_MAX_BUCKETS)
    }

    /// As [`Self::new`] with an explicit bucket cap.
    ///
    /// # Panics
    ///
    /// As [`Self::new`], and if `max_buckets == 0`.
    #[must_use]
    pub fn with_max_buckets(rate_per_minute: f64, burst: f64, max_buckets: usize) -> Self {
        assert!(rate_per_minute > 0.0, "rate_per_minute must be positive");
        assert!(burst > 0.0, "burst must be positive");
        assert!(max_buckets > 0, "max_buckets must be positive");
        Self {
            inner: Arc::new(Mutex::new(RateLimiterInner {
                rate_per_sec: rate_per_minute / 60.0,
                burst,
                max_buckets,
                buckets: HashMap::new(),
            })),
        }
    }

    /// Test a single request for `key`. Returns `true`
    /// if admitted, `false` if rate-limited.
    pub fn try_consume(&self, key: K) -> bool {
        self.try_consume_at(key, Instant::now())
    }

    /// As [`Self::try_consume`] but with an explicit
    /// `now` — used by tests that need deterministic
    /// time stepping without sleeping.
    pub fn try_consume_at(&self, key: K, now: Instant) -> bool {
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let burst = inner.burst;
        let rate = inner.rate_per_sec;
        if inner.buckets.len() >= inner.max_buckets && !inner.buckets.contains_key(&key) {
            inner.make_room(now);
        }
        let bucket = inner
            .buckets
            .entry(key)
            .or_insert_with(|| TokenBucket::new(burst, now));
        bucket.try_consume(rate, burst, now)
    }

    /// Drop buckets whose `last_refill` is older than
    /// `cutoff`. Semantics are intentionally simple: if
    /// an IP has not touched the limiter in
    /// `now - cutoff` seconds, its bucket is forgotten.
    /// A subsequent request from the same IP gets a
    /// fresh full-burst bucket — which is fine, because
    /// the inactivity window is the same wall-clock
    /// duration the limiter would have needed to refill
    /// it back to full anyway (`burst / rate_per_sec`).
    /// Returns the number of entries removed.
    pub fn prune_idle(&self, cutoff: Instant) -> usize {
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let before = inner.buckets.len();
        inner.buckets.retain(|_, b| b.last_refill >= cutoff);
        before - inner.buckets.len()
    }

    /// Current bucket count — observability hook.
    #[must_use]
    pub fn bucket_count(&self) -> usize {
        match self.inner.lock() {
            Ok(g) => g.buckets.len(),
            Err(p) => p.into_inner().buckets.len(),
        }
    }
}

/// One rate and burst pair, per minute.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limit {
    /// Sustained requests per minute.
    pub per_minute: f64,
    /// Bucket size.
    pub burst: f64,
}

/// The three buckets behind the rate-limited discovery endpoints.
///
/// * A request carrying a valid session token is charged to its **account**
///   and to a per-IP **ceiling** for authenticated traffic. People behind one
///   router (a home, an office, a carrier NAT) therefore do not share a
///   bucket, while one address holding many accounts still hits the ceiling.
/// * A request without a valid token (signup, a fresh install) is charged to
///   the per-IP **anonymous** bucket, as before.
///
/// The account is read from the token only to key the bucket; nothing about
/// the looked-up name is recorded against it (invariant #5).
#[derive(Debug)]
pub struct DiscoveryLimits {
    anonymous_ip: RateLimiter<IpAddr>,
    account: RateLimiter<[u8; ACCOUNT_ID_LEN]>,
    /// Session-gated writes (register, identity / KeyPackage / invite
    /// uploads, revocations): the same per-account rate as `account`, in a
    /// bucket of its own so lookups and writes never starve each other.
    account_writes: RateLimiter<[u8; ACCOUNT_ID_LEN]>,
    authenticated_ip: RateLimiter<IpAddr>,
    token_secret: Arc<TokenSecret>,
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Admitted.
    Admit,
    /// The account's bucket is empty.
    AccountExhausted,
    /// The source address's bucket (anonymous or ceiling) is empty.
    NetworkExhausted,
}

impl DiscoveryLimits {
    /// Build the three limiters.
    #[must_use]
    pub fn new(
        anonymous_ip: Limit,
        account: Limit,
        authenticated_ip: Limit,
        token_secret: Arc<TokenSecret>,
    ) -> Self {
        Self {
            anonymous_ip: RateLimiter::new(anonymous_ip.per_minute, anonymous_ip.burst),
            account: RateLimiter::new(account.per_minute, account.burst),
            account_writes: RateLimiter::new(account.per_minute, account.burst),
            authenticated_ip: RateLimiter::new(authenticated_ip.per_minute, authenticated_ip.burst),
            token_secret,
        }
    }

    /// Charge one request. `account` is the verified account id, if any.
    ///
    /// The authenticated path checks the address ceiling before the account,
    /// so a request refused by the ceiling does not also drain the account.
    pub fn admit_at(
        &self,
        ip: IpAddr,
        account: Option<[u8; ACCOUNT_ID_LEN]>,
        now: Instant,
    ) -> Verdict {
        let ip = network_key(ip);
        match account {
            None => {
                if self.anonymous_ip.try_consume_at(ip, now) {
                    Verdict::Admit
                } else {
                    Verdict::NetworkExhausted
                }
            },
            Some(account) => {
                if !self.authenticated_ip.try_consume_at(ip, now) {
                    Verdict::NetworkExhausted
                } else if self.account.try_consume_at(account, now) {
                    Verdict::Admit
                } else {
                    Verdict::AccountExhausted
                }
            },
        }
    }

    /// Charge one session-gated write to `account`.
    pub fn admit_write_at(&self, account: [u8; ACCOUNT_ID_LEN], now: Instant) -> Verdict {
        if self.account_writes.try_consume_at(account, now) {
            Verdict::Admit
        } else {
            Verdict::AccountExhausted
        }
    }

    /// The account a request's `Authorization: Bearer` token verifies to.
    ///
    /// A missing, malformed or expired token is not an error on these
    /// endpoints — they are anonymous by design — so it yields `None` and the
    /// request is charged to its address.
    fn bearer_account(&self, req: &Request<Body>, now_ms: i64) -> Option<[u8; ACCOUNT_ID_LEN]> {
        let token = req
            .headers()
            .get(header::AUTHORIZATION)?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")?;
        // The generation the token carries is not consulted here: the
        // limiter keys on the account, and only the two identity writes apply
        // a generation floor (ADR-0073 S3).
        Some(self.token_secret.verify(token, now_ms).ok()?.account_id)
    }

    /// Drop idle buckets from all three limiters; returns how many.
    pub fn prune_idle(&self, cutoff: Instant) -> usize {
        self.anonymous_ip.prune_idle(cutoff)
            + self.account.prune_idle(cutoff)
            + self.account_writes.prune_idle(cutoff)
            + self.authenticated_ip.prune_idle(cutoff)
    }

    /// Total bucket count — observability hook.
    #[must_use]
    pub fn bucket_count(&self) -> usize {
        self.anonymous_ip.bucket_count()
            + self.account.bucket_count()
            + self.account_writes.bucket_count()
            + self.authenticated_ip.bucket_count()
    }
}

/// Spawn the periodic prune-idle background task.
/// Runs until the tokio runtime is dropped. The
/// caller holds an `Arc<DiscoveryLimits>` clone in
/// `AppState`; the spawned task holds its own clone.
pub fn spawn_pruner(limits: Arc<DiscoveryLimits>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interval = DEFAULT_PRUNE_INTERVAL;
        let mut ticker = tokio::time::interval(interval);
        // Skip the immediate tick — let the service
        // warm up before we sweep.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let cutoff = Instant::now()
                .checked_sub(DEFAULT_PRUNE_AFTER)
                .unwrap_or_else(Instant::now);
            let dropped = limits.prune_idle(cutoff);
            if dropped > 0 {
                tracing::debug!(
                    dropped,
                    remaining = limits.bucket_count(),
                    "rate_limit: pruned idle buckets"
                );
            }
        }
    })
}

// =========================================================
// axum middleware
// =========================================================

/// Extract the source IP from an axum request.
///
/// Only values the fronting nginx wrote can be trusted; anything the client
/// sent is attacker-controlled and would let it pick its own bucket.
///
/// 1. `X-Real-IP` — nginx sets it to `$remote_addr`, replacing whatever the
///    client sent.
/// 2. The **rightmost** `X-Forwarded-For` entry — `$proxy_add_x_forwarded_for`
///    appends `$remote_addr` to the client-supplied list, so only the last
///    entry is nginx's. The leftmost entry is whatever the client claimed.
/// 3. The `ConnectInfo<SocketAddr>` extension (direct `cargo run` use).
///
/// This relies on the service being reachable only through nginx: it binds
/// `127.0.0.1` in production, so no client can set these headers directly.
pub(crate) fn source_ip(req: &Request<Body>) -> Option<IpAddr> {
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
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip())
}

/// Middleware function — identifies the source address and, when the request
/// carries a valid session token, the account; charges the matching buckets
/// ([`DiscoveryLimits::admit_at`]); returns 429 on reject.
///
/// Wire with `Router::route_layer(middleware::from_fn_with_state(...))`.
pub async fn middleware(
    State(limits): State<Arc<DiscoveryLimits>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let Some(ip) = source_ip(&req) else {
        // No identifiable source — fail closed.
        // Returning 429 (rather than 400) keeps the
        // stable error envelope clients already branch
        // on, and short-circuits before any OPRF compute.
        return rate_limited_response("rate-limited", "unable to identify source");
    };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let account = limits.bearer_account(&req, now_ms);
    match limits.admit_at(ip, account, Instant::now()) {
        Verdict::Admit => next.run(req).await,
        Verdict::AccountExhausted => rate_limited_response(
            "rate-limited",
            "too many discovery requests from your account — back off and retry",
        ),
        Verdict::NetworkExhausted => rate_limited_response(
            "rate-limited",
            "too many discovery requests from your network — back off and retry",
        ),
    }
}

/// Middleware for the session-gated writes: charge the request to the account
/// `auth::require_session` verified (read from the request extensions, so this
/// layer must sit INSIDE the session layer) and answer 429 when its bucket is
/// empty. A request that reaches here without the extension is a wiring bug
/// and fails closed with 429 as well.
///
/// Wire as the inner `route_layer`, with `require_session` added after it.
pub async fn write_middleware(
    State(limits): State<Arc<DiscoveryLimits>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let Some(account) = req
        .extensions()
        .get::<crate::auth::AuthenticatedAccount>()
        .map(|a| a.0)
    else {
        tracing::error!("write limiter reached without an authenticated account");
        return rate_limited_response("rate-limited", "unable to identify account");
    };
    match limits.admit_write_at(account, Instant::now()) {
        Verdict::Admit => next.run(req).await,
        Verdict::AccountExhausted | Verdict::NetworkExhausted => rate_limited_response(
            "rate-limited",
            "too many discovery writes from your account — back off and retry",
        ),
    }
}

fn rate_limited_response(code: &'static str, detail: &str) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({"code": code, "detail": detail})),
    )
        .into_response()
}

// =========================================================
// Tests
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(std::net::Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn pinned_defaults() {
        assert_eq!(DEFAULT_RATE_PER_MINUTE, 60.0);
        assert_eq!(DEFAULT_BURST, 10.0);
        assert_eq!(DEFAULT_PRUNE_AFTER.as_secs(), 300);
    }

    #[test]
    fn fresh_bucket_admits_up_to_burst() {
        let rl = RateLimiter::new(60.0, 10.0);
        let src = ip(192, 0, 2, 1);
        let now = Instant::now();
        for i in 0..10 {
            assert!(
                rl.try_consume_at(src, now),
                "first {} requests must be admitted",
                i + 1
            );
        }
        // 11th immediate request → rejected.
        assert!(!rl.try_consume_at(src, now), "burst+1 must be rejected");
    }

    #[test]
    fn bucket_refills_at_configured_rate() {
        // 60/min == 1/sec. Burst 5, drain to zero, then
        // wait 3 seconds → 3 tokens added → 3 admits +
        // 4th rejected.
        let rl = RateLimiter::new(60.0, 5.0);
        let src = ip(192, 0, 2, 2);
        let t0 = Instant::now();
        for _ in 0..5 {
            assert!(rl.try_consume_at(src, t0));
        }
        assert!(!rl.try_consume_at(src, t0));
        let t1 = t0 + Duration::from_secs(3);
        assert!(rl.try_consume_at(src, t1));
        assert!(rl.try_consume_at(src, t1));
        assert!(rl.try_consume_at(src, t1));
        assert!(!rl.try_consume_at(src, t1));
    }

    #[test]
    fn refill_capped_at_burst() {
        // 60/min, burst 5. Wait an hour from t0 — naive
        // accumulator would give 3600 tokens, but the
        // cap is 5.
        let rl = RateLimiter::new(60.0, 5.0);
        let src = ip(192, 0, 2, 3);
        let t0 = Instant::now();
        // First request creates the bucket at burst=5,
        // consumes 1 → 4 left.
        assert!(rl.try_consume_at(src, t0));
        let t1 = t0 + Duration::from_secs(3600);
        // Six consecutive admits must NOT be possible —
        // the cap holds at burst (5).
        let mut admitted = 0;
        for _ in 0..10 {
            if rl.try_consume_at(src, t1) {
                admitted += 1;
            }
        }
        assert_eq!(admitted, 5, "post-cap admits must equal burst");
    }

    #[test]
    fn per_ip_isolation() {
        let rl = RateLimiter::new(60.0, 3.0);
        let alice = ip(192, 0, 2, 10);
        let bob = ip(192, 0, 2, 11);
        let now = Instant::now();
        // Drain Alice.
        for _ in 0..3 {
            assert!(rl.try_consume_at(alice, now));
        }
        assert!(!rl.try_consume_at(alice, now));
        // Bob's bucket is untouched — first 3 admit.
        for _ in 0..3 {
            assert!(rl.try_consume_at(bob, now));
        }
        assert!(!rl.try_consume_at(bob, now));
    }

    #[test]
    fn prune_drops_stale_buckets() {
        // A bucket whose last_refill is older than the
        // cutoff must be dropped, regardless of token
        // count — the IP went silent and the limiter
        // can safely forget it.
        let rl = RateLimiter::new(60.0, 5.0);
        let src = ip(192, 0, 2, 20);
        let t0 = Instant::now();
        assert!(rl.try_consume_at(src, t0));
        assert_eq!(rl.bucket_count(), 1);
        // Cutoff strictly after t0 → bucket eligible.
        let cutoff = t0 + Duration::from_secs(1);
        let dropped = rl.prune_idle(cutoff);
        assert_eq!(dropped, 1);
        assert_eq!(rl.bucket_count(), 0);
    }

    #[test]
    fn prune_keeps_recent_buckets() {
        // A bucket touched after the cutoff stays —
        // the IP is still active.
        let rl = RateLimiter::new(60.0, 5.0);
        let src = ip(192, 0, 2, 21);
        let t0 = Instant::now();
        // Touch the bucket "later" so we can pick a
        // cutoff strictly before that touch. Avoids
        // calling Instant::checked_sub(_).unwrap() at
        // test time.
        let t1 = t0 + Duration::from_secs(10);
        assert!(rl.try_consume_at(src, t1));
        // Cutoff between t0 and t1 → bucket NOT
        // eligible (its last_refill is t1).
        let cutoff = t0 + Duration::from_secs(1);
        let dropped = rl.prune_idle(cutoff);
        assert_eq!(dropped, 0);
        assert_eq!(rl.bucket_count(), 1);
    }

    fn request_with(headers: &[(&str, &str)], peer: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri("/discovery/lookup");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let mut req = builder.body(Body::empty()).expect("request builds");
        if let Some(peer) = peer {
            let addr: std::net::SocketAddr = peer.parse().expect("socket addr parses");
            req.extensions_mut().insert(ConnectInfo(addr));
        }
        req
    }

    const SECRET_B64: &str = "ZGV2LXNlc3Npb24tdG9rZW4tc2VjcmV0LWZvci1sb2NhbC10ZXN0aW5nLW9ubHkhIQ==";

    fn limits(anonymous: f64, account: f64, ceiling: f64) -> DiscoveryLimits {
        let secret = TokenSecret::from_base64(SECRET_B64).expect("fixture secret decodes");
        DiscoveryLimits::new(
            Limit {
                per_minute: 60.0,
                burst: anonymous,
            },
            Limit {
                per_minute: 60.0,
                burst: account,
            },
            Limit {
                per_minute: 600.0,
                burst: ceiling,
            },
            Arc::new(secret),
        )
    }

    fn drain(
        limits: &DiscoveryLimits,
        src: IpAddr,
        account: Option<[u8; 16]>,
        now: Instant,
    ) -> usize {
        (0..1000)
            .take_while(|_| limits.admit_at(src, account, now) == Verdict::Admit)
            .count()
    }

    #[test]
    fn accounts_behind_one_address_do_not_share_a_bucket() {
        let l = limits(3.0, 5.0, 100.0);
        let router = ip(198, 51, 100, 1);
        let now = Instant::now();
        assert_eq!(drain(&l, router, Some([1; 16]), now), 5);
        // A second person behind the same router still has a full bucket.
        assert_eq!(drain(&l, router, Some([2; 16]), now), 5);
        // So does anonymous traffic from that router: it has its own bucket.
        assert_eq!(drain(&l, router, None, now), 3);
    }

    #[test]
    fn an_account_is_limited_across_addresses() {
        let l = limits(3.0, 5.0, 100.0);
        let now = Instant::now();
        for i in 0..5u8 {
            assert_eq!(
                l.admit_at(ip(203, 0, 113, i), Some([9; 16]), now),
                Verdict::Admit
            );
        }
        assert_eq!(
            l.admit_at(ip(203, 0, 113, 200), Some([9; 16]), now),
            Verdict::AccountExhausted
        );
    }

    #[test]
    fn one_address_cycling_accounts_hits_the_ceiling() {
        let l = limits(3.0, 5.0, 12.0);
        let src = ip(192, 0, 2, 7);
        let now = Instant::now();
        let admitted = (0..40u8)
            .filter(|n| l.admit_at(src, Some([*n; 16]), now) == Verdict::Admit)
            .count();
        assert_eq!(admitted, 12);
        assert_eq!(
            l.admit_at(src, Some([99; 16]), now),
            Verdict::NetworkExhausted
        );
    }

    #[test]
    fn writes_are_charged_per_account_in_their_own_bucket() {
        let l = limits(3.0, 5.0, 100.0);
        let now = Instant::now();
        for _ in 0..5 {
            assert_eq!(l.admit_write_at([3; 16], now), Verdict::Admit);
        }
        assert_eq!(l.admit_write_at([3; 16], now), Verdict::AccountExhausted);
        // Another account is untouched, and so are the account's lookups.
        assert_eq!(l.admit_write_at([4; 16], now), Verdict::Admit);
        assert_eq!(drain(&l, ip(192, 0, 2, 9), Some([3; 16]), now), 5);
    }

    #[test]
    fn a_ceiling_refusal_does_not_drain_the_account() {
        let l = limits(3.0, 5.0, 1.0);
        let now = Instant::now();
        assert_eq!(
            l.admit_at(ip(192, 0, 2, 1), Some([4; 16]), now),
            Verdict::Admit
        );
        assert_eq!(
            l.admit_at(ip(192, 0, 2, 1), Some([4; 16]), now),
            Verdict::NetworkExhausted
        );
        // The refused request cost the account nothing: four more addresses
        // (each with its own one-token ceiling) spend the remaining four tokens,
        // and only the fifth runs the account dry.
        for last in 2..=5 {
            assert_eq!(
                l.admit_at(ip(192, 0, 2, last), Some([4; 16]), now),
                Verdict::Admit
            );
        }
        assert_eq!(
            l.admit_at(ip(192, 0, 2, 6), Some([4; 16]), now),
            Verdict::AccountExhausted
        );
    }

    #[test]
    fn bearer_account_accepts_only_a_valid_token() {
        let l = limits(3.0, 5.0, 100.0);
        let now_ms: i64 = 1_800_000_000_000;
        let secret = TokenSecret::from_base64(SECRET_B64).expect("fixture secret decodes");
        let good = secret
            .issue(&[7; 16], 1, &[0u8; 32], 0, now_ms + 60_000)
            .expect("token issues");
        let expired = secret
            .issue(&[7; 16], 1, &[0u8; 32], 0, now_ms - 1)
            .expect("token issues");
        let other = TokenSecret::new(vec![0x42; 32])
            .expect("secret builds")
            .issue(&[7; 16], 1, &[0u8; 32], 0, now_ms + 60_000)
            .expect("token issues");
        let bearer = |t: &str| request_with(&[("authorization", &format!("Bearer {t}"))], None);

        assert_eq!(l.bearer_account(&bearer(&good), now_ms), Some([7; 16]));
        assert_eq!(l.bearer_account(&bearer(&expired), now_ms), None);
        assert_eq!(l.bearer_account(&bearer(&other), now_ms), None);
        assert_eq!(l.bearer_account(&bearer("garbage"), now_ms), None);
        assert_eq!(
            l.bearer_account(&request_with(&[("authorization", &good)], None), now_ms),
            None
        );
        assert_eq!(l.bearer_account(&request_with(&[], None), now_ms), None);
    }

    #[test]
    fn source_ip_ignores_client_supplied_leftmost_forwarded_for() {
        // nginx's `$proxy_add_x_forwarded_for` appends the real peer to
        // whatever the client sent. A client that sends a fresh fake value on
        // every request must still land in its own bucket.
        let req = request_with(&[("x-forwarded-for", "203.0.113.77, 198.51.100.9")], None);
        assert_eq!(source_ip(&req), Some(ip(198, 51, 100, 9)));
    }

    #[test]
    fn source_ip_prefers_real_ip_over_forwarded_for() {
        let req = request_with(
            &[
                ("x-forwarded-for", "203.0.113.77, 192.0.2.50"),
                ("x-real-ip", "198.51.100.9"),
            ],
            Some("127.0.0.1:40000"),
        );
        assert_eq!(source_ip(&req), Some(ip(198, 51, 100, 9)));
    }

    #[test]
    fn source_ip_skips_unparseable_real_ip() {
        let req = request_with(
            &[
                ("x-real-ip", "not-an-ip"),
                ("x-forwarded-for", "198.51.100.9"),
            ],
            None,
        );
        assert_eq!(source_ip(&req), Some(ip(198, 51, 100, 9)));
    }

    #[test]
    fn source_ip_rejects_unparseable_rightmost_forwarded_for() {
        // A garbage last entry must not fall back to the client's leftmost claim.
        let req = request_with(
            &[("x-forwarded-for", "203.0.113.77, garbage")],
            Some("127.0.0.1:40000"),
        );
        assert_eq!(source_ip(&req), Some(ip(127, 0, 0, 1)));
    }

    #[test]
    fn source_ip_falls_back_to_connect_info() {
        let req = request_with(&[], Some("[2001:db8::1]:5000"));
        assert_eq!(source_ip(&req), "2001:db8::1".parse().ok());
    }

    fn v6(s: &str) -> IpAddr {
        s.parse().expect("ipv6 parses")
    }

    #[test]
    fn network_key_groups_ipv6_by_slash_64() {
        assert_eq!(
            network_key(v6("2001:db8:1:2:aaaa:bbbb:cccc:dddd")),
            v6("2001:db8:1:2::")
        );
        assert_eq!(
            network_key(v6("2001:db8:1:2::1")),
            network_key(v6("2001:db8:1:2:ffff::"))
        );
        assert_ne!(
            network_key(v6("2001:db8:1:2::1")),
            network_key(v6("2001:db8:1:3::1"))
        );
        // IPv4 is keyed per address, and a mapped IPv4 as the address it carries.
        assert_eq!(network_key(ip(192, 0, 2, 1)), ip(192, 0, 2, 1));
        assert_eq!(network_key(v6("::ffff:192.0.2.1")), ip(192, 0, 2, 1));
        assert_ne!(network_key(ip(192, 0, 2, 1)), network_key(ip(192, 0, 2, 2)));
    }

    #[test]
    fn one_ipv6_slash_64_shares_the_anonymous_bucket() {
        let l = limits(3.0, 5.0, 100.0);
        let now = Instant::now();
        // Rotating the interface identifier inside one /64 buys nothing.
        let admitted = (0..40u16)
            .filter(|n| {
                let addr = v6(&format!("2001:db8:7:7::{n:x}"));
                l.admit_at(addr, None, now) == Verdict::Admit
            })
            .count();
        assert_eq!(admitted, 3);
        // A neighbouring /64 is a different network.
        assert_eq!(l.admit_at(v6("2001:db8:7:8::1"), None, now), Verdict::Admit);
    }

    #[test]
    fn one_ipv6_slash_64_cycling_accounts_hits_the_ceiling() {
        let l = limits(3.0, 5.0, 12.0);
        let now = Instant::now();
        let admitted = (0..40u8)
            .filter(|n| {
                let addr = v6(&format!("2001:db8:9:9::{n:x}"));
                l.admit_at(addr, Some([*n; 16]), now) == Verdict::Admit
            })
            .count();
        assert_eq!(admitted, 12);
    }

    #[test]
    fn the_bucket_map_never_exceeds_its_cap() {
        let rl = RateLimiter::with_max_buckets(60.0, 5.0, 64);
        let now = Instant::now();
        for n in 0..10_000u32 {
            let [a, b, c, d] = n.to_be_bytes();
            let _ = rl.try_consume_at(ip(10 + a, b, c, d), now);
            assert!(rl.bucket_count() <= 64);
        }
    }

    #[test]
    fn an_idle_bucket_is_evicted_before_an_older_active_one() {
        // 60/min, burst 100: a drained bucket needs 100 s to refill, a bucket
        // that spent one token needs 1 s.
        let rl = RateLimiter::with_max_buckets(60.0, 100.0, 2);
        let t0 = Instant::now();
        let drained = ip(192, 0, 2, 30);
        let idle = ip(192, 0, 2, 31);
        for _ in 0..100 {
            assert!(rl.try_consume_at(drained, t0));
        }
        assert!(!rl.try_consume_at(drained, t0));
        assert!(rl.try_consume_at(idle, t0 + Duration::from_secs(50)));
        // At t0+60 `idle` is full again and `drained` holds 60 of 100 tokens.
        let t1 = t0 + Duration::from_secs(60);
        assert!(rl.try_consume_at(ip(192, 0, 2, 32), t1));
        assert_eq!(rl.bucket_count(), 2);
        // `drained` kept its debt: 60 admits, then refused.
        let admitted = (0..100).filter(|_| rl.try_consume_at(drained, t1)).count();
        assert_eq!(admitted, 60);
    }

    #[test]
    fn with_no_idle_bucket_the_least_recently_touched_goes() {
        // Nothing refills within the test (1/min, burst 5).
        let rl = RateLimiter::with_max_buckets(1.0, 5.0, 8);
        let t0 = Instant::now();
        for n in 0..8u8 {
            let at = t0 + Duration::from_millis(u64::from(n));
            for _ in 0..5 {
                assert!(rl.try_consume_at(ip(192, 0, 2, n), at));
            }
        }
        // A ninth key evicts exactly one bucket, the oldest.
        assert!(rl.try_consume_at(ip(192, 0, 2, 100), t0 + Duration::from_millis(20)));
        assert_eq!(rl.bucket_count(), 8);
        let later = t0 + Duration::from_millis(21);
        // The oldest came back as a fresh bucket; the newest kept its debt.
        assert!(rl.try_consume_at(ip(192, 0, 2, 0), later));
        assert!(!rl.try_consume_at(ip(192, 0, 2, 7), later));
    }

    #[test]
    fn fresh_bucket_after_prune_starts_full() {
        // After a bucket is pruned, a subsequent
        // consume from the same IP gets a fresh
        // full-burst bucket. This is fine: the wall-
        // clock the IP was silent for the prune cutoff
        // would have been long enough to refill the
        // bucket to full anyway (burst / rate_per_sec).
        let rl = RateLimiter::new(60.0, 3.0);
        let src = ip(192, 0, 2, 22);
        let t0 = Instant::now();
        // Drain to zero.
        for _ in 0..3 {
            assert!(rl.try_consume_at(src, t0));
        }
        assert!(!rl.try_consume_at(src, t0));
        // Prune.
        rl.prune_idle(t0 + Duration::from_secs(1));
        // Fresh bucket → burst tokens again.
        let t1 = t0 + Duration::from_secs(2);
        for _ in 0..3 {
            assert!(rl.try_consume_at(src, t1));
        }
        assert!(!rl.try_consume_at(src, t1));
    }
}
