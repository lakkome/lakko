// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Overlap-window call-key ring — the SFrame epoch-rotation keyer.
//!
//! A [`CallKeyRing`] holds the **current** [`CallKey`] plus, during a rotation,
//! the **previous** one retained for a short overlap window so in-flight frames
//! sealed under the old key still open (ADR-0008; call-side invariants
//! C-8 / C-9 / C-10). It is the single source of truth for the 1-second overlap
//! **decision**: the retain-until-expiry logic, the `[OVERLAP_MIN_MS,
//! OVERLAP_MAX_MS]` clamp, and the receive-side try-current-then-previous fall
//! back all live here — not duplicated across the three platform media layers
//! (G-18, mid-call rekey).
//!
//! ## Why seed-based (not group-based)
//!
//! [`crate::group_call::GroupCallSession`] carries the same overlap fields but
//! re-derives its keys from the live MLS [`Group`](lakko_mls::Group) +
//! [`Identity`](lakko_mls::Identity). The client media layers (the web
//! `RTCRtpScriptTransform` worker, the iOS / Android native SFrame shims) run
//! **off the main thread** and hold only the 32-byte call-key **seed** the main
//! thread derived and handed across the boundary — they do not have the MLS
//! group. So the overlap keyer is exposed here as a **seed-based** type
//! ([`CallKeyRing::from_seed`] / [`CallKeyRing::rekey_from_seed_at`]) that the
//! FFI can wrap and the workers can drive, while `GroupCallSession` delegates to
//! it so the logic exists in exactly one place. This is the "core via FFI" shape
//! of the G-18 mid-call-rekey decision (ADR-0029).
//!
//! The core stays **time-source-free**: the caller supplies `now_ms` on rekey +
//! prune; the ring never reads a clock.

use crate::sframe::{CallKey, ParticipantKey, CALL_KEY_LEN};
use crate::{Error, Result};

/// Default SFrame epoch-overlap window — the previous-epoch key is retained this
/// long after a rotation so in-flight frames sealed under it still decode
/// (ADR-0008; C-8 / C-9).
pub const OVERLAP_DEFAULT_MS: u64 = 1_000;
/// Lower clamp on the overlap window. Below this the retention is too brief to
/// absorb in-flight frames.
pub const OVERLAP_MIN_MS: u64 = 50;
/// Upper clamp on the overlap window. Beyond this the previous key lingers long
/// enough to weaken seed-boundary forward secrecy (C-9 / C-10).
pub const OVERLAP_MAX_MS: u64 = 10_000;

/// Clamp a caller-supplied overlap to `[OVERLAP_MIN_MS, OVERLAP_MAX_MS]`.
#[must_use]
pub fn clamp_overlap(overlap_ms: u64) -> u64 {
    overlap_ms.clamp(OVERLAP_MIN_MS, OVERLAP_MAX_MS)
}

/// A previous-epoch [`CallKey`] retained for the overlap window, tagged with the
/// wall-clock instant (the caller's millisecond clock) it expires at.
///
/// The core stays time-source-free: the caller supplies `now_ms`; the expiry
/// **decision** (the 1-second window) is stamped here, not duplicated across
/// three platform timers (G-18).
struct RetainedCallKey {
    key: CallKey,
    expires_at_ms: u64,
}

/// The current + (optionally) previous [`CallKey`] for a call, with the
/// overlap-window fall-back on the receive path.
///
/// Seal always uses the current key; open tries the current key first and, on an
/// AEAD failure, falls back to the retained previous key while its overlap
/// window is live. See the module docs for the design rationale.
pub struct CallKeyRing {
    current: CallKey,
    previous: Option<RetainedCallKey>,
}

impl CallKeyRing {
    /// Build a ring around an already-derived current [`CallKey`], with no
    /// previous key retained.
    #[must_use]
    pub fn new(current: CallKey) -> Self {
        Self {
            current,
            previous: None,
        }
    }

    /// Build a ring directly from a 32-byte call-key seed — the shape the
    /// client media workers use, which receive the seed across the FFI /
    /// worker boundary rather than the MLS group.
    #[must_use]
    pub fn from_seed(seed: [u8; CALL_KEY_LEN]) -> Self {
        Self::new(CallKey::from_seed(seed))
    }

