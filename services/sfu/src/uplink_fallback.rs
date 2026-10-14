// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fall back to a publisher's light simulcast layer while its uplink is in
//! trouble.
//!
//! ## Why
//!
//! Measured on live calls and reproduced by `measure_uplink_dip`: a
//! publisher's uplink that loses most of its datagrams for about a second
//! cannot repair a stream of some 250 packets a second inside that second,
//! however often the SFU asks for the missing packets; the same dip repairs
//! a stream of 25 to 50 packets a second in a few hundred milliseconds. A
//! publisher that sends two simulcast layers therefore has one stream that
//! survives the dip and one that does not, and this module decides when the
//! subscribers of the track are moved onto the one that does.
//!
//! ## What it is
//!
//! One [`UplinkFallback`] per publisher video track that sends simulcast
//! layers. It is fed every packet the SFU receives on each layer (layer,
//! extended sequence number, arrival time) and keeps, per layer, the
//! **holes**: sequence numbers that were skipped and have not arrived, each
//! with the moment it was first seen. A layer is **in trouble** while it has
//! a hole older than [`HOLE_AGE`].
//!
//! Two states:
//!
//! - [`State::Full`]: subscribers may be on any layer. It ends when a layer
//!   other than the light one is in trouble.
//! - [`State::Fallback`]: subscribers may be on the light layer only. It
//!   ends when no other layer has been in trouble for [`RESTORE_AFTER`] and
//!   at least [`MIN_FALLBACK`] has passed since it began.
//!
//! Holes on the light layer change nothing: there is no lighter layer to
//! move to.
//!
//! A track that has shown one layer only never leaves [`State::Full`] and
//! never reports a transition, so a publisher without simulcast is not
//! affected by anything here.
//!
//! ## What it is not
//!
//! Pure bookkeeping over header fields the SFU already reads: no I/O, no
//! clock of its own (the caller supplies every time), no payload, no key.
//! Which layer is the light one is the caller's to say ([`pick_light`] is
//! the rule it uses), and acting on a transition (selecting layers, asking
//! for a keyframe) is the caller's too.
//!
//! ## Bounds
//!
//! At most [`MAX_LAYERS`] layers and [`MAX_HOLES`] holes per layer. A jump of
//! more than [`MAX_JUMP`] sequence numbers in either direction is a stream
//! that started over, not loss: the layer's holes are dropped and counting
//! starts at the new number. A hole is forgotten after [`HOLE_FORGET`]
//! whether or not it was filled.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How old a hole has to be for its layer to be in trouble.
///
/// Chosen, not measured. It has to be longer than one ordinary repair (the
/// SFU asks, the publisher resends: a round trip plus the SFU's own wait
/// before asking), or every single lost packet would move the subscribers;
/// and as short as possible beyond that, since the picture is standing still
/// while it runs.
pub const HOLE_AGE: Duration = Duration::from_millis(150);

/// How long the heavier layers must have been free of trouble before the
/// subscribers are moved back.
///
/// Chosen, not measured. Each move back costs the publisher a keyframe on
/// the full layer, so it should happen once the dip is over, not between
/// two bursts of it.
pub const RESTORE_AFTER: Duration = Duration::from_secs(3);

/// The least time a fallback lasts.
///
/// Chosen, not measured. It keeps a single late packet from producing two
/// layer switches within one second.
pub const MIN_FALLBACK: Duration = Duration::from_secs(2);

/// How long a hole is remembered. Past this it is no longer waited for: a
/// packet that has not come by then will not be shown.
///
/// Chosen, not measured; longer than the SFU keeps asking for a missing
/// packet (about 1.4 s in `measure_uplink_dip`).
pub const HOLE_FORGET: Duration = Duration::from_secs(2);

/// Most layers tracked for one track. A layer beyond that is ignored.
///
/// Chosen: a libwebrtc sender produces at most three simulcast layers.
pub const MAX_LAYERS: usize = 4;

/// Most holes kept for one layer. When more are open, the oldest are
/// dropped.
///
/// Chosen: about four seconds of a 250 packet a second stream, twice
/// [`HOLE_FORGET`].
pub const MAX_HOLES: usize = 1024;

