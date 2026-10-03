// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Loss on a publisher's uplink that retransmission did not repair
//! ([ADR-0067](../../../docs/adr/0067-loss-resilient-call-media.md), step 2b).
//!
//! ## Why the SFU asks for a keyframe itself
//!
//! Measured on a live two-laptop call (2026-10-03): every burst of loss on
//! a publisher's uplink cost each subscriber a freeze of 3.2-3.6 s. The
//! burst itself lasted about a second; the rest was the subscriber waiting
//! for retransmissions that could not come, then asking for a keyframe, then
//! receiving it. The SFU relayed that request at once and the publisher
//! answered in well under a second, so the wait sat entirely on the
//! subscriber's side.
//!
//! The SFU sees the same loss first, one hop earlier, and `str0m` already
//! asks the publisher for each missing packet. A packet still missing well
//! after those requests will not come, and every frame that depends on it
//! is lost for every subscriber. So the SFU asks the publisher for a
//! keyframe as soon as that is known, and a subscriber whose picture is
//! stuck decodes it on arrival without waiting out its own timer.
//!
//! ## What this tracks
//!
//! Per publisher video stream: the highest sequence number seen and the
//! numbers skipped below it, each with the time it was first missed. A
//! packet that arrives late or as a retransmission (which `str0m` hands over
//! under the number it repairs) fills its hole. A keyframe makes every hole
//! before it irrelevant. [`IngressLossWatch::unrepaired`] answers whether a
//! hole has outlived [`UNREPAIRED_LOSS_DEADLINE`], and starts over when it
//! says yes.
//!
//! Pure bookkeeping on sequence numbers: no payload, no key, no I/O.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How long a missing packet may stay missing before the SFU treats it as
/// lost for good.
///
/// Chosen, not measured. From below: `str0m` asks for a missing packet up to
/// five times, at least 33 ms apart, and the retransmission then needs one
/// round trip, which was 25-125 ms on the measured call; a deadline shorter
/// than that would ask for keyframes the retransmission was about to make
/// unnecessary. From above: every millisecond here is a millisecond of
/// frozen picture for every subscriber, and the subscriber's own path took
/// about three seconds.
pub const UNREPAIRED_LOSS_DEADLINE: Duration = Duration::from_millis(400);

/// The most holes tracked at once. A jump larger than this (or a stream
/// that loses more than this many packets before any is repaired) is
/// counted as loss without remembering each number.
pub const MAX_TRACKED_HOLES: u64 = 1024;

/// Missing packets of one publisher stream.
#[derive(Debug, Default)]
pub struct IngressLossWatch {
    /// Highest extended sequence number seen.
    highest: Option<u64>,
    /// Skipped sequence numbers and when each was first missed.
    holes: BTreeMap<u64, Instant>,
    /// When loss too large to track number by number was first seen.
    overflow_since: Option<Instant>,
}

impl IngressLossWatch {
    /// An empty watch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a packet with extended sequence number `seq` arriving at
    /// `now`. `starts_keyframe` is true for the first packet of a frame that
    /// depends on no other.
    pub fn observe(&mut self, seq: u64, starts_keyframe: bool, now: Instant) {
        match self.highest {
            None => self.highest = Some(seq),
            Some(highest) if seq > highest => {
                let skipped = seq - highest - 1;
                if skipped > 0 {
                    if skipped + self.holes.len() as u64 > MAX_TRACKED_HOLES {
                        self.overflow_since.get_or_insert(now);
                    } else {
                        for missing in highest + 1..seq {
                            self.holes.insert(missing, now);
                        }
                    }
                }
                self.highest = Some(seq);
            },
            Some(_) => {
                // Late or retransmitted: it fills its own hole, if any.
                self.holes.remove(&seq);
            },
        }
        if starts_keyframe {
            // Nothing before a keyframe is needed to decode what follows it.
            self.holes.retain(|missing, _| *missing > seq);
            self.overflow_since = None;
        }
    }

    /// Whether a hole has outlived [`UNREPAIRED_LOSS_DEADLINE`] at `now`.
    /// When it has, the watch forgets every current hole: the keyframe the
    /// caller asks for replaces all of them, and a hole that opens after
    /// this moment is a new loss with its own deadline.
    pub fn unrepaired(&mut self, now: Instant) -> bool {
        let oldest = self
            .holes
            .values()
            .min()
            .copied()
            .into_iter()
            .chain(self.overflow_since)
            .min();
        let Some(oldest) = oldest else {
            return false;
        };
        if now.saturating_duration_since(oldest) < UNREPAIRED_LOSS_DEADLINE {
            return false;
        }
        self.holes.clear();
        self.overflow_since = None;
        true
    }

