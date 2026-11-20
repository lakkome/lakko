// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The checks a sealed wake passes before push-gw reads a subscription row
//! ([ADR-0078] section 3): freshness, nonce dedup, and a per-group cap.
//!
//! - **Freshness.** A wake whose timestamp is more than [`MAX_SKEW_MS`] from
//!   push-gw's clock, either way, is refused, which bounds how long a nonce
//!   must be remembered.
//! - **Nonce dedup.** Each wake carries 16 random bytes; a second wake with
//!   a nonce seen within the window is refused. The set lives in memory and
//!   holds nonces only (no group, no time of day that could be joined to
//!   anything), bounded by [`MAX_TRACKED_NONCES`]; when it is full after
//!   evicting expired entries a new wake is refused rather than an unexpired
//!   nonce forgotten (fail closed). A restart forgets the set, so a wake
//!   captured within [`MAX_SKEW_MS`] before a restart can be replayed once;
//!   the cost is a duplicate notification, since a wake carries nothing but
//!   a pointer.
//! - **Per-group cap.** Before ADR-0078 the Delivery Service capped wakes
//!   per target `push_gid`; it no longer sees one, so the cap moved here.
//!   A member could otherwise drive one group's wakes through every journal
//!   it writes to, each with its own per-journal budget at the DS.
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use lakko_contacts::push_wake::PUSH_WAKE_NONCE_LEN;

/// Largest difference between a wake's timestamp and push-gw's clock, in
/// either direction. Chosen, not measured: phone clocks drift by seconds,
/// a misconfigured one by minutes, and the DS forwards within milliseconds.
pub(crate) const MAX_SKEW_MS: u64 = 10 * 60 * 1000;

/// Most nonces remembered at once. Chosen, not measured: at the DS's default
/// cap of one wake a second sustained per journal this is the window of
/// about 218 busy journals; a larger deployment raises it.
pub(crate) const MAX_TRACKED_NONCES: usize = 262_144;

/// Most groups the per-group cap tracks at once.
pub(crate) const MAX_TRACKED_GROUPS: usize = 65_536;

/// Default burst per group. Same value the DS used for its target cap.
pub(crate) const DEFAULT_BURST: u32 = 30;

/// Default sustained wakes per minute per group. Same value the DS used.
pub(crate) const DEFAULT_PER_MINUTE: u32 = 60;

/// Why a wake was refused before any row was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuardRefusal {
    /// Timestamp outside the window.
    Stale,
    /// The nonce was seen within the window.
    Replay,
    /// The nonce set is full of unexpired nonces (fail closed: the wake is
    /// refused rather than an unexpired nonce forgotten).
    NonceSetFull,
    /// The group's wake budget is spent.
    Capped,
}

#[derive(Clone, Copy)]
struct Bucket {
    tokens: f64,
    last_seen: Instant,
}

/// The three checks, shared by every forward.
pub(crate) struct WakeGuard {
    nonces: Mutex<HashMap<[u8; PUSH_WAKE_NONCE_LEN], Instant>>,
    buckets: Mutex<HashMap<[u8; 32], Bucket>>,
    nonce_ttl: Duration,
    max_nonces: usize,
    rate_per_sec: f64,
    burst: f64,
}

impl WakeGuard {
    /// A guard with the default cap and bounds.
    pub(crate) fn new(burst: u32, per_minute: u32) -> Self {
        Self::with_bounds(burst, per_minute, MAX_TRACKED_NONCES)
    }

    fn with_bounds(burst: u32, per_minute: u32, max_nonces: usize) -> Self {
        Self {
            nonces: Mutex::new(HashMap::new()),
            buckets: Mutex::new(HashMap::new()),
            // Twice the skew: a wake stamped MAX_SKEW_MS in the future stays
            // fresh for 2 x MAX_SKEW_MS of push-gw's clock.
            nonce_ttl: Duration::from_millis(2 * MAX_SKEW_MS),
            max_nonces: max_nonces.max(1),
            rate_per_sec: f64::from(per_minute.max(1)) / 60.0,
            burst: f64::from(burst.max(1)),
        }
    }

    /// Run the three checks for a wake stamped `timestamp_ms`, carrying
    /// `nonce`, for `push_gid`, at push-gw's wall clock `now_ms` and
    /// monotonic `now`.
    pub(crate) fn admit(
        &self,
        timestamp_ms: u64,
        nonce: &[u8; PUSH_WAKE_NONCE_LEN],
        push_gid: &[u8; 32],
        now_ms: u64,
        now: Instant,
    ) -> Result<(), GuardRefusal> {
        if timestamp_ms.abs_diff(now_ms) > MAX_SKEW_MS {
            return Err(GuardRefusal::Stale);
        }
        self.admit_nonce(nonce, now)?;
        if self.take_token(push_gid, now) {
            Ok(())
        } else {
            Err(GuardRefusal::Capped)
        }
    }

