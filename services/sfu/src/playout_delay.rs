// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The minimum playout delay a subscriber is asked to hold for one
//! publisher, and the detection of the loss bursts that raise it
//! ([ADR-0070](../../../docs/adr/0070-call-media-through-a-loss-burst.md)
//! step 4).
//!
//! ## Why
//!
//! A light stream is repaired through a second of heavy loss within a few
//! hundred milliseconds, not within one frame interval. A viewer that plays
//! frames as they arrive sees every one of those repairs as a stop; a viewer
//! that plays 300 ms late sees none of them. The delay costs the
//! conversation that much, so it is asked for only on a path that has shown
//! a burst, and given back after some minutes without one.
//!
//! ## What is here
//!
//! - [`PlayoutDelay`]: one per (subscriber, publisher). Told when a burst
//!   was seen on the publisher's uplink or on the subscriber's downlink, it
//!   answers the minimum playout delay to stamp on that publisher's video
//!   for that subscriber.
//! - [`UplinkBursts`]: one per publisher. Fed the sequence numbers of the
//!   publisher's video packets, it says when its uplink is in a burst.
//! - [`DownlinkBursts`]: one per subscriber. Fed the receiver reports and
//!   the retransmission requests of the subscriber, it says when its
//!   downlink is in a burst.
//!
//! ## What it is not
//!
//! Pure bookkeeping over header fields and RTCP figures the SFU already
//! has: no I/O, no clock of its own (the caller supplies every time), no
//! payload, no key. Stamping the value on packets is the caller's
//! ([`crate::media`]).
//!
//! Every constant below was chosen, not measured.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use crate::uplink_estimate::SeqLossCounter;
use crate::uplink_fallback::HOLE_AGE;

/// The minimum playout delay asked for after a burst, in milliseconds.
/// Chosen: the light track's repairs through a one-second dip were measured
/// to complete within about this long (`measure_two_track_dip`).
pub const BURST_DELAY_MS: u16 = 300;

/// How long a path must have been free of bursts before the delay starts
/// coming down. Chosen: the bursts seen on a home network came about every
/// 30 s, so three minutes without one is a path that has changed.
pub const QUIET_BEFORE_LOWERING: Duration = Duration::from_secs(180);

/// How much the delay comes down per step, in milliseconds. Chosen.
pub const LOWERING_STEP_MS: u16 = 100;

/// How long between two steps down. Chosen: slow enough that the picture
/// catching up (a receiver plays slightly fast to shorten its delay) is not
/// noticed.
pub const LOWERING_STEP_EVERY: Duration = Duration::from_secs(30);

/// The maximum playout delay stamped beside the minimum, in milliseconds.
/// Chosen to be generous: it must never be what limits a receiver's own
/// jitter buffer.
pub const MAX_PLAYOUT_DELAY_MS: u16 = 5000;

/// The share of a publisher's video packets that did not arrive in order in
/// one [`LOSS_INTERVAL`], above which its uplink is in a burst; and the same
/// for what a subscriber reports lost or asks to be sent again. Percent.
pub const BURST_LOSS_PERCENT: u64 = 10;

/// The interval the two loss shares are taken over.
pub const LOSS_INTERVAL: Duration = Duration::from_secs(1);

/// The fewest packets in a [`LOSS_INTERVAL`] for its loss share to count.
/// Chosen: below this one lost packet is already more than
/// [`BURST_LOSS_PERCENT`].
pub const MIN_INTERVAL_PACKETS: u64 = 20;

/// Most streams (SSRCs) of one publisher holes are kept for. A stream beyond
/// that starts the list over. Chosen: three simulcast layers and a light
/// sender, with room to spare.
pub const MAX_STREAMS: usize = 8;

/// Most holes kept for one stream; the oldest are dropped past it. Chosen:
/// about two seconds of a 250 packet a second stream.
pub const MAX_HOLES: usize = 512;

/// The largest step between two sequence numbers read as loss. A larger
/// one, either way, is a stream that started over.
pub const MAX_JUMP: u64 = 512;