    /// How many sequence numbers are missing right now (diagnostics and
    /// tests).
    #[must_use]
    pub fn missing(&self) -> usize {
        self.holes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn later(t: Instant, ms: u64) -> Instant {
        t + Duration::from_millis(ms)
    }

    #[test]
    fn an_unbroken_stream_never_asks() {
        let t0 = Instant::now();
        let mut w = IngressLossWatch::new();
        for (i, seq) in (100..200).enumerate() {
            w.observe(seq, false, later(t0, i as u64 * 10));
        }
        assert!(!w.unrepaired(later(t0, 5_000)));
    }

    #[test]
    fn a_repaired_hole_never_asks() {
        let t0 = Instant::now();
        let mut w = IngressLossWatch::new();
        w.observe(10, false, t0);
        w.observe(13, false, later(t0, 10));
        assert_eq!(w.missing(), 2);
        // The retransmissions arrive under the numbers they repair.
        w.observe(11, false, later(t0, 90));
        w.observe(12, false, later(t0, 120));
        assert_eq!(w.missing(), 0);
        assert!(!w.unrepaired(later(t0, 2_000)));
    }

    #[test]
    fn a_hole_past_the_deadline_asks_once_and_starts_over() {
        let t0 = Instant::now();
        let mut w = IngressLossWatch::new();
        w.observe(10, false, t0);
        w.observe(12, false, later(t0, 10));
        assert!(!w.unrepaired(later(t0, 10 + 399)));
        assert!(w.unrepaired(later(t0, 10 + 400)));
        // Forgotten: the keyframe asked for replaces it.
        assert_eq!(w.missing(), 0);
        assert!(!w.unrepaired(later(t0, 2_000)));
    }

    #[test]
    fn a_new_loss_after_asking_has_its_own_deadline() {
        let t0 = Instant::now();
        let mut w = IngressLossWatch::new();
        w.observe(1, false, t0);
        w.observe(3, false, t0);
        assert!(w.unrepaired(later(t0, 500)));
        w.observe(6, false, later(t0, 600));
        assert!(!w.unrepaired(later(t0, 900)));
        assert!(w.unrepaired(later(t0, 1_000)));
    }

    #[test]
    fn a_keyframe_clears_the_holes_before_it() {
        let t0 = Instant::now();
        let mut w = IngressLossWatch::new();
        w.observe(10, false, t0);
        w.observe(20, false, later(t0, 10));
        assert_eq!(w.missing(), 9);
        // A keyframe at 25 leaves 21..=24 missing; those are before it too.
        w.observe(25, true, later(t0, 50));
        assert_eq!(w.missing(), 0);
        assert!(!w.unrepaired(later(t0, 2_000)));
    }

    #[test]
    fn holes_inside_a_keyframe_still_count() {
        let t0 = Instant::now();
        let mut w = IngressLossWatch::new();
        w.observe(10, true, t0);
        w.observe(13, false, later(t0, 5));
        assert_eq!(w.missing(), 2);
        assert!(w.unrepaired(later(t0, 405)));
    }

    #[test]
    fn a_jump_too_large_to_track_still_asks() {
        let t0 = Instant::now();
        let mut w = IngressLossWatch::new();
        w.observe(0, false, t0);
        w.observe(MAX_TRACKED_HOLES + 10, false, later(t0, 1));
        assert_eq!(w.missing(), 0);
        assert!(!w.unrepaired(later(t0, 100)));
        assert!(w.unrepaired(later(t0, 401)));
    }

    #[test]
    fn a_keyframe_after_a_large_jump_cancels_it() {
        let t0 = Instant::now();
        let mut w = IngressLossWatch::new();
        w.observe(0, false, t0);
        w.observe(5_000, true, later(t0, 1));
        assert!(!w.unrepaired(later(t0, 1_000)));
    }

    #[test]
    fn a_duplicate_or_old_packet_changes_nothing() {
        let t0 = Instant::now();
        let mut w = IngressLossWatch::new();
        w.observe(50, false, t0);
        w.observe(50, false, later(t0, 1));
        w.observe(40, false, later(t0, 2));
        assert_eq!(w.missing(), 0);
        assert!(!w.unrepaired(later(t0, 2_000)));
    }
}
