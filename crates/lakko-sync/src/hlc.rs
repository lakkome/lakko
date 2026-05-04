// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Hybrid Logical Clock (Kulkarni–Demirbas–Browne 2014).
//!
//! Combines a wall-clock timestamp (so timestamps are intuitive in
//! the UI: "Alice edited this at 14:23") with a logical counter (so
//! events generated within the same millisecond, or events arriving
//! out of wall-clock order, still have a consistent total order).
//!
//! The clock has two operations:
//!
//! - [`HybridLogicalClock::tick`] — call before emitting a local
//!   event. Returns a monotonically-non-decreasing [`Hlc`].
//! - [`HybridLogicalClock::merge_remote`] — call when receiving
//!   an event from another device. Advances local state past the
//!   incoming timestamp and returns a fresh local stamp ordered
//!   strictly after both sides' previous events.
//!
//! Single-device usage (Phase 1) only ever calls `tick` —
//! `merge_remote` becomes important once Phase 2's DeviceGroup
//! ships and edits / read-receipts flow between paired devices.
//! Including it here keeps the API stable across phases.
//!
//! The wall-clock source is **caller-supplied** rather than read
//! from `SystemTime`. Reasons:
//!
//! 1. Tests stay deterministic.
//! 2. Future ports to `wasm32` (where reading `Date.now()` requires
//!    JS interop) plug their clock in via the same path.
//! 3. Honest acknowledgement that wall-clocks lie; the HLC formula
//!    is robust against `wall_now < local_pt` skew, but the call
//!    site that picks `wall_now` should know what source it's
//!    using.

use serde::{Deserialize, Serialize};

/// Hybrid Logical Clock value: physical milliseconds since the UNIX
/// epoch + a logical counter that breaks ties.
///
/// Total-ordered lexicographically: `(pt, lc)` is `<` `(pt', lc')`
/// iff `pt < pt'`, or `pt == pt'` and `lc < lc'`. Two `Hlc`s with
/// identical fields are equal.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct Hlc {
    /// Physical-time component, milliseconds since UNIX epoch.
    pub pt: u64,
    /// Logical counter — breaks ties when `pt` collides between
    /// rapid-fire events on the same device, or across merges.
    pub lc: u64,
}

impl Hlc {
    /// The smallest possible [`Hlc`]. Use as a sentinel for "before
    /// anything" rather than `Option<Hlc>` — keeps comparisons branch-
    /// free.
    pub const ZERO: Self = Self { pt: 0, lc: 0 };

    /// Construct an `Hlc` from raw fields. Most callers should use
    /// [`HybridLogicalClock::tick`] / [`HybridLogicalClock::merge_remote`]
    /// instead.
    #[must_use]
    pub const fn new(pt: u64, lc: u64) -> Self {
        Self { pt, lc }
    }
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Lexicographic on `(pt, lc)` — reuse the tuple's own Ord.
        (self.pt, self.lc).cmp(&(other.pt, other.lc))
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Per-device clock state. One instance per device-process; safe to
/// move across tasks but **not** thread-shared without external
/// synchronisation (no interior mutability — clone or wrap in a
/// `Mutex` if multiple writers).
#[derive(Clone, Debug)]
pub struct HybridLogicalClock {
    last: Hlc,
}

impl HybridLogicalClock {
    /// Fresh clock seeded at `Hlc::ZERO`. Subsequent ticks pull
    /// `pt` from the supplied `wall_now_ms`.
    #[must_use]
    pub const fn new() -> Self {
        Self { last: Hlc::ZERO }
    }

    /// Construct from an explicit prior state — used when restoring
    /// the clock from local storage between app launches so events
    /// emitted across restarts stay monotonic.
    #[must_use]
    pub const fn with_state(last: Hlc) -> Self {
        Self { last }
    }

    /// Most recent `Hlc` issued by this clock. `Hlc::ZERO` if no
    /// events have been emitted yet.
    #[must_use]
    pub const fn last(&self) -> Hlc {
        self.last
    }

    /// Issue a fresh stamp for a local event happening at wall-clock
    /// time `wall_now_ms`. The result is **strictly greater than**
    /// every previous return from this clock — the central
    /// HLC monotonicity guarantee.
    ///
    /// Algorithm (per Kulkarni et al. 2014, §3):
    ///
    /// ```text
    /// new_pt = max(last.pt, wall_now_ms)
    /// new_lc = if new_pt == last.pt then last.lc + 1 else 0
    /// ```
    pub fn tick(&mut self, wall_now_ms: u64) -> Hlc {
        let new_pt = self.last.pt.max(wall_now_ms);
        let new_lc = if new_pt == self.last.pt {
            self.last.lc.saturating_add(1)
        } else {
            0
        };
        self.last = Hlc {
            pt: new_pt,
            lc: new_lc,
        };
        self.last
    }

    /// Merge an `Hlc` received from another device. Returns a fresh
    /// local stamp ordered strictly after BOTH `self.last` and
    /// `remote`. Idempotent if called repeatedly on the same remote
    /// stamp (the second call just emits a fresh local tick because
    /// the clock has already advanced).
    ///
    /// Algorithm (per Kulkarni et al. 2014, §4):
    ///
    /// ```text
    /// new_pt = max(last.pt, remote.pt, wall_now_ms)
    /// new_lc = match (new_pt == last.pt, new_pt == remote.pt) {
    ///     (true, true)  => max(last.lc, remote.lc) + 1
    ///     (true, false) => last.lc + 1
    ///     (false, true) => remote.lc + 1
    ///     (false, false) => 0
    /// }
    /// ```
    pub fn merge_remote(&mut self, remote: Hlc, wall_now_ms: u64) -> Hlc {
        let new_pt = self.last.pt.max(remote.pt).max(wall_now_ms);
        let new_lc = match (new_pt == self.last.pt, new_pt == remote.pt) {
            (true, true) => self.last.lc.max(remote.lc).saturating_add(1),
            (true, false) => self.last.lc.saturating_add(1),
            (false, true) => remote.lc.saturating_add(1),
            (false, false) => 0,
        };
        self.last = Hlc {
            pt: new_pt,
            lc: new_lc,
        };
        self.last
    }
}

impl Default for HybridLogicalClock {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    // ----------------------------------------------------------------
    // Hlc ordering invariants
    // ----------------------------------------------------------------

