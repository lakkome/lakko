// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The SFU's estimate of the bitrate one publisher's path carries
//! ([ADR-0068](../../../docs/adr/0068-server-side-bitrate-control.md)).
//!
//! The SFU is the receiver of every publisher's media. For each packet
//! it knows when the publisher sent it (`abs-send-time`), when it arrived
//! and how large it was, and it sees the loss. From that it estimates what
//! the path carries and tells the publisher with REMB; the publisher's
//! target is then the smaller of that figure and its own receiver-report
//! loss rule.
//!
//! This module is the estimate alone: no I/O, no clock of its own (the
//! caller supplies every time), no RTP parsing. It reads a send time, an
//! arrival time and a size per packet, never a payload (invariant #2,
//! C-39).
//!
//! ## The algorithm
//!
//! It is the receive-side form of Google congestion control, the
//! well-known algorithm libwebrtc runs as `RemoteBitrateEstimatorAbsSendTime`
//! (`InterArrival` + trendline over-use detection + `AimdRateControl`),
//! written here from its description and from the libwebrtc M125 source.
//! The vendored `str0m` carries a port of the same pieces under
//! `src/bwe/delay/`, but privately and typed to its send-side transport
//! feedback records (`AckedPacket`, `TwccSeq`): using them from outside
//! the crate would have needed a fork change exposing four private modules
//! and feeding them invented feedback records, so the pieces are
//! implemented here instead. Where this module departs from libwebrtc is
//! what ADR-0068 section 2 asks for, and each departure is named at its
//! constant:
//!
//! - **delay.** Packets are grouped by send time; the difference between
//!   two groups' arrival spacing and send spacing is one delay sample; a
//!   line is fitted over the last samples and a slope above an adaptive
//!   threshold, sustained, is *over-use*: packets are queueing, the path
//!   is narrower than what is sent. On over-use the estimate goes to 0.85
//!   of the rate actually arriving.
//! - **increase.** While the detector is normal the estimate rises by 8%
//!   a second, capped at 1.5 times the highest rate that arrived in the
//!   recent past (so a pause in arrival does not lower the cap) and at
//!   the configured maximum.
//! - **loss.** Loss alone lowers the estimate only when it lasts: a burst
//!   shorter than [`LOSS_WINDOW`] with no over-use changes nothing; loss
//!   past the window lowers the estimate in steps.
//! - **bounds.** A configured start value and maximum, and the video
//!   encoder's own minimum as the only floor.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

// ------------------------------------------------------------
// Constants
// ------------------------------------------------------------

/// Start value of the estimate when `LAKKO_SFU_BITRATE_START_KBPS` is
/// unset, in bits per second.
///
/// Chosen, not measured. A libwebrtc sender starts at 300 kbit/s and,
/// until two seconds after its first receiver report, raises its own
/// estimate to the REMB it is sent; this is the figure it jumps to. A path
/// that cannot carry it shows as growing delay and is cut by the detector.
pub const DEFAULT_START_BPS: u64 = 1_500_000;

/// Ceiling of the estimate when `LAKKO_SFU_BITRATE_MAX_KBPS` is unset, in
/// bits per second.
///
/// Chosen, not measured: meant to sit above what one camera stream with
/// audio, retransmissions and FEC asks for, so that it is the encoder's own
/// maximum and not this figure that bounds a publisher on a good path.
pub const DEFAULT_MAX_BPS: u64 = 5_000_000;

/// Floor of the estimate, in bits per second: libwebrtc's minimum video
/// bitrate (`kMinVideoBitrateBps`, 30 kbit/s). ADR-0068 allows no floor
/// above the codec's minimum; a figure below it would ask an encoder for
/// less than it can produce.
pub const MIN_ESTIMATE_BPS: u64 = 30_000;

/// Fractional bits of the 24-bit `abs-send-time` value (6.18 fixed point
/// seconds, RFC draft `abs-send-time`): one unit is 2^-18 s.
const ABS_SEND_TIME_FRACTION_BITS: u32 = 18;
/// The value wraps at 2^24 units, 64 seconds.
const ABS_SEND_TIME_MODULUS: i64 = 1 << 24;

/// Packets sent within this span of a group's first packet belong to the
/// group. libwebrtc: `kTimestampGroupLengthMs` = 5.
const SEND_GROUP_LENGTH_MS: f64 = 5.0;
/// A packet that arrives within this long of the group's last, having
/// been delayed less than it, is part of the same burst. libwebrtc:
/// `kBurstDeltaThreshold` = 5 ms.
const BURST_ARRIVAL_MS: f64 = 5.0;
/// Longest a burst may run and still be one group. libwebrtc:
/// `kMaxBurstDuration` = 100 ms.
const MAX_BURST_DURATION_MS: f64 = 100.0;
/// A gap in arrival longer than this starts the delay detector over; the
/// estimate is kept. libwebrtc: `kStreamTimeOut` = 2 s.
const STREAM_TIMEOUT: Duration = Duration::from_secs(2);
/// A single sample whose arrival spacing differs from its send spacing by
/// more than this is a clock jump, not a queue; the detector starts over.
/// libwebrtc: `kArrivalTimeOffsetThresholdMs` = 3000.
const ARRIVAL_OFFSET_RESET_MS: f64 = 3000.0;

/// Delay samples the line is fitted over. libwebrtc trendline:
/// `kDefaultTrendlineWindowSize` = 20.
const TRENDLINE_WINDOW: usize = 20;
/// Exponential smoothing of the accumulated delay. libwebrtc:
/// `kDefaultTrendlineSmoothingCoeff` = 0.9.
const TRENDLINE_SMOOTHING: f64 = 0.9;
/// Gain on the fitted slope before it is compared with the threshold.
/// libwebrtc: `kDefaultTrendlineThresholdGain` = 4.0.
const TRENDLINE_GAIN: f64 = 4.0;
/// The slope is scaled by the number of samples seen, up to this many.
/// libwebrtc: `kMinNumDeltas` = 60.
const TRENDLINE_MAX_DELTAS: f64 = 60.0;
/// Initial over-use threshold, in milliseconds. libwebrtc: 12.5.
const THRESHOLD_INITIAL_MS: f64 = 12.5;
/// Bounds of the adaptive threshold. libwebrtc: 6 and 600.
const THRESHOLD_MIN_MS: f64 = 6.0;
const THRESHOLD_MAX_MS: f64 = 600.0;
/// Threshold adaptation gains, upwards and downwards. libwebrtc: `k_up`
/// 0.0087, `k_down` 0.039.
const THRESHOLD_K_UP: f64 = 0.0087;
const THRESHOLD_K_DOWN: f64 = 0.039;
/// A slope this far above the threshold is a spike the threshold does not
/// adapt to. libwebrtc: `kMaxAdaptOffsetMs` = 15.
const THRESHOLD_MAX_ADAPT_OFFSET_MS: f64 = 15.0;
/// Longest time step the threshold adapts over. libwebrtc: 100 ms.
const THRESHOLD_MAX_STEP_MS: f64 = 100.0;
/// The slope must stay above the threshold for this long, over at least
/// two samples, to count as over-use. libwebrtc:
/// `kOverUsingTimeThreshold` = 10 ms.
const OVERUSE_TIME_THRESHOLD_MS: f64 = 10.0;

/// On over-use the estimate goes to this fraction of the arriving rate.
/// libwebrtc: `kDefaultBackoffFactor` = 0.85; the figure ADR-0068 names.
const BACKOFF_FACTOR: f64 = 0.85;
/// Multiplicative increase per second while the detector is normal.
/// libwebrtc: `MultiplicativeRateIncrease`, alpha = 1.08. libwebrtc
/// switches to a slower additive increase once it believes it is near the
/// link capacity; this module does not, so that an estimate cut by real
/// congestion comes back at one known rate.
const INCREASE_PER_SECOND: f64 = 1.08;
/// Least increase per rate update, in bits per second. libwebrtc: 1000.
const MIN_INCREASE_BPS: f64 = 1000.0;
/// The estimate is not raised beyond this multiple of the highest rate
/// that arrived recently, plus [`INCREASE_CAP_SLACK_BPS`]. libwebrtc:
/// `1.5 * estimated_throughput + 10 kbps`, against the current throughput;
/// ADR-0068 asks for the recent highest instead.
const INCREASE_CAP_FACTOR: f64 = 1.5;
/// See [`INCREASE_CAP_FACTOR`]. libwebrtc: 10 kbit/s.
const INCREASE_CAP_SLACK_BPS: f64 = 10_000.0;
/// How far back "recent" reaches for the highest arriving rate.
///
/// Chosen, not measured: longer than the loss bursts seen on live calls
/// (about a second every 30 s) and than a camera encoder's quiet spell on
/// a still picture, short enough that a publisher who turned the camera
/// off is not held to a video-sized cap for long.
const PEAK_WINDOW: Duration = Duration::from_secs(10);
/// The rate control runs at most this often, except on over-use.
/// libwebrtc: `AimdRateControl::GetFeedbackInterval`, lower bound 200 ms.
const RATE_UPDATE_INTERVAL: Duration = Duration::from_millis(200);
/// After a cut for over-use, no further cut for this long unless the
/// arriving rate is under half the estimate. libwebrtc:
/// `TimeToReduceFurther`, the round-trip time clamped to 10..200 ms; the
/// SFU does not know the round-trip time here and uses the upper bound.
const DECREASE_INTERVAL: Duration = Duration::from_millis(200);