    /// Rotate to `next`, stashing the outgoing current key as the previous key
    /// with the given absolute expiry instant (in the caller's millisecond
    /// clock). Any earlier previous key is overwritten — only the most-recent
    /// previous epoch is retained (C-10).
    ///
    /// Pass `u64::MAX` for a previous key that never auto-expires (the caller
    /// then owns the drop via [`Self::drop_previous`]).
    pub fn rekey(&mut self, next: CallKey, expires_at_ms: u64) {
        let prior = std::mem::replace(&mut self.current, next);
        self.previous = Some(RetainedCallKey {
            key: prior,
            expires_at_ms,
        });
    }

    /// Clock-aware rotate: the previous key expires at `now_ms +
    /// clamp(overlap_ms)`, so the 1-second overlap **decision** lives here
    /// rather than in a per-platform timer. `overlap_ms` is clamped to
    /// `[OVERLAP_MIN_MS, OVERLAP_MAX_MS]` (pass [`OVERLAP_DEFAULT_MS`] for the
    /// ADR-0008 default).
    pub fn rekey_at(&mut self, next: CallKey, now_ms: u64, overlap_ms: u64) {
        let expires_at_ms = now_ms.saturating_add(clamp_overlap(overlap_ms));
        self.rekey(next, expires_at_ms);
    }

    /// Clock-aware rotate straight from a 32-byte seed — the entry point the FFI
    /// wraps for the client media workers on a mid-call epoch change.
    pub fn rekey_from_seed_at(&mut self, seed: [u8; CALL_KEY_LEN], now_ms: u64, overlap_ms: u64) {
        self.rekey_at(CallKey::from_seed(seed), now_ms, overlap_ms);
    }

    /// Borrow the current [`CallKey`].
    #[must_use]
    pub fn current(&self) -> &CallKey {
        &self.current
    }

    /// Borrow the retained previous [`CallKey`], if one is held. Diagnostic /
    /// test surface — production receive paths funnel through
    /// [`Self::try_open_with_overlap`] / [`Self::try_open_framed_with_overlap`].
    #[must_use]
    pub fn previous(&self) -> Option<&CallKey> {
        self.previous.as_ref().map(|r| &r.key)
    }

    /// Derive the seal-side [`ParticipantKey`] for a composite SFrame `kid`
    /// (ADR-0035: `(sender_leaf << 8) | stream_index`) from the **current** key.
    /// Seal always uses the current epoch. Keying on the full KID keeps every
    /// one of a participant's concurrent streams on a distinct key + nonce-salt
    /// space, so their independent counters cannot reuse a `(key, nonce)` pair.
    ///
    /// # Errors
    ///
    /// See [`CallKey::participant_key`].
    pub fn participant_key(&self, kid: u64) -> Result<ParticipantKey> {
        self.current.participant_key(kid)
    }

    /// Open a raw `(aad, ciphertext, frame_counter)` SFrame with the overlap
    /// behaviour: try the current key's per-stream key for `kid` (ADR-0035)
    /// first; on AEAD failure fall back to the previous key while one is
    /// retained. Returns the plaintext on either path.
    ///
    /// Outside an overlap window (no rotation yet, or the previous key was
    /// dropped / pruned) this collapses to "current key only".
    ///
    /// # Errors
    ///
    /// - [`Error::Crypto`] when the frame authenticates under **neither** key.
    /// - Any non-AEAD error from [`ParticipantKey::open`] is propagated
    ///   immediately (it is not an overlap-fall-back case).
    pub fn try_open_with_overlap(
        &self,
        kid: u64,
        aad: &[u8],
        ciphertext: &[u8],
        frame_counter: u32,
    ) -> Result<Vec<u8>> {
        let current = self.current.participant_key(kid)?;
        match current.open(aad, ciphertext, frame_counter) {
            Ok(pt) => return Ok(pt),
            Err(Error::Crypto(_)) => {},
            Err(other) => return Err(other),
        }
        if let Some(prev) = self.previous.as_ref() {
            let prev_key = prev.key.participant_key(kid)?;
            return prev_key.open(aad, ciphertext, frame_counter);
        }
        Err(Error::Crypto(
            "frame failed authentication under current key (no overlap window active)".into(),
        ))
    }

