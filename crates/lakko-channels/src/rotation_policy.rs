// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Channel content-key rotation **policy** (G4-2-rotation-timer).
//!
//! Pure logic — given the current rotation state + a policy,
//! decides whether the publisher should rotate the channel
//! content key right now. The platform layer (web /
//! iOS / Android) owns the actual rotation orchestration:
//!
//! 1. Periodic timer (every ~5 minutes or on app foreground)
//!    calls [`RotationPolicy::should_rotate`].
//! 2. If `true` — platform calls `PublisherGroup::rotate` to
//!    advance the MLS epoch + derives a new content seed.
//! 3. Platform walks the subscriber registry, reseals the
//!    new seed under each subscriber's HPKE public key (via
//!    `lakko_channels::subscribe::seal_seed`), and POSTs the
//!    batch to `POST /v1/channels/:id/rotate` (the atomic
//!    fanout endpoint shipped in G4-4).
//! 4. Platform calls [`RotationState::after_rotation`] to
//!    record the moment + reset the post counter.
//! 5. Every published post bumps the counter via
//!    [`RotationState::after_post`].
//!
//! Why split policy from orchestration:
//!
//! - **Single source of truth** — the rotation rule lives in
//!   one place + is shared by three platforms via UniFFI +
//!   wasm-bindgen, so iOS and Android cannot drift to a
//!   slightly different cadence.
//! - **Test-light** — `should_rotate` is a pure function over
//!   `(now_ms, state, policy)`. No mocks, no clocks, no I/O.
//! - **Cheap to invoke** — the platform timer just calls
//!   `should_rotate` every poll; no Rust-side state machine
//!   on the FFI boundary.
//!
//! Defaults pinned in [`RotationPolicy::DEFAULT`] follow the
//! ADR-0010 §"PublisherGroup" rotation cadence: every 24 h or
//! every 1 000 posts, whichever lands first. Tighter cadence
//! shrinks the leak window for any single content key; looser
//! cadence reduces rotation traffic. 24 h / 1 k is the
//! conservative middle ground.

#![allow(clippy::module_name_repetitions)]

/// Default rotation interval — 24 hours in milliseconds.
pub const DEFAULT_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;

/// Default post count after which a rotation is forced even
/// if the time interval has not elapsed.
pub const DEFAULT_MAX_POSTS: u32 = 1000;

/// The two thresholds that govern when a publisher rotates
/// the channel content key. Either trigger fires
/// independently — rotation happens on whichever lands first.
///
/// Pinned in ADR-0010 §"PublisherGroup":
/// > Ротация content key — на membership change PublisherGroup
/// > и по расписанию (24 h / N сообщений).
///
/// The membership-change path is automatic (every MLS
/// epoch bump triggers a key derivation); the policy here
/// covers the **scheduled** half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationPolicy {
    /// Maximum milliseconds between rotations. Once
    /// `now_ms - state.last_rotation_ms >= interval_ms`,
    /// [`RotationPolicy::should_rotate`] returns `true`.
    ///
    /// `0` disables the time trigger (only the post count
    /// can drive rotations).
    pub interval_ms: u64,
    /// Maximum posts between rotations. Once
    /// `state.posts_since_rotation >= max_posts`,
    /// [`RotationPolicy::should_rotate`] returns `true`.
    ///
    /// `0` disables the count trigger (only the time
    /// interval can drive rotations).
    pub max_posts: u32,
}

impl RotationPolicy {
    /// The recommended cadence — 24 h or 1 000 posts,
    /// whichever lands first.
    pub const DEFAULT: Self = Self {
        interval_ms: DEFAULT_INTERVAL_MS,
        max_posts: DEFAULT_MAX_POSTS,
    };

    /// Construct a policy. Both thresholds may be zero, in
    /// which case `should_rotate` always returns `false` (no
    /// scheduled rotation; only membership-change-driven).
    #[must_use]
    pub const fn new(interval_ms: u64, max_posts: u32) -> Self {
        Self {
            interval_ms,
            max_posts,
        }
    }