// ---- Lakko's own rules around a cut ----
//
// libwebrtc's rule on over-use is "0.85 of the rate arriving now", and then
// a rise of 8% a second. Measured on live calls (2026-10-04) that is what
// turns a one-second hiccup of a home network into half a minute: during the
// hiccup next to nothing arrives, so the estimate went from 1500 kbit/s to
// 44-106 kbit/s in one step and then crept back. The cut itself is kept: it
// is the right answer when a link has really narrowed, and it cannot be told
// from a hiccup while it happens. What differs is what follows it. The
// constants below are chosen, not measured.

/// Over-use while less than this share of the estimate is arriving says the
/// path is carrying next to nothing, not how much it can carry.
const BLACKOUT_FRACTION: f64 = 0.2;

/// How long such a blackout is waited out before it is taken for the link's
/// real capacity and cut on.
const BLACKOUT_HOLD: Duration = Duration::from_secs(2);

/// Cuts this close to the first cut belong to one episode.
const EPISODE_LENGTH: Duration = Duration::from_secs(3);

/// A cut later than [`EPISODE_LENGTH`] after an episode began and sooner than
/// this after the previous cut means the link is narrower, not that it
/// hiccuped: the fast way back is given up.
const REPEAT_WINDOW: Duration = Duration::from_secs(10);

/// Within one episode the cuts together never take the estimate below this
/// share of what it was before the episode. Each cut makes the publisher
/// send less, so less arrives, so the next cut is deeper: left alone, three
/// cuts in a second took 2500 kbit/s to 65 kbit/s in the model of the
/// measured dip. A link that really narrowed further than this is reached
/// once the episode is over.
const EPISODE_FLOOR_FRACTION: f64 = 0.25;

/// The rise per second once this long has passed since the last cut with no
/// over-use, and the faster one after twice as long.
///
/// libwebrtc rises 8% a second and finds a path's capacity with probe
/// clusters; a sender that follows REMB has no use for its probes, so here
/// the rise itself is the probe. Seen on a live call (2026-10-04): a mobile
/// uplink showed delay growth in its first seconds, the estimate was cut to
/// 78 kbit/s, and at 8% a second it took a minute and a half to reach the
/// 2500 kbit/s that network then carried. Right after a cut the rise stays
/// careful, so a path that really is narrow is approached slowly; a path
/// that stays quiet is climbed fast. Chosen, not measured.
const QUIET_AFTER_CUT: Duration = Duration::from_secs(3);
const INCREASE_PER_SECOND_QUIET: f64 = 1.25;
const INCREASE_PER_SECOND_LONG_QUIET: f64 = 1.5;

/// The rise per second on the way back to the estimate held before an
/// episode: a hiccup that cut the estimate to a tenth is undone in under two
/// seconds, and a link that really narrowed answers with over-use as the
/// rate passes what it carries. Above that estimate the rise is
/// [`INCREASE_PER_SECOND`].
const RESTORE_PER_SECOND: f64 = 4.0;

/// Window of the arriving rate an over-use cut is taken from.
///
/// Chosen, not measured. libwebrtc's receive-side estimator uses one
/// second; half that follows a capacity drop sooner, at the price of a
/// noisier figure at low frame rates.
const ARRIVING_WINDOW: Duration = Duration::from_millis(500);
/// Window of the arriving rate the recent highest is taken from: one
/// second, libwebrtc's `kBitrateWindow`. Longer than [`ARRIVING_WINDOW`]
/// so that a burst released by a queue does not read as a high rate.
const PEAK_RATE_WINDOW: Duration = Duration::from_secs(1);
/// A rate window answers only after this much of it has been observed.
/// Chosen, not measured.
const MIN_RATE_SPAN: Duration = Duration::from_millis(200);
/// Most packets one rate window holds; the oldest is dropped beyond it.
/// 4096 packets in a second is about 40 Mbit/s of full-size packets.
const MAX_RATE_SAMPLES: usize = 4096;

/// A loss fraction above this is "lossy". libwebrtc's receiver-report
/// rule: `kDefaultHighLossThreshold` = 0.1.
const LOSS_HIGH_FRACTION: f64 = 0.10;
/// Loss lowers the estimate only after it has been above
/// [`LOSS_HIGH_FRACTION`] for this long without a clean interval.
/// Chosen (ADR-0068 section 2: 3 s), not measured.
pub const LOSS_WINDOW: Duration = Duration::from_secs(3);
/// Past the window, at most one loss step per this long. Chosen, not
/// measured: one step per loss interval the SFU feeds.
const LOSS_STEP_INTERVAL: Duration = Duration::from_secs(1);
/// A loss fraction is formed only over at least this many expected
/// packets; fewer are carried into the next interval. libwebrtc:
/// `kLimitNumPackets` = 20.
const MIN_LOSS_PACKETS: u64 = 20;

/// Most receive streams one [`SeqLossCounter`] follows. Chosen: three
/// simulcast layers, audio, and room to spare.
const MAX_LOSS_STREAMS: usize = 16;
/// A sequence number this far ahead of the highest seen is a stream that
/// started over, not this many lost packets. Chosen, not measured: about
/// ten seconds of a 3 Mbit/s stream.
const MAX_SEQ_JUMP: u64 = 3000;

// ------------------------------------------------------------
// Small numeric helpers
// ------------------------------------------------------------

/// A count or rate as `f64`. Every value this module converts is far
/// below 2^53, where the conversion is exact.
#[allow(clippy::cast_precision_loss)]
const fn as_f64(v: u64) -> f64 {
    v as f64
}

/// A non-negative finite `f64` as whole bits per second; anything else
/// (negative, NaN) is zero and an oversized value saturates.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn as_bps(v: f64) -> u64 {
    if v.is_finite() && v > 0.0 {
        // `as` saturates at `u64::MAX` for an oversized float.
        v as u64
    } else {
        0
    }
}

/// Milliseconds from `origin` to `at`, zero when `at` is earlier.
fn ms_since(origin: Instant, at: Instant) -> f64 {
    at.saturating_duration_since(origin).as_secs_f64() * 1000.0
}

// ------------------------------------------------------------
// Public types
// ------------------------------------------------------------

/// Start value and ceiling of one publisher's estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EstimatorConfig {
    /// The estimate before any packet has arrived, in bits per second.
    pub start_bps: u64,
    /// The estimate never exceeds this, in bits per second.
    pub max_bps: u64,
}

impl Default for EstimatorConfig {
    fn default() -> Self {
        Self {
            start_bps: DEFAULT_START_BPS,
            max_bps: DEFAULT_MAX_BPS,
        }
    }
}

/// What the delay detector currently believes about the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectorState {
    /// Delay is flat: the path carries what is sent.
    Normal,
    /// Delay is growing: packets are queueing.
    Overuse,
    /// Delay is shrinking: a queue is draining.
    Underuse,
}

impl DetectorState {
    /// Stable, low-cardinality label for logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Overuse => "overuse",
            Self::Underuse => "underuse",
        }
    }
}

/// Why the estimate last changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeReason {
    /// It has not changed: it is the configured start value.
    Start,
    /// Raised while the detector was normal.
    Increase,
    /// Cut to a fraction of the arriving rate on over-use.
    Overuse,
    /// Stepped down because loss lasted past [`LOSS_WINDOW`].
    Loss,
}

impl ChangeReason {
    /// Stable, low-cardinality label for logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Increase => "increase",
            Self::Overuse => "overuse",
            Self::Loss => "loss",
        }
    }
}

/// One received packet, as the estimator sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketArrival {
    /// The packet's `abs-send-time` header extension: 24 bits, 6.18 fixed
    /// point seconds, wrapping every 64 s. `None` when the packet carried
    /// none; it then counts toward the arriving rate only.
    pub send_time_24: Option<u32>,
    /// When the packet reached the SFU.
    pub arrival: Instant,
    /// Its size in bytes (RTP header and payload).
    pub size: usize,
    /// The packet repairs an earlier loss (it arrived on the
    /// retransmission stream, or behind a later packet of its stream).
    ///
    /// A retransmission crossed the path like any other packet and its
    /// sender stamps it with the time it was really sent, so it takes
    /// part in the delay samples and in the arriving rate; the flag is
    /// kept for the count in [`Snapshot::retransmitted`].
    pub retransmission: bool,
}