/// The largest step between two sequence numbers that is read as loss or
/// reordering. A larger one, forward or backward, is a stream that started
/// over.
///
/// Chosen: equal to [`MAX_HOLES`], so one step can never open more holes
/// than a layer keeps.
pub const MAX_JUMP: u64 = 1024;

/// Where the subscribers of a track may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State<L> {
    /// On any layer.
    Full,
    /// On the light layer only.
    Fallback {
        /// The light layer, fixed for as long as the fallback lasts.
        light: L,
        /// When the fallback began.
        since: Instant,
    },
}

/// Which way a transition went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// [`State::Full`] to [`State::Fallback`].
    ToFallback,
    /// [`State::Fallback`] to [`State::Full`].
    ToFull,
}

impl Direction {
    /// Label of the state left, for a log line.
    #[must_use]
    pub const fn from_str(self) -> &'static str {
        match self {
            Self::ToFallback => "full",
            Self::ToFull => "fallback",
        }
    }

    /// Label of the state entered, for a log line and the `direction`
    /// label of the counter.
    #[must_use]
    pub const fn to_str(self) -> &'static str {
        match self {
            Self::ToFallback => "fallback",
            Self::ToFull => "full",
        }
    }
}

/// Why a transition happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// A layer other than the light one has a hole older than [`HOLE_AGE`].
    HoleOlderThanLimit,
    /// No such layer has been in trouble for [`RESTORE_AFTER`], and the
    /// fallback has lasted [`MIN_FALLBACK`].
    UplinkClean,
}

impl Reason {
    /// Stable label for a log line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HoleOlderThanLimit => "hole-older-than-limit",
            Self::UplinkClean => "uplink-clean",
        }
    }
}

/// One change of state, as [`UplinkFallback::poll`] reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition<L> {
    /// Which way.
    pub direction: Direction,
    /// The light layer of the fallback entered or left.
    pub light: L,
    /// Why.
    pub reason: Reason,
    /// Age of the oldest hole on a layer other than the light one at the
    /// moment of the transition. `None` when there is none.
    pub oldest_hole: Option<Duration>,
}

/// The holes of one layer.
#[derive(Debug, Clone)]
struct LayerHoles<L> {
    layer: L,
    /// Highest sequence number seen. `None` before the first packet.
    highest: Option<u64>,
    /// Missing sequence number to the moment it was first known missing.
    /// Those moments never decrease with the sequence number (see
    /// `newest_seen`), so the first entry is always the oldest hole.
    holes: BTreeMap<u64, Instant>,
    /// The latest moment a hole was opened at; a later hole is never
    /// given an earlier one.
    newest_seen: Option<Instant>,
    /// A hole that was filled only after it had passed [`HOLE_AGE`], and
    /// has not been reported by a poll yet: when it was filled and how old
    /// it was. Without this a hole that grew old and closed between two
    /// polls would go unseen.
    late_fill: Option<(Instant, Duration)>,
}

impl<L: Copy + Eq> LayerHoles<L> {
    const fn new(layer: L) -> Self {
        Self {
            layer,
            highest: None,
            holes: BTreeMap::new(),
            newest_seen: None,
            late_fill: None,
        }
    }

    /// Start over at `seq`: what was missing on the stream before is no
    /// longer waited for.
    fn restart(&mut self, seq: u64) {
        self.holes.clear();
        self.highest = Some(seq);
    }

    fn on_packet(&mut self, seq: u64, arrival: Instant) {
        let Some(highest) = self.highest else {
            self.highest = Some(seq);
            return;
        };
        if seq > highest {
            let skipped = seq - highest - 1;
            if skipped >= MAX_JUMP {
                self.restart(seq);
                return;
            }
            // A hole is never dated before the one opened before it, even
            // if the caller's times went backwards.
            let seen = self.newest_seen.map_or(arrival, |n| n.max(arrival));
            if skipped > 0 {
                self.newest_seen = Some(seen);
            }
            for missing in (highest + 1)..seq {
                self.holes.insert(missing, seen);
            }
            while self.holes.len() > MAX_HOLES {
                self.holes.pop_first();
            }
            self.highest = Some(seq);
            return;
        }
        if highest - seq >= MAX_JUMP {
            self.restart(seq);
            return;
        }
        // At or behind the highest: a repair, a reordered packet or a
        // duplicate. Only the first two close a hole.
        if let Some(seen) = self.holes.remove(&seq) {
            let age = arrival.saturating_duration_since(seen);
            if age > HOLE_AGE && self.late_fill.map_or(true, |(_, a)| age > a) {
                self.late_fill = Some((arrival, age));
            }
        }
    }

