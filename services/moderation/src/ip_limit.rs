// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A per-network limit on the report routes, in memory only.
//!
//! The token pays for a report; this limit is what keeps one network from
//! hammering the status route (which spends nothing) or flooding the intake
//! with malformed bodies. The key is the network a request came from, as
//! `services/discovery` keys it: an IPv4 address, or an IPv6 /64. The
//! address is taken from `X-Real-IP` (set by nginx), else the rightmost
//! `X-Forwarded-For` entry, else the socket. Nothing is stored or logged.
//!
//! Defaults (chosen, not measured): a burst of 30 requests, refilled at one
//! every 2 seconds. At most [`MAX_BUCKETS`] networks are tracked; at the cap
//! the full (idle) buckets are dropped first, then the oldest eighth.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::error;

/// Most networks tracked at once.
pub const MAX_BUCKETS: usize = 100_000;

/// The limiter.
#[derive(Debug)]
pub struct IpLimiter {
    burst: f64,
    per_sec: f64,
    buckets: Mutex<HashMap<u128, (f64, Instant)>>,
}

impl IpLimiter {
    /// A limiter with `burst` requests and a refill of `per_sec`.
    #[must_use]
    pub fn new(burst: u32, per_sec: f64) -> Self {
        Self {
            burst: f64::from(burst),
            per_sec,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// The defaults of the module doc.
    #[must_use]
    pub fn default_limits() -> Self {
        Self::new(30, 0.5)
    }

    /// Charge one request from `ip` at `now`; `false` when over the limit.
    pub fn admit_at(&self, ip: IpAddr, now: Instant) -> bool {
        let key = network_key(ip);
        let mut map = self
            .buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if map.len() >= MAX_BUCKETS && !map.contains_key(&key) {
            let burst = self.burst;
            let per_sec = self.per_sec;
            map.retain(|_, (tokens, at)| {
                *tokens + now.duration_since(*at).as_secs_f64() * per_sec < burst
            });
            if map.len() >= MAX_BUCKETS {
                let mut ages: Vec<(u128, Instant)> = map.iter().map(|(k, v)| (*k, v.1)).collect();
                ages.sort_by_key(|(_, at)| *at);
                for (k, _) in ages.iter().take(MAX_BUCKETS / 8) {
                    map.remove(k);
                }
            }
        }
        let entry = map.entry(key).or_insert((self.burst, now));
        let refilled =
            (entry.0 + now.duration_since(entry.1).as_secs_f64() * self.per_sec).min(self.burst);
        if refilled < 1.0 {
            entry.0 = refilled;
            entry.1 = now;
            return false;
        }
        entry.0 = refilled - 1.0;
        entry.1 = now;
        true
    }
}

/// IPv4 as itself, IPv6 by its /64, IPv4-mapped IPv6 as the IPv4 address.
fn network_key(ip: IpAddr) -> u128 {
    match ip {
        IpAddr::V4(v4) => u128::from(u32::from(v4)),
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or_else(
            || ((u128::from(v6) >> 64) << 64) | (1u128 << 63),
            |v4| u128::from(u32::from(v4)),
        ),
    }
}

/// The request's source address, as nginx wrote it.
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

/// Middleware: refuse with `429 {"error": "rate-limited"}` over the limit,
/// and when no source address can be told (fail closed).
pub async fn middleware(
    State(limiter): State<Arc<IpLimiter>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let admitted = source_ip(&req).is_some_and(|ip| limiter.admit_at(ip, Instant::now()));
    if admitted {
        next.run(req).await
    } else {
        error(StatusCode::TOO_MANY_REQUESTS, "rate-limited")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::time::Duration;

    #[test]
    fn a_burst_is_admitted_then_refused_then_refilled() {
        let l = IpLimiter::new(3, 1.0);
        let ip = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));
        let t = Instant::now();
        assert!(l.admit_at(ip, t) && l.admit_at(ip, t) && l.admit_at(ip, t));
        assert!(!l.admit_at(ip, t));
        assert!(l.admit_at(ip, t + Duration::from_secs(1)));
        let other = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 8));
        assert!(l.admit_at(other, t));
    }

    #[test]
    fn one_ipv6_64_shares_a_bucket_and_mapped_v4_is_v4() {
        let a: Ipv6Addr = "2001:db8:1:2::1".parse().unwrap_or(Ipv6Addr::UNSPECIFIED);
        let b: Ipv6Addr = "2001:db8:1:2:ffff::9"
            .parse()
            .unwrap_or(Ipv6Addr::UNSPECIFIED);
        let c: Ipv6Addr = "2001:db8:1:3::1".parse().unwrap_or(Ipv6Addr::UNSPECIFIED);
        assert_eq!(network_key(IpAddr::V6(a)), network_key(IpAddr::V6(b)));
        assert_ne!(network_key(IpAddr::V6(a)), network_key(IpAddr::V6(c)));
        let v4 = Ipv4Addr::new(192, 0, 2, 1);
        assert_eq!(
            network_key(IpAddr::V6(v4.to_ipv6_mapped())),
            network_key(IpAddr::V4(v4))
        );
        assert_ne!(
            network_key(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
            network_key(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        );
    }
}
