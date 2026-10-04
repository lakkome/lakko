// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Which of a remote camera's two pictures a tile shows.
//!
//! `docs/ux/group-call-grid.md` section 7.5 and ADR-0067 step 5. A publisher's
//! uplink that loses most of its packets for about a second cannot be
//! repaired inside a 720p stream in that second: the picture stands for about
//! a second and then plays the late second fast. The light layer (a quarter
//! of the size, 150 kbit/s) is repaired in a fraction of that. So a viewer
//! receives both as two tracks, decodes both all the time, and the tile shows
//! the light one the moment the full one stands still.
//!
//! This module is the rule and its counters, written once because web had it
//! in TypeScript and iOS and Android were about to write it twice more. Pure:
//! no screen, no timers, and the caller supplies every timestamp (one clock,
//! whole milliseconds, any origin). One [`LayerDisplay`] per remote tile; the
//! tile reports each presented frame of each track and a low-frequency tick
//! ([`LAYER_TICK_MS`]), and reads back what to show.
//!
//! The rules:
//!
//! - [`ShownLayer::Full`] is shown by default, and always while there is no
//!   light track.
//! - `Full` -> `Light` when the full track has presented no frame for
//!   [`FULL_STALL_MS`] and the light track presented one within
//!   [`LIGHT_FRESH_MS`].
//! - `Light` -> `Full` only once the full track has presented frames with no
//!   gap longer than [`FULL_STALL_MS`] for [`FULL_STEADY_MS`]. A gap inside
//!   that window starts it again. The window also hides the fast catch-up
//!   playback that follows a stall.
//! - Before the full track's first frame, `Light` is shown only when the full
//!   track has presented nothing for [`FIRST_FRAME_GRACE_MS`] while the light
//!   one plays, so a slow first key frame on the full layer does not leave
//!   the tile empty.
//! - When both stand still, whatever is shown stays shown.
//!
//! A **displayed stall** is the shown track presenting no frame for more than
//! [`FULL_STALL_MS`]. Its length is the whole time the picture stood: from
//! the shown track's last presented frame (or from the moment that track
//! became the shown one, when that is later) until its next frame or until it
//! stops being shown. A stall the switch to `Light` cuts short is counted at
//! the length it had when the switch happened: with the tile's tick that is
//! between [`FULL_STALL_MS`] and that plus one tick, and it is what the
//! viewer saw.
//!
//! Everything is integer arithmetic on `u64` milliseconds, saturating, so
//! wasm, iOS and Android cannot disagree and no input can panic. Time never
//! runs backwards: a timestamp earlier than the latest one seen is read as
//! the latest one seen.

/// The full track is standing still once it has presented no frame for this
/// long, milliseconds. Chosen, not measured: about four frame intervals at
/// 30 frames a second and two at 15, so ordinary jitter does not trip it.
pub const FULL_STALL_MS: u64 = 150;

/// The light track is playing if it presented a frame within this,
/// milliseconds. Chosen, not measured: longer than [`FULL_STALL_MS`] so a
/// light track at a low frame rate still counts as playing when the full one
/// is found standing.
pub const LIGHT_FRESH_MS: u64 = 250;

/// How long the full track must present frames without a gap longer than
/// [`FULL_STALL_MS`] before the tile returns to it, milliseconds. Chosen, not
/// measured: the late second a recovered stream plays fast was about a second
/// long on the calls that prompted this.
pub const FULL_STEADY_MS: u64 = 1000;

/// How long a full track that has never presented a frame is waited for
/// before a playing light track is shown instead, milliseconds. Chosen, not
/// measured.
pub const FIRST_FRAME_GRACE_MS: u64 = 1500;

/// How often a tile calls [`LayerDisplay::tick`], milliseconds, so a track
/// that stands still is noticed without a frame arriving. Chosen, not
/// measured: a third of [`FULL_STALL_MS`].
pub const LAYER_TICK_MS: u64 = 50;

/// Which track a tile shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShownLayer {
    /// The person's main picture.
    Full,
    /// The light companion.
    Light,
}