    /// Open an RFC 9605 framed SFrame (`open_framed` — the wire carries its own
    /// KID + counter header) with the same overlap behaviour as
    /// [`Self::try_open_with_overlap`]. `kid` is the composite `(leaf, stream)`
    /// key identifier the caller read from the frame's header (ADR-0035). The
    /// header is identical regardless of which seed sealed the frame (it is
    /// derived from the KID + counter, not the seed), so the previous key parses
    /// the same header and opens a late frame from the old epoch.
    ///
    /// # Errors
    ///
    /// - [`Error::Crypto`] when the frame authenticates under **neither** key.
    /// - A header-parse error (malformed wire) is propagated immediately — it is
    ///   not an overlap-fall-back case.
    pub fn try_open_framed_with_overlap(&self, kid: u64, wire: &[u8]) -> Result<Vec<u8>> {
        let current = self.current.participant_key(kid)?;
        match current.open_framed(wire) {
            Ok(pt) => return Ok(pt),
            Err(Error::Crypto(_)) => {},
            Err(other) => return Err(other),
        }
        if let Some(prev) = self.previous.as_ref() {
            let prev_key = prev.key.participant_key(kid)?;
            return prev_key.open_framed(wire);
        }
        Err(Error::Crypto(
            "framed frame failed authentication under current key (no overlap window active)"
                .into(),
        ))
    }

    /// Drop the retained previous key once it has expired at `now_ms` (G-18).
    /// Idempotent — a no-op when there is no previous key, it is still inside its
    /// window, or it was retained with no expiry (`u64::MAX`).
    pub fn prune_expired(&mut self, now_ms: u64) {
        if let Some(prev) = self.previous.as_ref() {
            if now_ms >= prev.expires_at_ms {
                self.previous = None;
            }
        }
    }

    /// `true` when a previous key is retained AND still inside its overlap
    /// window at `now_ms`. A key retained with no expiry (`u64::MAX`) is always
    /// live until dropped.
    #[must_use]
    pub fn has_live_previous(&self, now_ms: u64) -> bool {
        self.previous
            .as_ref()
            .is_some_and(|prev| now_ms < prev.expires_at_ms)
    }

