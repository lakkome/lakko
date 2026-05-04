// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Blinded per-conversation presence tokens for Lakko.
//!
//! ## Why not a global "online" flag
//!
//! A global "Alice is online" indicator on the server would let any
//! observer (the server itself, network monitors, compromised
//! infrastructure) correlate Alice's online state across every group
//! she belongs to — which materialises the social graph the rest of
//! the architecture goes to great lengths to avoid (see ADR-0005
//! §13.8 and plan §4).
//!
//! Lakko's answer: **per-group, blinded, time-rotating tokens**.
//! Within one MLS group, members can compute each other's expected
//! tokens because they share the group secret; outside that group
//! (server, observers, members of *other* groups) the tokens look
//! like uniform 16 random bytes that rotate every few minutes.
//!
//! ## Token construction
//!
//! ```text
//! token = BLAKE3-keyed(presence_seed, member_pseudonym ‖ epoch_be)[..16]
//! ```
//!
//! Where:
//!
//! - `presence_seed` is a 32-byte secret derived from the MLS group
//!   exporter (label `"lakko/presence/v1"`, scoped per group). It
//!   rotates whenever the group's epoch advances; the MLS state
//!   machine guarantees every member computes the same value at the
//!   same epoch.
//! - `member_pseudonym` is a 32-byte stable identifier of the
//!   sender within the group (typically derived from MLS leaf
//!   index + a per-group salt). Members of the group have a way
//!   to enumerate everyone's `member_pseudonym`; the server does
//!   not.
//! - `epoch` is wall-clock-based. With the default 60-second
//!   chunking, a token published at time T stays valid for ≤ 1
//!   minute, after which the sender re-publishes a fresh one.
//!
//! ## What this crate does and does not do
//!
//! Provides:
//!
//! - [`PresenceToken`] newtype + [`derive_token`].
//! - [`current_epoch`] utility (wall-clock → epoch counter).
//! - [`PresenceTracker`] — a small `HashMap<PresenceToken, last_seen_ms>`
//!   ring used by the UI to answer "who's currently active in this
//!   group?".
//!
//! Does NOT cover:
//!
//! - Sourcing `presence_seed` from the MLS exporter (lives in
//!   `lakko-mls` once we wire the integration).
//! - Sending / receiving tokens through `services/ds` (apps glue).
//! - Typing / read-receipt indicators — those are separate
//!   per-group features (`lakko-sync` for read receipts).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;

use lakko_crypto::hash;
use lakko_crypto::secret::SecretBytes;

/// Truncation length of a presence token in bytes. 16 bytes = 128 bits
/// of preimage strength, which is more than enough — the token is
/// defensive (anti-correlation), not authenticating; an attacker
/// guessing one is just guessing some legitimate member is present
/// for one epoch, which they could already do by joining the group.
pub const PRESENCE_TOKEN_LEN: usize = 16;

/// Default presence epoch in seconds. Tokens rotate every minute,
/// which matches the granularity humans tolerate for "active now"
/// indicators on the receiving UI side.
pub const DEFAULT_EPOCH_SECS: u64 = 60;

/// Default sliding window during which a token is treated as "still
/// present". One minute past the last observed beat — covers the
/// epoch boundary plus a bit of network delay.
pub const DEFAULT_PRESENCE_WINDOW_MS: u64 = 60_000;

/// 16-byte presence token. Newtype keeps the API type-safe and
/// avoids accidental mixing with other 16-byte values.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct PresenceToken([u8; PRESENCE_TOKEN_LEN]);

impl PresenceToken {
    /// Wrap raw bytes (e.g. when reading off a wire format).
    #[must_use]
    pub const fn from_bytes(bytes: [u8; PRESENCE_TOKEN_LEN]) -> Self {
        Self(bytes)
    }

    /// Borrow the underlying 16 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; PRESENCE_TOKEN_LEN] {
        &self.0
    }
}

/// Derive a `PresenceToken` for `(presence_seed, member_pseudonym, epoch)`.
///
/// Members of the same group with the same seed and the recipient's
/// `member_pseudonym` compute the exact same token, which is how the
/// "is this Alice's beat?" check works on the receiving side. Every
/// other observer sees uniform 16 bytes that rotate per-epoch.
#[must_use]
pub fn derive_token(
    presence_seed: &SecretBytes<32>,
    member_pseudonym: &[u8; 32],
    epoch: u64,
) -> PresenceToken {
    // BLAKE3-keyed(seed, member_pseudonym ‖ epoch_be) — keyed mode
    // means a leaked `member_pseudonym ‖ epoch` (which is public on
    // the wire eventually, since it's derivable from group state)
    // doesn't let anyone without the seed reverse the token.
    let mut input = [0u8; 32 + 8];
    input[..32].copy_from_slice(member_pseudonym);
    input[32..].copy_from_slice(&epoch.to_be_bytes());
    let digest = hash::blake3_keyed(presence_seed.as_bytes(), &input);
    let mut out = [0u8; PRESENCE_TOKEN_LEN];
    out.copy_from_slice(&digest[..PRESENCE_TOKEN_LEN]);
    PresenceToken(out)
}