/// How long a hole is kept. Past this the packet is no longer waited for.
/// Chosen: longer than the SFU keeps asking for a missing packet.
pub const HOLE_FORGET: Duration = Duration::from_secs(2);

/// Why a pair's delay is what it is, for a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// No burst has been seen.
    NoBurst,
    /// A burst on the publisher's uplink.
    UplinkBurst,
    /// A burst on the subscriber's downlink.
    DownlinkBurst,
    /// No burst for long enough: the delay is coming down or is back at
    /// zero.
    Quiet,
}

impl Reason {
    /// A stable label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoBurst => "no-burst",
            Self::UplinkBurst => "uplink-burst",
            Self::DownlinkBurst => "downlink-burst",
            Self::Quiet => "quiet",
        }
    }
}

/// The minimum playout delay of one (subscriber, publisher) pair.
///
/// Zero until a burst; [`BURST_DELAY_MS`] from a burst on; after
/// [`QUIET_BEFORE_LOWERING`] without one it comes down by
/// [`LOWERING_STEP_MS`], and by as much again every
/// [`LOWERING_STEP_EVERY`], to zero. A burst at any time takes it back to
/// [`BURST_DELAY_MS`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlayoutDelay {
    /// The latest burst and which side it was on.
    last_burst: Option<(Instant, Reason)>,
}

impl PlayoutDelay {
    /// A pair on which no burst has been seen.
    #[must_use]
    pub const fn new() -> Self {
        Self { last_burst: None }
    }

    /// A burst was seen on the publisher's uplink at `at`.
    pub fn on_uplink_burst(&mut self, at: Instant) {
        self.note(at, Reason::UplinkBurst);
    }

    /// A burst was seen on the subscriber's downlink at `at`.
    pub fn on_downlink_burst(&mut self, at: Instant) {
        self.note(at, Reason::DownlinkBurst);
    }

    /// The latest burst wins; a time that went backwards changes nothing.
    fn note(&mut self, at: Instant, reason: Reason) {
        if self.last_burst.map_or(true, |(last, _)| at >= last) {
            self.last_burst = Some((at, reason));
        }
    }

    /// The minimum playout delay to stamp at `now`, in milliseconds.
    #[must_use]
    pub fn delay_ms(&self, now: Instant) -> u16 {
        self.delay_and_reason(now).0
    }

    /// As [`Self::delay_ms`], with why.
    #[must_use]
    pub fn delay_and_reason(&self, now: Instant) -> (u16, Reason) {
        let Some((at, side)) = self.last_burst else {
            return (0, Reason::NoBurst);
        };
        let quiet = now.saturating_duration_since(at);
        if quiet < QUIET_BEFORE_LOWERING {
            return (BURST_DELAY_MS, side);
        }
        // One step at the end of the quiet time and one more per full
        // step interval after it.
        let past = quiet.saturating_sub(QUIET_BEFORE_LOWERING).as_millis();
        let steps = 1 + past / LOWERING_STEP_EVERY.as_millis().max(1);
        let lowered = steps.saturating_mul(u128::from(LOWERING_STEP_MS));
        let delay = u128::from(BURST_DELAY_MS).saturating_sub(lowered);
        (u16::try_from(delay).unwrap_or(0), Reason::Quiet)
    }
}

/// The holes of one stream of a publisher.
#[derive(Debug, Default)]
struct StreamHoles {
    /// Highest sequence number seen.
    highest: u64,
    /// Missing sequence number to when it was first known missing.
    holes: BTreeMap<u64, Instant>,
}

/// Detects bursts of loss on one publisher's uplink from the extended RTP
/// sequence numbers of its video packets.
///
/// The uplink is **in a burst** at a poll when either holds:
///
/// - a packet of any of its video streams has been missing for longer than
///   [`HOLE_AGE`] (150 ms: longer than one ordinary repair), or
/// - over the last complete [`LOSS_INTERVAL`] more than
///   [`BURST_LOSS_PERCENT`] of its video packets did not arrive in order
///   ([`SeqLossCounter`]: a retransmission does not take back the loss its
///   gap was counted as), with at least [`MIN_INTERVAL_PACKETS`] expected.
#[derive(Debug)]
pub struct UplinkBursts {
    streams: HashMap<u32, StreamHoles>,
    loss: SeqLossCounter,
    interval_started: Option<Instant>,
}