/// The estimator's figures for a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    /// The current estimate, in bits per second.
    pub estimate_bps: u64,
    /// The rate arriving over the last [`ARRIVING_WINDOW`], in bits per
    /// second; zero when too little has been observed.
    pub arriving_bps: u64,
    /// The delay detector's state.
    pub state: DetectorState,
    /// Why the estimate last changed.
    pub last_change: ChangeReason,
    /// Retransmitted packets seen since the estimator was created.
    pub retransmitted: u64,
}

// ------------------------------------------------------------
// Arriving rate
// ------------------------------------------------------------

/// Bytes that arrived over a sliding window.
#[derive(Debug)]
struct RateWindow {
    window: Duration,
    samples: VecDeque<(Instant, u64)>,
    bytes: u64,
    /// When the window began observing (creation, or the first packet
    /// after it ran empty).
    observing_since: Option<Instant>,
}

impl RateWindow {
    const fn new(window: Duration) -> Self {
        Self {
            window,
            samples: VecDeque::new(),
            bytes: 0,
            observing_since: None,
        }
    }

    fn evict(&mut self, now: Instant) {
        while let Some(&(at, size)) = self.samples.front() {
            if now.saturating_duration_since(at) < self.window
                && self.samples.len() <= MAX_RATE_SAMPLES
            {
                break;
            }
            self.bytes = self.bytes.saturating_sub(size);
            self.samples.pop_front();
        }
    }

    fn record(&mut self, at: Instant, size: usize) {
        self.evict(at);
        if self.observing_since.is_none() {
            self.observing_since = Some(at);
        }
        let size = u64::try_from(size).unwrap_or(u64::MAX);
        self.bytes = self.bytes.saturating_add(size);
        self.samples.push_back((at, size));
    }

    /// Bits per second over the window ending at `now`; `None` until
    /// [`MIN_RATE_SPAN`] has been observed.
    fn rate_bps(&mut self, now: Instant) -> Option<f64> {
        self.evict(now);
        let observed = now.saturating_duration_since(self.observing_since?);
        if observed < MIN_RATE_SPAN {
            return None;
        }
        let span = observed.min(self.window).as_secs_f64();
        Some(as_f64(self.bytes) * 8.0 / span)
    }
}

/// The highest value over a sliding window.
#[derive(Debug, Default)]
struct SlidingMax {
    /// Decreasing in value, increasing in time.
    samples: VecDeque<(Instant, f64)>,
}

impl SlidingMax {
    fn record(&mut self, at: Instant, value: f64) {
        while self.samples.back().is_some_and(|&(_, v)| v <= value) {
            self.samples.pop_back();
        }
        self.samples.push_back((at, value));
    }

    fn max(&mut self, now: Instant, window: Duration) -> Option<f64> {
        while self.samples.len() > 1
            && self
                .samples
                .front()
                .is_some_and(|&(at, _)| now.saturating_duration_since(at) > window)
        {
            self.samples.pop_front();
        }
        // The newest sample is kept however old it is: a pause in arrival
        // does not lower the cap (ADR-0068 section 2).
        self.samples.front().map(|&(_, v)| v)
    }
}

// ------------------------------------------------------------
// Delay: send-time unwrap, groups, trendline
// ------------------------------------------------------------

/// Unwraps the 24-bit send time into a running count of 2^-18 s units.
#[derive(Debug, Default)]
struct SendClock {
    last_raw: Option<u32>,
    unwrapped: i64,
}

impl SendClock {
    /// The send time of this packet in milliseconds on a clock that does
    /// not wrap. The shortest way round the 64 s circle from the previous
    /// packet is taken, so a packet sent up to 32 s before or after it is
    /// placed correctly.
    #[allow(clippy::cast_precision_loss)]
    fn unwrap_ms(&mut self, raw: u32) -> f64 {
        let raw = raw & 0x00ff_ffff;
        if let Some(last) = self.last_raw {
            let mut diff = (i64::from(raw) - i64::from(last)).rem_euclid(ABS_SEND_TIME_MODULUS);
            if diff >= ABS_SEND_TIME_MODULUS / 2 {
                diff -= ABS_SEND_TIME_MODULUS;
            }
            self.unwrapped = self.unwrapped.saturating_add(diff);
        }
        self.last_raw = Some(raw);
        // Exact below 2^53 units, about a thousand years of sending.
        self.unwrapped as f64 * 1000.0 / f64::from(1u32 << ABS_SEND_TIME_FRACTION_BITS)
    }
}

/// Packets sent close together, treated as one for timing. Times in
/// milliseconds.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_field_names)]
struct Group {
    first_send_ms: f64,
    last_send_ms: f64,
    first_arrival_ms: f64,
    last_arrival_ms: f64,
}

/// One delay sample: the spacing of two groups at the sender and at the
/// receiver.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_field_names)]
struct GroupDelta {
    send_delta_ms: f64,
    arrival_delta_ms: f64,
    arrival_ms: f64,
}

#[derive(Debug, Default)]
struct InterArrival {
    current: Option<Group>,
    previous: Option<Group>,
}

impl InterArrival {
    /// Take one packet; a completed group pair comes back as a sample.
    fn on_packet(&mut self, send_ms: f64, arrival_ms: f64) -> Option<GroupDelta> {
        let Some(group) = self.current.as_mut() else {
            self.current = Some(Group {
                first_send_ms: send_ms,
                last_send_ms: send_ms,
                first_arrival_ms: arrival_ms,
                last_arrival_ms: arrival_ms,
            });
            return None;
        };
        if send_ms < group.first_send_ms {
            // Sent before the group began: out of order, no timing use.
            return None;
        }
        let send_delta = send_ms - group.last_send_ms;
        let arrival_delta = arrival_ms - group.last_arrival_ms;
        let in_burst = arrival_delta - send_delta < 0.0
            && arrival_delta <= BURST_ARRIVAL_MS
            && arrival_ms - group.first_arrival_ms < MAX_BURST_DURATION_MS;
        let belongs =
            send_delta == 0.0 || in_burst || send_ms - group.first_send_ms <= SEND_GROUP_LENGTH_MS;
        if belongs {
            group.last_send_ms = group.last_send_ms.max(send_ms);
            group.last_arrival_ms = group.last_arrival_ms.max(arrival_ms);
            return None;
        }
        let finished = *group;
        let sample = self.previous.map(|previous| GroupDelta {
            send_delta_ms: finished.last_send_ms - previous.last_send_ms,
            arrival_delta_ms: finished.last_arrival_ms - previous.last_arrival_ms,
            arrival_ms: finished.last_arrival_ms,
        });
        self.previous = Some(finished);
        self.current = Some(Group {
            first_send_ms: send_ms,
            last_send_ms: send_ms,
            first_arrival_ms: arrival_ms,
            last_arrival_ms: arrival_ms,
        });
        sample
    }
}

/// Over-use detection from the trend of the smoothed accumulated delay
/// (libwebrtc `TrendlineEstimator`).
#[derive(Debug)]
struct Trendline {
    /// (arrival ms since the first sample, smoothed delay ms).
    history: VecDeque<(f64, f64)>,
    first_arrival_ms: Option<f64>,
    samples_seen: f64,
    accumulated_delay_ms: f64,
    smoothed_delay_ms: f64,
    threshold_ms: f64,
    previous_trend: f64,
    time_over_using_ms: Option<f64>,
    overuse_count: u32,
    last_threshold_update_ms: Option<f64>,
    state: DetectorState,
}

impl Trendline {
    fn new() -> Self {
        Self {
            history: VecDeque::with_capacity(TRENDLINE_WINDOW + 1),
            first_arrival_ms: None,
            samples_seen: 0.0,
            accumulated_delay_ms: 0.0,
            smoothed_delay_ms: 0.0,
            threshold_ms: THRESHOLD_INITIAL_MS,
            previous_trend: 0.0,
            time_over_using_ms: None,
            overuse_count: 0,
            last_threshold_update_ms: None,
            state: DetectorState::Normal,
        }
    }

    fn on_sample(&mut self, sample: GroupDelta) {
        let delay = sample.arrival_delta_ms - sample.send_delta_ms;
        self.samples_seen = (self.samples_seen + 1.0).min(1000.0);
        self.accumulated_delay_ms += delay;
        self.smoothed_delay_ms = TRENDLINE_SMOOTHING * self.smoothed_delay_ms
            + (1.0 - TRENDLINE_SMOOTHING) * self.accumulated_delay_ms;
        let first = *self.first_arrival_ms.get_or_insert(sample.arrival_ms);
        self.history
            .push_back((sample.arrival_ms - first, self.smoothed_delay_ms));
        while self.history.len() > TRENDLINE_WINDOW {
            self.history.pop_front();
        }
        if self.history.len() < TRENDLINE_WINDOW {
            return;
        }
        let trend = self.slope().unwrap_or(self.previous_trend);
        self.detect(trend, sample.send_delta_ms, sample.arrival_ms);
    }

