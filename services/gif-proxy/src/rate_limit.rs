// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Per-account in-memory token bucket.
//!
//! Keyed by the 16-byte account id taken from a verified session token. The
//! map lives only in process memory, is never logged, and is bounded: an entry
//! idle for longer than a full refill is indistinguishable from a fresh one and
//! is swept first; if the map is still full, the least recently used entry is
//! dropped. Dropping an active entry hands that account at most one extra burst,
//! which is the accepted price of a hard memory ceiling.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::config::RateLimitConfig;

/// Default ceiling on tracked accounts per limiter.
pub(crate) const DEFAULT_MAX_ENTRIES: usize = 65_536;

/// Account id as carried by a verified session token.
pub(crate) type AccountKey = [u8; 16];

/// Outcome of one admission check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Allowed,
    /// Refused; one token is available again after `retry_after_secs`.
    Limited {
        retry_after_secs: u64,
    },
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_seen: Instant,
}

/// A bounded map of token buckets.
pub(crate) struct AccountRateLimiter {
    buckets: Mutex<HashMap<AccountKey, Bucket>>,
    rate_per_sec: f64,
    burst: f64,
    /// Time for an empty bucket to refill completely.
    idle_after: Duration,
    max_entries: usize,
}

impl AccountRateLimiter {
    pub(crate) fn new(config: RateLimitConfig, max_entries: usize) -> Self {
        // `Config` guarantees both values are >= 1; the `max` keeps a
        // hand-built config from producing a division by zero.
        let rate_per_sec = f64::from(config.per_minute.max(1)) / 60.0;
        let burst = f64::from(config.burst.max(1));
        let idle_after =
            Duration::try_from_secs_f64(burst / rate_per_sec).unwrap_or(Duration::from_secs(60));
        Self {
            buckets: Mutex::new(HashMap::new()),
            rate_per_sec,
            burst,
            idle_after,
            max_entries: max_entries.max(1),
        }
    }

    /// Admit or refuse one request from `key` now.
    pub(crate) fn check(&self, key: AccountKey) -> Decision {
        self.check_at(key, Instant::now())
    }

    /// Admit or refuse one request from `key` at `now` (the testable core).
    pub(crate) fn check_at(&self, key: AccountKey, now: Instant) -> Decision {
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);

        if !buckets.contains_key(&key) && buckets.len() >= self.max_entries {
            self.evict(&mut buckets, now);
        }

        let bucket = buckets.entry(key).or_insert(Bucket {
            tokens: self.burst,
            last_seen: now,
        });
        let elapsed = now
            .saturating_duration_since(bucket.last_seen)
            .as_secs_f64();
        bucket.tokens = elapsed
            .mul_add(self.rate_per_sec, bucket.tokens)
            .min(self.burst);
        bucket.last_seen = now;
        let admitted = bucket.tokens >= 1.0;
        if admitted {
            bucket.tokens -= 1.0;
        }
        let remaining = bucket.tokens;
        drop(buckets);