    fn admit_nonce(
        &self,
        nonce: &[u8; PUSH_WAKE_NONCE_LEN],
        now: Instant,
    ) -> Result<(), GuardRefusal> {
        let mut seen = self.nonces.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(at) = seen.get(nonce) {
            if now.saturating_duration_since(*at) < self.nonce_ttl {
                return Err(GuardRefusal::Replay);
            }
        }
        if seen.len() >= self.max_nonces {
            let ttl = self.nonce_ttl;
            seen.retain(|_, at| now.saturating_duration_since(*at) < ttl);
            if seen.len() >= self.max_nonces {
                return Err(GuardRefusal::NonceSetFull);
            }
        }
        seen.insert(*nonce, now);
        Ok(())
    }

    fn take_token(&self, push_gid: &[u8; 32], now: Instant) -> bool {
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        if !buckets.contains_key(push_gid) && buckets.len() >= MAX_TRACKED_GROUPS {
            let idle = Duration::try_from_secs_f64(self.burst / self.rate_per_sec)
                .unwrap_or(Duration::from_secs(60));
            buckets.retain(|_, b| now.saturating_duration_since(b.last_seen) < idle);
            if buckets.len() >= MAX_TRACKED_GROUPS {
                if let Some(oldest) = buckets
                    .iter()
                    .min_by_key(|(_, b)| b.last_seen)
                    .map(|(k, _)| *k)
                {
                    buckets.remove(&oldest);
                }
            }
        }
        let bucket = buckets.entry(*push_gid).or_insert(Bucket {
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
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Read a positive integer knob, or its default.
pub(crate) fn knob(name: &str, default: u32) -> anyhow::Result<u32> {
    match std::env::var(name).ok().filter(|v| !v.trim().is_empty()) {
        None => Ok(default),
        Some(raw) => match raw.trim().parse::<u32>() {
            Ok(n) if n > 0 => Ok(n),
            _ => anyhow::bail!("{name} must be a positive integer"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW_MS: u64 = 1_800_000_000_000;

    #[test]
    fn a_fresh_wake_passes_once() {
        let g = WakeGuard::new(30, 60);
        let t = Instant::now();
        assert_eq!(g.admit(NOW_MS, &[1; 16], &[9; 32], NOW_MS, t), Ok(()));
        assert_eq!(
            g.admit(NOW_MS, &[1; 16], &[9; 32], NOW_MS, t),
            Err(GuardRefusal::Replay)
        );
        assert_eq!(g.admit(NOW_MS, &[2; 16], &[9; 32], NOW_MS, t), Ok(()));
    }

    #[test]
    fn a_stale_or_future_wake_is_refused() {
        let g = WakeGuard::new(30, 60);
        let t = Instant::now();
        for ts in [NOW_MS - MAX_SKEW_MS - 1, NOW_MS + MAX_SKEW_MS + 1, 0] {
            assert_eq!(
                g.admit(ts, &[3; 16], &[9; 32], NOW_MS, t),
                Err(GuardRefusal::Stale)
            );
        }
        assert_eq!(
            g.admit(NOW_MS - MAX_SKEW_MS, &[3; 16], &[9; 32], NOW_MS, t),
            Ok(())
        );
    }

    #[test]
    fn a_nonce_is_forgotten_only_after_its_window() {
        let g = WakeGuard::new(30, 60);
        let t = Instant::now();
        g.admit(NOW_MS, &[4; 16], &[9; 32], NOW_MS, t).unwrap();
        let later = t + Duration::from_millis(2 * MAX_SKEW_MS - 1);
        assert_eq!(
            g.admit(NOW_MS, &[4; 16], &[9; 32], NOW_MS, later),
            Err(GuardRefusal::Replay)
        );
    }

    #[test]
    fn a_full_nonce_set_refuses_rather_than_forgets() {
        let g = WakeGuard::with_bounds(1000, 6000, 2);
        let t = Instant::now();
        g.admit(NOW_MS, &[1; 16], &[9; 32], NOW_MS, t).unwrap();
        g.admit(NOW_MS, &[2; 16], &[9; 32], NOW_MS, t).unwrap();
        assert_eq!(
            g.admit(NOW_MS, &[3; 16], &[9; 32], NOW_MS, t),
            Err(GuardRefusal::NonceSetFull)
        );
        // Once the first two expire, room is made.
        let later = t + Duration::from_millis(2 * MAX_SKEW_MS + 1);
        assert_eq!(g.admit(NOW_MS, &[3; 16], &[9; 32], NOW_MS, later), Ok(()));
    }

    #[test]
    fn a_group_is_capped_after_its_burst() {
        let g = WakeGuard::new(2, 60);
        let t = Instant::now();
        assert_eq!(g.admit(NOW_MS, &[1; 16], &[7; 32], NOW_MS, t), Ok(()));
        assert_eq!(g.admit(NOW_MS, &[2; 16], &[7; 32], NOW_MS, t), Ok(()));
        assert_eq!(
            g.admit(NOW_MS, &[3; 16], &[7; 32], NOW_MS, t),
            Err(GuardRefusal::Capped)
        );
        // Another group has its own budget.
        assert_eq!(g.admit(NOW_MS, &[4; 16], &[8; 32], NOW_MS, t), Ok(()));
        // And the first refills (60/min = one a second).
        assert_eq!(
            g.admit(
                NOW_MS,
                &[5; 16],
                &[7; 32],
                NOW_MS,
                t + Duration::from_millis(1100)
            ),
            Ok(())
        );
    }
}