    /// Unconditionally drop the previous key — the manual-timer path. Idempotent.
    pub fn drop_previous(&mut self) {
        self.previous = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::SframeHeader;

    const SEED_A: [u8; CALL_KEY_LEN] = [0x11; CALL_KEY_LEN];
    const SEED_B: [u8; CALL_KEY_LEN] = [0x22; CALL_KEY_LEN];
    const LEAF: u32 = 3;

    /// The composite KID for a leaf's first (stream-0) SFrame stream (ADR-0035).
    /// The ring is media-type-agnostic; these tests exercise one stream per leaf.
    fn kid_of(leaf: u32) -> u64 {
        SframeHeader::kid_for(leaf, 0).expect("stream 0 fits")
    }

    /// Seal under a ring's current key and return the framed wire.
    fn seal_framed(ring: &CallKeyRing, leaf: u32, plaintext: &[u8]) -> Vec<u8> {
        let mut pk = ring.participant_key(kid_of(leaf)).expect("participant key");
        pk.seal_framed(plaintext).expect("seal_framed")
    }

    #[test]
    fn framed_round_trip_no_overlap() {
        let ring = CallKeyRing::from_seed(SEED_A);
        let wire = seal_framed(&ring, LEAF, b"hello");
        let opened = ring
            .try_open_framed_with_overlap(kid_of(LEAF), &wire)
            .expect("open");
        assert_eq!(opened, b"hello");
        // No rotation yet → no previous key retained.
        assert!(!ring.has_live_previous(0));
        assert!(ring.previous().is_none());
    }

    #[test]
    fn overlap_opens_both_old_and_new_epoch_frames() {
        let mut ring = CallKeyRing::from_seed(SEED_A);
        // A frame sealed under epoch A, still in flight when the rotation lands.
        let old_wire = seal_framed(&ring, LEAF, b"old-epoch");

        // Rotate to epoch B with a 1 s overlap window at t=1000.
        ring.rekey_from_seed_at(SEED_B, 1_000, OVERLAP_DEFAULT_MS);
        assert!(ring.has_live_previous(1_500));

        // A frame sealed under the NEW epoch B.
        let new_wire = seal_framed(&ring, LEAF, b"new-epoch");

        // The old frame opens via the retained previous key…
        assert_eq!(
            ring.try_open_framed_with_overlap(kid_of(LEAF), &old_wire)
                .expect("old opens via overlap"),
            b"old-epoch"
        );
        // …and the new frame opens via the current key.
        assert_eq!(
            ring.try_open_framed_with_overlap(kid_of(LEAF), &new_wire)
                .expect("new opens via current"),
            b"new-epoch"
        );
    }

    #[test]
    fn prune_after_expiry_closes_the_overlap_window() {
        let mut ring = CallKeyRing::from_seed(SEED_A);
        let old_wire = seal_framed(&ring, LEAF, b"old-epoch");
        ring.rekey_from_seed_at(SEED_B, 1_000, OVERLAP_DEFAULT_MS); // expires at 2000

        // Inside the window: prune is a no-op, the old frame still opens.
        ring.prune_expired(1_500);
        assert!(ring.has_live_previous(1_500));
        assert_eq!(
            ring.try_open_framed_with_overlap(kid_of(LEAF), &old_wire)
                .expect("still inside window"),
            b"old-epoch"
        );

        // At/after expiry: prune drops the previous key, the old frame no longer
        // opens (seed-boundary forward secrecy fully in effect, C-9/C-10).
        assert!(!ring.has_live_previous(2_000));
        ring.prune_expired(2_000);
        assert!(ring.previous().is_none());
        assert!(ring
            .try_open_framed_with_overlap(kid_of(LEAF), &old_wire)
            .is_err());
    }

    #[test]
    fn overlap_window_is_clamped_to_bounds() {
        // Below the floor → clamped up to OVERLAP_MIN_MS.
        let mut low = CallKeyRing::from_seed(SEED_A);
        low.rekey_from_seed_at(SEED_B, 0, 1);
        assert!(low.has_live_previous(OVERLAP_MIN_MS - 1));
        assert!(!low.has_live_previous(OVERLAP_MIN_MS));

        // Above the ceiling → clamped down to OVERLAP_MAX_MS.
        let mut high = CallKeyRing::from_seed(SEED_A);
        high.rekey_from_seed_at(SEED_B, 0, 10 * OVERLAP_MAX_MS);
        assert!(high.has_live_previous(OVERLAP_MAX_MS - 1));
        assert!(!high.has_live_previous(OVERLAP_MAX_MS));
    }

    #[test]
    fn raw_open_with_overlap_round_trips_current_and_previous() {
        let mut ring = CallKeyRing::from_seed(SEED_A);
        let mut pk_old = ring.participant_key(kid_of(LEAF)).expect("pk old");
        let (counter_old, cipher_old) = pk_old.seal(b"aad", b"raw-old").expect("seal old");

        ring.rekey_from_seed_at(SEED_B, 1_000, OVERLAP_DEFAULT_MS);
        let mut pk_new = ring.participant_key(kid_of(LEAF)).expect("pk new");
        let (counter_new, cipher_new) = pk_new.seal(b"aad", b"raw-new").expect("seal new");

        assert_eq!(
            ring.try_open_with_overlap(kid_of(LEAF), b"aad", &cipher_old, counter_old)
                .expect("old via overlap"),
            b"raw-old"
        );
        assert_eq!(
            ring.try_open_with_overlap(kid_of(LEAF), b"aad", &cipher_new, counter_new)
                .expect("new via current"),
            b"raw-new"
        );
    }

    #[test]
    fn no_expiry_previous_survives_prune_and_closes_on_manual_drop() {
        let mut ring = CallKeyRing::from_seed(SEED_A);
        let old_wire = seal_framed(&ring, LEAF, b"old-epoch");
        // Manual-timer path: retain with the u64::MAX no-auto-expiry sentinel.
        ring.rekey(CallKey::from_seed(SEED_B), u64::MAX);

        // A realistic clock tick never reaches u64::MAX, so prune leaves it +
        // the old frame still opens through the retained previous key.
        let far_future = 9_999_999_999_u64;
        ring.prune_expired(far_future);
        assert!(ring.has_live_previous(far_future));
        assert_eq!(
            ring.try_open_framed_with_overlap(kid_of(LEAF), &old_wire)
                .expect("still retained"),
            b"old-epoch"
        );

        // A manual drop closes it (the platform's own overlap-timer path).
        ring.drop_previous();
        assert!(ring.previous().is_none());
        assert!(ring
            .try_open_framed_with_overlap(kid_of(LEAF), &old_wire)
            .is_err());
    }
}
