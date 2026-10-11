// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! When a burst of loss on a publisher's video stream has ended and left
//! packets nobody will repair, so that the SFU itself asks the publisher for
//! a key frame instead of waiting for a subscriber to do it
//! ([ADR-0070](../../../docs/adr/0070-call-media-through-a-loss-burst.md),
//! note of 2026-10-09).
//!
//! ## Why
//!
//! Seen on a live call: a home Wi-Fi uplink lost about 80% of its datagrams
//! for one second every 30 s. The SFU asked for the missing packets all
//! through the burst and after it, and most never came. Every viewer then
//! held frames it could not decode until its own decoder gave up and asked
//! for a key frame, about three seconds after the burst had ended. The SFU
//! knew at the end of the burst what the viewers found out three seconds
//! later.
//!
//! ## The rule
//!
//! Per video stream of a publisher (main, middle and light each on its own;
//! each simulcast layer on its own), from the sequence numbers of the packets
//! as they reach the SFU, retransmissions, FEC packets and packets rebuilt
//! from FEC included:
//!
//! - a **burst** opens with the first missing packet and stays open while
//!   packets go on missing;
//! - it has **ended** when [`BURST_END_PACKETS`] packets in a row arrived in
//!   sequence and no new packet went missing for [`BURST_END_QUIET`];
//! - it ended **unrepaired** when, at that moment or later, a packet is still
//!   missing [`UNREPAIRED_AFTER`] after it was first known missing, or when
//!   the burst left packets that retransmission cannot cover: missing for
//!   longer than [`RETRANSMISSION_SPAN`], or further than
//!   [`RETRANSMISSION_WINDOW`] packets behind the newest;
//! - a burst that ended with every packet there asks for nothing. Neither
//!   does one whose missing packets all precede a key frame that has started
//!   arriving since.
//!
//! One answer per burst ([`BurstEnds::poll`]). A packet that goes missing
//! before the answer is the same burst going on, and the wait starts over.
//!
//! ## A stream that carries FEC
//!
//! A publisher that sends ULPFEC inside RED numbers its FEC packets in the
//! sequence of the media stream, and does not send a lost FEC packet again
//! (recalled from libwebrtc, not read for this). So on such a stream a
//! packet that stays
//! missing is either media somebody needs or an FEC packet nobody does, and
//! nothing in a packet that never arrived says which. The rule there is by
//! number: the burst ended unrepaired only when more packets stay missing
//! than the FEC share of the stream explains, namely more than
//! [`FEC_SLACK`] plus one and a half times that share of what the burst
//! lost. The share is counted over the packets that arrived in sequence,
//! lately. One or two packets that stay missing on such a stream never ask
//! for anything. A stream on which no FEC packet has been seen is judged
//! exactly: one packet that stays missing is unrepaired.
//!
//! The quiet time doubles as the last chance of retransmission: by the time a
//! burst counts as ended, every packet still missing has been asked for at
//! least once more on a path that is carrying packets again (every 60 ms,
//! every 20 ms on a light stream).
//!
//! ## What it is not
//!
//! Pure bookkeeping over header fields: no I/O, no clock of its own (the
//! caller supplies every time), no payload, no key. Asking the publisher,
//! and deciding whether anybody is being sent the stream, is the caller's
//! ([`crate::media`]).
//!
//! Every constant below was chosen, not measured.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::time::{Duration, Instant};

/// How long no new packet may have gone missing on a stream for its burst to
/// count as ended. Chosen: two rounds of the SFU's 60 ms retransmission
/// schedule, so what is still missing afterwards was asked for on the
/// recovered path and did not come.
pub const BURST_END_QUIET: Duration = Duration::from_millis(120);

/// How many packets in a row must have arrived in sequence since the last
/// one went missing. Chosen: in a burst that loses eight datagrams in ten,
/// five in a row arrive by chance about once in three thousand tries, so a
/// lull inside the burst is not taken for its end; at the 50 packets a second
/// of a light stream it is a tenth of a second.
pub const BURST_END_PACKETS: u32 = 5;