    #[test]
    fn hlc_orders_by_pt_then_lc() {
        assert!(Hlc::new(10, 5) < Hlc::new(11, 0));
        assert!(Hlc::new(10, 5) < Hlc::new(10, 6));
        assert_eq!(Hlc::new(10, 5).cmp(&Hlc::new(10, 5)), Ordering::Equal);
        assert!(Hlc::new(10, 6) > Hlc::new(10, 5));
    }

    #[test]
    fn zero_is_less_than_any_real_event() {
        assert!(Hlc::ZERO < Hlc::new(1, 0));
        assert!(Hlc::ZERO < Hlc::new(0, 1));
    }

    // ----------------------------------------------------------------
    // tick monotonicity
    // ----------------------------------------------------------------

    #[test]
    fn tick_strictly_increases() {
        let mut c = HybridLogicalClock::new();
        let a = c.tick(1_000);
        let b = c.tick(1_001);
        let c2 = c.tick(1_001); // same wall_now → lc bumps
        assert!(a < b);
        assert!(b < c2);
    }

    #[test]
    fn tick_preserves_monotonicity_when_wall_clock_jumps_back() {
        let mut c = HybridLogicalClock::new();
        let a = c.tick(2_000);
        // Wall clock skewed backwards (NTP correction, suspend/resume).
        let b = c.tick(1_500);
        assert!(a < b, "tick must never return a smaller Hlc");
        assert_eq!(b.pt, a.pt, "pt floor stays at the previous max");
        assert_eq!(b.lc, a.lc + 1, "lc bumps because pt didn't advance");
    }

    #[test]
    fn tick_uses_wall_clock_when_it_advances() {
        let mut c = HybridLogicalClock::new();
        let _ = c.tick(1_000);
        let later = c.tick(2_000);
        assert_eq!(later.pt, 2_000);
        assert_eq!(later.lc, 0, "lc resets when pt advances");
    }

    #[test]
    fn tick_lc_grows_within_same_ms() {
        let mut c = HybridLogicalClock::new();
        let mut prev = Hlc::ZERO;
        for _ in 0..1_000 {
            let now = c.tick(5_000);
            assert!(now > prev);
            assert_eq!(now.pt, 5_000);
            prev = now;
        }
    }

    // ----------------------------------------------------------------
    // merge_remote
    // ----------------------------------------------------------------

    #[test]
    fn merge_remote_advances_past_both_sides() {
        let mut c = HybridLogicalClock::with_state(Hlc::new(1_000, 5));
        let remote = Hlc::new(2_000, 3);
        let merged = c.merge_remote(remote, 1_500);
        // new_pt = max(1000, 2000, 1500) = 2000
        // new_lc = remote.lc + 1 = 4
        assert!(merged > Hlc::new(1_000, 5));
        assert!(merged > remote);
        assert_eq!(merged, Hlc::new(2_000, 4));
    }

    #[test]
    fn merge_remote_local_dominates() {
        let mut c = HybridLogicalClock::with_state(Hlc::new(5_000, 7));
        let remote = Hlc::new(3_000, 99);
        let merged = c.merge_remote(remote, 4_000);
        // new_pt = 5000 (from local)
        // new_lc = local.lc + 1 = 8
        assert_eq!(merged, Hlc::new(5_000, 8));
    }

    #[test]
    fn merge_remote_wall_clock_dominates() {
        let mut c = HybridLogicalClock::with_state(Hlc::new(1_000, 0));
        let remote = Hlc::new(2_000, 0);
        let merged = c.merge_remote(remote, 9_999);
        // new_pt = 9999, neither matches → lc resets to 0
        assert_eq!(merged, Hlc::new(9_999, 0));
    }

    #[test]
    fn merge_remote_tie_takes_max_lc_plus_one() {
        let mut c = HybridLogicalClock::with_state(Hlc::new(1_000, 5));
        let remote = Hlc::new(1_000, 8);
        let merged = c.merge_remote(remote, 500);
        // new_pt = 1000 (matches both), max(5, 8) + 1 = 9
        assert_eq!(merged, Hlc::new(1_000, 9));
    }

    #[test]
    fn merge_remote_idempotent_on_repeat() {
        let mut c = HybridLogicalClock::new();
        let remote = Hlc::new(2_000, 3);
        let first = c.merge_remote(remote, 1_000);
        let second = c.merge_remote(remote, 1_000);
        assert!(second > first, "repeated merge must keep advancing");
    }

    // ----------------------------------------------------------------
    // Persistence helpers
    // ----------------------------------------------------------------

    #[test]
    fn with_state_round_trips_through_last() {
        let stamp = Hlc::new(123_456, 7);
        let c = HybridLogicalClock::with_state(stamp);
        assert_eq!(c.last(), stamp);
    }

    #[test]
    fn restoring_state_keeps_monotonicity_across_restart() {
        let mut c = HybridLogicalClock::new();
        let pre_restart = c.tick(1_000);

        // Simulate restart with persistence.
        let saved = c.last();
        let mut c2 = HybridLogicalClock::with_state(saved);

        let post_restart = c2.tick(1_000);
        assert!(post_restart > pre_restart);
    }
}