/// Map a UNIX-millisecond timestamp to the current epoch counter
/// for this presence chunking. Wraps around at u64 limits, but
/// that's ~580 million years from now — practically infinite.
///
/// Passing `epoch_secs = 0` is a misconfiguration; we return `0`
/// instead of panicking so a presence system stuck at epoch 0 is
/// visibly broken and easy to spot in logs / UI.
#[must_use]
pub fn current_epoch(now_unix_ms: u64, epoch_secs: u64) -> u64 {
    if epoch_secs == 0 {
        return 0;
    }
    now_unix_ms / 1_000 / epoch_secs
}

/// Tracks `(token, last_seen_ms)` for a single group's UI list.
///
/// One instance per group. The caller pumps it with every observed
/// token (typically arriving as MLS application messages from
/// `services/ds`); [`PresenceTracker::is_present`] /
/// [`PresenceTracker::active_tokens`] answer "is this peer beating
/// right now?".
///
/// This is a UX helper, not a security boundary — silent peers
/// fall off the list, but a real adversary can fake "presence"
/// just as easily as they could fake any other in-group message
/// (which means: not at all without breaking MLS).
#[derive(Clone, Debug)]
pub struct PresenceTracker {
    presence_window_ms: u64,
    seen: HashMap<PresenceToken, u64>,
}

impl PresenceTracker {
    /// New tracker. `presence_window_ms` is the sliding window —
    /// a token is "active" if its `last_seen_ms` is within this many
    /// milliseconds of the call to [`PresenceTracker::is_present`].
    #[must_use]
    pub fn new(presence_window_ms: u64) -> Self {
        Self {
            presence_window_ms,
            seen: HashMap::new(),
        }
    }

    /// New tracker with [`DEFAULT_PRESENCE_WINDOW_MS`].
    #[must_use]
    pub fn default_window() -> Self {
        Self::new(DEFAULT_PRESENCE_WINDOW_MS)
    }

    /// Record an observation of `token` at wall-clock time `now_unix_ms`.
    /// Repeated calls keep only the latest timestamp.
    pub fn observe(&mut self, token: PresenceToken, now_unix_ms: u64) {
        // Insert-or-update only if the new timestamp is newer; out-of-
        // order arrivals (network jitter, retries) shouldn't pull a
        // peer's "last seen" backwards.
        let entry = self.seen.entry(token).or_insert(0);
        if now_unix_ms > *entry {
            *entry = now_unix_ms;
        }
    }

    /// Is this token within the presence window as of `now_unix_ms`?
    #[must_use]
    pub fn is_present(&self, token: &PresenceToken, now_unix_ms: u64) -> bool {
        match self.seen.get(token) {
            Some(&ts) => now_unix_ms.saturating_sub(ts) <= self.presence_window_ms,
            None => false,
        }
    }

    /// All tokens currently within the presence window.
    #[must_use]
    pub fn active_tokens(&self, now_unix_ms: u64) -> Vec<PresenceToken> {
        self.seen
            .iter()
            .filter(|(_, &ts)| now_unix_ms.saturating_sub(ts) <= self.presence_window_ms)
            .map(|(t, _)| *t)
            .collect()
    }

    /// Drop tokens whose `last_seen_ms` is older than the presence
    /// window. Cheap; safe to call from a periodic timer.
    pub fn purge_expired(&mut self, now_unix_ms: u64) {
        let window = self.presence_window_ms;
        self.seen
            .retain(|_, ts| now_unix_ms.saturating_sub(*ts) <= window);
    }