/// How long after a packet was first known missing it counts as not
/// repaired. Chosen: the playout delay a viewer is asked to hold after a
/// burst ([`crate::playout_delay::BURST_DELAY_MS`]). A packet that arrives
/// later than that is too late for the frame it belongs to.
pub const UNREPAIRED_AFTER: Duration = Duration::from_millis(300);

/// How long the SFU keeps asking a publisher for one missing packet: 25
/// requests 60 ms apart in the vendored `str0m`
/// (`streams/register_nack.rs`). A packet missing for longer will not be
/// asked for again.
pub const RETRANSMISSION_SPAN: Duration = Duration::from_millis(1500);

/// How far behind the newest packet of its stream a missing packet is still
/// asked for (`MAX_MISORDER` in the vendored `str0m`).
pub const RETRANSMISSION_WINDOW: u64 = 512;

/// The largest step between two sequence numbers read as loss (forward) or
/// as a late packet (back). A larger one, either way, is a stream that
/// started over, which is not a burst. Chosen: about forty seconds of a 250
/// packet a second stream.
pub const MAX_JUMP: u64 = 10_000;

/// Most streams of one publisher kept. A stream beyond that starts the table
/// over. Chosen: three senders or three simulcast layers, camera and screen,
/// with room to spare.
pub const MAX_STREAMS: usize = 12;

/// On a stream that carries FEC, how many packets may stay missing beyond
/// the FEC share of what a burst lost before the burst counts as
/// unrepaired. Chosen: a lost FEC packet or two must never cost a key
/// frame.
pub const FEC_SLACK: u64 = 2;

/// How many packets that arrived in sequence the FEC share of a stream is
/// counted over; at this many both counts are halved. Chosen: a few seconds
/// of a main stream, tens of seconds of a light one.
pub const FEC_SHARE_WINDOW: u32 = 1024;

/// What a packet handed to [`BurstEnds::on_packet`] is, by its header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrived {
    /// A media packet.
    Media,
    /// The first media packet of a frame with no dependencies, by its
    /// frame descriptor: every packet before it is no longer needed by
    /// anybody.
    KeyFrameStart,
    /// An FEC packet (it takes a sequence number of the stream).
    Fec,
}

/// A burst that ended unrepaired on one stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurstEnd<K> {
    /// The stream, as the caller named it.
    pub stream: K,
    /// Packets that went missing during the burst, repaired or not.
    pub lost: u64,
    /// Of those, how many never arrived.
    pub unrepaired: u64,
    /// Whether some went beyond what retransmission covers
    /// ([`RETRANSMISSION_SPAN`], [`RETRANSMISSION_WINDOW`]).
    pub beyond_repair: bool,
    /// From the first missing packet to the last one that went missing.
    pub lasted: Duration,
}

/// The burst that is open on a stream.
#[derive(Debug, Clone, Copy)]
struct Burst {
    /// When the first packet was known missing.
    started: Instant,
    /// When the latest one was.
    last_hole: Instant,
    /// Packets that arrived in sequence since.
    in_order: u32,
    /// Packets that went missing in it.
    lost: u64,
    /// Of those, how many have arrived since.
    repaired: u64,
    /// Some of them are past retransmission.
    beyond_repair: bool,
}

/// One stream of a publisher.
#[derive(Debug)]
struct Stream {
    /// Highest extended sequence number seen.
    highest: u64,
    /// Missing sequence number to when it was first known missing. Never
    /// more than [`RETRANSMISSION_WINDOW`] entries: a hole further behind
    /// the newest packet is dropped and counted as beyond repair.
    holes: BTreeMap<u64, Instant>,
    burst: Option<Burst>,
    /// Packets that arrived in sequence lately, and how many of them were
    /// FEC ([`FEC_SHARE_WINDOW`]).
    in_sequence: u32,
    in_sequence_fec: u32,
}

impl Stream {
    const fn new(seq: u64) -> Self {
        Self {
            highest: seq,
            holes: BTreeMap::new(),
            burst: None,
            in_sequence: 0,
            in_sequence_fec: 0,
        }
    }

