// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Sequence-number and timestamp mapping for one downstream RTP stream
//! ([ADR-0066](../../../docs/adr/0066-sfu-forwards-rtp-packets.md)).
//!
//! The SFU forwards a publisher's RTP packets to each subscriber on a
//! stream of the subscriber's own. A [`StreamMap`] decides which sequence
//! number and RTP timestamp a forwarded packet carries there.
//!
//! ## The rule
//!
//! A downstream value is the publisher's value plus an offset that stays
//! fixed for as long as the downstream stream is fed from one source
//! stream. Three consequences, each the point of the ADR:
//!
//! - **A gap stays a gap.** A packet the SFU never received leaves its
//!   sequence number unused downstream, so the subscriber sees the loss and
//!   can ask for the packet again.
//! - **A late packet lands in its own slot.** A reordered packet, or a
//!   retransmission the publisher sent after a request, maps to the number
//!   it would have had on time and fills the subscriber's gap.
//! - **Nothing depends on arrival order**, so nothing has to be held.
//!
//! ## Where the numbering starts
//!
//! The first packet forwarded keeps the publisher's 16-bit wire sequence
//! number and RTP timestamp; only the rollover count is dropped. `str0m`
//! hands the SFU extended (64-bit) sequence numbers, and a subscriber that
//! joins after the publisher's sequence number has wrapped would otherwise
//! be sent a first packet whose extended number is past 65535. SRTP derives
//! its packet index from that count and a receiver assumes it starts at
//! zero, so the downstream stream has to start inside the first cycle.
//!
//! ## Changing the source stream
//!
//! A simulcast layer switch (or a publisher that starts sending under a new
//! SSRC) replaces the source stream under an unchanged downstream stream.
//! [`StreamMap::rebase`] picks new offsets at the first packet taken from
//! the new source, so the downstream sequence number continues at the next
//! unused value and the timestamp moves on by the time that actually
//! passed. Packets of the new source that precede that first packet have no
//! slot downstream and are refused.
//!
//! ## The wall clock of a packet
//!
//! `str0m` builds its RTCP sender reports from the last packet written: that
//! packet's RTP timestamp and the wall-clock time it was given. The SFU has
//! no better clock for a publisher's media than the moment a packet reached
//! it, so a packet that advances the stream is given its arrival time. A
//! late packet is not: its timestamp is old and its arrival is new, and a
//! sender report built right after it would tell the subscriber the stream
//! is running late by however long the packet took to be repaired. It is
//! given the time its timestamp stands for on the mapping the newest packet
//! set — the newest packet's arrival, minus the distance between the two
//! timestamps.
//!
//! Pure bookkeeping: no payload, no key, no I/O.

use std::time::{Duration, Instant};

/// Mask selecting the rollover count of an extended sequence number, i.e.
/// everything above the 16 bits that travel in the RTP header.
const ROLLOVER_MASK: u64 = !0xffff;

/// Where one forwarded packet sits on the downstream stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapped {
    /// Extended downstream sequence number. The low 16 bits are the RTP
    /// header value.
    pub seq_no: u64,
    /// Downstream RTP timestamp, in the clock rate of the payload.
    pub rtp_time: u32,
    /// Wall-clock time `rtp_time` corresponds to, for the sender reports
    /// str0m builds: the packet's arrival when it advances the stream, the
    /// matching earlier moment when it is a late packet.
    pub wallclock: Instant,
}

/// Mapping from one source stream onto one downstream stream.
///
/// Created by [`StreamMap::start`] with the first packet forwarded, advanced
/// by [`StreamMap::map`] for every later packet of the same source stream,
/// and moved onto a different source stream by [`StreamMap::rebase`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamMap {
    /// Lowest source sequence number that has a downstream slot.
    floor_source: u64,
    /// The downstream sequence number `floor_source` maps to.
    floor_downstream: u64,
    /// Added (modulo 2^32) to a source timestamp.
    time_offset: u32,
    /// Highest downstream sequence number handed out so far.
    newest_seq_no: u64,
    /// Downstream timestamp of the packet that carried `newest_seq_no`.
    newest_rtp_time: u32,
    /// When that packet reached the SFU.
    newest_arrival: Instant,
}