    /// Number of tokens currently tracked (active + within-window-but-
    /// not-yet-purged). Mostly useful for tests + ops debugging.
    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// `true` if no observations have been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

impl Default for PresenceTracker {
    fn default() -> Self {
        Self::default_window()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(byte: u8) -> SecretBytes<32> {
        SecretBytes::<32>::new([byte; 32])
    }

    fn member(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    // ----------------------------------------------------------------
    // derive_token
    // ----------------------------------------------------------------

    #[test]
    fn derive_token_is_deterministic() {
        let s = seed(0xAA);
        let m = member(0x11);
        let a = derive_token(&s, &m, 100);
        let b = derive_token(&s, &m, 100);
        assert_eq!(a, b);
    }

    #[test]
    fn token_changes_with_epoch() {
        let s = seed(0xAA);
        let m = member(0x11);
        assert_ne!(derive_token(&s, &m, 100), derive_token(&s, &m, 101));
    }

    #[test]
    fn token_changes_with_member() {
        let s = seed(0xAA);
        assert_ne!(
            derive_token(&s, &member(0x11), 100),
            derive_token(&s, &member(0x22), 100),
        );
    }

    #[test]
    fn token_changes_with_seed() {
        let m = member(0x11);
        // Different seeds → different tokens (cross-group anti-correlation).
        assert_ne!(
            derive_token(&seed(0xAA), &m, 100),
            derive_token(&seed(0xBB), &m, 100),
        );
    }

    #[test]
    fn token_is_16_bytes() {
        let t = derive_token(&seed(0), &member(0), 0);
        assert_eq!(t.as_bytes().len(), 16);
    }

    // ----------------------------------------------------------------
    // current_epoch
    // ----------------------------------------------------------------

    #[test]
    fn current_epoch_chunks_by_seconds() {
        // 60s chunking: ms 0..59999 → 0; 60000..119999 → 1.
        assert_eq!(current_epoch(0, 60), 0);
        assert_eq!(current_epoch(59_999, 60), 0);
        assert_eq!(current_epoch(60_000, 60), 1);
        assert_eq!(current_epoch(119_999, 60), 1);
        assert_eq!(current_epoch(120_000, 60), 2);
    }

    #[test]
    fn current_epoch_handles_zero_chunk() {
        // Defensive — release builds should return 0 instead of panicking.
        assert_eq!(current_epoch(123_456, 0), 0);
    }

    // ----------------------------------------------------------------
    // PresenceTracker
    // ----------------------------------------------------------------

    #[test]
    fn observe_then_is_present_within_window() {
        let mut t = PresenceTracker::new(60_000);
        let token = PresenceToken::from_bytes([1; PRESENCE_TOKEN_LEN]);
        t.observe(token, 1_000_000);
        assert!(t.is_present(&token, 1_000_000));
        assert!(t.is_present(&token, 1_059_999));
    }

    #[test]
    fn token_falls_off_after_window() {
        let mut t = PresenceTracker::new(60_000);
        let token = PresenceToken::from_bytes([1; PRESENCE_TOKEN_LEN]);
        t.observe(token, 1_000_000);
        assert!(!t.is_present(&token, 1_060_001));
    }

    #[test]
    fn unseen_token_is_not_present() {
        let t = PresenceTracker::new(60_000);
        let token = PresenceToken::from_bytes([42; PRESENCE_TOKEN_LEN]);
        assert!(!t.is_present(&token, 0));
    }

    #[test]
    fn observe_keeps_latest_timestamp() {
        let mut t = PresenceTracker::new(60_000);
        let token = PresenceToken::from_bytes([1; PRESENCE_TOKEN_LEN]);
        t.observe(token, 2_000);
        // Late-arriving older beat must not move "last seen" backwards.
        t.observe(token, 1_000);
        // Confirm the later timestamp won.
        assert!(t.is_present(&token, 60_000));
        assert!(!t.is_present(&token, 70_000));
    }

    #[test]
    fn active_tokens_returns_only_within_window() {
        let mut t = PresenceTracker::new(60_000);
        let stale = PresenceToken::from_bytes([1; PRESENCE_TOKEN_LEN]);
        let fresh = PresenceToken::from_bytes([2; PRESENCE_TOKEN_LEN]);
        t.observe(stale, 0);
        t.observe(fresh, 100_000);
        let active = t.active_tokens(120_000);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0], fresh);
    }

    #[test]
    fn purge_expired_drops_old_entries() {
        let mut t = PresenceTracker::new(60_000);
        let a = PresenceToken::from_bytes([1; PRESENCE_TOKEN_LEN]);
        let b = PresenceToken::from_bytes([2; PRESENCE_TOKEN_LEN]);
        t.observe(a, 0);
        t.observe(b, 100_000);
        assert_eq!(t.len(), 2);
        t.purge_expired(120_000);
        assert_eq!(t.len(), 1);
        assert!(!t.is_present(&a, 120_000));
        assert!(t.is_present(&b, 120_000));
    }

    #[test]
    fn default_constructor_uses_default_window() {
        let t = PresenceTracker::default();
        let token = PresenceToken::from_bytes([0; PRESENCE_TOKEN_LEN]);
        assert!(!t.is_present(&token, 0));
        assert!(t.is_empty());
    }

    // ----------------------------------------------------------------
    // End-to-end "Alice is present" check
    // ----------------------------------------------------------------

    #[test]
    fn alice_publishes_bob_observes_and_recognises() {
        // Two members of the same group share a seed; both also know
        // each other's member_pseudonym (from MLS group state).
        let group_seed = seed(0xCD);
        let alice = member(0xAA);
        let bob = member(0xBB);

        // At t = 1000s Alice publishes a beat.
        let t_ms: u64 = 1_000_000;
        let epoch = current_epoch(t_ms, DEFAULT_EPOCH_SECS);
        let alice_token = derive_token(&group_seed, &alice, epoch);

        // Bob's tracker observes the token (carried inside an MLS
        // application message — outside this crate's scope).
        let mut bob_tracker = PresenceTracker::default_window();
        bob_tracker.observe(alice_token, t_ms);

        // Bob can re-derive the expected token from the group secrets
        // he holds and confirm it matches Alice.
        let expected_alice = derive_token(&group_seed, &alice, epoch);
        assert!(bob_tracker.is_present(&expected_alice, t_ms));

        // Bob's own beat — different member_pseudonym → different token.
        let bob_token = derive_token(&group_seed, &bob, epoch);
        assert_ne!(alice_token, bob_token);
        assert!(!bob_tracker.is_present(&bob_token, t_ms));
    }
}
