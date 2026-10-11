// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Which of a publisher's camera senders somebody is being sent, so that
//! the publisher can stop encoding and uploading the ones nobody watches
//! ([ADR-0070](../../../docs/adr/0070-call-media-through-a-loss-burst.md),
//! note of 2026-10-11, "senders nobody watches").
//!
//! ## Why
//!
//! Seen on a live call of four (2026-10-09): every tile showed the middle
//! picture, and every browser went on uploading its main sender, up to
//! 2.7 Mbit/s, which the SFU forwarded to nobody. Two of the browsers
//! shared one mobile link whose uplink stalled repeatedly.
//!
//! ## The rule
//!
//! A publisher that declared its senders (a light track in its offer) is
//! told, in a `video-needed` frame, whether its **main** and its **middle**
//! sender are needed. The light sender always is and is not in the frame.
//!
//! - A sender is **needed** while at least one subscriber wants that
//!   picture of the publisher (the caller counts them, see
//!   `crate::media::MediaPeer::heavy_wanted_of`). Only a subscriber that
//!   **named** the publisher at that quality in its latest `video-wants`
//!   counts; one that said nothing of it counts for nothing heavy, so
//!   somebody joining a call never switches a sender on before it has
//!   said what it shows.
//! - **On, at once.** The moment a sender that was told `false` is needed,
//!   the publisher is told `true`.
//! - **Off, late.** A sender is told `false` only after it has been
//!   unneeded for [`UNNEEDED_AFTER`] without a break. A publisher that has
//!   just declared its senders is therefore told `true` first.
//! - A publisher without a middle sender is always told `middle: false`.
//!
//! ## What it is not
//!
//! Pure bookkeeping: no I/O, no clock of its own (the caller supplies every
//! time), no packet, no key. Counting the subscribers, sending the frame and
//! everything an idle sender means for the rest of the media loop are the
//! caller's ([`crate::media`]).
//!
//! Every constant below was chosen, not measured.

use std::time::{Duration, Instant};

/// How long a sender must have been unneeded, without a break, before its
/// publisher is told so. Chosen: long enough that a viewer flipping between
/// the grid and the speaker view, or a subscriber leaving and coming back,
/// does not stop and start an encoder; short against the minutes a call
/// spends in one layout.
pub const UNNEEDED_AFTER: Duration = Duration::from_secs(10);

/// How long a sender the publisher was told is not needed must have sent
/// nothing before the SFU takes it for switched off (idle). Chosen: several
/// frame intervals of any camera sender, so the gap between two frames is
/// never taken for it.
pub const IDLE_QUIET: Duration = Duration::from_millis(300);

/// How long the rate last measured on a sender that has gone idle is taken
/// as what the sender costs when it is switched back on. After that the
/// nominal rate of its role is. Chosen: a sender switched back on within a
/// minute most likely encodes what it did before.
pub const RATE_MEMORY: Duration = Duration::from_secs(60);

/// How long after an idle sender's first packet its measured rate is not
/// yet trusted (the meter's window still holds the silence): two windows
/// of the one second meter.
pub const RATE_SETTLE: Duration = Duration::from_secs(2);

/// What an idle main sender is taken to cost once its last measured rate
/// is too old, bits per second. Chosen: what the Lakko web client's 720p
/// sender is capped near.
pub const NOMINAL_MAIN_BPS: u64 = 2_500_000;

/// And an idle middle sender: the cap of the web client's half-size
/// sender.
pub const NOMINAL_MIDDLE_BPS: u64 = 500_000;

/// The content of one `video-needed` frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Needed {
    /// The main sender.
    pub main: bool,
    /// The middle sender.
    pub middle: bool,
}

/// How many subscribers want each heavy picture of one publisher right
/// now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Wanted {
    /// Subscribers that want the main picture.
    pub main: usize,
    /// Subscribers that want the middle picture, or keep it needed as
    /// what their allocation falls back to.
    pub middle: usize,
}