/// What a [`LayerDisplay`] counted; milliseconds where it says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LayerDisplayCounters {
    /// Times the tile switched to the light track.
    pub light_switches: u64,
    /// Total time the light track was the shown one, milliseconds.
    pub light_ms: u64,
    /// The longest displayed stall, milliseconds.
    pub stall_max_ms: u64,
    /// Total displayed stall time, milliseconds.
    pub stall_total_ms: u64,
}

impl LayerDisplayCounters {
    /// Counters with nothing counted.
    pub const NONE: Self = Self {
        light_switches: 0,
        light_ms: 0,
        stall_max_ms: 0,
        stall_total_ms: 0,
    };

    /// Two sets of counters as one: sums, and the longer of the longest
    /// stalls.
    #[must_use]
    pub const fn plus(self, other: Self) -> Self {
        Self {
            light_switches: self.light_switches.saturating_add(other.light_switches),
            light_ms: self.light_ms.saturating_add(other.light_ms),
            stall_max_ms: if self.stall_max_ms > other.stall_max_ms {
                self.stall_max_ms
            } else {
                other.stall_max_ms
            },
            stall_total_ms: self.stall_total_ms.saturating_add(other.stall_total_ms),
        }
    }
}

/// One remote tile's choice between its two tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerDisplay {
    current: ShownLayer,
    /// When the shown track became the shown one.
    shown_since: u64,
    /// The grace for the full track's first frame runs from here.
    started_at: u64,
    /// The last time the clock was advanced; time never runs backwards.
    last_eval: u64,
    light_available: bool,
    last_full: Option<u64>,
    last_light: Option<u64>,
    /// Start of the full track's current run of frames with no gap longer
    /// than [`FULL_STALL_MS`]; `None` while it has none.
    steady_since: Option<u64>,
    /// How much of the displayed stall in progress is already counted.
    stall_counted: u64,
    total: LayerDisplayCounters,
    interval: LayerDisplayCounters,
}

impl LayerDisplay {
    /// A tile that started showing this peer at `now_ms`; `light_available`
    /// is whether the peer has a light track yet.
    #[must_use]
    pub const fn new(now_ms: u64, light_available: bool) -> Self {
        Self {
            current: ShownLayer::Full,
            shown_since: now_ms,
            started_at: now_ms,
            last_eval: now_ms,
            light_available,
            last_full: None,
            last_light: None,
            steady_since: None,
            stall_counted: 0,
            total: LayerDisplayCounters::NONE,
            interval: LayerDisplayCounters::NONE,
        }
    }

    /// What the tile shows.
    #[must_use]
    pub const fn shown(&self) -> ShownLayer {
        self.current
    }

    /// The full track presented a frame at `now_ms`.
    pub fn full_frame(&mut self, now_ms: u64) -> ShownLayer {
        let now = self.advance(now_ms);
        if self.current == ShownLayer::Full {
            // Close the stall this frame ends, at its full length.
            self.count_stall(now);
            self.stall_counted = 0;
        }
        let follows_closely = self
            .last_full
            .is_some_and(|last| now.saturating_sub(last) <= FULL_STALL_MS);
        if self.steady_since.is_none() || !follows_closely {
            self.steady_since = Some(now);
        }
        self.last_full = Some(now);
        self.settle(now)
    }

    /// The light track presented a frame at `now_ms`.
    pub fn light_frame(&mut self, now_ms: u64) -> ShownLayer {
        let now = self.advance(now_ms);
        if self.current == ShownLayer::Light {
            self.count_stall(now);
            self.stall_counted = 0;
        }
        self.last_light = Some(now);
        self.settle(now)
    }

    /// Time has passed with no frame. Notices a stall without a frame
    /// arriving.
    pub fn tick(&mut self, now_ms: u64) -> ShownLayer {
        let now = self.advance(now_ms);
        self.settle(now)
    }

    /// The peer's light track appeared or went away. Without one the tile
    /// shows [`ShownLayer::Full`], whatever the full track is doing.
    pub fn set_light_available(&mut self, available: bool, now_ms: u64) -> ShownLayer {
        let now = self.advance(now_ms);
        self.light_available = available;
        if !available {
            self.last_light = None;
        }
        self.settle(now)
    }