    /// Least-squares slope of the history; `None` when every sample has
    /// the same arrival time.
    fn slope(&self) -> Option<f64> {
        let n = as_f64(u64::try_from(self.history.len()).unwrap_or(1));
        let (sum_x, sum_y) = self
            .history
            .iter()
            .fold((0.0, 0.0), |acc, &(x, y)| (acc.0 + x, acc.1 + y));
        let (mean_x, mean_y) = (sum_x / n, sum_y / n);
        let (num, den) = self.history.iter().fold((0.0, 0.0), |acc, &(x, y)| {
            (
                acc.0 + (x - mean_x) * (y - mean_y),
                acc.1 + (x - mean_x) * (x - mean_x),
            )
        });
        (den != 0.0 && num.is_finite() && den.is_finite()).then(|| num / den)
    }

    fn detect(&mut self, trend: f64, send_delta_ms: f64, now_ms: f64) {
        let modified = self.samples_seen.min(TRENDLINE_MAX_DELTAS) * trend * TRENDLINE_GAIN;
        if modified > self.threshold_ms {
            let over = match self.time_over_using_ms {
                // Assume over-use for half the time since the last sample.
                None => send_delta_ms / 2.0,
                Some(t) => t + send_delta_ms,
            };
            self.time_over_using_ms = Some(over);
            self.overuse_count = self.overuse_count.saturating_add(1);
            if over > OVERUSE_TIME_THRESHOLD_MS
                && self.overuse_count > 1
                && trend >= self.previous_trend
            {
                self.time_over_using_ms = Some(0.0);
                self.overuse_count = 0;
                self.state = DetectorState::Overuse;
            }
        } else {
            self.time_over_using_ms = None;
            self.overuse_count = 0;
            self.state = if modified < -self.threshold_ms {
                DetectorState::Underuse
            } else {
                DetectorState::Normal
            };
        }
        self.previous_trend = trend;
        self.adapt_threshold(modified, now_ms);
    }

    fn adapt_threshold(&mut self, modified: f64, now_ms: f64) {
        let last = *self.last_threshold_update_ms.get_or_insert(now_ms);
        self.last_threshold_update_ms = Some(now_ms);
        let magnitude = modified.abs();
        if magnitude > self.threshold_ms + THRESHOLD_MAX_ADAPT_OFFSET_MS {
            return;
        }
        let k = if magnitude < self.threshold_ms {
            THRESHOLD_K_DOWN
        } else {
            THRESHOLD_K_UP
        };
        let step = (now_ms - last).clamp(0.0, THRESHOLD_MAX_STEP_MS);
        self.threshold_ms = (self.threshold_ms + k * (magnitude - self.threshold_ms) * step)
            .clamp(THRESHOLD_MIN_MS, THRESHOLD_MAX_MS);
    }
}

// ------------------------------------------------------------
// The estimator
// ------------------------------------------------------------

/// What the rate control is doing (libwebrtc `RateControlState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RateState {
    Hold,
    Increase,
    Decrease,
}

/// The estimate of the bitrate one publisher's path carries. One per
/// publisher, fed with the packets of all its streams.
#[derive(Debug)]
pub struct UplinkEstimator {
    config: EstimatorConfig,
    /// Zero point of the arrival clock.
    origin: Instant,
    estimate_bps: f64,
    last_change: ChangeReason,

    send_clock: SendClock,
    inter_arrival: InterArrival,
    trendline: Trendline,
    last_arrival: Option<Instant>,

    arriving: RateWindow,
    peak_rate: RateWindow,
    peak: SlidingMax,

    rate_state: RateState,
    last_rate_update: Option<Instant>,
    last_decrease: Option<Instant>,
    /// The estimate held before the current episode of cuts; the fast way
    /// back leads here. `None` when there is nothing to come back to, or the
    /// cuts have shown the link to be narrower.
    restore_to: Option<f64>,
    /// When the current episode of cuts began.
    episode_start: Option<Instant>,
    /// Since when over-use has been seen with next to nothing arriving.
    blackout_since: Option<Instant>,

    loss_expected: u64,
    loss_lost: u64,
    last_loss_feed: Option<Instant>,
    lossy_since: Option<Instant>,
    last_loss_step: Option<Instant>,
    /// The latest loss fraction formed was above [`LOSS_HIGH_FRACTION`].
    lossy: bool,

    retransmitted: u64,
}

impl UplinkEstimator {
    /// A fresh estimator at the configured start value. `now` is the zero
    /// point every later arrival time is measured from.
    #[must_use]
    pub fn new(config: EstimatorConfig, now: Instant) -> Self {
        let max_bps = config.max_bps.max(MIN_ESTIMATE_BPS);
        let start_bps = config.start_bps.clamp(MIN_ESTIMATE_BPS, max_bps);
        Self {
            config: EstimatorConfig { start_bps, max_bps },
            origin: now,
            estimate_bps: as_f64(start_bps),
            last_change: ChangeReason::Start,
            send_clock: SendClock::default(),
            inter_arrival: InterArrival::default(),
            trendline: Trendline::new(),
            last_arrival: None,
            arriving: RateWindow::new(ARRIVING_WINDOW),
            peak_rate: RateWindow::new(PEAK_RATE_WINDOW),
            peak: SlidingMax::default(),
            rate_state: RateState::Hold,
            last_rate_update: None,
            last_decrease: None,
            restore_to: None,
            episode_start: None,
            blackout_since: None,
            loss_expected: 0,
            loss_lost: 0,
            last_loss_feed: None,
            lossy_since: None,
            last_loss_step: None,
            lossy: false,
            retransmitted: 0,
        }
    }

    /// The current estimate, in bits per second.
    #[must_use]
    pub fn estimate_bps(&self) -> u64 {
        as_bps(self.estimate_bps).clamp(MIN_ESTIMATE_BPS, self.config.max_bps)
    }

    /// The delay detector's state.
    #[must_use]
    pub const fn state(&self) -> DetectorState {
        self.trendline.state
    }

    /// Why the estimate last changed.
    #[must_use]
    pub const fn last_change(&self) -> ChangeReason {
        self.last_change
    }

    /// The figures for a log line, with the arriving rate as of `now`.
    pub fn snapshot(&mut self, now: Instant) -> Snapshot {
        Snapshot {
            estimate_bps: self.estimate_bps(),
            arriving_bps: self.arriving.rate_bps(now).map_or(0, as_bps),
            state: self.state(),
            last_change: self.last_change,
            retransmitted: self.retransmitted,
        }
    }

    /// Take one received packet.
    pub fn on_packet(&mut self, packet: PacketArrival) {
        let now = packet.arrival;
        if packet.retransmission {
            self.retransmitted = self.retransmitted.saturating_add(1);
        }
        if self
            .last_arrival
            .is_some_and(|last| now.saturating_duration_since(last) > STREAM_TIMEOUT)
        {
            self.reset_delay_detector();
        }
        // Arrival times handed in out of order (they are not, from one
        // socket) must not move the clock backwards.
        self.last_arrival = Some(self.last_arrival.map_or(now, |last| last.max(now)));
        self.arriving.record(now, packet.size);
        self.peak_rate.record(now, packet.size);

        let mut overuse_now = false;
        if let Some(raw) = packet.send_time_24 {
            let send_ms = self.send_clock.unwrap_ms(raw);
            let arrival_ms = ms_since(self.origin, now);
            if let Some(sample) = self.inter_arrival.on_packet(send_ms, arrival_ms) {
                if (sample.arrival_delta_ms - sample.send_delta_ms).abs() > ARRIVAL_OFFSET_RESET_MS
                    || sample.send_delta_ms < 0.0
                {
                    self.reset_delay_detector();
                } else {
                    let before = self.trendline.state;
                    self.trendline.on_sample(sample);
                    overuse_now = self.trendline.state == DetectorState::Overuse
                        && before != DetectorState::Overuse;
                }
            }
        }
        self.update_rate(now, overuse_now);
    }