    /// Forget holes past [`HOLE_FORGET`] and return the age of the oldest
    /// one left.
    fn oldest_hole(&mut self, now: Instant) -> Option<Duration> {
        loop {
            let (_, seen) = self.holes.first_key_value()?;
            let age = now.saturating_duration_since(*seen);
            if age < HOLE_FORGET {
                return Some(age);
            }
            self.holes.pop_first();
        }
    }
}

/// The fallback policy of one publisher video track. See the module
/// documentation.
#[derive(Debug, Clone)]
pub struct UplinkFallback<L> {
    layers: Vec<LayerHoles<L>>,
    state: State<L>,
    /// The last moment a layer other than the light one was in trouble.
    last_trouble: Option<Instant>,
}

impl<L: Copy + Eq> Default for UplinkFallback<L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L: Copy + Eq> UplinkFallback<L> {
    /// A track nothing has been received on yet, in [`State::Full`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            layers: Vec::new(),
            state: State::Full,
            last_trouble: None,
        }
    }

    /// One packet the SFU received on `layer`: its extended sequence number
    /// and when it arrived. Retransmissions and packets rebuilt from FEC
    /// count; they are what closes a hole.
    pub fn on_packet(&mut self, layer: L, seq: u64, arrival: Instant) {
        if let Some(holes) = self.layers.iter_mut().find(|l| l.layer == layer) {
            holes.on_packet(seq, arrival);
            return;
        }
        if self.layers.len() >= MAX_LAYERS {
            return;
        }
        let mut holes = LayerHoles::new(layer);
        holes.on_packet(seq, arrival);
        self.layers.push(holes);
    }

    /// The current state.
    #[must_use]
    pub const fn state(&self) -> State<L> {
        self.state
    }

    /// The only layer subscribers of this track may be on right now, or
    /// `None` when they may be on any.
    #[must_use]
    pub const fn restricted_to(&self) -> Option<L> {
        match self.state {
            State::Full => None,
            State::Fallback { light, .. } => Some(light),
        }
    }

    /// How many layers a packet has been seen on.
    #[must_use]
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Holes currently open across all layers. For tests and bounds.
    #[must_use]
    pub fn open_holes(&self) -> usize {
        self.layers.iter().map(|l| l.holes.len()).sum()
    }

    /// Move the policy to `now` and report a change of state, if there is
    /// one.
    ///
    /// `light` is the track's light layer as the caller sees it now, or
    /// `None` when it cannot tell (see [`pick_light`]); while in
    /// [`State::Fallback`] the layer the fallback began with is used and
    /// `light` is not consulted. Without a light layer, or with fewer than
    /// two layers seen, nothing ever changes.
    pub fn poll(&mut self, now: Instant, light: Option<L>) -> Option<Transition<L>> {
        let light = match self.state {
            State::Fallback { light, .. } => Some(light),
            State::Full => light,
        };
        // Every layer is aged, the light one included, so its holes are
        // forgotten on time too.
        let mut oldest: Option<Duration> = None;
        // A hole that grew old and was filled since the last poll: trouble
        // at the moment it was filled, not now.
        let mut filled_late: Option<Instant> = None;
        for holes in &mut self.layers {
            let age = holes.oldest_hole(now);
            let late = holes.late_fill.take();
            if Some(holes.layer) == light {
                continue;
            }
            if let Some(age) = age {
                oldest = Some(oldest.map_or(age, |o| o.max(age)));
            }
            if let Some((at, _)) = late {
                filled_late = Some(filled_late.map_or(at, |f| f.max(at)));
            }
            if matches!(self.state, State::Full) {
                // Reported as the age of the hole behind a transition.
                if let Some((_, late_age)) = late {
                    oldest = Some(oldest.map_or(late_age, |o| o.max(late_age)));
                }
            }
        }
        let light = light?;
        if self.layers.len() < 2 {
            return None;
        }
        let trouble = oldest.is_some_and(|age| age > HOLE_AGE);
        if let Some(at) = filled_late {
            self.last_trouble = Some(self.last_trouble.map_or(at, |l| l.max(at)));
        }
        if trouble {
            self.last_trouble = Some(now);
        }
        match self.state {
            State::Full => {
                if !trouble {
                    return None;
                }
                self.state = State::Fallback { light, since: now };
                Some(Transition {
                    direction: Direction::ToFallback,
                    light,
                    reason: Reason::HoleOlderThanLimit,
                    oldest_hole: oldest,
                })
            },
            State::Fallback { since, .. } => {
                let clean_for = self
                    .last_trouble
                    .map_or(Duration::MAX, |at| now.saturating_duration_since(at));
                if trouble
                    || clean_for < RESTORE_AFTER
                    || now.saturating_duration_since(since) < MIN_FALLBACK
                {
                    return None;
                }
                self.state = State::Full;
                Some(Transition {
                    direction: Direction::ToFull,
                    light,
                    reason: Reason::UplinkClean,
                    oldest_hole: oldest,
                })
            },
        }
    }
}