impl StreamMap {
    /// Begin a downstream stream at the first packet forwarded to it.
    ///
    /// `seq_no` is the source's extended sequence number, `rtp_time` its RTP
    /// timestamp and `arrival` the time the packet reached the SFU. Returns
    /// the map together with that packet's own downstream position.
    #[must_use]
    pub fn start(seq_no: u64, rtp_time: u32, arrival: Instant) -> (Self, Mapped) {
        let floor_source = seq_no & ROLLOVER_MASK;
        let first = Mapped {
            seq_no: seq_no - floor_source,
            rtp_time,
            wallclock: arrival,
        };
        let map = Self {
            floor_source,
            floor_downstream: 0,
            time_offset: 0,
            newest_seq_no: first.seq_no,
            newest_rtp_time: first.rtp_time,
            newest_arrival: arrival,
        };
        (map, first)
    }

    /// Map a packet of the current source stream. `clock_hz` is the clock
    /// rate of its payload.
    ///
    /// `None` when the packet precedes everything this downstream stream can
    /// number: it is older than the cycle the stream started in, or older
    /// than the packet the stream was last re-based at. Such a packet would
    /// collide with a number already used, so it is not forwarded.
    pub fn map(
        &mut self,
        seq_no: u64,
        rtp_time: u32,
        arrival: Instant,
        clock_hz: u32,
    ) -> Option<Mapped> {
        let distance = seq_no.checked_sub(self.floor_source)?;
        let seq_no = self.floor_downstream.checked_add(distance)?;
        let rtp_time = rtp_time.wrapping_add(self.time_offset);
        if seq_no > self.newest_seq_no {
            self.newest_seq_no = seq_no;
            self.newest_rtp_time = rtp_time;
            self.newest_arrival = arrival;
            return Some(Mapped {
                seq_no,
                rtp_time,
                wallclock: arrival,
            });
        }
        Some(Mapped {
            seq_no,
            rtp_time,
            wallclock: self
                .wallclock_of_late(rtp_time, clock_hz)
                .unwrap_or(arrival),
        })
    }

    /// The moment a late packet's timestamp stands for: the newest packet's
    /// arrival, moved back by how far the late packet's timestamp is behind
    /// the newest one. `None` when that cannot be computed (the timestamp is
    /// not behind at all, or the result would precede the clock's origin);
    /// the caller then falls back on the arrival time.
    fn wallclock_of_late(&self, rtp_time: u32, clock_hz: u32) -> Option<Instant> {
        let behind = self.newest_rtp_time.wrapping_sub(rtp_time);
        if behind > u32::MAX / 2 {
            return None;
        }
        let micros = u64::from(behind)
            .checked_mul(1_000_000)?
            .checked_div(u64::from(clock_hz))?;
        self.newest_arrival
            .checked_sub(Duration::from_micros(micros))
    }

    /// Continue the downstream stream from a different source stream,
    /// starting at this packet.
    ///
    /// The packet takes the next unused downstream sequence number. Its
    /// timestamp is the newest downstream timestamp plus the time that
    /// passed between the two arrivals, in `clock_hz` ticks and never less
    /// than one tick, so the downstream timeline keeps moving forward at the
    /// rate of the wall clock across the change.
    pub fn rebase(
        &mut self,
        seq_no: u64,
        rtp_time: u32,
        arrival: Instant,
        clock_hz: u32,
    ) -> Mapped {
        let elapsed = arrival.saturating_duration_since(self.newest_arrival);
        let ticks = elapsed.as_micros().saturating_mul(u128::from(clock_hz)) / 1_000_000;
        // Half the timestamp space is the furthest a receiver can tell
        // "later" from "earlier"; a longer silence is clamped to it.
        let ticks = u32::try_from(ticks)
            .unwrap_or(u32::MAX)
            .clamp(1, u32::MAX / 2);
        let mapped = Mapped {
            seq_no: self.newest_seq_no.saturating_add(1),
            rtp_time: self.newest_rtp_time.wrapping_add(ticks),
            wallclock: arrival,
        };
        self.floor_source = seq_no;
        self.floor_downstream = mapped.seq_no;
        self.time_offset = mapped.rtp_time.wrapping_sub(rtp_time);
        self.newest_seq_no = mapped.seq_no;
        self.newest_rtp_time = mapped.rtp_time;
        self.newest_arrival = arrival;
        mapped
    }

