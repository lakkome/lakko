// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! IR-style connection-throttle model (G5-7).
//!
//! Iran (and TSPU to a degree) degrades suspected circumvention flows with a
//! BANDWIDTH CAP rather than a hard block — the connection still works, just at
//! a speed too low to be usable, which is deniable ("the Internet is just
//! slow"). This module models that speed-cap as a byte token-bucket so the lab
//! can QUANTIFY how a flow degrades under a given throttle.
//!
//! This is a MODEL, not a live observer feature: the observer is drop-only (it
//! never forwards bytes), so it cannot actually throttle a flow — applying the
//! cap needs a proxy layer. The token-bucket math here is pure + deterministic
//! (the caller supplies the clock in millis, so tests are reproducible) and is
//! the verifiable core; wiring it onto a real forwarded flow is a future
//! proxy-layer follow-up.

/// A token-bucket rate limiter measured in BYTES. `capacity` is the burst
/// ceiling (max tokens that can accumulate); `rate_per_sec` is the sustained
/// refill rate. One token = one byte.
#[derive(Clone, Debug)]
pub struct TokenBucket {
    capacity: u64,
    rate_per_sec: u64,
    tokens: u64,
    last_refill_ms: u64,
}

/// The outcome of [`TokenBucket::consume`]: how many of the requested bytes pass
/// NOW, and how long the caller must wait before the remainder can pass at the
/// sustained rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThrottleDecision {
    /// Bytes allowed through immediately.
    pub passed: u64,
    /// Milliseconds until the un-passed remainder could pass at `rate_per_sec`
    /// (0 when everything passed).
    pub delay_ms: u64,
}

impl TokenBucket {
    /// A bucket starting FULL (a fresh connection gets its full burst).
    /// `rate_per_sec` of 0 is clamped to 1 to avoid a divide-by-zero in the
    /// delay computation (a true zero-rate cap is a block, not a throttle —
    /// model that with the GFW detector, not here).
    #[must_use]
    pub fn new(capacity: u64, rate_per_sec: u64, now_ms: u64) -> Self {
        Self {
            capacity,
            rate_per_sec: rate_per_sec.max(1),
            tokens: capacity,
            last_refill_ms: now_ms,
        }
    }

    /// Refill based on elapsed time (capped at `capacity`), then consume up to
    /// `bytes`. Returns how many passed now + the delay for the remainder.
    pub fn consume(&mut self, bytes: u64, now_ms: u64) -> ThrottleDecision {
        // Refill: add `elapsed_ms * rate / 1000` tokens, saturating at capacity.
        let elapsed = now_ms.saturating_sub(self.last_refill_ms);
        let refill = elapsed.saturating_mul(self.rate_per_sec) / 1000;
        self.tokens = self.tokens.saturating_add(refill).min(self.capacity);
        self.last_refill_ms = now_ms;

        let passed = bytes.min(self.tokens);
        self.tokens -= passed;
        let remainder = bytes - passed;
        // Time for `remainder` bytes to become available at the sustained rate.
        let delay_ms = if remainder == 0 {
            0
        } else {
            // ceil(remainder * 1000 / rate) so a partial token still waits.
            remainder.saturating_mul(1000).div_ceil(self.rate_per_sec)
        };
        ThrottleDecision { passed, delay_ms }
    }

    /// Current available tokens (bytes that would pass immediately).
    #[must_use]
    pub fn available(&self) -> u64 {
        self.tokens
    }
}