/// The light layer of a track, from its layers' measured bitrates in bits
/// per second (`0` for a layer not measured yet): the one with the lowest
/// measured bitrate when every layer is measured and one is strictly
/// lowest, otherwise `preferred` if it is among them, otherwise `None`.
/// Fewer than two layers have no light one.
#[must_use]
pub fn pick_light<L: Copy + Eq>(layers: &[(L, u64)], preferred: L) -> Option<L> {
    if layers.len() < 2 {
        return None;
    }
    let by_name = layers
        .iter()
        .find(|(l, _)| *l == preferred)
        .map(|(l, _)| *l);
    if layers.iter().any(|(_, bps)| *bps == 0) {
        return by_name;
    }
    let (lowest, lowest_bps) = layers.iter().min_by_key(|(_, bps)| *bps).copied()?;
    if layers.iter().filter(|(_, bps)| *bps == lowest_bps).count() > 1 {
        return by_name;
    }
    Some(lowest)
}

/// The heaviest layer of a track: the one with the highest measured bitrate
/// when every layer is measured (above zero) and it is strictly above every
/// other. `None` otherwise.
#[must_use]
pub fn pick_heaviest<L: Copy + Eq>(layers: &[(L, u64)]) -> Option<L> {
    let (top, top_bps) = layers.iter().max_by_key(|(_, bps)| *bps).copied()?;
    if layers.iter().any(|(_, bps)| *bps == 0)
        || layers.iter().filter(|(_, bps)| *bps == top_bps).count() > 1
    {
        return None;
    }
    Some(top)
}

#[cfg(test)]
mod tests {
    use super::*;

    const Q: u8 = b'q';
    const F: u8 = b'f';

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// One millisecond short of `d`.
    fn just_under(d: Duration) -> Duration {
        d.saturating_sub(ms(1))
    }

    /// Feed both layers a clean run of packets, one every 10 ms from
    /// `from` for `count` packets, starting at sequence number `seq`, and
    /// poll after each. Returns the transitions seen.
    fn clean(
        fb: &mut UplinkFallback<u8>,
        from: Instant,
        seq: u64,
        count: u64,
    ) -> Vec<Transition<u8>> {
        let mut seen = Vec::new();
        for i in 0..count {
            let at = from + ms(10 * i);
            fb.on_packet(Q, seq + i, at);
            fb.on_packet(F, seq + i, at);
            seen.extend(fb.poll(at, Some(Q)));
        }
        seen
    }