impl Default for UplinkBursts {
    fn default() -> Self {
        Self::new()
    }
}

impl UplinkBursts {
    /// A publisher nothing has been received from yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            streams: HashMap::new(),
            loss: SeqLossCounter::new(),
            interval_started: None,
        }
    }

    /// One video packet the SFU received from the publisher: its stream
    /// (SSRC), extended sequence number and arrival time. Retransmissions
    /// and packets rebuilt from FEC count; they are what closes a hole. So
    /// do FEC packets themselves: under RED an FEC packet takes a sequence
    /// number of the media stream, and left out it would read as a hole.
    pub fn on_packet(&mut self, ssrc: u32, seq: u64, arrival: Instant) {
        self.interval_started.get_or_insert(arrival);
        let _ = self.loss.on_packet(ssrc, seq);
        if !self.streams.contains_key(&ssrc) && self.streams.len() >= MAX_STREAMS {
            self.streams.clear();
        }
        let Some(stream) = self.streams.get_mut(&ssrc) else {
            self.streams.insert(
                ssrc,
                StreamHoles {
                    highest: seq,
                    holes: BTreeMap::new(),
                },
            );
            return;
        };
        if seq > stream.highest {
            if seq - stream.highest > MAX_JUMP {
                stream.holes.clear();
            } else {
                for missing in (stream.highest + 1)..seq {
                    stream.holes.insert(missing, arrival);
                }
                while stream.holes.len() > MAX_HOLES {
                    stream.holes.pop_first();
                }
            }
            stream.highest = seq;
        } else if stream.highest - seq > MAX_JUMP {
            stream.holes.clear();
            stream.highest = seq;
        } else {
            stream.holes.remove(&seq);
        }
    }

    /// Move to `now` and say whether the uplink is in a burst.
    pub fn poll(&mut self, now: Instant) -> bool {
        if let Some(started) = self.interval_started {
            if now.saturating_duration_since(started) >= LOSS_INTERVAL {
                self.interval_started = Some(now);
                let (expected, lost) = self.loss.take_interval();
                if expected >= MIN_INTERVAL_PACKETS
                    && lost.saturating_mul(100) > expected.saturating_mul(BURST_LOSS_PERCENT)
                {
                    return true;
                }
            }
        }
        let mut old_hole = false;
        for stream in self.streams.values_mut() {
            while let Some((_, seen)) = stream.holes.first_key_value() {
                let age = now.saturating_duration_since(*seen);
                if age >= HOLE_FORGET {
                    stream.holes.pop_first();
                    continue;
                }
                // Holes are inserted in order of sequence number with
                // times that do not decrease within one stream as long as
                // the caller's clock does not; the first is the oldest.
                old_hole |= age > HOLE_AGE;
                break;
            }
        }
        old_hole
    }

    /// Holes currently kept across all streams. For tests and bounds.
    #[must_use]
    pub fn open_holes(&self) -> usize {
        self.streams.values().map(|s| s.holes.len()).sum()
    }

    /// Forget the stream `ssrc`: its sender was switched off on the SFU's
    /// word ([`crate::video_needed`]). A packet missing when it stopped is
    /// no burst, and its first packet after it is switched back on starts
    /// the stream afresh, whatever its sequence number.
    pub fn forget(&mut self, ssrc: u32) {
        self.streams.remove(&ssrc);
        self.loss.forget(ssrc);
    }
}