        if admitted {
            return Decision::Allowed;
        }
        let wait = (1.0 - remaining) / self.rate_per_sec;
        let retry_after_secs = Duration::try_from_secs_f64(wait)
            .map_or(1, |d| d.as_secs() + u64::from(d.subsec_nanos() > 0));
        Decision::Limited {
            retry_after_secs: retry_after_secs.max(1),
        }
    }

    /// Make room for one new entry.
    fn evict(&self, buckets: &mut HashMap<AccountKey, Bucket>, now: Instant) {
        buckets.retain(|_, b| now.saturating_duration_since(b.last_seen) < self.idle_after);
        if buckets.len() >= self.max_entries {
            if let Some(oldest) = buckets
                .iter()
                .min_by_key(|(_, b)| b.last_seen)
                .map(|(k, _)| *k)
            {
                buckets.remove(&oldest);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn tracked(&self) -> usize {
        self.buckets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    #[cfg(test)]
    pub(crate) fn is_tracked(&self, key: &AccountKey) -> bool {
        self.buckets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn key(n: u8) -> AccountKey {
        [n; 16]
    }

    fn limiter(per_minute: u32, burst: u32, max_entries: usize) -> AccountRateLimiter {
        AccountRateLimiter::new(RateLimitConfig { per_minute, burst }, max_entries)
    }

    #[test]
    fn a_burst_is_admitted_then_refused() {
        let rl = limiter(60, 3, 16);
        let t0 = Instant::now();
        for _ in 0..3 {
            assert_eq!(rl.check_at(key(1), t0), Decision::Allowed);
        }
        assert!(matches!(rl.check_at(key(1), t0), Decision::Limited { .. }));
    }

    #[test]
    fn tokens_refill_over_time() {
        // 60/min = one token per second.
        let rl = limiter(60, 1, 16);
        let t0 = Instant::now();
        assert_eq!(rl.check_at(key(1), t0), Decision::Allowed);
        assert!(matches!(
            rl.check_at(key(1), t0 + Duration::from_millis(500)),
            Decision::Limited { .. }
        ));
        assert_eq!(
            rl.check_at(key(1), t0 + Duration::from_millis(1600)),
            Decision::Allowed
        );
    }

    #[test]
    fn retry_after_rounds_up_to_whole_seconds() {
        // 30/min = one token every 2 s.
        let rl = limiter(30, 1, 16);
        let t0 = Instant::now();
        assert_eq!(rl.check_at(key(1), t0), Decision::Allowed);
        assert_eq!(
            rl.check_at(key(1), t0),
            Decision::Limited {
                retry_after_secs: 2
            }
        );
        assert_eq!(
            rl.check_at(key(1), t0 + Duration::from_millis(1500)),
            Decision::Limited {
                retry_after_secs: 1
            }
        );
    }

    #[test]
    fn accounts_do_not_share_a_bucket() {
        let rl = limiter(1, 1, 16);
        let t0 = Instant::now();
        assert_eq!(rl.check_at(key(1), t0), Decision::Allowed);
        assert!(matches!(rl.check_at(key(1), t0), Decision::Limited { .. }));
        assert_eq!(rl.check_at(key(2), t0), Decision::Allowed);
    }

    #[test]
    fn the_map_never_exceeds_its_ceiling() {
        let rl = limiter(60, 5, 4);
        let t0 = Instant::now();
        for n in 0..50u8 {
            rl.check_at(key(n), t0 + Duration::from_millis(u64::from(n)));
            assert!(rl.tracked() <= 4, "tracked {} after key {n}", rl.tracked());
        }
    }

    #[test]
    fn idle_entries_are_swept_before_active_ones() {
        // burst 5 at 60/min refills fully in 5 s.
        let rl = limiter(60, 5, 3);
        let t0 = Instant::now();
        rl.check_at(key(1), t0); // will be idle
        rl.check_at(key(2), t0 + Duration::from_secs(9));
        rl.check_at(key(3), t0 + Duration::from_secs(9));
        // Full: key 1 idle for 10 s (> 5 s) goes, keys 2 and 3 stay.
        rl.check_at(key(4), t0 + Duration::from_secs(10));
        assert!(!rl.is_tracked(&key(1)));
        assert!(rl.is_tracked(&key(2)));
        assert!(rl.is_tracked(&key(3)));
        assert!(rl.is_tracked(&key(4)));
    }

    #[test]
    fn when_nothing_is_idle_the_least_recently_used_goes() {
        let rl = limiter(60, 5, 3);
        let t0 = Instant::now();
        rl.check_at(key(1), t0);
        rl.check_at(key(2), t0 + Duration::from_secs(1));
        rl.check_at(key(3), t0 + Duration::from_secs(2));
        rl.check_at(key(1), t0 + Duration::from_secs(3)); // touch key 1
        rl.check_at(key(4), t0 + Duration::from_secs(3));
        assert!(rl.is_tracked(&key(1)));
        assert!(!rl.is_tracked(&key(2)));
        assert!(rl.is_tracked(&key(3)));
        assert!(rl.is_tracked(&key(4)));
    }

    #[test]
    fn a_swept_idle_entry_behaves_like_a_full_bucket() {
        let rl = limiter(60, 2, 1);
        let t0 = Instant::now();
        assert_eq!(rl.check_at(key(1), t0), Decision::Allowed);
        assert_eq!(rl.check_at(key(1), t0), Decision::Allowed);
        // key 2 pushes key 1 out once key 1 is idle past a full refill (2 s).
        rl.check_at(key(2), t0 + Duration::from_secs(3));
        assert!(!rl.is_tracked(&key(1)));
        let later = t0 + Duration::from_secs(3);
        assert_eq!(rl.check_at(key(1), later), Decision::Allowed);
    }
}
