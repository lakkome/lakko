// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

#![allow(
    unreachable_pub,
    dead_code,
    clippy::float_cmp,
)]

//! Per-IP token-bucket rate limiter for discovery's
//! anonymous endpoints — OPRF evaluation, username
//! lookup, channel-name lookup.
//!
//! ## Threat model
//!
//! Defense in depth on top of the existing PoW gate.
//! PoW makes brute-force expensive per request, but a
//! botnet can distribute the PoW work across thousands
//! of hosts. The rate limiter caps **server CPU spent
//! per source IP** on the OPRF compute (Ristretto255
//! scalar multiplication) regardless of how many PoW
//! tokens the attacker mints.
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
//! Production runs behind system-nginx which sets
//! `X-Forwarded-For` with the original client address.
//! The middleware honours that header first; falls
//! back to `ConnectInfo<SocketAddr>` for direct dev
//! `cargo run` use.
//!
//! ## Cleanup
//!
//! Idle buckets (full token count + last touch >
//! `prune_after`) are dropped periodically by a
//! background task spun up at boot, so a global memory
//! ceiling stays bounded under a Slowloris-style
//! enumerator that touches one IP every refresh
//! interval.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
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

/// Idle threshold for the background pruner. Buckets
/// with full tokens AND last-touched older than this
/// are dropped on the next sweep.
pub const DEFAULT_PRUNE_AFTER: Duration = Duration::from_secs(300);

/// Background sweep cadence. Hourly is fine — the
/// bucket map is keyed by `IpAddr` (16 bytes) +
/// 32 bytes of bucket state ≈ 48 bytes per entry;
/// even a million long-lived IPs only burn ~48 MB
/// between sweeps.
pub const DEFAULT_PRUNE_INTERVAL: Duration = Duration::from_secs(300);

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

/// Shared rate limiter — one instance per service.
/// Cheap to clone (the inner state is behind an
/// `Arc<Mutex<...>>`).
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<RateLimiterInner>>,
}

#[derive(Debug)]
struct RateLimiterInner {
    /// `rate_per_minute / 60.0` precomputed.
    rate_per_sec: f64,
    /// Maximum tokens per bucket.
    burst: f64,
    /// Per-IP buckets.
    buckets: HashMap<IpAddr, TokenBucket>,
}

impl RateLimiter {
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
        assert!(rate_per_minute > 0.0, "rate_per_minute must be positive");
        assert!(burst > 0.0, "burst must be positive");
        Self {
            inner: Arc::new(Mutex::new(RateLimiterInner {
                rate_per_sec: rate_per_minute / 60.0,
                burst,
                buckets: HashMap::new(),
            })),
        }
    }

    /// Test a single request from `ip`. Returns `true`
    /// if admitted, `false` if rate-limited.
    pub fn try_consume(&self, ip: IpAddr) -> bool {
        self.try_consume_at(ip, Instant::now())
    }

    /// As [`Self::try_consume`] but with an explicit
    /// `now` — used by tests that need deterministic
    /// time stepping without sleeping.
    pub fn try_consume_at(&self, ip: IpAddr, now: Instant) -> bool {
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let burst = inner.burst;
        let rate = inner.rate_per_sec;
        let bucket = inner
            .buckets
            .entry(ip)
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

/// Spawn the periodic prune-idle background task.
/// Runs until the tokio runtime is dropped. The
/// caller holds an `Arc<RateLimiter>` clone in
/// `AppState`; the spawned task holds its own clone.
pub fn spawn_pruner(limiter: RateLimiter) -> tokio::task::JoinHandle<()> {
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
            let _ = cutoff; // (`-D clippy::unchecked_duration_subtraction` is satisfied by the checked_sub above)
            let dropped = limiter.prune_idle(cutoff);
            if dropped > 0 {
                tracing::debug!(
                    dropped,
                    remaining = limiter.bucket_count(),
                    "rate_limit: pruned idle buckets"
                );
            }
        }
    })
}

// =========================================================
// axum middleware
// =========================================================

/// Extract the source IP from an axum request. Looks
/// at `X-Forwarded-For` first (production nginx sets
/// it), then `X-Real-IP`, finally the
/// `ConnectInfo<SocketAddr>` extension.
fn source_ip(req: &Request<Body>) -> Option<IpAddr> {
    if let Some(xff) = req.headers().get("x-forwarded-for") {
        if let Ok(s) = xff.to_str() {
            // XFF is comma-separated; the original
            // client is the leftmost entry. nginx's
            // `proxy_set_header X-Forwarded-For
            // $proxy_add_x_forwarded_for` produces
            // that order.
            if let Some(first) = s.split(',').next() {
                if let Ok(ip) = first.trim().parse::<IpAddr>() {
                    return Some(ip);
                }
            }
        }
    }
    if let Some(real) = req.headers().get("x-real-ip") {
        if let Ok(s) = real.to_str() {
            if let Ok(ip) = s.trim().parse::<IpAddr>() {
                return Some(ip);
            }
        }
    }
    req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip())
}

/// Middleware function — extracts the source IP, runs
/// `RateLimiter::try_consume`, returns 429 on reject.
///
/// Wire with `Router::route_layer(middleware::from_fn_with_state(...))`.
pub async fn middleware(
    State(limiter): State<Arc<RateLimiter>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let Some(ip) = source_ip(&req) else {
        // No identifiable source — fail closed.
        // Returning 429 (rather than 400) keeps the
        // stable error envelope clients already branch
        // on, and short-circuits before any OPRF compute.
        return rate_limited_response(
            "rate-limited",
            "unable to identify source",
        );
    };
    if limiter.try_consume(ip) {
        next.run(req).await
    } else {
        rate_limited_response(
            "rate-limited",
            "too many discovery requests from your network — back off and retry",
        )
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
        assert!(
            !rl.try_consume_at(src, now),
            "burst+1 must be rejected"
        );
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