    /// Take the loss seen over the interval that ended at `now`: of
    /// `expected` packets, `lost` did not arrive.
    ///
    /// Loss shorter than [`LOSS_WINDOW`] changes nothing but to hold the
    /// increase while it lasts. Past the window the estimate steps down,
    /// once per [`LOSS_STEP_INTERVAL`], to `1 - loss / 2` of the smaller
    /// of itself and the rate the publisher is sending (the arriving rate
    /// with the loss added back): the receiver-report rule a libwebrtc
    /// sender applies to itself, taken here only once the loss has proved
    /// it is not a burst.
    pub fn on_loss(&mut self, now: Instant, expected: u64, lost: u64) {
        let interval_start = self.last_loss_feed.replace(now).unwrap_or(now);
        self.loss_expected = self.loss_expected.saturating_add(expected);
        self.loss_lost = self.loss_lost.saturating_add(lost.min(expected));
        if self.loss_expected < MIN_LOSS_PACKETS {
            // Too few packets for a figure. That is the state right after a
            // cut, when little is being sent: if none of the few was lost the
            // loss is over, and waiting for twenty packets to say so would
            // hold the rise for seconds.
            if self.loss_lost == 0 {
                self.lossy = false;
                self.lossy_since = None;
            }
            return;
        }
        let fraction = (as_f64(self.loss_lost) / as_f64(self.loss_expected)).clamp(0.0, 1.0);
        self.loss_expected = 0;
        self.loss_lost = 0;
        self.lossy = fraction > LOSS_HIGH_FRACTION;
        if !self.lossy {
            self.lossy_since = None;
            return;
        }
        let since = *self.lossy_since.get_or_insert(interval_start);
        let lasted = now.saturating_duration_since(since) >= LOSS_WINDOW;
        let step_due = self.last_loss_step.map_or(true, |at| {
            now.saturating_duration_since(at) >= LOSS_STEP_INTERVAL
        });
        if !(lasted && step_due) {
            return;
        }
        self.last_loss_step = Some(now);
        let mut base = self.estimate_bps;
        if let Some(arriving) = self.arriving.rate_bps(now) {
            if fraction < 1.0 {
                base = base.min(arriving / (1.0 - fraction));
            }
        }
        // Loss that outlasted the window is no hiccup: there is no old rate
        // to hurry back to.
        self.restore_to = None;
        self.set_estimate(base * (1.0 - 0.5 * fraction), ChangeReason::Loss);
    }

    fn reset_delay_detector(&mut self) {
        self.send_clock = SendClock::default();
        self.inter_arrival = InterArrival::default();
        self.trendline = Trendline::new();
    }

    fn set_estimate(&mut self, bps: f64, reason: ChangeReason) {
        let bounded = bps.clamp(as_f64(MIN_ESTIMATE_BPS), as_f64(self.config.max_bps));
        if as_bps(bounded) != as_bps(self.estimate_bps) {
            self.estimate_bps = bounded;
            self.last_change = reason;
        }
    }

    /// The rate control (libwebrtc `AimdRateControl`), run on a packet.
    fn update_rate(&mut self, now: Instant, overuse_now: bool) {
        let due = self.last_rate_update.map_or(true, |at| {
            now.saturating_duration_since(at) >= RATE_UPDATE_INTERVAL
        });
        if !(due || overuse_now) {
            return;
        }
        let since_last = self
            .last_rate_update
            .map(|at| now.saturating_duration_since(at));
        self.last_rate_update = Some(now);

        if let Some(rate) = self.peak_rate.rate_bps(now) {
            self.peak.record(now, rate);
        }

        match self.trendline.state {
            DetectorState::Normal => {
                if self.rate_state == RateState::Hold {
                    self.rate_state = RateState::Increase;
                    // The first step after a hold raises nothing.
                    return;
                }
            },
            DetectorState::Overuse => self.rate_state = RateState::Decrease,
            DetectorState::Underuse => self.rate_state = RateState::Hold,
        }

        match self.rate_state {
            RateState::Hold => {},
            RateState::Increase => self.increase(now, since_last),
            RateState::Decrease => {
                self.decrease(now);
                self.rate_state = RateState::Hold;
            },
        }
    }

    fn increase(&mut self, now: Instant, since_last: Option<Duration>) {
        if self.lossy {
            // Loss that has not yet proved to be more than a burst lowers
            // nothing, and raises nothing either. That holds on the way back
            // from an episode too: a narrowed path whose queue is full shows
            // no more delay growth, only loss, and loss is then the one sign
            // that the old rate does not fit.
            return;
        }
        let Some(peak) = self.peak.max(now, PEAK_WINDOW) else {
            return;
        };
        let cap =
            (INCREASE_CAP_FACTOR * peak + INCREASE_CAP_SLACK_BPS).min(as_f64(self.config.max_bps));
        if self.estimate_bps >= cap {
            return;
        }
        let elapsed = since_last.map_or(0.0, |d| d.as_secs_f64().min(1.0));
        // Below the estimate held before an episode of cuts the path has
        // already shown it carries more: come back fast. Above it, probe.
        let quiet_for = self
            .last_decrease
            .into_iter()
            .chain(self.last_loss_step)
            .max()
            .map(|at| now.saturating_duration_since(at));
        let probing = match quiet_for {
            Some(d) if d < QUIET_AFTER_CUT => INCREASE_PER_SECOND,
            Some(d) if d < 2 * QUIET_AFTER_CUT => INCREASE_PER_SECOND_QUIET,
            // Never cut, or quiet for long.
            _ => INCREASE_PER_SECOND_LONG_QUIET,
        };
        let (per_second, cap) = match self.restore_to {
            Some(restore) if self.estimate_bps < restore => (RESTORE_PER_SECOND, cap.min(restore)),
            _ => (probing, cap),
        };
        let step = (self.estimate_bps * (per_second.powf(elapsed) - 1.0)).max(MIN_INCREASE_BPS);
        self.set_estimate((self.estimate_bps + step).min(cap), ChangeReason::Increase);
        if self
            .restore_to
            .is_some_and(|restore| self.estimate_bps >= restore)
        {
            self.restore_to = None;
        }
    }

    /// A cut on over-use: libwebrtc's, to 0.85 of the arriving rate, with
    /// two additions of Lakko's own (see the constants): it is not made
    /// while next to nothing is arriving unless that lasts, and the estimate
    /// held before it is remembered as the place to come back to fast.
    fn decrease(&mut self, now: Instant) {
        let Some(arriving) = self.arriving.rate_bps(now) else {
            return;
        };
        if arriving < BLACKOUT_FRACTION * self.estimate_bps {
            let since = *self.blackout_since.get_or_insert(now);
            if now.saturating_duration_since(since) < BLACKOUT_HOLD {
                return;
            }
        } else {
            self.blackout_since = None;
        }
        let since_cut = self
            .last_decrease
            .map(|at| now.saturating_duration_since(at));
        let allowed = since_cut.map_or(true, |d| {
            d >= DECREASE_INTERVAL || arriving < 0.5 * self.estimate_bps
        });
        if !allowed {
            return;
        }
        let fresh = since_cut.map_or(true, |d| d > REPEAT_WINDOW);
        let in_episode = self
            .episode_start
            .is_some_and(|at| now.saturating_duration_since(at) <= EPISODE_LENGTH);
        let before_episode = if fresh {
            Some(self.estimate_bps)
        } else if in_episode {
            self.restore_to
        } else {
            None
        };
        let floor = before_episode.map_or(0.0, |b| EPISODE_FLOOR_FRACTION * b);
        let target = (BACKOFF_FACTOR * arriving).max(floor);
        if target >= self.estimate_bps {
            return;
        }
        if fresh {
            // A fresh episode: remember where to come back to.
            self.episode_start = Some(now);
        }
        // Outside an episode and soon after one, over-use says the link is
        // narrower: `before_episode` is `None` and the fast way back goes.
        self.restore_to = before_episode;
        self.last_decrease = Some(now);
        self.set_estimate(target, ChangeReason::Overuse);
    }
}

// ------------------------------------------------------------
// Loss from sequence numbers
// ------------------------------------------------------------

/// Counts, from extended RTP sequence numbers, how many packets a
/// publisher sent and how many of them did not arrive in order, across all
/// its streams, between two calls of [`Self::take_interval`].
///
/// A packet that arrives behind a later one of its stream is a
/// retransmission or a reordered packet; the two cannot be told apart
/// here. It is reported as late and does not take back the loss its gap
/// was counted as: the estimator wants what the path dropped, and a
/// retransmission is no evidence the original arrived. Reordering is
/// thereby counted as loss, which on one hop from a client is rare.
#[derive(Debug, Default)]
pub struct SeqLossCounter {
    /// Highest extended sequence number seen, per SSRC.
    highest: HashMap<u32, u64>,
    expected: u64,
    received: u64,
}

impl SeqLossCounter {
    /// An empty counter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take one packet. Returns `true` when it arrived behind a later
    /// packet of its stream (a retransmission, or reordered).
    pub fn on_packet(&mut self, ssrc: u32, seq: u64) -> bool {
        if !self.highest.contains_key(&ssrc) && self.highest.len() >= MAX_LOSS_STREAMS {
            self.highest.clear();
        }
        let Some(highest) = self.highest.get_mut(&ssrc) else {
            self.highest.insert(ssrc, seq);
            self.expected = self.expected.saturating_add(1);
            self.received = self.received.saturating_add(1);
            return false;
        };
        if seq <= *highest {
            return true;
        }
        let advance = seq - *highest;
        *highest = seq;
        // A jump too large to be loss is a stream that started over.
        let counted = if advance > MAX_SEQ_JUMP { 1 } else { advance };
        self.expected = self.expected.saturating_add(counted);
        self.received = self.received.saturating_add(1);
        false
    }