    /// Downstream sequence number minus the source's, modulo 2^16: what a
    /// field that names a source sequence number on the wire (the `SN base`
    /// of an FEC packet, see [`crate::video_fec`]) has to be moved by. Zero
    /// until the stream is re-based.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // the low 16 bits are the point
    pub const fn seq_offset(&self) -> u16 {
        self.floor_downstream.wrapping_sub(self.floor_source) as u16
    }

    /// Downstream RTP timestamp minus the source's, modulo 2^32. Zero until
    /// the stream is re-based.
    #[must_use]
    pub const fn time_offset(&self) -> u32 {
        self.time_offset
    }

    /// Highest downstream sequence number handed out so far.
    #[must_use]
    pub const fn newest_seq_no(&self) -> u64 {
        self.newest_seq_no
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use str0m::rtp::SeqNo;

    /// The RTP header value of an extended downstream sequence number, taken
    /// the way str0m takes it when it writes the packet.
    fn wire(mapped: Mapped) -> u16 {
        SeqNo::from(mapped.seq_no).as_u16()
    }

    #[test]
    fn the_first_packet_keeps_the_publishers_wire_values() {
        let t0 = Instant::now();
        let (map, first) = StreamMap::start(1_234, 90_000, t0);
        assert_eq!(
            first,
            Mapped {
                seq_no: 1_234,
                rtp_time: 90_000,
                wallclock: t0,
            }
        );
        assert_eq!(map.newest_seq_no(), 1_234);
    }

    #[test]
    fn a_stream_joined_after_a_rollover_starts_in_the_first_cycle() {
        // The publisher's sequence number has wrapped three times.
        let source: u64 = (3 << 16) + 4_464;
        let (_, first) = StreamMap::start(source, 7, Instant::now());
        assert_eq!(first.seq_no, 4_464, "the rollover count is dropped");
        assert_eq!(
            SeqNo::from(first.seq_no).roc(),
            0,
            "SRTP needs the first packet in cycle zero"
        );
        assert_eq!(
            wire(first),
            SeqNo::from(source).as_u16(),
            "the wire value is the publisher's"
        );
    }

    #[test]
    fn packets_in_order_map_one_to_one() {
        let t0 = Instant::now();
        let (mut map, first) = StreamMap::start(500, 1_000, t0);
        let second = map.map(501, 1_960, t0, 90_000).expect("in range");
        let third = map.map(502, 2_920, t0, 90_000).expect("in range");
        assert_eq!([first.seq_no, second.seq_no, third.seq_no], [500, 501, 502]);
        assert_eq!(
            [first.rtp_time, second.rtp_time, third.rtp_time],
            [1_000, 1_960, 2_920]
        );
        assert_eq!(map.newest_seq_no(), 502);
    }

    #[test]
    fn a_gap_in_the_source_is_a_gap_downstream() {
        let t0 = Instant::now();
        let (mut map, first) = StreamMap::start(500, 0, t0);
        // 501 and 502 never reached the SFU.
        let after = map.map(503, 2_880, t0, 90_000).expect("in range");
        assert_eq!(
            after.seq_no - first.seq_no,
            3,
            "the two missing numbers stay unused"
        );
    }

    #[test]
    fn a_late_packet_maps_to_its_own_slot_and_does_not_move_the_head() {
        let t0 = Instant::now();
        let (mut map, _) = StreamMap::start(500, 0, t0);
        let head = map
            .map(503, 2_880, t0 + Duration::from_millis(60), 90_000)
            .expect("in range");
        // 501 was retransmitted and arrives after 503.
        let late = map
            .map(501, 960, t0 + Duration::from_millis(90), 90_000)
            .expect("in range");
        assert_eq!(late.seq_no, 501, "it fills the slot it left empty");
        assert_eq!(late.rtp_time, 960);
        assert_eq!(map.newest_seq_no(), head.seq_no, "the head stays at 503");
        // A re-base after the late packet still continues from the head.
        let next = map.rebase(9_000, 5, t0 + Duration::from_millis(100), 48_000);
        assert_eq!(next.seq_no, 504);
    }

    /// What str0m's sender report sees: a packet that advances the stream is
    /// stamped with its arrival, a late one with the moment its timestamp
    /// stands for, so a report built after either says the same thing.
    #[test]
    fn a_late_packet_is_given_the_wall_clock_its_timestamp_stands_for() {
        let t0 = Instant::now() + Duration::from_secs(10);
        let (mut map, first) = StreamMap::start(500, 90_000, t0);
        assert_eq!(first.wallclock, t0);

        // Two frames later (66 ms at 90 kHz), arriving 70 ms after the first.
        let head_arrival = t0 + Duration::from_millis(70);
        let head = map
            .map(503, 90_000 + 5_940, head_arrival, 90_000)
            .expect("in range");
        assert_eq!(head.wallclock, head_arrival, "the head keeps its arrival");

        // A packet of the frame in between is repaired 120 ms later still.
        let late = map
            .map(
                501,
                90_000 + 2_970,
                head_arrival + Duration::from_millis(120),
                90_000,
            )
            .expect("in range");
        assert_eq!(
            late.wallclock,
            head_arrival
                .checked_sub(Duration::from_millis(33))
                .expect("after the clock's origin"),
            "33 ms of timestamp behind the head is 33 ms before the head's arrival"
        );

        // A late packet of the head's own frame shares its moment.
        let same_frame = map
            .map(
                502,
                90_000 + 5_940,
                head_arrival + Duration::from_millis(200),
                90_000,
            )
            .expect("in range");
        assert_eq!(same_frame.wallclock, head_arrival);
    }

    #[test]
    fn a_packet_from_before_the_first_one_forwarded_still_has_a_slot() {
        let t0 = Instant::now();
        let (mut map, _) = StreamMap::start(500, 5_000, t0);
        let earlier = map.map(498, 3_080, t0, 90_000).expect("same cycle");
        assert_eq!(earlier.seq_no, 498);
    }

    #[test]
    fn a_packet_older_than_the_starting_cycle_is_refused() {
        let t0 = Instant::now();
        // Started three packets into the second cycle.
        let (mut map, first) = StreamMap::start((1 << 16) | 3, 0, t0);
        assert_eq!(first.seq_no, 3);
        assert_eq!(
            map.map((1 << 16) - 1, 0, t0, 90_000),
            None,
            "there is no downstream number below zero"
        );
        assert_eq!(map.map(1 << 16, 0, t0, 90_000).map(|m| m.seq_no), Some(0));
    }

    #[test]
    fn the_sixteen_bit_wire_value_wraps_while_the_extended_one_keeps_counting() {
        let t0 = Instant::now();
        let (mut map, first) = StreamMap::start(65_534, 0, t0);
        let second = map.map(65_535, 0, t0, 90_000).expect("in range");
        let third = map.map(65_536, 0, t0, 90_000).expect("in range");
        let fourth = map.map(65_537, 0, t0, 90_000).expect("in range");
        assert_eq!(
            [wire(first), wire(second), wire(third), wire(fourth)],
            [65_534, 65_535, 0, 1]
        );
        assert_eq!(
            [first.seq_no, second.seq_no, third.seq_no, fourth.seq_no],
            [65_534, 65_535, 65_536, 65_537],
            "str0m's extended number carries the rollover into SRTP"
        );
        assert_eq!(SeqNo::from(third.seq_no).roc(), 1);
    }

    #[test]
    fn the_timestamp_wraps_modulo_two_to_the_thirty_two() {
        let t0 = Instant::now();
        let (mut map, first) = StreamMap::start(10, u32::MAX - 1_000, t0);
        let next = map.map(11, 1_999, t0, 90_000).expect("in range");
        assert_eq!(first.rtp_time, u32::MAX - 1_000);
        assert_eq!(next.rtp_time, 1_999, "offset zero: the source value");
    }

    #[test]
    fn a_rebase_continues_the_sequence_and_moves_time_by_the_wall_clock() {
        let t0 = Instant::now();
        let (mut map, _) = StreamMap::start(700, 10_000, t0);
        let last_old = map
            .map(702, 16_000, t0 + Duration::from_millis(66), 90_000)
            .expect("in range");

        // The new layer has its own, unrelated numbering and timestamps.
        let switch_at = t0 + Duration::from_millis(106);
        let first_new = map.rebase(40_000, 3_000_000_000, switch_at, 90_000);
        assert_eq!(
            first_new.seq_no,
            last_old.seq_no + 1,
            "no hole and no repeat at the switch"
        );
        assert_eq!(
            first_new.rtp_time,
            last_old.rtp_time + 3_600,
            "40 ms at 90 kHz after the last packet of the old layer"
        );

        // The new layer then maps with its new, fixed offsets.
        let second_new = map
            .map(40_001, 3_000_003_000, switch_at, 90_000)
            .expect("in range");
        assert_eq!(second_new.seq_no, first_new.seq_no + 1);
        assert_eq!(second_new.rtp_time, first_new.rtp_time + 3_000);
        // A gap on the new layer is preserved like any other.
        let after_gap = map
            .map(40_004, 3_000_006_000, switch_at, 90_000)
            .expect("in range");
        assert_eq!(after_gap.seq_no, first_new.seq_no + 4);
    }

    #[test]
    fn packets_of_the_new_source_from_before_the_rebase_are_refused() {
        let t0 = Instant::now();
        let (mut map, _) = StreamMap::start(700, 0, t0);
        let first_new = map.rebase(40_000, 0, t0, 90_000);
        assert_eq!(
            map.map(39_999, 0, t0, 90_000),
            None,
            "its number downstream belongs to the old layer"
        );
        assert_eq!(
            map.map(40_000, 0, t0, 90_000).map(|m| m.seq_no),
            Some(first_new.seq_no)
        );
    }

    #[test]
    fn a_rebase_always_moves_the_timestamp_forward() {
        let t0 = Instant::now();
        let (mut map, first) = StreamMap::start(1, 500, t0);
        // Same instant, and even an arrival clock that reads earlier.
        let same = map.rebase(90, 77, t0, 90_000);
        assert_eq!(same.rtp_time, first.rtp_time + 1, "at least one tick");
        let earlier = map.rebase(
            900,
            77,
            t0.checked_sub(Duration::from_millis(5)).unwrap_or(t0),
            90_000,
        );
        assert_eq!(earlier.rtp_time, same.rtp_time + 1);
        assert_eq!(earlier.seq_no, same.seq_no + 1);
    }

    #[test]
    fn a_rebase_after_a_long_silence_is_clamped() {
        let t0 = Instant::now();
        let (mut map, first) = StreamMap::start(1, 0, t0);
        let late = map.rebase(5, 0, t0 + Duration::from_secs(60 * 60 * 24), 90_000);
        assert_eq!(late.rtp_time, first.rtp_time + u32::MAX / 2);
    }
}