    /// Decide whether the publisher should rotate now.
    ///
    /// Returns `true` if EITHER of:
    /// - The time trigger fires:
    ///   `interval_ms > 0 && now_ms.saturating_sub(state.last_rotation_ms) >= interval_ms`.
    /// - The count trigger fires:
    ///   `max_posts > 0 && state.posts_since_rotation >= max_posts`.
    ///
    /// **Saturating arithmetic on the time difference**
    /// matters: `now_ms < state.last_rotation_ms` can happen
    /// after a host clock jump (NTP correction, manual
    /// timezone change on Android, suspend/resume on iOS).
    /// We never want a clock skew to surface as a spurious
    /// rotation — `saturating_sub` yields `0` for that case,
    /// well below `interval_ms`, so the time trigger stays
    /// silent until the clock catches up.
    #[must_use]
    pub const fn should_rotate(self, now_ms: u64, state: RotationState) -> bool {
        let time_fired = self.interval_ms > 0
            && now_ms.saturating_sub(state.last_rotation_ms) >= self.interval_ms;
        let count_fired = self.max_posts > 0 && state.posts_since_rotation >= self.max_posts;
        time_fired || count_fired
    }
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Snapshot of when the channel was last rotated + how many
/// posts have happened since. Persisted by the platform
/// alongside the channel record (web: IndexedDB; iOS:
/// SQLCipher; Android: SQLCipher). The publisher loads it
/// on app start, calls [`RotationPolicy::should_rotate`]
/// periodically, and writes it back after every rotation +
/// every published post.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RotationState {
    /// Unix milliseconds when the last rotation completed.
    /// `0` means "never rotated yet" — the first call to
    /// `should_rotate` with a non-zero `interval_ms` will
    /// return `true` (assuming `now_ms >= interval_ms`).
    /// Genesis publishers should call
    /// [`RotationState::after_rotation`] right after creating
    /// the PublisherGroup so the first scheduled rotation
    /// lands ~`interval_ms` later, not immediately.
    pub last_rotation_ms: u64,
    /// Count of posts published since the last rotation.
    /// Bumped by [`RotationState::after_post`]; reset by
    /// [`RotationState::after_rotation`].
    pub posts_since_rotation: u32,
}