/// One heavy sender of a publisher.
#[derive(Debug, Clone, Copy, Default)]
struct Sender {
    /// Since when nobody has wanted it; `None` while somebody does.
    unneeded_since: Option<Instant>,
}

impl Sender {
    /// Take the current need and say what the publisher is to be told.
    fn update(&mut self, wanted: bool, now: Instant) -> bool {
        if wanted {
            self.unneeded_since = None;
            return true;
        }
        let since = *self.unneeded_since.get_or_insert(now);
        now.saturating_duration_since(since) < UNNEEDED_AFTER
    }
}

/// What one publisher has been told, and since when each of its heavy
/// senders has been unneeded.
#[derive(Debug, Clone, Copy, Default)]
pub struct VideoNeeded {
    main: Sender,
    middle: Sender,
    told: Option<Needed>,
}

impl VideoNeeded {
    /// A publisher that has been told nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            main: Sender {
                unneeded_since: None,
            },
            middle: Sender {
                unneeded_since: None,
            },
            told: None,
        }
    }

    /// Take the current count of subscribers that want each picture and
    /// return the frame to send, if what the publisher is to be told
    /// differs from what it was last told (or it was told nothing yet).
    /// The returned content is recorded as told; a caller that could not
    /// deliver it calls [`Self::forget_told`].
    ///
    /// `has_middle` says whether the publisher has a middle sender at
    /// all; without one `middle` is `false` at once and for good.
    pub fn update(&mut self, wanted: Wanted, has_middle: bool, now: Instant) -> Option<Needed> {
        let main = self.main.update(wanted.main > 0, now);
        let middle = if has_middle {
            self.middle.update(wanted.middle > 0, now)
        } else {
            self.middle.unneeded_since = None;
            false
        };
        let next = Needed { main, middle };
        if self.told == Some(next) {
            return None;
        }
        self.told = Some(next);
        Some(next)
    }

    /// What the publisher was last told; `None` before the first frame.
    #[must_use]
    pub const fn told(&self) -> Option<Needed> {
        self.told
    }

    /// The last frame did not reach the publisher: the next
    /// [`Self::update`] returns the content again.
    pub fn forget_told(&mut self) {
        self.told = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn wanted(main: usize, middle: usize) -> Wanted {
        Wanted { main, middle }
    }

    const fn needed(main: bool, middle: bool) -> Needed {
        Needed { main, middle }
    }

    #[test]
    fn the_first_frame_says_true_for_every_sender_the_publisher_has() {
        let t0 = Instant::now();
        // Nobody wants anything yet: off is late, so the first word is on.
        let mut with_middle = VideoNeeded::new();
        assert_eq!(with_middle.told(), None);
        assert_eq!(
            with_middle.update(wanted(0, 0), true, t0),
            Some(needed(true, true))
        );
        assert_eq!(with_middle.told(), Some(needed(true, true)));
        // A publisher without a middle sender never gets `middle: true`.
        let mut without = VideoNeeded::new();
        assert_eq!(
            without.update(wanted(0, 0), false, t0),
            Some(needed(true, false))
        );
        assert_eq!(
            without.update(wanted(3, 3), false, t0),
            None,
            "a middle picture wanted of a publisher that has none changes nothing"
        );
    }

    #[test]
    fn every_combination_of_need_is_told_as_it_is_once_the_delay_has_passed() {
        for (main, middle) in [(0, 0), (0, 1), (1, 0), (2, 3)] {
            let t0 = Instant::now();
            let mut state = VideoNeeded::new();
            let _ = state.update(wanted(main, middle), true, t0);
            let _ = state.update(wanted(main, middle), true, t0 + UNNEEDED_AFTER);
            assert_eq!(
                state.told(),
                Some(needed(main > 0, middle > 0)),
                "wanted main={main} middle={middle}"
            );
        }
    }

    #[test]
    fn off_is_told_only_after_the_whole_delay_and_on_at_once() {
        let t0 = Instant::now();
        let mut state = VideoNeeded::new();
        assert_eq!(
            state.update(wanted(1, 1), true, t0),
            Some(needed(true, true))
        );
        // The last viewer of the main picture goes to the middle one.
        let gone = t0 + Duration::from_secs(5);
        assert_eq!(state.update(wanted(0, 1), true, gone), None);
        let almost = gone + UNNEEDED_AFTER.saturating_sub(Duration::from_millis(1));
        assert_eq!(state.update(wanted(0, 1), true, almost), None);
        assert_eq!(
            state.update(wanted(0, 1), true, gone + UNNEEDED_AFTER),
            Some(needed(false, true))
        );
        // Nothing more while nothing changes.
        assert_eq!(
            state.update(wanted(0, 1), true, gone + UNNEEDED_AFTER * 3),
            None
        );
        // Somebody wants it again: told in the same call.
        let back = gone + UNNEEDED_AFTER * 4;
        assert_eq!(
            state.update(wanted(1, 1), true, back),
            Some(needed(true, true))
        );
    }

    #[test]
    fn a_need_that_comes_back_inside_the_delay_starts_it_over() {
        let t0 = Instant::now();
        let mut state = VideoNeeded::new();
        let _ = state.update(wanted(1, 0), true, t0);
        // Grid, speaker view, grid: the main picture is unwanted for nine
        // seconds, wanted for a moment, unwanted again.
        let s = Duration::from_secs;
        assert_eq!(state.update(wanted(0, 1), true, t0 + s(1)), None);
        assert_eq!(state.update(wanted(1, 0), true, t0 + s(10)), None);
        assert_eq!(state.update(wanted(0, 1), true, t0 + s(11)), None);
        // Ten seconds after the first time it went unwanted: still on.
        assert_eq!(state.update(wanted(0, 1), true, t0 + s(12)), None);
        assert_eq!(state.told(), Some(needed(true, true)));
        assert_eq!(
            state.update(wanted(0, 1), true, t0 + s(21)),
            Some(needed(false, true))
        );
    }

    #[test]
    fn an_undelivered_frame_is_returned_again() {
        let t0 = Instant::now();
        let mut state = VideoNeeded::new();
        let first = state.update(wanted(1, 1), true, t0);
        assert!(first.is_some());
        assert_eq!(state.update(wanted(1, 1), true, t0), None);
        state.forget_told();
        assert_eq!(state.update(wanted(1, 1), true, t0), first);
    }

    #[test]
    fn a_middle_sender_declared_later_is_told_then() {
        let t0 = Instant::now();
        let mut state = VideoNeeded::new();
        assert_eq!(
            state.update(wanted(1, 0), false, t0),
            Some(needed(true, false))
        );
        // The publisher's next offer declares a middle sender.
        assert_eq!(
            state.update(wanted(1, 0), true, t0 + Duration::from_secs(1)),
            Some(needed(true, true))
        );
        assert_eq!(
            state.update(
                wanted(1, 0),
                true,
                t0 + Duration::from_secs(1) + UNNEEDED_AFTER
            ),
            Some(needed(true, false))
        );
    }

    #[test]
    fn the_constants_keep_their_order() {
        // A viewer's layout flip must not stop an encoder.
        assert!(UNNEEDED_AFTER >= Duration::from_secs(5));
        // A gap between two frames is not an idle sender.
        assert!(IDLE_QUIET >= Duration::from_millis(200));
        assert!(IDLE_QUIET < UNNEEDED_AFTER);
        const { assert!(NOMINAL_MAIN_BPS > NOMINAL_MIDDLE_BPS) };
        const { assert!(NOMINAL_MIDDLE_BPS > 0) };
        assert!(RATE_MEMORY > RATE_SETTLE);
    }
}