    /// The tile could not observe frames between the last report and
    /// `now_ms` (a page or an app in the background presents none). Nothing
    /// is counted for that time, what was known about either track is
    /// forgotten, and the tile starts again on [`ShownLayer::Full`] as a new
    /// tile would. Not counted as a switch.
    pub fn resync(&mut self, now_ms: u64) -> ShownLayer {
        let now = now_ms.max(self.last_eval);
        self.last_eval = now;
        self.current = ShownLayer::Full;
        self.shown_since = now;
        self.started_at = now;
        self.last_full = None;
        self.last_light = None;
        self.steady_since = None;
        self.stall_counted = 0;
        self.current
    }

    /// Everything counted since the tile started.
    #[must_use]
    pub const fn counters(&self) -> LayerDisplayCounters {
        self.total
    }

    /// Everything counted since the last drain, and start a new interval. A
    /// displayed stall still in progress is reported in each interval it runs
    /// through: its time is split between them, and each interval's longest
    /// stall is the length it had reached by then.
    pub fn drain(&mut self) -> LayerDisplayCounters {
        let out = self.interval;
        self.interval = LayerDisplayCounters::NONE;
        out
    }

    /// Move the clock to `t` (never backwards) and count the time shown
    /// light.
    fn advance(&mut self, t: u64) -> u64 {
        let now = t.max(self.last_eval);
        let dt = now.saturating_sub(self.last_eval);
        if dt > 0 && self.current == ShownLayer::Light {
            self.count(LayerDisplayCounters {
                light_ms: dt,
                ..LayerDisplayCounters::NONE
            });
        }
        self.last_eval = now;
        now
    }

    /// Count the shown track's stall up to `now`, decide what is shown from
    /// `now` on, and count again in case the newly shown track is the one
    /// standing still.
    fn settle(&mut self, now: u64) -> ShownLayer {
        self.count_stall(now);
        self.decide(now);
        self.count_stall(now);
        self.current
    }

    fn decide(&mut self, now: u64) {
        if !self.light_available {
            if self.current == ShownLayer::Light {
                self.switch_to(ShownLayer::Full, now);
            }
            return;
        }
        let full_gap = self.last_full.map(|last| now.saturating_sub(last));
        if self.current == ShownLayer::Full {
            let light_fresh = self
                .last_light
                .is_some_and(|last| now.saturating_sub(last) <= LIGHT_FRESH_MS);
            if !light_fresh {
                return;
            }
            let standing = full_gap.map_or_else(
                || now.saturating_sub(self.started_at) >= FIRST_FRAME_GRACE_MS,
                |gap| gap >= FULL_STALL_MS,
            );
            if standing {
                // The run of steady full frames is over; the next full frame
                // starts a new one.
                self.steady_since = None;
                self.switch_to(ShownLayer::Light, now);
            }
            return;
        }
        // Shown: light.
        if !full_gap.is_some_and(|gap| gap <= FULL_STALL_MS) {
            // A gap inside the steady window starts it again.
            self.steady_since = None;
            return;
        }
        if self
            .steady_since
            .is_some_and(|since| now.saturating_sub(since) >= FULL_STEADY_MS)
        {
            self.switch_to(ShownLayer::Full, now);
        }
    }

    fn switch_to(&mut self, next: ShownLayer, now: u64) {
        self.current = next;
        self.shown_since = now;
        self.stall_counted = 0;
        if next == ShownLayer::Light {
            self.count(LayerDisplayCounters {
                light_switches: 1,
                ..LayerDisplayCounters::NONE
            });
        }
    }

    /// Count the displayed stall of the shown track up to `now`.
    fn count_stall(&mut self, now: u64) {
        let last = match self.current {
            ShownLayer::Full => self.last_full,
            ShownLayer::Light => self.last_light,
        };
        let Some(last) = last else { return };
        if now.saturating_sub(last) <= FULL_STALL_MS {
            return;
        }
        let length = now.saturating_sub(last.max(self.shown_since));
        self.count(LayerDisplayCounters {
            stall_total_ms: length.saturating_sub(self.stall_counted),
            stall_max_ms: length,
            ..LayerDisplayCounters::NONE
        });
        self.stall_counted = self.stall_counted.max(length);
    }