    /// Count one packet that arrived ahead of every earlier one.
    fn count(&mut self, what: Arrived) {
        if self.in_sequence >= FEC_SHARE_WINDOW {
            self.in_sequence /= 2;
            self.in_sequence_fec /= 2;
        }
        self.in_sequence += 1;
        if what == Arrived::Fec {
            self.in_sequence_fec += 1;
        }
    }

    /// Whether `unrepaired` packets of a burst that lost `lost` are more
    /// than FEC packets nobody sends again explain (see the module
    /// documentation). On a stream without FEC any is.
    fn beyond_fec(&self, lost: u64, unrepaired: u64) -> bool {
        if self.in_sequence_fec == 0 {
            return unrepaired > 0;
        }
        // unrepaired - slack > 1.5 * share * lost, in whole numbers.
        let over = u128::from(unrepaired.saturating_sub(FEC_SLACK));
        over * 2 * u128::from(self.in_sequence)
            > 3 * u128::from(self.in_sequence_fec) * u128::from(lost)
    }
}

/// The burst bookkeeping of every video stream one publisher sends, keyed by
/// whatever the caller names a stream by.
#[derive(Debug)]
pub struct BurstEnds<K> {
    streams: HashMap<K, Stream>,
}

impl<K: Copy + Eq + Hash> Default for BurstEnds<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Copy + Eq + Hash> BurstEnds<K> {
    /// A publisher nothing has been received from yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            streams: HashMap::new(),
        }
    }

    /// One packet of `stream` reached the SFU: its extended sequence number,
    /// arrival time and what it is. Every packet that takes a sequence
    /// number of the stream is handed over: media, retransmissions (as the
    /// packet they repair), FEC packets and packets rebuilt from FEC.
    pub fn on_packet(&mut self, stream: K, seq: u64, arrival: Instant, what: Arrived) {
        let key_frame_start = what == Arrived::KeyFrameStart;
        if !self.streams.contains_key(&stream) && self.streams.len() >= MAX_STREAMS {
            self.streams.clear();
        }
        let Some(state) = self.streams.get_mut(&stream) else {
            self.streams.insert(stream, Stream::new(seq));
            return;
        };
        if seq > state.highest {
            let step = seq - state.highest;
            if step > MAX_JUMP {
                *state = Stream::new(seq);
                return;
            }
            state.count(what);
            if step == 1 {
                if let Some(burst) = state.burst.as_mut() {
                    burst.in_order = burst.in_order.saturating_add(1);
                }
            } else {
                let burst = state.burst.get_or_insert(Burst {
                    started: arrival,
                    last_hole: arrival,
                    in_order: 0,
                    lost: 0,
                    repaired: 0,
                    beyond_repair: false,
                });
                burst.last_hole = arrival;
                burst.in_order = 0;
                burst.lost = burst.lost.saturating_add(step - 1);
                // Only what retransmission still covers is kept one by one.
                let keep_from = seq.saturating_sub(RETRANSMISSION_WINDOW);
                if state.highest + 1 < keep_from {
                    burst.beyond_repair = true;
                }
                for missing in (state.highest + 1).max(keep_from)..seq {
                    state.holes.insert(missing, arrival);
                }
            }
            state.highest = seq;
            // Holes the newest packet has left too far behind.
            let keep_from = seq.saturating_sub(RETRANSMISSION_WINDOW);
            while state
                .holes
                .first_key_value()
                .is_some_and(|(missing, _)| *missing < keep_from)
            {
                state.holes.pop_first();
                if let Some(burst) = state.burst.as_mut() {
                    burst.beyond_repair = true;
                }
            }
        } else if state.highest - seq > MAX_JUMP {
            *state = Stream::new(seq);
            return;
        } else {
            // A late packet: it closes its hole if that is still tracked.
            if state.holes.remove(&seq).is_some() {
                if let Some(burst) = state.burst.as_mut() {
                    burst.repaired = burst.repaired.saturating_add(1);
                }
            }
        }
        if key_frame_start {
            // What is missing before a key frame is missing for nobody.
            let before = state.holes.len();
            state.holes = state.holes.split_off(&seq);
            let closed = (before - state.holes.len()) as u64;
            if let Some(burst) = state.burst.as_mut() {
                if seq == state.highest {
                    // The newest packet: everything the burst lost is
                    // older, tracked or not.
                    burst.beyond_repair = false;
                    burst.lost = 0;
                    burst.repaired = 0;
                } else {
                    burst.repaired = burst.repaired.saturating_add(closed);
                }
            }
            if state.holes.is_empty() {
                state.burst = None;
            }
        }
    }

    /// Move to `now` and hand out the bursts that have ended unrepaired
    /// since the last call, each once. A burst that ended with nothing
    /// missing is closed without a word.
    pub fn poll(&mut self, now: Instant) -> Vec<BurstEnd<K>> {
        let mut ended = Vec::new();
        for (key, state) in &mut self.streams {
            let Some(mut burst) = state.burst else {
                continue;
            };
            // Holes are inserted in order of sequence number with times that
            // do not decrease as long as the caller's clock does not: the
            // first is the oldest.
            while state.holes.first_key_value().is_some_and(|(_, seen)| {
                now.saturating_duration_since(*seen) >= RETRANSMISSION_SPAN
            }) {
                state.holes.pop_first();
                burst.beyond_repair = true;
            }
            if state.holes.is_empty() && !burst.beyond_repair {
                // Repaired, every packet of it.
                state.burst = None;
                continue;
            }
            state.burst = Some(burst);
            let over = burst.in_order >= BURST_END_PACKETS
                && now.saturating_duration_since(burst.last_hole) >= BURST_END_QUIET;
            if !over {
                continue;
            }
            let unrepaired = burst.beyond_repair
                || state.holes.first_key_value().is_some_and(|(_, seen)| {
                    now.saturating_duration_since(*seen) >= UNREPAIRED_AFTER
                });
            if !unrepaired {
                continue;
            }
            // The burst is over and what is still missing will not come.
            let missing = burst.lost.saturating_sub(burst.repaired);
            if state.beyond_fec(burst.lost, missing) {
                ended.push(BurstEnd {
                    stream: *key,
                    lost: burst.lost,
                    unrepaired: missing,
                    beyond_repair: burst.beyond_repair,
                    lasted: burst.last_hole.saturating_duration_since(burst.started),
                });
            }
            state.holes.clear();
            state.burst = None;
        }
        ended
    }

    /// Packets currently tracked as missing across all streams. For tests
    /// and bounds.
    #[must_use]
    pub fn open_holes(&self) -> usize {
        self.streams.values().map(|s| s.holes.len()).sum()
    }

    /// Forget `stream`: its sender was switched off on the SFU's word
    /// ([`crate::video_needed`]). What was missing when it stopped is
    /// missing for nobody, and the first packet after it is switched back
    /// on starts the stream's books afresh, whatever its sequence number:
    /// neither the stop nor the return is a burst.
    pub fn forget(&mut self, stream: K) {
        self.streams.remove(&stream);
    }

    /// Whether a burst is open on `stream`.
    #[must_use]
    pub fn in_burst(&self, stream: K) -> bool {
        self.streams.get(&stream).is_some_and(|s| s.burst.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forgotten_stream_starts_over_without_a_burst() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut ends: BurstEnds<u8> = BurstEnds::new();
        for seq in 0..10u64 {
            ends.on_packet(7, seq, t0 + ms(seq * 10), Arrived::Media);
        }
        // Two packets lost right before the sender is switched off.
        ends.on_packet(7, 12, t0 + ms(120), Arrived::Media);
        assert!(ends.in_burst(7));
        assert_eq!(ends.open_holes(), 2);
        ends.forget(7);
        assert!(!ends.in_burst(7));
        assert_eq!(ends.open_holes(), 0);
        // It comes back a minute later 200 sequence numbers on: no burst,
        // and nothing ever ends unrepaired.
        let back = t0 + Duration::from_secs(60);
        for i in 0..40u64 {
            ends.on_packet(7, 212 + i, back + ms(i * 10), Arrived::Media);
        }
        assert!(!ends.in_burst(7));
        assert!(ends.poll(back + Duration::from_secs(5)).is_empty());
        // Another stream of the publisher is untouched by it.
        ends.on_packet(8, 0, t0, Arrived::Media);
        ends.on_packet(8, 3, t0 + ms(10), Arrived::Media);
        ends.forget(7);
        assert!(ends.in_burst(8));
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Packets `first..first + n`, `every` apart from `t0`, without the
    /// sequence numbers in `skip`. Returns the arrival time of the last
    /// slot.
    fn feed(
        b: &mut BurstEnds<u8>,
        stream: u8,
        first: u64,
        n: u64,
        skip: &[u64],
        t0: Instant,
        every: Duration,
    ) -> Instant {
        let mut at = t0;
        for i in 0..n {
            at = t0 + every * u32::try_from(i).expect("fits");
            let seq = first + i;
            if !skip.contains(&seq) {
                b.on_packet(stream, seq, at, Arrived::Media);
            }
        }
        at
    }

    /// Poll every 10 ms over `[from, until]` and collect what ended, with
    /// the time of the poll that said so.
    fn poll_over(
        b: &mut BurstEnds<u8>,
        from: Instant,
        until: Instant,
    ) -> Vec<(Duration, BurstEnd<u8>)> {
        let mut out = Vec::new();
        let mut at = from;
        while at <= until {
            for end in b.poll(at) {
                out.push((at.duration_since(from), end));
            }
            at += ms(10);
        }
        out
    }

    #[test]
    fn a_clean_stream_never_ends_a_burst() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        let last = feed(&mut b, 1, 100, 1000, &[], t0, ms(4));
        assert!(poll_over(&mut b, t0, last + ms(3000)).is_empty());
        assert!(!b.in_burst(1));
        assert_eq!(b.open_holes(), 0);
    }

    #[test]
    fn a_burst_that_retransmission_repaired_asks_for_nothing() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // 100..150 at 250 packets a second, 110..=129 lost.
        let lost: Vec<u64> = (110..130).collect();
        feed(&mut b, 1, 100, 50, &lost, t0, ms(4));
        assert!(b.in_burst(1));
        assert_eq!(b.open_holes(), 20);
        // The stream goes on in sequence; the retransmissions arrive 80 ms
        // after the burst.
        let after = t0 + ms(200);
        feed(&mut b, 1, 150, 20, &[], after, ms(4));
        for seq in &lost {
            b.on_packet(1, *seq, after + ms(80), Arrived::Media);
        }
        feed(&mut b, 1, 170, 500, &[], after + ms(80), ms(4));
        assert!(poll_over(&mut b, t0, after + ms(4000)).is_empty());
        assert!(!b.in_burst(1), "closed, without a word");
    }

    #[test]
    fn an_unrepaired_burst_ends_once_after_the_stated_times() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // One second at 250 packets a second in which only every fifth
        // packet arrives, then the stream in sequence again.
        let lost: Vec<u64> = (1000..1250).filter(|s| s % 5 != 0).collect();
        let burst_last = feed(&mut b, 1, 1000, 250, &lost, t0, ms(4));
        // In the burst: polled all through it, nothing ends.
        assert!(poll_over(&mut b, t0, burst_last).is_empty());
        let resumed = burst_last + ms(4);
        feed(&mut b, 1, 1250, 40, &[], resumed, ms(4));
        let ended = poll_over(&mut b, resumed, resumed + ms(160));
        assert_eq!(ended.len(), 1, "{ended:?}");
        let (after, end) = ended[0];
        // The stream goes on: the burst is not answered a second time.
        feed(&mut b, 1, 1290, 1000, &[], resumed + ms(160), ms(4));
        assert!(
            poll_over(&mut b, resumed + ms(160), resumed + ms(5000)).is_empty(),
            "one answer per burst"
        );
        // The last hole opened with the first packet in sequence; the quiet
        // time counts from there.
        assert!(
            after + ms(4) >= BURST_END_QUIET && after <= BURST_END_QUIET + ms(20),
            "asked {after:?} after the stream was in sequence again"
        );
        assert_eq!(end.stream, 1);
        assert_eq!(end.lost, 200);
        assert_eq!(end.unrepaired, 200);
        assert!(!end.beyond_repair);
        assert!(end.lasted >= ms(900) && end.lasted <= ms(1000));
        assert_eq!(b.open_holes(), 0, "what was missing is let go");
        assert!(!b.in_burst(1));
    }

    #[test]
    fn a_short_burst_waits_for_its_packets_before_it_counts_as_unrepaired() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // Two packets lost, then in sequence.
        feed(&mut b, 1, 100, 10, &[104, 105], t0, ms(4));
        let known = t0 + ms(24);
        feed(&mut b, 1, 110, 500, &[], t0 + ms(40), ms(4));
        let ended = poll_over(&mut b, known, known + ms(2000));
        assert_eq!(ended.len(), 1);
        let (after, end) = ended[0];
        assert!(
            after >= UNREPAIRED_AFTER && after <= UNREPAIRED_AFTER + ms(20),
            "not at the end of the burst but when the repair is late: {after:?}"
        );
        assert_eq!((end.lost, end.unrepaired), (2, 2));
    }

    #[test]
    fn a_short_burst_repaired_before_the_stated_time_asks_for_nothing() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        feed(&mut b, 1, 100, 10, &[104, 105], t0, ms(4));
        feed(&mut b, 1, 110, 40, &[], t0 + ms(40), ms(4));
        // Ended (in sequence, quiet) but the holes are young: nothing yet.
        assert!(b.poll(t0 + ms(24) + ms(200)).is_empty());
        assert!(b.in_burst(1));
        b.on_packet(1, 104, t0 + ms(250), Arrived::Media);
        b.on_packet(1, 105, t0 + ms(250), Arrived::Media);
        assert!(poll_over(&mut b, t0 + ms(250), t0 + ms(3000)).is_empty());
        assert!(!b.in_burst(1));
    }

    #[test]
    fn a_second_burst_inside_the_wait_restarts_it() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        let lost: Vec<u64> = (100..150).filter(|s| s % 5 != 0).collect();
        feed(&mut b, 1, 100, 50, &lost, t0, ms(4));
        // In sequence for 80 ms: not yet the stated quiet time.
        let calm = t0 + ms(200);
        feed(&mut b, 1, 150, 20, &[], calm, ms(4));
        assert!(poll_over(&mut b, t0, calm + ms(80)).is_empty());
        // Loss again.
        let again = calm + ms(80);
        let lost2: Vec<u64> = (170..220).filter(|s| s % 5 != 0).collect();
        let end_of_second = feed(&mut b, 1, 170, 50, &lost2, again, ms(4));
        assert!(poll_over(&mut b, again, end_of_second).is_empty());
        let resumed = end_of_second + ms(4);
        feed(&mut b, 1, 220, 500, &[], resumed, ms(4));
        let ended = poll_over(&mut b, resumed, resumed + ms(3000));
        assert_eq!(ended.len(), 1, "one burst, one answer: {ended:?}");
        assert_eq!(ended[0].1.lost, 80, "both halves are the one burst");
        assert!(ended[0].0 + ms(4) >= BURST_END_QUIET);
    }

    #[test]
    fn a_lull_inside_a_burst_is_not_its_end() {
        let t0 = Instant::now();
        // Four packets in sequence right after a hole, 100 ms apart: quiet
        // for long enough, but not enough packets.
        let mut b = BurstEnds::new();
        b.on_packet(1, 100, t0, Arrived::Media);
        b.on_packet(1, 105, t0 + ms(20), Arrived::Media);
        for (i, seq) in (106..110).enumerate() {
            b.on_packet(
                1,
                seq,
                t0 + ms(100 * (u64::try_from(i).expect("fits") + 1)),
                Arrived::Media,
            );
        }
        // 420 ms after the hole, four packets in sequence: still the burst.
        assert!(poll_over(&mut b, t0, t0 + ms(1000)).is_empty());
        assert!(b.in_burst(1));
        // The fifth ends it; the hole is long past repair.
        b.on_packet(1, 110, t0 + ms(1010), Arrived::Media);
        assert_eq!(b.poll(t0 + ms(1010)).len(), 1);
    }

    #[test]
    fn a_stream_that_stops_in_a_burst_is_not_asked_until_it_runs_again() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        feed(&mut b, 1, 100, 10, &[104, 105, 106], t0, ms(4));
        // Nothing more arrives for ten seconds.
        assert!(poll_over(&mut b, t0, t0 + ms(10_000)).is_empty());
        assert_eq!(b.open_holes(), 0, "holes are let go after the span");
        assert!(b.in_burst(1), "but the burst is remembered");
        // The stream runs again, in sequence.
        let back = t0 + ms(10_000);
        feed(&mut b, 1, 110, 10, &[], back, ms(20));
        let ended = poll_over(&mut b, back, back + ms(1000));
        assert_eq!(ended.len(), 1);
        assert!(ended[0].1.beyond_repair);
        assert_eq!(ended[0].1.unrepaired, 3);
    }

    #[test]
    fn a_burst_longer_than_retransmission_covers_is_unrepaired_at_its_end() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // 600 packets missing in one step: more than the window.
        b.on_packet(1, 1000, t0, Arrived::Media);
        b.on_packet(1, 1601, t0 + ms(2400), Arrived::Media);
        assert_eq!(b.open_holes() as u64, RETRANSMISSION_WINDOW);
        let resumed = t0 + ms(2400);
        feed(&mut b, 1, 1602, 20, &[], resumed + ms(4), ms(4));
        // Every tracked hole is repaired at once; the 88 beyond the window
        // cannot be.
        for seq in (1601 - RETRANSMISSION_WINDOW)..1601 {
            b.on_packet(1, seq, resumed + ms(60), Arrived::Media);
        }
        assert_eq!(b.open_holes(), 0);
        let ended = poll_over(&mut b, resumed, resumed + ms(1000));
        assert_eq!(ended.len(), 1);
        let (after, end) = ended[0];
        assert!(end.beyond_repair);
        // 20 more went out of the window with the packets in sequence.
        assert_eq!((end.lost, end.unrepaired), (600, 108));
        assert!(after <= BURST_END_QUIET + ms(20), "{after:?}");
    }

    #[test]
    fn holes_are_bounded_and_a_stream_that_starts_over_is_not_a_burst() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // Every second packet missing for a long time.
        for i in 0..5000u64 {
            b.on_packet(1, 2 * i, t0, Arrived::Media);
        }
        assert!(b.open_holes() as u64 <= RETRANSMISSION_WINDOW);
        // A step too large to be loss, either way: the stream starts over.
        b.on_packet(1, 10_000 + MAX_JUMP + 1, t0, Arrived::Media);
        assert_eq!(b.open_holes(), 0);
        assert!(!b.in_burst(1));
        b.on_packet(1, 5, t0, Arrived::Media);
        assert!(!b.in_burst(1));
        // A late packet from further back than the window changes nothing.
        let mut late = BurstEnds::new();
        late.on_packet(1, 2000, t0, Arrived::Media);
        late.on_packet(1, 2003, t0, Arrived::Media);
        late.on_packet(1, 2003 - RETRANSMISSION_WINDOW - 50, t0, Arrived::Media);
        assert!(late.in_burst(1));
        assert_eq!(late.open_holes(), 2);
        // Many streams: the table starts over instead of growing.
        for stream in 10..60u8 {
            b.on_packet(stream, 1, t0, Arrived::Media);
        }
        assert!(b.streams.len() <= MAX_STREAMS);
    }

    #[test]
    fn a_key_frame_that_has_started_arriving_closes_what_was_missing_before_it() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        let lost: Vec<u64> = (100..150).filter(|s| s % 5 != 0).collect();
        feed(&mut b, 1, 100, 50, &lost, t0, ms(4));
        assert!(b.in_burst(1));
        // The publisher sends a key frame of its own accord.
        b.on_packet(1, 150, t0 + ms(200), Arrived::KeyFrameStart);
        assert!(!b.in_burst(1));
        feed(&mut b, 1, 151, 500, &[], t0 + ms(204), ms(4));
        assert!(poll_over(&mut b, t0, t0 + ms(4000)).is_empty());

        // A hole after the key frame's first packet is still a hole.
        let t1 = t0 + ms(5000);
        feed(&mut b, 1, 651, 10, &[655], t1, ms(4));
        b.on_packet(1, 661, t1 + ms(40), Arrived::KeyFrameStart);
        b.on_packet(1, 663, t1 + ms(44), Arrived::Media);
        assert!(b.in_burst(1));
        assert_eq!(b.open_holes(), 1, "655 is forgotten, 662 is not");
    }

    #[test]
    fn each_stream_is_judged_on_its_own() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // Stream 1 loses and never repairs; stream 2 loses and repairs;
        // stream 3 loses nothing.
        feed(&mut b, 1, 100, 20, &[105, 106, 107], t0, ms(4));
        feed(&mut b, 2, 500, 20, &[505, 506], t0, ms(4));
        feed(&mut b, 3, 900, 20, &[], t0, ms(4));
        b.on_packet(2, 505, t0 + ms(100), Arrived::Media);
        b.on_packet(2, 506, t0 + ms(100), Arrived::Media);
        for stream in 1..=3u8 {
            feed(
                &mut b,
                stream,
                [0, 120, 520, 920][usize::from(stream)],
                200,
                &[],
                t0 + ms(80),
                ms(4),
            );
        }
        let ended = poll_over(&mut b, t0, t0 + ms(3000));
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].1.stream, 1);
    }
    /// A stream in which every third packet is FEC: `first..first + n`,
    /// 4 ms apart from `t0`, without the sequence numbers in `skip`.
    fn feed_with_fec(b: &mut BurstEnds<u8>, first: u64, n: u64, skip: &[u64], t0: Instant) {
        for i in 0..n {
            let seq = first + i;
            if skip.contains(&seq) {
                continue;
            }
            let what = if seq % 3 == 2 {
                Arrived::Fec
            } else {
                Arrived::Media
            };
            b.on_packet(1, seq, t0 + ms(4 * i), what);
        }
    }

    #[test]
    fn on_a_stream_with_fec_a_lost_packet_or_two_never_ask_for_anything() {
        let t0 = Instant::now();
        let mut b = BurstEnds::new();
        // Ten seconds; every 125th packet is lost and never comes again,
        // and once two in a row are.
        let mut skip: Vec<u64> = (0..2500).filter(|s| s % 125 == 60).collect();
        skip.push(1061);
        // Fed and polled as time passes, packet by packet.
        for i in 0..2500u64 {
            let at = t0 + ms(4 * i);
            feed_with_fec(&mut b, i, 1, &skip, at);
            assert!(b.poll(at).is_empty(), "asked at packet {i}");
        }
        assert!(poll_over(&mut b, t0 + ms(10_000), t0 + ms(14_000)).is_empty());
        assert!(!b.in_burst(1), "each is closed without a word");
        assert_eq!(b.open_holes(), 0);
    }

    #[test]
    fn on_a_stream_with_fec_only_more_than_its_share_left_missing_is_unrepaired() {
        let t0 = Instant::now();
        // A second of a clean stream first, so the share is known: 1/3.
        // Then 60 packets lost in a row.
        let lost: Vec<u64> = (300..360).collect();
        let run = |repaired: &[u64]| {
            let mut b = BurstEnds::new();
            feed_with_fec(&mut b, 0, 400, &lost, t0);
            let after = t0 + ms(4 * 400);
            for seq in repaired {
                b.on_packet(1, *seq, after + ms(60), Arrived::Media);
            }
            poll_over(&mut b, after, after + ms(3000))
        };
        // Every media packet came again; the 20 FEC packets did not.
        let media: Vec<u64> = lost.iter().copied().filter(|s| s % 3 != 2).collect();
        assert!(run(&media).is_empty(), "what is missing is FEC's share");
        // Three media packets short of that: still within the slack and
        // the half again.
        assert!(run(&media[3..]).is_empty());
        // Nothing came again: 60 missing against a share of 20.
        let ended = run(&[]);
        assert_eq!(ended.len(), 1);
        assert_eq!((ended[0].1.lost, ended[0].1.unrepaired), (60, 60));
        // Half of the media came again: 40 missing, more than 2 + 30.
        let ended = run(&media[..20]);
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].1.unrepaired, 40);
    }
}