/// Detects bursts of loss on one subscriber's downlink from what the
/// subscriber tells the SFU.
///
/// The downlink is **in a burst** when either holds:
///
/// - a receiver report from the subscriber, for any video stream the SFU
///   sends it, says more than [`BURST_LOSS_PERCENT`] of the packets were
///   lost since the report before ([`Self::on_receiver_report`]), or
/// - over a complete [`LOSS_INTERVAL`] the subscriber asked by NACK for
///   more than [`BURST_LOSS_PERCENT`] of the video packets the SFU sent it
///   in that interval, with at least [`MIN_INTERVAL_PACKETS`] sent
///   ([`Self::on_interval`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DownlinkBursts {
    interval_started: Option<Instant>,
    /// Cumulative counts at the start of the interval.
    sent_at_start: u64,
    nacked_at_start: u64,
}

impl DownlinkBursts {
    /// A subscriber nothing has been heard from yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            interval_started: None,
            sent_at_start: 0,
            nacked_at_start: 0,
        }
    }

    /// Whether a receiver report's loss fraction (in 1/256, as RTCP carries
    /// it) is a burst.
    #[must_use]
    pub fn on_receiver_report(fraction_lost: u8) -> bool {
        u64::from(fraction_lost).saturating_mul(100) > 256 * BURST_LOSS_PERCENT
    }

    /// Hand over the cumulative number of video packets sent to the
    /// subscriber (first transmissions) and the cumulative number it has
    /// asked for by NACK, at `now`. Returns `true` when an interval has
    /// just completed and was a burst.
    pub fn on_interval(&mut self, now: Instant, sent: u64, nacked: u64) -> bool {
        let Some(started) = self.interval_started else {
            self.interval_started = Some(now);
            self.sent_at_start = sent;
            self.nacked_at_start = nacked;
            return false;
        };
        if now.saturating_duration_since(started) < LOSS_INTERVAL {
            return false;
        }
        // Counters that went backwards (a stream was reset) start over.
        let sent_in = sent.saturating_sub(self.sent_at_start);
        let nacked_in = nacked.saturating_sub(self.nacked_at_start);
        self.interval_started = Some(now);
        self.sent_at_start = sent;
        self.nacked_at_start = nacked;
        sent_in >= MIN_INTERVAL_PACKETS
            && nacked_in.saturating_mul(100) > sent_in.saturating_mul(BURST_LOSS_PERCENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn the_delay_is_zero_until_a_burst() {
        let t0 = Instant::now();
        let d = PlayoutDelay::new();
        assert_eq!(d.delay_and_reason(t0), (0, Reason::NoBurst));
        assert_eq!(d.delay_ms(t0 + secs(3600)), 0);
    }

    #[test]
    fn a_burst_on_either_side_raises_it_to_the_burst_delay() {
        let t0 = Instant::now();
        let mut up = PlayoutDelay::new();
        up.on_uplink_burst(t0);
        assert_eq!(
            up.delay_and_reason(t0),
            (BURST_DELAY_MS, Reason::UplinkBurst)
        );
        let mut down = PlayoutDelay::new();
        down.on_downlink_burst(t0);
        assert_eq!(
            down.delay_and_reason(t0 + secs(1)),
            (BURST_DELAY_MS, Reason::DownlinkBurst)
        );
        assert_eq!(BURST_DELAY_MS, 300);
    }

    #[test]
    fn it_holds_for_the_quiet_time_then_steps_down_to_zero() {
        let t0 = Instant::now();
        let mut d = PlayoutDelay::new();
        d.on_uplink_burst(t0);
        let just_before = Duration::from_millis(179_999);
        assert_eq!(d.delay_ms(t0 + just_before), 300);
        assert_eq!(
            d.delay_and_reason(t0 + QUIET_BEFORE_LOWERING),
            (200, Reason::Quiet)
        );
        assert_eq!(d.delay_ms(t0 + secs(180 + 29)), 200);
        assert_eq!(d.delay_ms(t0 + secs(180 + 30)), 100);
        assert_eq!(d.delay_ms(t0 + secs(180 + 59)), 100);
        assert_eq!(d.delay_ms(t0 + secs(180 + 60)), 0);
        assert_eq!(
            d.delay_and_reason(t0 + secs(100_000)),
            (0, Reason::Quiet),
            "and never below zero"
        );
    }

    #[test]
    fn a_burst_at_any_time_takes_it_back_up() {
        let t0 = Instant::now();
        let mut d = PlayoutDelay::new();
        d.on_uplink_burst(t0);
        // While coming down.
        assert_eq!(d.delay_ms(t0 + secs(215)), 100);
        d.on_downlink_burst(t0 + secs(215));
        assert_eq!(
            d.delay_and_reason(t0 + secs(215)),
            (300, Reason::DownlinkBurst)
        );
        // The quiet time counts from the latest burst.
        assert_eq!(d.delay_ms(t0 + secs(215 + 179)), 300);
        assert_eq!(d.delay_ms(t0 + secs(215 + 180)), 200);
        // After it reached zero.
        d.on_uplink_burst(t0 + secs(1000));
        assert_eq!(d.delay_ms(t0 + secs(1000)), 300);
        // Within the hold: the hold starts over.
        d.on_uplink_burst(t0 + secs(1100));
        assert_eq!(d.delay_ms(t0 + secs(1000 + 181)), 300);
    }

    #[test]
    fn an_earlier_burst_reported_late_does_not_shorten_the_hold() {
        let t0 = Instant::now();
        let mut d = PlayoutDelay::new();
        d.on_uplink_burst(t0 + secs(100));
        d.on_downlink_burst(t0);
        assert_eq!(
            d.delay_and_reason(t0 + secs(200)),
            (300, Reason::UplinkBurst)
        );
    }

    /// A stream of `n` packets from `first`, 4 ms apart from `t0`, without
    /// the sequence numbers in `skip`.
    fn feed(up: &mut UplinkBursts, ssrc: u32, first: u64, n: u64, skip: &[u64], t0: Instant) {
        for i in 0..n {
            let seq = first + i;
            if !skip.contains(&seq) {
                up.on_packet(ssrc, seq, t0 + Duration::from_millis(4 * i));
            }
        }
    }

    #[test]
    fn a_clean_uplink_is_never_in_a_burst() {
        let t0 = Instant::now();
        let mut up = UplinkBursts::new();
        feed(&mut up, 1, 100, 1000, &[], t0);
        for ms in (0..4000).step_by(50) {
            assert!(!up.poll(t0 + Duration::from_millis(ms)));
        }
        assert_eq!(up.open_holes(), 0);
    }

    #[test]
    fn a_hole_is_a_burst_once_it_is_older_than_the_limit_and_until_it_closes() {
        let t0 = Instant::now();
        let mut up = UplinkBursts::new();
        // 100..=110 without 105: the hole is known at the arrival of 106.
        feed(&mut up, 1, 100, 11, &[105], t0);
        let known = t0 + Duration::from_millis(24);
        assert_eq!(up.open_holes(), 1);
        assert!(!up.poll(known + HOLE_AGE), "at the limit is not past it");
        assert!(up.poll(known + HOLE_AGE + Duration::from_millis(1)));
        // The retransmission arrives.
        up.on_packet(1, 105, known + Duration::from_millis(200));
        assert_eq!(up.open_holes(), 0);
        assert!(!up.poll(known + Duration::from_millis(201)));
    }

    #[test]
    fn a_hole_repaired_in_time_is_not_a_burst() {
        let t0 = Instant::now();
        let mut up = UplinkBursts::new();
        feed(&mut up, 1, 100, 11, &[105], t0);
        up.on_packet(1, 105, t0 + Duration::from_millis(90));
        for ms in (0..900).step_by(10) {
            assert!(!up.poll(t0 + Duration::from_millis(ms)));
        }
    }

    #[test]
    fn a_hole_is_forgotten_and_holes_are_bounded() {
        let t0 = Instant::now();
        let mut up = UplinkBursts::new();
        up.on_packet(1, 100, t0);
        up.on_packet(1, 102, t0);
        assert!(up.poll(t0 + Duration::from_millis(500)));
        assert!(!up.poll(t0 + HOLE_FORGET), "no longer waited for");
        assert_eq!(up.open_holes(), 0);

        // A jump too large to be loss opens nothing.
        up.on_packet(1, 102 + MAX_JUMP + 1, t0);
        assert_eq!(up.open_holes(), 0);
        // Many holes: only the newest are kept.
        let mut seq = 10_000_000;
        up.on_packet(2, seq, t0);
        for _ in 0..2000 {
            seq += 2;
            up.on_packet(2, seq, t0);
        }
        assert_eq!(up.open_holes(), MAX_HOLES);
        // Many streams: the list starts over instead of growing.
        for ssrc in 10..40 {
            up.on_packet(ssrc, 1, t0);
        }
        assert!(up.streams.len() <= MAX_STREAMS);
    }

    #[test]
    fn a_lossy_second_is_a_burst_even_when_every_hole_was_repaired_quickly() {
        let t0 = Instant::now();
        let mut up = UplinkBursts::new();
        // 250 packets in a second, every fifth one late by 40 ms.
        for i in 0..250u64 {
            let at = t0 + Duration::from_millis(4 * i);
            if i % 5 == 4 {
                continue;
            }
            up.on_packet(1, 1000 + i, at);
            if i % 5 == 0 && i > 0 {
                up.on_packet(1, 1000 + i - 1, at + Duration::from_millis(40));
            }
        }
        assert!(
            !up.poll(t0 + Duration::from_millis(999)),
            "no old hole, and the interval is not over"
        );
        assert!(up.poll(t0 + LOSS_INTERVAL), "20% did not arrive in order");
        // The next interval is clean.
        assert!(!up.poll(t0 + LOSS_INTERVAL + Duration::from_millis(10)));
    }

    #[test]
    fn a_few_lost_packets_of_a_thin_stream_are_not_a_lossy_second() {
        let t0 = Instant::now();
        let mut up = UplinkBursts::new();
        // 10 packets in the interval, 3 of them late: 30%, of too few.
        for i in 0..10u64 {
            if i % 3 == 1 {
                continue;
            }
            up.on_packet(1, i, t0 + Duration::from_millis(100 * i));
        }
        for i in [1u64, 4, 7] {
            up.on_packet(1, i, t0 + Duration::from_millis(100 * i + 120));
        }
        assert!(!up.poll(t0 + LOSS_INTERVAL));
    }

    #[test]
    fn a_receiver_report_above_ten_percent_is_a_burst() {
        assert!(!DownlinkBursts::on_receiver_report(0));
        // 25/256 = 9.8%, 26/256 = 10.2%.
        assert!(!DownlinkBursts::on_receiver_report(25));
        assert!(DownlinkBursts::on_receiver_report(26));
        assert!(DownlinkBursts::on_receiver_report(255));
    }

    #[test]
    fn nacks_for_more_than_a_tenth_of_a_second_of_packets_are_a_burst() {
        let t0 = Instant::now();
        let mut down = DownlinkBursts::new();
        assert!(!down.on_interval(t0, 1000, 50), "the first call starts");
        assert!(
            !down.on_interval(t0 + Duration::from_millis(999), 1300, 200),
            "not before the interval is over"
        );
        // 300 sent, 31 asked for again.
        assert!(down.on_interval(t0 + LOSS_INTERVAL, 1300, 81));
        // The next interval: 300 sent, 30 asked for, exactly a tenth.
        assert!(!down.on_interval(t0 + 2 * LOSS_INTERVAL, 1600, 111));
        // Too few packets sent for a share to mean anything.
        assert!(!down.on_interval(t0 + 3 * LOSS_INTERVAL, 1610, 121));
        // Counters that went backwards do not underflow.
        assert!(!down.on_interval(t0 + 4 * LOSS_INTERVAL, 5, 0));
    }

    #[test]
    fn reason_labels_are_stable() {
        assert_eq!(Reason::NoBurst.as_str(), "no-burst");
        assert_eq!(Reason::UplinkBurst.as_str(), "uplink-burst");
        assert_eq!(Reason::DownlinkBurst.as_str(), "downlink-burst");
        assert_eq!(Reason::Quiet.as_str(), "quiet");
    }
}