    fn count(&mut self, add: LayerDisplayCounters) {
        self.total = self.total.plus(add);
        self.interval = self.interval.plus(add);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use ShownLayer::{Full, Light};

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Which {
        FullOnly,
        LightOnly,
        Both,
    }

    /// Frames of one track every `step` ms over `[from, to]`.
    fn play(d: &mut LayerDisplay, which: Which, from: u64, to: u64, step: u64) {
        let mut t = from;
        while t <= to {
            if which != Which::LightOnly {
                d.full_frame(t);
            }
            if which != Which::FullOnly {
                d.light_frame(t);
            }
            t += step;
        }
    }

    /// A tile whose two tracks have both played up to `t = 1000`.
    fn playing() -> LayerDisplay {
        let mut d = LayerDisplay::new(0, true);
        play(&mut d, Which::Both, 0, 1000, 20);
        d
    }

    const fn c(
        light_switches: u64,
        light_ms: u64,
        stall_max_ms: u64,
        stall_total_ms: u64,
    ) -> LayerDisplayCounters {
        LayerDisplayCounters {
            light_switches,
            light_ms,
            stall_max_ms,
            stall_total_ms,
        }
    }

    #[test]
    fn the_constants_are_the_chosen_values() {
        assert_eq!(FULL_STALL_MS, 150);
        assert_eq!(LIGHT_FRESH_MS, 250);
        assert_eq!(FULL_STEADY_MS, 1000);
        assert_eq!(FIRST_FRAME_GRACE_MS, 1500);
        assert_eq!(LAYER_TICK_MS, 50);
    }

    // ----- what is shown ---------------------------------------------------

    #[test]
    fn shows_full_by_default() {
        let mut d = LayerDisplay::new(0, true);
        assert_eq!(d.shown(), Full);
        assert_eq!(d.tick(50), Full);
    }

    #[test]
    fn stays_on_full_while_both_tracks_play() {
        let d = playing();
        assert_eq!(d.shown(), Full);
        assert_eq!(d.counters(), LayerDisplayCounters::NONE);
    }

    #[test]
    fn switches_to_light_when_full_stalls_while_light_is_fresh() {
        let mut d = playing();
        // Full stops at 1000; light keeps playing.
        play(&mut d, Which::LightOnly, 1020, 1140, 20);
        assert_eq!(d.tick(1149), Full);
        assert_eq!(d.tick(1150), Light);
        assert_eq!(d.counters().light_switches, 1);
    }

    #[test]
    fn switches_on_a_light_frame_as_well_as_on_a_tick() {
        let mut d = playing();
        play(&mut d, Which::LightOnly, 1020, 1140, 20);
        assert_eq!(d.light_frame(1160), Light);
    }

    #[test]
    fn does_not_switch_while_the_full_gap_is_under_the_threshold() {
        let mut d = playing();
        play(&mut d, Which::LightOnly, 1020, 1100, 20);
        assert_eq!(d.tick(1100), Full);
        // The full track resumes inside the threshold: nothing happened.
        assert_eq!(d.full_frame(1140), Full);
        assert_eq!(d.tick(1200), Full);
        assert_eq!(d.counters().light_switches, 0);
    }

    #[test]
    fn does_not_switch_to_a_light_track_that_is_not_fresh() {
        let mut d = playing();
        // Light's last frame was at 1000 as well; at 1251 it is 251 ms old.
        assert_eq!(d.tick(1250), Light);
        let mut e = playing();
        assert_eq!(e.tick(1251), Full);
        assert_eq!(e.tick(2000), Full);
        assert_eq!(e.counters().light_switches, 0);
    }

    #[test]
    fn stays_on_what_is_shown_when_both_stall_and_counts_a_displayed_stall() {
        let mut d = playing();
        // Both stop at 1000; first look is past the light track's freshness.
        for t in (1300..=2000).step_by(50) {
            assert_eq!(d.tick(t), Full);
        }
        assert_eq!(d.counters().light_switches, 0);
        assert_eq!(d.counters().stall_max_ms, 1000);
        assert_eq!(d.counters().stall_total_ms, 1000);

        // The same while light is the shown one.
        let mut e = playing();
        play(&mut e, Which::LightOnly, 1020, 1200, 20);
        assert_eq!(e.shown(), Light);
        for t in (1500..=2200).step_by(50) {
            assert_eq!(e.tick(t), Light);
        }
        assert_eq!(e.counters().light_switches, 1);
        // Light's last frame was at 1200.
        assert_eq!(e.counters().stall_max_ms, 1000);
    }

    #[test]
    fn returns_to_full_only_after_the_steady_window() {
        let mut d = playing();
        play(&mut d, Which::LightOnly, 1020, 1200, 20);
        assert_eq!(d.shown(), Light);
        // Full comes back at 2000 and plays without a gap; light keeps
        // playing.
        for t in (2000..3000).step_by(20) {
            d.light_frame(t);
            assert_eq!(d.full_frame(t), Light);
        }
        assert_eq!(d.tick(2999), Light);
        assert_eq!(d.full_frame(3000), Full);
        assert_eq!(d.counters().light_switches, 1);
    }

    #[test]
    fn returns_on_a_tick_too_once_the_window_has_passed() {
        let mut d = playing();
        play(&mut d, Which::LightOnly, 1020, 1200, 20);
        play(&mut d, Which::Both, 2000, 2980, 20);
        assert_eq!(d.shown(), Light);
        assert_eq!(d.tick(3000), Full);
    }

    #[test]
    fn starts_the_steady_window_again_after_a_gap_inside_it() {
        let mut d = playing();
        play(&mut d, Which::LightOnly, 1020, 1200, 20);
        // Full plays 2000..2500, then a gap of 200 ms, then plays on from
        // 2700.
        for t in (2000..=4000u64).step_by(20) {
            d.light_frame(t);
            if !(2501..2700).contains(&t) {
                d.full_frame(t);
            }
            // 1000 ms after the first return (3000) is not enough: the
            // window restarted at 2700 and ends at 3700.
            assert_eq!(d.shown(), if t < 3700 { Light } else { Full }, "t={t}");
        }
        assert_eq!(d.counters().light_switches, 1);
    }

    #[test]
    fn starts_the_steady_window_again_when_a_tick_sees_the_gap() {
        let mut d = playing();
        play(&mut d, Which::LightOnly, 1020, 1200, 20);
        play(&mut d, Which::FullOnly, 2000, 2500, 20);
        assert_eq!(d.tick(2700), Light);
        play(&mut d, Which::FullOnly, 2700, 3680, 20);
        assert_eq!(d.shown(), Light);
        assert_eq!(d.full_frame(3700), Full);
    }

    #[test]
    fn a_gap_of_exactly_the_threshold_does_not_restart_the_window() {
        let mut d = playing();
        play(&mut d, Which::LightOnly, 1020, 1200, 20);
        d.full_frame(2000);
        d.full_frame(2150);
        play(&mut d, Which::FullOnly, 2200, 2980, 20);
        assert_eq!(d.full_frame(3000), Full);
    }

    #[test]
    fn always_shows_full_without_a_light_track() {
        let mut d = LayerDisplay::new(0, false);
        play(&mut d, Which::FullOnly, 0, 1000, 20);
        for t in (1050..=5000).step_by(50) {
            assert_eq!(d.tick(t), Full);
        }
        assert_eq!(d.counters().light_switches, 0);
        assert_eq!(d.counters().light_ms, 0);
        // Light frames reported for a track that is not there change
        // nothing.
        assert_eq!(d.light_frame(5010), Full);
    }

    #[test]
    fn goes_back_to_full_at_once_when_the_light_track_goes_away() {
        let mut d = playing();
        play(&mut d, Which::LightOnly, 1020, 1200, 20);
        assert_eq!(d.shown(), Light);
        assert_eq!(d.set_light_available(false, 1210), Full);
        assert_eq!(d.counters().light_switches, 1);
        // A light track that comes back is not fresh until it presents a
        // frame.
        assert_eq!(d.set_light_available(true, 1220), Full);
        assert_eq!(d.light_frame(1230), Light);
        assert_eq!(d.counters().light_switches, 2);
    }

    // ----- before the full track's first frame -----------------------------

    #[test]
    fn does_not_show_light_during_the_grace() {
        let mut d = LayerDisplay::new(0, true);
        for t in (0..FIRST_FRAME_GRACE_MS).step_by(20) {
            assert_eq!(d.light_frame(t), Full);
        }
        assert_eq!(d.tick(FIRST_FRAME_GRACE_MS - 1), Full);
    }

    #[test]
    fn shows_a_playing_light_track_once_the_grace_has_passed() {
        let mut d = LayerDisplay::new(0, true);
        play(&mut d, Which::LightOnly, 0, 1480, 20);
        assert_eq!(d.shown(), Full);
        assert_eq!(d.light_frame(1500), Light);
        assert_eq!(d.counters().light_switches, 1);
        // The full track's first frame starts the steady window.
        play(&mut d, Which::Both, 2000, 2980, 20);
        assert_eq!(d.shown(), Light);
        assert_eq!(d.full_frame(3000), Full);
    }

    #[test]
    fn does_not_show_a_light_track_that_is_not_playing() {
        let mut d = LayerDisplay::new(0, true);
        d.light_frame(100);
        assert_eq!(d.tick(1500), Full);
        assert_eq!(d.tick(3000), Full);
    }

    #[test]
    fn counts_no_stall_while_nothing_has_been_presented() {
        let mut d = LayerDisplay::new(0, true);
        for t in (50..=3000).step_by(50) {
            d.tick(t);
        }
        assert_eq!(d.counters(), LayerDisplayCounters::NONE);
    }

    #[test]
    fn the_full_tracks_first_frame_inside_the_grace_keeps_full() {
        let mut d = LayerDisplay::new(0, true);
        play(&mut d, Which::LightOnly, 0, 1000, 20);
        play(&mut d, Which::Both, 1020, 3000, 20);
        assert_eq!(d.shown(), Full);
        assert_eq!(d.counters().light_switches, 0);
    }

    // ----- counters --------------------------------------------------------

    #[test]
    fn counts_the_time_light_was_shown() {
        let mut d = playing();
        play(&mut d, Which::LightOnly, 1020, 1140, 20);
        assert_eq!(d.tick(1150), Light);
        play(&mut d, Which::LightOnly, 1160, 1980, 20);
        // Full back from 2000, shown again at 3000.
        play(&mut d, Which::Both, 2000, 3000, 20);
        assert_eq!(d.shown(), Full);
        d.tick(3500);
        assert_eq!(d.counters().light_ms, 3000 - 1150);
        assert_eq!(d.counters().light_switches, 1);
    }

    #[test]
    fn counts_a_stall_the_switch_cut_short_at_the_length_it_had() {
        // First look after the full track stopped is at a gap of 180 ms.
        let mut d = playing();
        play(&mut d, Which::LightOnly, 1020, 1140, 20);
        assert_eq!(d.tick(1180), Light);
        assert_eq!(d.counters().stall_max_ms, 180);
        assert_eq!(d.counters().stall_total_ms, 180);
        // A look at exactly the threshold switches before any stall is
        // counted.
        let mut e = playing();
        play(&mut e, Which::LightOnly, 1020, 1140, 20);
        assert_eq!(e.tick(1150), Light);
        assert_eq!(e.counters().stall_total_ms, 0);
    }

    #[test]
    fn counts_a_displayed_stall_of_the_full_track_once_at_its_whole_length() {
        let mut d = LayerDisplay::new(0, false);
        play(&mut d, Which::FullOnly, 0, 1000, 20);
        for t in (1050..=1950).step_by(50) {
            d.tick(t);
        }
        d.full_frame(1987);
        play(&mut d, Which::FullOnly, 2000, 3000, 20);
        assert_eq!(d.counters().stall_max_ms, 987);
        assert_eq!(d.counters().stall_total_ms, 987);
    }

    #[test]
    fn adds_up_several_stalls_and_keeps_the_longest() {
        let mut d = LayerDisplay::new(0, false);
        play(&mut d, Which::FullOnly, 0, 1000, 20);
        d.full_frame(1400);
        play(&mut d, Which::FullOnly, 1420, 2000, 20);
        d.full_frame(2900);
        d.full_frame(3000);
        assert_eq!(d.counters().stall_max_ms, 900);
        assert_eq!(d.counters().stall_total_ms, 400 + 900);
    }

    #[test]
    fn does_not_count_a_gap_of_exactly_the_threshold() {
        let mut d = LayerDisplay::new(0, false);
        d.full_frame(0);
        d.full_frame(150);
        assert_eq!(d.counters().stall_total_ms, 0);
        d.full_frame(301);
        assert_eq!(d.counters().stall_total_ms, 151);
    }

    #[test]
    fn measures_a_light_stall_from_when_light_became_the_shown_one() {
        let mut d = playing();
        // Light's last frame is at 1100; the switch happens at 1300 (gap
        // 200, still fresh), and light then stands still as well.
        play(&mut d, Which::LightOnly, 1020, 1100, 20);
        assert_eq!(d.tick(1300), Light);
        let after_switch = d.counters().stall_total_ms;
        assert_eq!(after_switch, 300); // the full track's stall, 1000..1300
        d.tick(1800);
        // Light stood on screen from 1300, not from its last frame at 1100.
        assert_eq!(d.counters().stall_total_ms, after_switch + 500);
        assert_eq!(d.counters().stall_max_ms, 500);
    }

    #[test]
    fn drains_an_interval_and_keeps_the_totals() {
        let mut d = LayerDisplay::new(0, false);
        play(&mut d, Which::FullOnly, 0, 1000, 20);
        d.tick(1500);
        assert_eq!(d.drain(), c(0, 0, 500, 500));
        // The stall runs on into the next interval: its time is split, and
        // the interval's longest stall is the length it has reached.
        d.tick(1800);
        assert_eq!(d.drain(), c(0, 0, 800, 300));
        assert_eq!(d.drain(), LayerDisplayCounters::NONE);
        assert_eq!(d.counters(), c(0, 0, 800, 800));
    }

    #[test]
    fn never_lets_time_run_backwards() {
        let mut d = playing();
        play(&mut d, Which::LightOnly, 1020, 1200, 20);
        assert_eq!(d.shown(), Light);
        let before = d.counters();
        d.tick(900);
        d.full_frame(800);
        assert_eq!(d.counters().light_ms, before.light_ms);
        assert_eq!(d.counters().stall_total_ms, before.stall_total_ms);
    }

    // ----- a page or an app that was in the background ---------------------

    #[test]
    fn counts_nothing_for_the_hidden_time_and_starts_again_on_full() {
        let mut d = playing();
        play(&mut d, Which::LightOnly, 1020, 1200, 20);
        assert_eq!(d.shown(), Light);
        let before = d.counters();
        assert_eq!(d.resync(60_000), Full);
        assert_eq!(d.counters(), before);
        // A new grace runs from the return.
        play(&mut d, Which::LightOnly, 60_000, 61_480, 20);
        assert_eq!(d.shown(), Full);
        assert_eq!(d.light_frame(61_500), Light);
    }

    #[test]
    fn adds_counters_sums_and_the_longer_of_the_longest_stalls() {
        assert_eq!(
            c(1, 100, 300, 400).plus(c(2, 50, 200, 250)),
            c(3, 150, 300, 650)
        );
        assert_eq!(
            c(u64::MAX, u64::MAX, 1, u64::MAX).plus(c(1, 1, 2, 1)),
            c(u64::MAX, u64::MAX, 2, u64::MAX)
        );
    }

    // ----- arbitrary event sequences ---------------------------------------

    #[derive(Debug, Clone, Copy)]
    enum Event {
        Full(u64),
        Light(u64),
        Tick(u64),
        Available(bool, u64),
        Resync(u64),
        Drain,
    }

    /// A timestamp: mostly near the previous ones, sometimes anywhere in the
    /// whole range, so both ordinary playback and the extremes are reached.
    fn time() -> impl Strategy<Value = u64> {
        prop_oneof![
            8 => 0u64..20_000,
            1 => any::<u64>(),
            1 => (u64::MAX - 5_000)..=u64::MAX,
        ]
    }

    fn event() -> impl Strategy<Value = Event> {
        prop_oneof![
            6 => time().prop_map(Event::Full),
            6 => time().prop_map(Event::Light),
            4 => time().prop_map(Event::Tick),
            1 => (any::<bool>(), time()).prop_map(|(a, t)| Event::Available(a, t)),
            1 => time().prop_map(Event::Resync),
            1 => Just(Event::Drain),
        ]
    }

    /// Steps of playback time: events in increasing time, the shape a real
    /// tile produces.
    fn ordered_events() -> impl Strategy<Value = Vec<Event>> {
        proptest::collection::vec((0u8..6, 0u64..400, any::<bool>()), 0..300).prop_map(|steps| {
            let mut t = 0u64;
            steps
                .into_iter()
                .map(|(kind, dt, flag)| {
                    t = t.saturating_add(dt);
                    match kind {
                        0 | 1 => Event::Full(t),
                        2 | 3 => Event::Light(t),
                        4 => Event::Tick(t),
                        _ if flag => Event::Available(dt % 2 == 0, t),
                        _ => Event::Drain,
                    }
                })
                .collect()
        })
    }

    /// Apply `events`, checking after each one what must always hold.
    fn run(start: u64, light_available: bool, events: &[Event]) -> Result<(), TestCaseError> {
        let mut d = LayerDisplay::new(start, light_available);
        let mut available = light_available;
        let mut previous = d.counters();
        let mut drained = LayerDisplayCounters::NONE;
        for event in events {
            let returned = match *event {
                Event::Full(t) => d.full_frame(t),
                Event::Light(t) => d.light_frame(t),
                Event::Tick(t) => d.tick(t),
                Event::Available(a, t) => {
                    available = a;
                    d.set_light_available(a, t)
                },
                Event::Resync(t) => {
                    let shown = d.resync(t);
                    prop_assert_eq!(shown, Full);
                    shown
                },
                Event::Drain => {
                    let part = d.drain();
                    drained = drained.plus(part);
                    d.shown()
                },
            };
            prop_assert_eq!(returned, d.shown());
            // Never light without a light track.
            prop_assert!(available || d.shown() == Full);
            // The totals never go down.
            let now = d.counters();
            prop_assert!(now.light_switches >= previous.light_switches);
            prop_assert!(now.light_ms >= previous.light_ms);
            prop_assert!(now.stall_max_ms >= previous.stall_max_ms);
            prop_assert!(now.stall_total_ms >= previous.stall_total_ms);
            // The longest stall is part of the total stall time.
            prop_assert!(now.stall_max_ms <= now.stall_total_ms);
            previous = now;
        }
        // The intervals add up to the totals.
        let rest = d.drain();
        prop_assert_eq!(drained.plus(rest), d.counters());
        Ok(())
    }

    proptest! {
        #[test]
        fn arbitrary_events_never_panic_and_keep_the_invariants(
            start in time(),
            light_available in any::<bool>(),
            events in proptest::collection::vec(event(), 0..200),
        ) {
            run(start, light_available, &events)?;
        }

        #[test]
        fn ordered_playback_keeps_the_invariants(
            light_available in any::<bool>(),
            events in ordered_events(),
        ) {
            run(0, light_available, &events)?;
        }

        #[test]
        fn a_tile_without_a_light_track_never_counts_light(
            events in proptest::collection::vec(
                prop_oneof![
                    time().prop_map(Event::Full),
                    time().prop_map(Event::Light),
                    time().prop_map(Event::Tick),
                    time().prop_map(Event::Resync),
                ],
                0..200,
            ),
        ) {
            let mut d = LayerDisplay::new(0, false);
            for event in &events {
                let shown = match *event {
                    Event::Full(t) => d.full_frame(t),
                    Event::Light(t) => d.light_frame(t),
                    Event::Tick(t) => d.tick(t),
                    Event::Resync(t) => d.resync(t),
                    Event::Available(..) | Event::Drain => d.shown(),
                };
                prop_assert_eq!(shown, Full);
            }
            prop_assert_eq!(d.counters().light_switches, 0);
            prop_assert_eq!(d.counters().light_ms, 0);
        }
    }
}