    /// `(expected, lost)` since the last call, and start over.
    pub fn take_interval(&mut self) -> (u64, u64) {
        let out = (self.expected, self.expected.saturating_sub(self.received));
        self.expected = 0;
        self.received = 0;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKET_BYTES: usize = 1200;
    /// The sender puts packets on the wire in ticks this far apart.
    const TICK_US: u64 = 5_000;
    /// One-way delay of an uncongested path.
    const BASE_DELAY_US: u64 = 20_000;
    const NOMINAL_BPS: u64 = 2_000_000;

    /// What the path does to a packet sent at a given moment.
    #[derive(Clone, Copy)]
    enum Fate {
        /// Arrives after the base delay.
        Deliver,
        /// Never arrives.
        Drop,
        /// Held until the given moment (microseconds), then released.
        HoldUntil(u64),
    }

    /// A publisher that sends at the smaller of its nominal rate and the
    /// estimate (what REMB makes of a libwebrtc sender), a path, and the
    /// estimator at the far end.
    struct Sim {
        t0: Instant,
        est: UplinkEstimator,
        loss: SeqLossCounter,
        /// Simulation clock, microseconds.
        now_us: u64,
        /// Offset of the sender's clock, to place the 64 s wrap.
        send_clock_offset_us: u64,
        seq: u64,
        budget_bits: f64,
        /// Bottleneck capacity; `None` is an unlimited path.
        capacity_bps: Option<u64>,
        /// When the bottleneck finishes sending what it holds.
        queue_free_us: u64,
        /// Longest a packet may wait in the bottleneck queue.
        max_queue_us: u64,
        /// In flight: (arrival µs, send µs, seq), arrival order.
        in_flight: VecDeque<(u64, u64, u64)>,
        released_us: u64,
        /// (seconds, estimate) sampled every 100 ms.
        trace: Vec<(f64, u64)>,
        follow_estimate: bool,
    }

    impl Sim {
        fn new(config: EstimatorConfig) -> Self {
            let t0 = Instant::now();
            Self {
                t0,
                est: UplinkEstimator::new(config, t0),
                loss: SeqLossCounter::new(),
                now_us: 0,
                send_clock_offset_us: 0,
                seq: 0,
                budget_bits: 0.0,
                capacity_bps: None,
                queue_free_us: 0,
                max_queue_us: 300_000,
                in_flight: VecDeque::new(),
                released_us: 0,
                trace: Vec::new(),
                follow_estimate: true,
            }
        }

        fn at(&self, us: u64) -> Instant {
            self.t0 + Duration::from_micros(us)
        }

        fn send_time_24(&self, send_us: u64) -> u32 {
            let us = u128::from(send_us + self.send_clock_offset_us);
            u32::try_from((us * (1 << 18) / 1_000_000) & 0x00ff_ffff).unwrap()
        }

        fn deliver_due(&mut self) {
            while let Some(&(arrival_us, send_us, seq)) = self.in_flight.front() {
                if arrival_us > self.now_us {
                    break;
                }
                self.in_flight.pop_front();
                let late = self.loss.on_packet(1, seq);
                self.est.on_packet(PacketArrival {
                    send_time_24: Some(self.send_time_24(send_us)),
                    arrival: self.at(arrival_us),
                    size: PACKET_BYTES,
                    retransmission: late,
                });
            }
        }

        fn send_tick(&mut self, fate: impl Fn(u64, u64) -> Fate) {
            let rate = if self.follow_estimate {
                NOMINAL_BPS.min(self.est.estimate_bps())
            } else {
                NOMINAL_BPS
            };
            self.budget_bits += as_f64(rate) * as_f64(TICK_US) / 1e6;
            let packet_bits = as_f64(PACKET_BYTES as u64 * 8);
            while self.budget_bits >= packet_bits {
                self.budget_bits -= packet_bits;
                let seq = self.seq;
                self.seq += 1;
                let send_us = self.now_us;
                let arrival_us = match fate(send_us, seq) {
                    Fate::Drop => continue,
                    Fate::HoldUntil(release_us) => {
                        // Released back to back, 100 µs apart.
                        self.released_us = self.released_us.max(release_us) + 100;
                        self.released_us
                    },
                    Fate::Deliver => match self.capacity_bps {
                        None => send_us + BASE_DELAY_US,
                        Some(capacity) => {
                            let start = self.queue_free_us.max(send_us);
                            if start - send_us > self.max_queue_us {
                                continue; // tail drop
                            }
                            let service = PACKET_BYTES as u64 * 8 * 1_000_000 / capacity;
                            self.queue_free_us = start + service;
                            self.queue_free_us + BASE_DELAY_US
                        },
                    },
                };
                self.in_flight.push_back((arrival_us, send_us, seq));
            }
        }

        /// Run until `until_s`, deciding each packet's fate with `fate`.
        fn run(&mut self, until_s: f64, fate: impl Fn(u64, u64) -> Fate) {
            let end_us = as_bps(until_s * 1e6);
            while self.now_us < end_us {
                self.deliver_due();
                if self.now_us % TICK_US == 0 {
                    self.send_tick(&fate);
                }
                if self.now_us % 1_000_000 == 0 && self.now_us > 0 {
                    let (expected, lost) = self.loss.take_interval();
                    self.est.on_loss(self.at(self.now_us), expected, lost);
                }
                if self.now_us % 100_000 == 0 {
                    self.trace
                        .push((as_f64(self.now_us) / 1e6, self.est.estimate_bps()));
                }
                self.now_us += 1_000;
            }
        }

        /// Estimates sampled in `[from_s, to_s)`.
        fn between(&self, from_s: f64, to_s: f64) -> Vec<u64> {
            self.trace
                .iter()
                .filter(|(t, _)| *t >= from_s && *t < to_s)
                .map(|(_, e)| *e)
                .collect()
        }
    }

    fn config(start_kbps: u64, max_kbps: u64) -> EstimatorConfig {
        EstimatorConfig {
            start_bps: start_kbps * 1000,
            max_bps: max_kbps * 1000,
        }
    }

    /// (a) A steady 2 Mbit/s stream with constant delay: the estimate
    /// rises to the cap and stays.
    #[test]
    fn a_steady_stream_with_constant_delay_rises_to_the_cap_and_stays() {
        let mut sim = Sim::new(config(1500, 2500));
        sim.run(20.0, |_, _| Fate::Deliver);
        let trace = &sim.trace;
        // Never down on the way.
        for pair in trace.windows(2) {
            assert!(pair[1].1 >= pair[0].1, "estimate fell: {pair:?}");
        }
        assert_eq!(sim.between(0.0, 0.3), vec![1_500_000; 3], "starts at start");
        // Never cut, so the rise is the fast one: above the start value
        // within a second, at the maximum within three.
        let at_1s = sim.between(1.0, 1.1)[0];
        assert!(
            at_1s > 1_500_000 && at_1s < 2_500_000,
            "rising, not jumping, at 1 s: {at_1s}"
        );
        for e in sim.between(3.0, 20.0) {
            assert_eq!(e, 2_500_000, "at the configured maximum from 3 s on");
        }
        assert_eq!(sim.est.state(), DetectorState::Normal);
        assert_eq!(sim.est.last_change(), ChangeReason::Increase);
    }

    /// (a), the other cap: with a maximum far away, the estimate stops at
    /// 1.5 times the highest rate that arrived plus 10 kbit/s.
    #[test]
    fn a_steady_stream_is_capped_at_one_and_a_half_times_what_arrives() {
        let mut sim = Sim::new(config(1500, 100_000));
        sim.run(30.0, |_, _| Fate::Deliver);
        for e in sim.between(20.0, 30.0) {
            assert!(
                (2_950_000..=3_100_000).contains(&e),
                "1.5 x 2 Mbit/s + 10 kbit/s, got {e}"
            );
        }
    }

    /// (b) A path that carries 500 kbit/s while 2 Mbit/s is offered: delay
    /// grows, then packets are dropped. The estimate comes down to about
    /// the carried rate and, once the one attempt at the fast way back has
    /// failed, stays near it.
    #[test]
    fn a_narrow_path_brings_the_estimate_down_to_what_it_carries() {
        let mut sim = Sim::new(config(1500, 2500));
        sim.run(8.0, |_, _| Fate::Deliver);
        let before = sim.between(7.9, 8.0)[0];
        assert!(before >= 2_000_000, "risen before the path narrows");
        sim.capacity_bps = Some(500_000);
        sim.queue_free_us = sim.now_us;
        sim.run(40.0, |_, _| Fate::Deliver);
        let after_1s = sim.between(9.0, 9.1)[0];
        assert!(
            after_1s < 1_000_000,
            "cut within a second of the path narrowing: {after_1s}"
        );
        // The first cuts are one episode, so the estimator tries the fast
        // way back once; the path answers with over-use and the way back is
        // given up. Until then the estimate may stand above what the path
        // carries, but never near what it was.
        for e in sim.between(9.0, 16.0) {
            assert!(
                e < 1_300_000,
                "no return to the old rate on a narrowed path: {e}"
            );
        }
        for e in sim.between(16.0, 40.0) {
            assert!(
                (300_000..=650_000).contains(&e),
                "near the 500 kbit/s the path carries from 8 s after it narrowed: {e}"
            );
        }
        assert_ne!(sim.est.last_change(), ChangeReason::Start);
    }

    /// (c) A one-second total outage in a steady stream, with no delay
    /// growth before or after: the estimate does not fall.
    #[test]
    fn a_one_second_outage_does_not_lower_the_estimate() {
        let mut sim = Sim::new(config(1500, 2500));
        let outage = |send_us: u64, _| {
            if (12_000_000..13_000_000).contains(&send_us) {
                Fate::Drop
            } else {
                Fate::Deliver
            }
        };
        sim.run(25.0, outage);
        let before = sim.between(11.9, 12.0)[0];
        assert_eq!(before, 2_500_000);
        for e in sim.between(12.0, 25.0) {
            assert_eq!(e, before, "unchanged through the outage and after");
        }
        assert_ne!(sim.est.last_change(), ChangeReason::Loss);
        assert_ne!(sim.est.last_change(), ChangeReason::Overuse);
    }

    /// (c) again while the estimate is still on its way up: the outage
    /// holds the rise for the lossy interval and nothing falls.
    #[test]
    fn an_outage_during_the_rise_only_pauses_it() {
        let mut sim = Sim::new(config(1500, 2500));
        let outage = |send_us: u64, _| {
            if (2_000_000..3_000_000).contains(&send_us) {
                Fate::Drop
            } else {
                Fate::Deliver
            }
        };
        sim.run(20.0, outage);
        for pair in sim.trace.windows(2) {
            assert!(pair[1].1 >= pair[0].1, "estimate fell: {pair:?}");
        }
        assert_eq!(sim.between(15.0, 15.1)[0], 2_500_000);
    }

    /// (d) The same outage, but the packets were held in a queue and are
    /// released in a burst when it ends.
    ///
    /// What the estimator does: nothing. The released packets arrive
    /// within the burst rule's 100 ms and form groups whose arrival
    /// spacing is far shorter than their send spacing; the one large
    /// positive delay sample at the release is followed by negative ones
    /// that take the accumulated delay back to where it was, so the trend
    /// never stays above the threshold with a rising slope, and no loss is
    /// seen at all. That is the right call: delay that jumps and
    /// immediately returns is a queue that was held, not a path narrower
    /// than what is sent, and cutting the bitrate would not have prevented
    /// it.
    #[test]
    fn a_held_and_released_queue_does_not_lower_the_estimate() {
        let mut sim = Sim::new(config(1500, 2500));
        let hold = |send_us: u64, _| {
            if (12_000_000..13_000_000).contains(&send_us) {
                Fate::HoldUntil(13_000_000 + BASE_DELAY_US)
            } else {
                Fate::Deliver
            }
        };
        sim.run(25.0, hold);
        let before = sim.between(11.9, 12.0)[0];
        assert_eq!(before, 2_500_000);
        for e in sim.between(12.0, 25.0) {
            assert_eq!(e, before, "unchanged through the hold and the release");
        }
        assert_eq!(sim.est.last_change(), ChangeReason::Increase);
    }

    /// What a live call showed (2026-10-04): for about a second a home
    /// network passed a third of the packets, late and later, and dropped the
    /// rest; then it was itself again. The estimator cannot tell this from a
    /// link that narrowed while it happens, and it cuts. What makes it a
    /// second of trouble and not half a minute is the way back: the estimate
    /// is at what it was within three seconds of the end of the dip.
    #[test]
    fn a_one_second_dip_is_undone_within_three_seconds() {
        let mut sim = Sim::new(config(1500, 2500));
        let dip = |send_us: u64, seq: u64| {
            if (12_000_000..13_000_000).contains(&send_us) {
                if seq % 10 < 7 {
                    Fate::Drop
                } else {
                    // Delay that grows through the dip: a queue filling.
                    Fate::HoldUntil(send_us + BASE_DELAY_US + (send_us - 12_000_000) / 2)
                }
            } else {
                Fate::Deliver
            }
        };
        sim.run(30.0, dip);
        let before = sim.between(11.9, 12.0)[0];
        assert_eq!(before, 2_500_000);
        let lowest = sim.between(12.0, 14.0).into_iter().min().unwrap_or(before);
        assert!(
            lowest < before / 2,
            "the model must reproduce the cut seen on the call: lowest {lowest}"
        );
        for e in sim.between(16.0, 30.0) {
            assert!(
                e >= before / 10 * 9,
                "back within three seconds of the end of the dip: {e}"
            );
        }
    }

    /// What a live call showed on a mobile uplink (2026-10-04): cut to
    /// under 100 kbit/s in its first seconds, on a network that then carried
    /// 2500 kbit/s. From a low estimate on a quiet path the rise must not
    /// take a minute and a half.
    #[test]
    fn a_low_estimate_on_a_quiet_path_is_climbed_in_seconds() {
        let mut sim = Sim::new(config(1500, 2500));
        // A path that carries 80 kbit/s for the first 12 s, then everything:
        // the estimate is brought low the way the estimator itself does it.
        sim.capacity_bps = Some(80_000);
        sim.run(12.0, |_, _| Fate::Deliver);
        let low = sim.between(11.9, 12.0)[0];
        assert!(low < 200_000, "brought low by the narrow start: {low}");
        sim.capacity_bps = None;
        sim.run(32.0, |_, _| Fate::Deliver);
        let after = sim.between(31.9, 32.0)[0];
        assert!(
            after >= 1_800_000,
            "back near the nominal 2 Mbit/s within 20 s of the path opening: {after}"
        );
    }

    /// (e) Loss of 20% that lasts ten seconds with no delay growth: the
    /// estimate holds for the loss window, then steps down once a second,
    /// and rises again once the loss is over.
    #[test]
    fn loss_that_lasts_steps_the_estimate_down() {
        let mut sim = Sim::new(config(1500, 2500));
        let lossy = |send_us: u64, seq: u64| {
            if (12_000_000..22_000_000).contains(&send_us) && seq % 5 == 0 {
                Fate::Drop
            } else {
                Fate::Deliver
            }
        };
        sim.run(40.0, lossy);
        let before = sim.between(11.9, 12.0)[0];
        assert_eq!(before, 2_500_000);
        for e in sim.between(12.0, 15.0) {
            assert_eq!(e, before, "nothing changes inside the loss window");
        }
        // One step a second from the end of the window to the end of the
        // loss, each lower than the last.
        let steps: Vec<u64> = (15..=22)
            .map(|s| sim.between(f64::from(s) + 0.5, f64::from(s) + 0.6)[0])
            .collect();
        for pair in steps.windows(2) {
            assert!(pair[1] < pair[0], "each second lower: {steps:?}");
        }
        assert!(
            (1_750_000..=1_850_000).contains(&steps[0]),
            "first step is 0.9 of the 2 Mbit/s being sent: {steps:?}"
        );
        let lowest = *steps.last().unwrap();
        assert!(
            lowest > 700_000 && lowest < 1_200_000,
            "eight steps of 0.9: {steps:?}"
        );
        // Over: the estimate rises again.
        let later = sim.between(35.0, 35.1)[0];
        assert!(later > lowest, "rises once the loss is over: {later}");
    }

    /// Loss shorter than the window, repeated, never lowers the estimate.
    #[test]
    fn two_second_loss_bursts_never_lower_the_estimate() {
        let mut sim = Sim::new(config(1500, 2500));
        let bursts = |send_us: u64, seq: u64| {
            let in_burst = (send_us / 1_000_000) % 10 < 2 && send_us > 10_000_000;
            if in_burst && seq % 2 == 0 {
                Fate::Drop
            } else {
                Fate::Deliver
            }
        };
        sim.run(60.0, bursts);
        for pair in sim.trace.windows(2) {
            assert!(pair[1].1 >= pair[0].1, "estimate fell: {pair:?}");
        }
    }

    /// (f) The 24-bit send time wraps every 64 s: a stream running across
    /// several wraps behaves exactly like one that never meets a wrap.
    #[test]
    fn the_send_time_wrap_changes_nothing() {
        let mut plain = Sim::new(config(1500, 2500));
        plain.run(20.0, |_, _| Fate::Deliver);
        let mut wrapping = Sim::new(config(1500, 2500));
        // The wrap falls 3 s into the run.
        wrapping.send_clock_offset_us = 61_000_000;
        wrapping.run(20.0, |_, _| Fate::Deliver);
        assert_eq!(plain.trace, wrapping.trace);
        assert_eq!(wrapping.est.state(), DetectorState::Normal);

        let mut long = Sim::new(config(1500, 2500));
        long.run(200.0, |_, _| Fate::Deliver);
        for e in long.between(10.0, 200.0) {
            assert_eq!(e, 2_500_000, "three wraps in, still at the cap");
        }
    }

    #[test]
    fn send_clock_unwraps_forwards_and_backwards_across_the_wrap() {
        let mut clock = SendClock::default();
        let near_wrap = 0x00ff_fff0;
        assert!(clock.unwrap_ms(near_wrap).abs() < f64::EPSILON);
        // 0x20 units later, across the wrap.
        let forward = clock.unwrap_ms(0x10);
        assert!((forward - 32.0 * 1000.0 / 262_144.0).abs() < 1e-9);
        // And back before it: a reordered packet.
        let back = clock.unwrap_ms(near_wrap);
        assert!(back.abs() < 1e-9);
        // Values above 24 bits are masked, not trusted.
        let masked = clock.unwrap_ms(0xff00_0000 | near_wrap);
        assert!(masked.abs() < 1e-9);
    }

    /// (f) Reordered and duplicated packets, retransmissions and packets
    /// without a send time: nothing panics and the estimate stays where a
    /// clean stream puts it.
    #[test]
    fn reordering_duplicates_and_retransmissions_do_not_disturb_the_estimate() {
        let t0 = Instant::now();
        let mut est = UplinkEstimator::new(config(1500, 2500), t0);
        let mut loss = SeqLossCounter::new();
        let unit =
            |us: u64| u32::try_from((u128::from(us) * 262_144 / 1_000_000) & 0xff_ffff).unwrap();
        let feed = |est: &mut UplinkEstimator,
                    loss: &mut SeqLossCounter,
                    send_us: u64,
                    arrival_us: u64,
                    seq: u64,
                    with_time: bool| {
            let late = loss.on_packet(7, seq);
            est.on_packet(PacketArrival {
                send_time_24: with_time.then(|| unit(send_us)),
                arrival: t0 + Duration::from_micros(arrival_us),
                size: PACKET_BYTES,
                retransmission: late,
            });
            late
        };
        let mut lates = 0;
        for i in 0..4000u64 {
            let send = i * 5_000;
            let arrival = send + BASE_DELAY_US;
            if feed(&mut est, &mut loss, send, arrival, i, i % 50 != 0) {
                lates += 1;
            }
            if i % 7 == 0 && i > 0 {
                // The previous packet again, arriving now: a duplicate.
                if feed(&mut est, &mut loss, send - 5_000, arrival + 10, i - 1, true) {
                    lates += 1;
                }
            }
            if i % 11 == 0 && i > 3 {
                // A packet from three ticks ago with its own old send
                // time: reordered.
                if feed(
                    &mut est,
                    &mut loss,
                    send - 15_000,
                    arrival + 20,
                    i - 3,
                    true,
                ) {
                    lates += 1;
                }
            }
            if i % 13 == 0 && i > 20 {
                // A retransmission: an old sequence number, sent now.
                if feed(&mut est, &mut loss, send, arrival + 30, i - 20, true) {
                    lates += 1;
                }
            }
            if i % 200 == 199 {
                let (expected, missing) = loss.take_interval();
                assert_eq!(missing, 0, "nothing was lost");
                est.on_loss(t0 + Duration::from_micros(arrival), expected, missing);
            }
        }
        let now = t0 + Duration::from_secs(20);
        let snap = est.snapshot(now);
        assert!(lates > 1000);
        assert_eq!(snap.retransmitted, lates);
        assert_eq!(snap.state, DetectorState::Normal);
        assert!(
            snap.estimate_bps >= 1_500_000 && snap.estimate_bps <= 2_500_000,
            "{snap:?}"
        );
    }

    /// (f) A stream that stops and resumes, for one second, for five and
    /// for two minutes (past the point where the send time is ambiguous):
    /// the estimate is kept and the detector starts clean.
    #[test]
    fn a_stream_that_stops_and_resumes_keeps_its_estimate() {
        let mut sim = Sim::new(config(1500, 2500));
        sim.follow_estimate = false;
        let gaps = |send_us: u64, _| {
            let s = send_us / 1_000_000;
            if (15..16).contains(&s) || (20..25).contains(&s) || (30..150).contains(&s) {
                Fate::Drop
            } else {
                Fate::Deliver
            }
        };
        sim.run(170.0, gaps);
        for e in sim.between(10.0, 170.0) {
            assert_eq!(e, 2_500_000);
        }
        assert_eq!(sim.est.state(), DetectorState::Normal);
        let snap = sim.est.snapshot(sim.at(sim.now_us));
        assert!(
            (1_900_000..=2_100_000).contains(&snap.arriving_bps),
            "arriving again at the nominal rate: {snap:?}"
        );
    }

    /// (f) Absurd input: zero and huge sizes, a send time that jumps at
    /// random, arrival times that go backwards. Nothing panics and the
    /// estimate stays inside its bounds.
    #[test]
    fn absurd_input_stays_inside_the_bounds() {
        let t0 = Instant::now();
        let mut est = UplinkEstimator::new(config(1500, 2500), t0);
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for i in 0..20_000u64 {
            let r = next();
            let size = match r % 5 {
                0 => 0,
                1 => usize::MAX,
                _ => (r % 1500) as usize,
            };
            let arrival = t0 + Duration::from_micros((i * 700).saturating_sub(r % 5_000));
            est.on_packet(PacketArrival {
                send_time_24: (r % 9 != 0)
                    .then_some(u32::try_from((r >> 8) & 0xffff_ffff).unwrap()),
                arrival,
                size,
                retransmission: r % 3 == 0,
            });
            if i % 500 == 0 {
                est.on_loss(arrival, r % 1000, next() % 2000);
            }
            let e = est.estimate_bps();
            assert!((MIN_ESTIMATE_BPS..=2_500_000).contains(&e), "{e}");
        }
        let _ = est.snapshot(t0 + Duration::from_secs(3600));
    }

    #[test]
    fn the_start_value_is_clamped_into_the_bounds() {
        let now = Instant::now();
        let low = UplinkEstimator::new(config(1, 2500), now);
        assert_eq!(low.estimate_bps(), MIN_ESTIMATE_BPS);
        let high = UplinkEstimator::new(config(9000, 2500), now);
        assert_eq!(high.estimate_bps(), 2_500_000);
        let inverted = UplinkEstimator::new(config(1500, 0), now);
        assert_eq!(inverted.estimate_bps(), MIN_ESTIMATE_BPS);
        assert_eq!(low.last_change(), ChangeReason::Start);
    }

    #[test]
    fn a_publisher_that_sends_less_than_the_start_value_keeps_it() {
        // 2 Mbit/s nominal against a 4 Mbit/s start: the estimate is above
        // 1.5 x what arrives plus the slack, so it is not raised, and with
        // no over-use it is not lowered either.
        let mut sim = Sim::new(config(4000, 5000));
        sim.run(15.0, |_, _| Fate::Deliver);
        for (_, e) in &sim.trace {
            assert_eq!(*e, 4_000_000);
        }
        assert_eq!(sim.est.last_change(), ChangeReason::Start);
    }

    #[test]
    fn seq_loss_counter_counts_gaps_and_flags_late_packets() {
        let mut c = SeqLossCounter::new();
        assert!(!c.on_packet(1, 100));
        assert!(!c.on_packet(1, 101));
        assert!(!c.on_packet(1, 105)); // 102..=104 missing
        assert!(c.on_packet(1, 103), "behind the highest: late");
        assert!(c.on_packet(1, 105), "a duplicate is late too");
        assert!(!c.on_packet(2, 9));
        assert_eq!(c.take_interval(), (7, 3));
        assert_eq!(c.take_interval(), (0, 0));
        // A stream that starts over is not thousands of lost packets.
        assert!(!c.on_packet(1, 1_000_000));
        assert_eq!(c.take_interval(), (1, 0));
        // More streams than the table holds: it starts over, no panic.
        for ssrc in 10..10 + 2 * u32::try_from(MAX_LOSS_STREAMS).unwrap() {
            assert!(!c.on_packet(ssrc, 1));
        }
        assert!(c.highest.len() <= MAX_LOSS_STREAMS);
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(DetectorState::Normal.as_str(), "normal");
        assert_eq!(DetectorState::Overuse.as_str(), "overuse");
        assert_eq!(DetectorState::Underuse.as_str(), "underuse");
        assert_eq!(ChangeReason::Start.as_str(), "start");
        assert_eq!(ChangeReason::Increase.as_str(), "increase");
        assert_eq!(ChangeReason::Overuse.as_str(), "overuse");
        assert_eq!(ChangeReason::Loss.as_str(), "loss");
    }
}