impl RotationState {
    /// Genesis state — never rotated, zero posts. Pair with
    /// [`RotationState::after_rotation`] right after
    /// PublisherGroup creation so the first scheduled
    /// rotation does not land immediately on `now_ms ==
    /// interval_ms`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last_rotation_ms: 0,
            posts_since_rotation: 0,
        }
    }

    /// Stamp a fresh state right after a successful rotation
    /// fanout (server returned 2xx, new epoch landed).
    #[must_use]
    pub const fn after_rotation(now_ms: u64) -> Self {
        Self {
            last_rotation_ms: now_ms,
            posts_since_rotation: 0,
        }
    }

    /// Bump the post counter, saturating at `u32::MAX` so a
    /// pathologically chatty channel never wraps to `0` and
    /// silently disables the count trigger. (Reaching
    /// `u32::MAX` posts between rotations would itself be a
    /// deeply broken state — `max_posts` defaults to 1 000.)
    #[must_use]
    pub const fn after_post(self) -> Self {
        Self {
            last_rotation_ms: self.last_rotation_ms,
            posts_since_rotation: self.posts_since_rotation.saturating_add(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR_MS: u64 = 60 * 60 * 1000;

    #[test]
    fn default_policy_pins_24h_and_1000_posts() {
        let p = RotationPolicy::default();
        assert_eq!(p.interval_ms, 24 * HOUR_MS);
        assert_eq!(p.max_posts, 1000);
    }

    #[test]
    fn no_rotation_right_after_genesis() {
        let p = RotationPolicy::default();
        let s = RotationState::after_rotation(1_000_000);
        // 1 hour later — neither trigger fires.
        assert!(!p.should_rotate(1_000_000 + HOUR_MS, s));
    }

    #[test]
    fn time_trigger_fires_at_exact_interval_boundary() {
        let p = RotationPolicy::default();
        let s = RotationState::after_rotation(1_000_000);
        let now = 1_000_000 + 24 * HOUR_MS;
        assert!(p.should_rotate(now, s));
        // One ms before the boundary — still silent.
        assert!(!p.should_rotate(now - 1, s));
    }

    #[test]
    fn count_trigger_fires_at_exact_post_count() {
        let p = RotationPolicy::default();
        let mut s = RotationState::after_rotation(1_000_000);
        for _ in 0..999 {
            s = s.after_post();
        }
        assert!(!p.should_rotate(1_000_000, s));
        s = s.after_post(); // 1000th post
        assert!(p.should_rotate(1_000_000, s));
    }

    #[test]
    fn either_trigger_alone_is_sufficient() {
        // Time fires, count is below threshold.
        let p = RotationPolicy::default();
        let s = RotationState {
            last_rotation_ms: 0,
            posts_since_rotation: 0,
        };
        assert!(p.should_rotate(24 * HOUR_MS, s));

        // Count fires, time is below threshold.
        let s2 = RotationState {
            last_rotation_ms: 1_000_000,
            posts_since_rotation: 1_500,
        };
        assert!(p.should_rotate(1_000_000, s2));
    }

    #[test]
    fn clock_skew_does_not_force_spurious_rotation() {
        // Host clock jumped backward — now_ms < last_rotation_ms.
        // Saturating arithmetic clamps the diff to 0, so the
        // time trigger stays silent regardless of how far
        // back the clock went.
        let p = RotationPolicy::default();
        let s = RotationState::after_rotation(10 * HOUR_MS);
        assert!(!p.should_rotate(5 * HOUR_MS, s));
        // Even now_ms = 0 must not trigger.
        assert!(!p.should_rotate(0, s));
    }

    #[test]
    fn zero_interval_disables_time_trigger() {
        let p = RotationPolicy {
            interval_ms: 0,
            max_posts: 100,
        };
        // Even with `now_ms` infinitely far past
        // `last_rotation_ms`, the time trigger stays silent.
        let s = RotationState::after_rotation(0);
        assert!(!p.should_rotate(u64::MAX, s));
    }

    #[test]
    fn zero_max_posts_disables_count_trigger() {
        let p = RotationPolicy {
            interval_ms: HOUR_MS,
            max_posts: 0,
        };
        let mut s = RotationState::after_rotation(0);
        for _ in 0..10_000u32 {
            s = s.after_post();
        }
        assert!(!p.should_rotate(HOUR_MS - 1, s));
    }

    #[test]
    fn both_zero_means_no_scheduled_rotation_ever() {
        let p = RotationPolicy {
            interval_ms: 0,
            max_posts: 0,
        };
        let s = RotationState::after_rotation(0);
        assert!(!p.should_rotate(u64::MAX, s));
        let s2 = RotationState {
            last_rotation_ms: 0,
            posts_since_rotation: u32::MAX,
        };
        assert!(!p.should_rotate(u64::MAX, s2));
    }

    #[test]
    fn after_post_saturates_at_u32_max() {
        let mut s = RotationState {
            last_rotation_ms: 0,
            posts_since_rotation: u32::MAX - 1,
        };
        s = s.after_post();
        assert_eq!(s.posts_since_rotation, u32::MAX);
        // One more — saturates, does not wrap.
        s = s.after_post();
        assert_eq!(s.posts_since_rotation, u32::MAX);
    }

    #[test]
    fn after_rotation_resets_post_counter() {
        let s = RotationState {
            last_rotation_ms: 100,
            posts_since_rotation: 500,
        };
        let s2 = RotationState::after_rotation(200);
        assert_ne!(s, s2);
        assert_eq!(s2.last_rotation_ms, 200);
        assert_eq!(s2.posts_since_rotation, 0);
    }

    #[test]
    fn new_genesis_state_is_zeroed() {
        let s = RotationState::new();
        assert_eq!(s.last_rotation_ms, 0);
        assert_eq!(s.posts_since_rotation, 0);
    }

    #[test]
    fn custom_policy_round_trip() {
        // 1 minute / 5 posts — tight cadence for tests.
        let p = RotationPolicy::new(60_000, 5);
        let mut s = RotationState::after_rotation(1_000);
        assert!(!p.should_rotate(2_000, s));
        for _ in 0..4 {
            s = s.after_post();
        }
        assert!(!p.should_rotate(2_000, s));
        s = s.after_post(); // 5th post — count trigger
        assert!(p.should_rotate(2_000, s));
    }
}