    #[test]
    fn no_trouble_stays_full() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        assert!(clean(&mut fb, t0, 100, 1000).is_empty());
        assert_eq!(fb.state(), State::Full);
        assert_eq!(fb.restricted_to(), None);
        assert_eq!(fb.open_holes(), 0);
    }

    #[test]
    fn a_hole_that_closes_within_the_limit_changes_nothing() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        clean(&mut fb, t0, 100, 10);
        // 110 is skipped on the full layer and comes 100 ms later.
        let t = t0 + ms(100);
        fb.on_packet(F, 111, t);
        fb.on_packet(Q, 110, t);
        assert_eq!(fb.poll(t, Some(Q)), None);
        assert_eq!(fb.open_holes(), 1);
        assert_eq!(fb.poll(t + ms(100), Some(Q)), None);
        fb.on_packet(F, 110, t + ms(100));
        assert_eq!(fb.open_holes(), 0);
        assert_eq!(fb.poll(t + ms(400), Some(Q)), None);
        assert_eq!(fb.state(), State::Full);
    }

    #[test]
    fn a_hole_older_than_the_limit_falls_back() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        clean(&mut fb, t0, 100, 10);
        let t = t0 + ms(100);
        fb.on_packet(F, 112, t);
        assert_eq!(fb.poll(t + HOLE_AGE, Some(Q)), None, "not older yet");
        let at = t + HOLE_AGE + ms(1);
        let tr = fb.poll(at, Some(Q)).expect("falls back");
        assert_eq!(tr.direction, Direction::ToFallback);
        assert_eq!(tr.light, Q);
        assert_eq!(tr.reason, Reason::HoleOlderThanLimit);
        assert_eq!(tr.oldest_hole, Some(HOLE_AGE + ms(1)));
        assert_eq!(
            fb.state(),
            State::Fallback {
                light: Q,
                since: at
            }
        );
        assert_eq!(fb.restricted_to(), Some(Q));
        // Reported once.
        assert_eq!(fb.poll(at + ms(1), Some(Q)), None);
    }

    #[test]
    fn a_hole_filled_late_between_two_polls_is_still_trouble() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        clean(&mut fb, t0, 100, 10);
        let t = t0 + ms(100);
        fb.on_packet(F, 111, t);
        // No poll while it is open; it is filled 300 ms later.
        fb.on_packet(F, 110, t + ms(300));
        let tr = fb.poll(t + ms(301), Some(Q)).expect("falls back");
        assert_eq!(tr.oldest_hole, Some(ms(300)));
    }

    /// Open one old hole on the full layer at `t` and return the moment
    /// the fallback began.
    fn fall_back(fb: &mut UplinkFallback<u8>, t: Instant, seq: u64) -> Instant {
        fb.on_packet(F, seq, t);
        fb.on_packet(F, seq + 2, t);
        let at = t + HOLE_AGE + ms(1);
        assert!(fb.poll(at, Some(Q)).is_some());
        at
    }

    #[test]
    fn restore_needs_both_timers() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        fb.on_packet(Q, 1, t0);
        let since = fall_back(&mut fb, t0, 10);
        // The hole is filled at once; the last trouble is `since`.
        fb.on_packet(F, 11, since);
        assert_eq!(fb.poll(since + MIN_FALLBACK, Some(Q)), None);
        assert_eq!(fb.poll(since + just_under(RESTORE_AFTER), Some(Q)), None);
        let tr = fb
            .poll(since + RESTORE_AFTER, Some(Q))
            .expect("back to full");
        assert_eq!(tr.direction, Direction::ToFull);
        assert_eq!(tr.reason, Reason::UplinkClean);
        assert_eq!(tr.light, Q);
        assert_eq!(fb.state(), State::Full);
    }

    #[test]
    fn the_least_fallback_time_holds_even_when_the_uplink_is_clean() {
        // With `RESTORE_AFTER` above `MIN_FALLBACK` the first always
        // decides; this pins that the second is checked on its own.
        assert!(RESTORE_AFTER >= MIN_FALLBACK);
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        fb.on_packet(Q, 1, t0);
        let since = fall_back(&mut fb, t0, 10);
        fb.on_packet(F, 11, since);
        assert_eq!(fb.poll(since + just_under(MIN_FALLBACK), Some(Q)), None);
        assert!(matches!(fb.state(), State::Fallback { .. }));
    }

    #[test]
    fn trouble_during_the_restore_wait_starts_it_again() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        fb.on_packet(Q, 1, t0);
        let since = fall_back(&mut fb, t0, 10);
        fb.on_packet(F, 11, since);
        // Two seconds in, another hole grows old.
        let again = since + ms(2000);
        fb.on_packet(F, 20, again);
        let trouble_at = again + HOLE_AGE + ms(1);
        assert_eq!(fb.poll(trouble_at, Some(Q)), None);
        fb.on_packet(F, 13, trouble_at);
        for missing in 14..20 {
            fb.on_packet(F, missing, trouble_at);
        }
        assert_eq!(fb.open_holes(), 0);
        // Three seconds after the first trouble is not enough any more.
        assert_eq!(fb.poll(since + RESTORE_AFTER + ms(1), Some(Q)), None);
        assert_eq!(
            fb.poll(trouble_at + just_under(RESTORE_AFTER), Some(Q)),
            None
        );
        assert!(fb.poll(trouble_at + RESTORE_AFTER, Some(Q)).is_some());
    }

    #[test]
    fn an_abandoned_hole_is_forgotten_and_the_track_restores() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        fb.on_packet(Q, 1, t0);
        let since = fall_back(&mut fb, t0, 10);
        // Never filled. It stops counting at HOLE_FORGET.
        let last_seen = t0 + just_under(HOLE_FORGET);
        let forgotten = t0 + HOLE_FORGET;
        assert_eq!(fb.poll(last_seen, Some(Q)), None);
        assert_eq!(fb.open_holes(), 1);
        assert_eq!(fb.poll(forgotten, Some(Q)), None);
        assert_eq!(fb.open_holes(), 0);
        assert!(since < forgotten);
        assert_eq!(
            fb.poll(last_seen + just_under(RESTORE_AFTER), Some(Q)),
            None
        );
        assert!(fb.poll(last_seen + RESTORE_AFTER, Some(Q)).is_some());
    }

    #[test]
    fn holes_on_the_light_layer_alone_change_nothing() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        clean(&mut fb, t0, 100, 10);
        let t = t0 + ms(100);
        fb.on_packet(Q, 150, t);
        fb.on_packet(F, 110, t);
        for step in 1..40 {
            assert_eq!(fb.poll(t + ms(100 * step), Some(Q)), None);
        }
        assert_eq!(fb.state(), State::Full);
        assert_eq!(fb.open_holes(), 0, "and they are forgotten on time");
    }

    #[test]
    fn a_single_layer_never_falls_back() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        fb.on_packet(F, 1, t0);
        fb.on_packet(F, 50, t0);
        assert_eq!(fb.layer_count(), 1);
        for step in 0..30 {
            assert_eq!(fb.poll(t0 + ms(100 * step), Some(Q)), None);
            assert_eq!(fb.poll(t0 + ms(100 * step), None), None);
        }
        assert_eq!(fb.state(), State::Full);
    }

    #[test]
    fn without_a_light_layer_nothing_changes() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        fb.on_packet(Q, 1, t0);
        fb.on_packet(F, 1, t0);
        fb.on_packet(F, 9, t0);
        assert_eq!(fb.poll(t0 + ms(500), None), None);
        assert_eq!(fb.state(), State::Full);
    }

    #[test]
    fn the_light_layer_is_fixed_while_the_fallback_lasts() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        fb.on_packet(Q, 1, t0);
        let since = fall_back(&mut fb, t0, 10);
        // The caller now names the other layer (its measured rate fell
        // under the light one's during the dip). The state does not move
        // and the holes of the full layer still count.
        assert_eq!(fb.poll(since + ms(10), Some(F)), None);
        assert_eq!(fb.restricted_to(), Some(Q));
    }

    #[test]
    fn duplicates_and_reordering_open_no_hole() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        fb.on_packet(Q, 1, t0);
        for seq in [10_u64, 11, 11, 10, 13, 12, 12, 13, 9, 14] {
            fb.on_packet(F, seq, t0);
        }
        assert_eq!(fb.open_holes(), 0);
        assert_eq!(fb.poll(t0 + ms(1000), Some(Q)), None);
    }

    #[test]
    fn a_stream_restart_drops_the_old_holes() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        fb.on_packet(Q, 1, t0);
        fb.on_packet(F, 60_000, t0);
        fb.on_packet(F, 60_010, t0);
        assert_eq!(fb.open_holes(), 9);
        // The publisher starts the layer over at a low number...
        fb.on_packet(F, 5, t0 + ms(10));
        assert_eq!(fb.open_holes(), 0);
        // ...and a far jump forward is a restart as well, not a million
        // holes.
        fb.on_packet(F, 5 + 1_000_000, t0 + ms(20));
        assert_eq!(fb.open_holes(), 0);
        assert_eq!(fb.poll(t0 + ms(1000), Some(Q)), None);
        assert_eq!(fb.state(), State::Full);
    }

    #[test]
    fn absurd_jumps_do_not_panic_or_grow() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        let mut x: u64 = 0x1234_5678_9abc_def0;
        for i in 0..20_000_u64 {
            // xorshift: sequence numbers all over the 64-bit range, on
            // more layers than are tracked.
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let layer = u8::try_from(x % 9).unwrap_or(0);
            let seq = match i % 4 {
                0 => x,
                1 => u64::MAX - (x % 3),
                2 => x % 3,
                _ => x % 5000,
            };
            fb.on_packet(layer, seq, t0 + ms(i % 50));
            assert!(fb.layer_count() <= MAX_LAYERS);
            assert!(fb.open_holes() <= MAX_LAYERS * MAX_HOLES);
            if i % 97 == 0 {
                let _ = fb.poll(t0 + ms(i), Some(0));
            }
        }
        // Everything is forgotten once time has passed.
        let _ = fb.poll(t0 + Duration::from_secs(3600), Some(0));
        assert_eq!(fb.open_holes(), 0);
    }

    #[test]
    fn a_long_lossy_stream_stays_bounded() {
        let t0 = Instant::now();
        let mut fb = UplinkFallback::new();
        fb.on_packet(Q, 1, t0);
        // Every second packet missing for ten minutes at 250 a second,
        // never polled: the cap alone bounds it.
        for i in 0..150_000_u64 {
            fb.on_packet(F, i * 2, t0 + ms(i * 4));
        }
        assert_eq!(fb.open_holes(), MAX_HOLES);
    }

    #[test]
    fn times_going_backwards_keep_the_oldest_hole_first() {
        let earlier = Instant::now();
        let t0 = earlier + Duration::from_secs(5);
        let mut fb = UplinkFallback::new();
        fb.on_packet(Q, 1, t0);
        fb.on_packet(F, 1, t0);
        fb.on_packet(F, 3, t0);
        // A later hole reported with an earlier time.
        fb.on_packet(F, 5, earlier);
        // The first hole is 100 ms old; the second is not older than it.
        assert_eq!(fb.poll(t0 + ms(100), Some(Q)), None);
        assert!(fb.poll(t0 + HOLE_AGE + ms(1), Some(Q)).is_some());
    }

    #[test]
    fn the_light_layer_is_the_lowest_measured_or_the_named_one() {
        assert_eq!(pick_light(&[(Q, 150_000), (F, 1_500_000)], Q), Some(Q));
        // Measured rates decide over the name.
        assert_eq!(pick_light(&[(Q, 900_000), (F, 100_000)], Q), Some(F));
        // Unmeasured, or tied: the name.
        assert_eq!(pick_light(&[(Q, 0), (F, 0)], Q), Some(Q));
        assert_eq!(pick_light(&[(Q, 0), (F, 800_000)], Q), Some(Q));
        assert_eq!(pick_light(&[(Q, 5), (F, 5)], Q), Some(Q));
        assert_eq!(pick_light(&[(b'l', 0), (b'h', 0)], Q), None);
        assert_eq!(pick_light(&[(b'l', 7), (b'h', 7)], Q), None);
        // One layer has no light one.
        assert_eq!(pick_light(&[(Q, 150_000)], Q), None);
        assert_eq!(pick_light::<u8>(&[], Q), None);
    }

    #[test]
    fn the_heaviest_layer_is_strictly_the_highest_measured() {
        assert_eq!(pick_heaviest(&[(Q, 150_000), (F, 1_500_000)]), Some(F));
        assert_eq!(pick_heaviest(&[(Q, 0), (F, 0)]), None);
        assert_eq!(pick_heaviest(&[(Q, 0), (F, 800_000)]), None);
        assert_eq!(pick_heaviest(&[(Q, 150_000), (F, 0)]), None);
        assert_eq!(pick_heaviest(&[(Q, 9), (F, 9)]), None);
        assert_eq!(pick_heaviest::<u8>(&[]), None);
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(Direction::ToFallback.from_str(), "full");
        assert_eq!(Direction::ToFallback.to_str(), "fallback");
        assert_eq!(Direction::ToFull.from_str(), "fallback");
        assert_eq!(Direction::ToFull.to_str(), "full");
        assert_eq!(Reason::HoleOlderThanLimit.as_str(), "hole-older-than-limit");
        assert_eq!(Reason::UplinkClean.as_str(), "uplink-clean");
    }
}