/// Model the wall-clock (ms) a `total`-byte transfer takes through a throttle of
/// `rate_per_sec` with burst `capacity`: the burst passes instantly, the rest is
/// rate-limited. A diagnostic for the lab (logged at boot when
/// `LAKKO_SNI_THROTTLE_BYTES_PER_SEC` is set) — the observer is drop-only and
/// never applies the throttle.
#[must_use]
pub fn model_transfer_ms(total: u64, rate_per_sec: u64, capacity: u64) -> u64 {
    // One consume of the whole transfer on a fresh, full bucket: `passed` is the
    // burst, `delay_ms` is the time for the post-burst remainder at the rate.
    TokenBucket::new(capacity, rate_per_sec, 0)
        .consume(total, 0)
        .delay_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_bucket_passes_its_full_burst_immediately() {
        let mut tb = TokenBucket::new(1000, 100, 0);
        let d = tb.consume(1000, 0);
        assert_eq!(
            d,
            ThrottleDecision {
                passed: 1000,
                delay_ms: 0
            }
        );
        assert_eq!(tb.available(), 0);
    }

    #[test]
    fn over_burst_passes_the_burst_and_delays_the_rest() {
        let mut tb = TokenBucket::new(500, 100, 0);
        // Ask for 800 with a 500 burst @ 100 B/s -> 500 pass, 300 wait 3000 ms.
        let d = tb.consume(800, 0);
        assert_eq!(d.passed, 500);
        assert_eq!(d.delay_ms, 3000);
    }

    #[test]
    fn tokens_refill_at_the_sustained_rate() {
        let mut tb = TokenBucket::new(1000, 100, 0);
        tb.consume(1000, 0); // drain
        assert_eq!(tb.available(), 0);
        // After 2 s at 100 B/s -> 200 tokens back.
        let d = tb.consume(150, 2000);
        assert_eq!(d.passed, 150);
        assert_eq!(tb.available(), 50);
    }

    #[test]
    fn refill_never_exceeds_capacity_after_a_long_idle() {
        let mut tb = TokenBucket::new(1000, 100, 0);
        tb.consume(1000, 0); // drain to 0
                             // Idle 1 hour: refill would be 360_000 tokens but caps at 1000.
        let d = tb.consume(1000, 3_600_000);
        assert_eq!(d.passed, 1000);
        assert_eq!(d.delay_ms, 0);
        assert_eq!(tb.available(), 0);
    }

    #[test]
    fn sustained_throughput_approaches_the_rate_cap() {
        // Drive 10 KiB through a 100 B/s cap (burst 100) in 100 B chunks,
        // advancing the clock by each chunk's delay. The total wall-clock must
        // be ~ (10240 - 100) / 100 s once the burst is spent.
        let mut tb = TokenBucket::new(100, 100, 0);
        let mut now = 0u64;
        let mut delivered = 0u64;
        // First chunk uses the burst.
        for _ in 0..102 {
            let d = tb.consume(100, now);
            delivered += d.passed;
            now += d.delay_ms.max(if d.passed == 100 { 1000 } else { 0 });
        }
        assert!(
            delivered >= 10_000,
            "should have delivered ~10 KiB: {delivered}"
        );
        // ~100 chunks of 100 B after the burst, each gated to 1 s -> ~100 s.
        assert!(
            (90_000..=110_000).contains(&now),
            "sustained delivery should take ~100 s at 100 B/s, took {now} ms"
        );
    }

    #[test]
    fn model_transfer_ms_is_post_burst_remainder_over_rate() {
        // 1 MiB through a 10 KB/s cap with a 10 KB burst: (1048576-10000) bytes
        // at 10000 B/s = ceil(1038576*1000/10000) = 103858 ms (~104 s).
        let ms = model_transfer_ms(1_048_576, 10_000, 10_000);
        assert_eq!(ms, 103_858);
        // A burst >= total means the whole thing passes instantly.
        assert_eq!(model_transfer_ms(5_000, 10_000, 10_000), 0);
    }

    #[test]
    fn zero_rate_is_clamped_to_one_not_a_divide_by_zero() {
        let mut tb = TokenBucket::new(0, 0, 0);
        // capacity 0 -> nothing passes immediately; rate clamped to 1 B/s, so
        // 10 bytes wait 10_000 ms rather than panicking.
        let d = tb.consume(10, 0);
        assert_eq!(d.passed, 0);
        assert_eq!(d.delay_ms, 10_000);
    }
}
