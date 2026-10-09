// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Trying a rate on a subscriber's downlink before a larger set of
//! tracks is given
//! ([ADR-0070](../../../docs/adr/0070-call-media-through-a-loss-burst.md)
//! step 6).
//!
//! Pure: no I/O, no clock of its own, no `str0m` type. One
//! [`DownlinkProbe`] per subscriber.
//!
//! ## Why
//!
//! The SFU relays tracks it cannot thin. Toward one subscriber it sends
//! either a set of tracks or a larger set; in between there is nothing.
//! What arrives at the subscriber can therefore never lead an estimate
//! up: while the heavy tracks are paused little is sent, and little
//! arriving says nothing of what the path would carry. So before the
//! allocation ([`crate::downlink_allocation`]) gives a subscriber the
//! next larger set, the SFU **tries the rate of that set on the path**:
//! for [`PROBE_DURATION`] it sends padding on top of the media, so that
//! the two together run at the rate the larger set would need, and the
//! set is given only if the subscriber's transport-wide feedback shows
//! the path carried it.
//!
//! ## What a probe is
//!
//! - **Padding** is never new media: the caller sends copies of packets
//!   it has already sent this subscriber, on the retransmission stream
//!   (`StreamTx::resend`), which a receiver discards by sequence number.
//!   This module only says how many bytes are owed
//!   ([`DownlinkProbe::padding_owed`]).
//! - **Carried** means: of the packets sent during the probe, the
//!   feedback acknowledged at least [`PROBE_PASS_NUM`]/[`PROBE_PASS_DEN`]
//!   of the target rate, reported at most
//!   [`PROBE_MAX_LOSS_NUM`]/[`PROBE_MAX_LOSS_DEN`] of them missing, and
//!   the caller saw no queue stand on the path at any point
//!   ([`DownlinkProbe::on_trouble`]). A queue ends a probe at once,
//!   before it overflows.
//! - **Bounds.** One probe at a time per subscriber; a target of at most
//!   [`PROBE_MAX_BPS`]; padding for at most [`PROBE_DURATION`] per probe
//!   and at most [`PROBE_BURST`] worth of the target in one step. After a
//!   probe that failed the next starts no sooner than
//!   [`PROBE_FIRST_RETRY`], and each further failure doubles the wait up
//!   to [`PROBE_MAX_RETRY`], so a narrow path is not hammered. That wait
//!   is for the rate that failed and for more: a probe toward **less**
//!   ([`DownlinkProbe::can_start_toward`]) may start
//!   [`PROBE_BELOW_FAILED_AFTER`] after the failure, because a path that
//!   does not carry two main pictures may well carry a main and a middle
//!   one. After the path itself lowered the estimate, no probe starts for
//!   [`PROBE_AFTER_TROUBLE`]. After a probe that was carried the next
//!   (toward the set after) may start after [`PROBE_AFTER_SUCCESS`].
//!
//! Every constant is **chosen, not measured**.

use std::time::{Duration, Instant};

/// How long padding is sent for one probe.
pub const PROBE_DURATION: Duration = Duration::from_secs(1);

/// A probe does not jump to its target: over this long its rate climbs
/// in a straight line from what was being sent to the target, and the
/// verdict is on what was acknowledged of the packets sent after the
/// climb. A queue the target would build therefore starts late and
/// grows slowly at first. Chosen.
pub const PROBE_RAMP: Duration = Duration::from_millis(400);

/// The queue a probe may build on the path, as a time at its target
/// rate, **whatever the feedback says and whether it comes at all**:
/// padding is only sent while the bytes sent in the probe and not yet
/// reported on by the subscriber's feedback come to no more than what
/// the path delivers (the rate the reports of this probe acknowledge,
/// starting from what was being sent before it) in the time since the
/// last report, at most the feedback's usual lag, plus this. On a path
/// that carries less
/// than the target, what is not yet reported on beyond the feedback's
/// lag is what waits in its queue, so the probe stops adding to the
/// queue at about this depth and fails as short; when feedback stops,
/// padding stops. Chosen: well under the 100 ms and more that a home
/// router queues.
pub const PROBE_QUEUE_ALLOWANCE: Duration = Duration::from_millis(40);

/// The feedback lag assumed when the caller has none to give, and the
/// bounds a given one is held to.
pub const PROBE_DEFAULT_FEEDBACK_LAG: Duration = Duration::from_millis(150);
/// See [`PROBE_DEFAULT_FEEDBACK_LAG`].
pub const PROBE_MIN_FEEDBACK_LAG: Duration = Duration::from_millis(30);
/// See [`PROBE_DEFAULT_FEEDBACK_LAG`].
pub const PROBE_MAX_FEEDBACK_LAG: Duration = Duration::from_millis(400);

/// How long after the end of the padding the feedback on its last
/// packets is waited for before the probe is judged on what has come.
pub const PROBE_RESULT_WAIT: Duration = Duration::from_millis(500);

/// A probe that was held back ([`PROBE_QUEUE_ALLOWANCE`]) did not try
/// its rate: it is carried only if what it sent is short of what its
/// schedule asked for by no more than this long at the target rate.
/// Chosen: four padding steps.
pub const PROBE_SHORTFALL: Duration = Duration::from_millis(20);

/// A probe is carried when the feedback acknowledged at least this
/// fraction of its target rate (numerator).
pub const PROBE_PASS_NUM: u64 = 9;
/// See [`PROBE_PASS_NUM`].
pub const PROBE_PASS_DEN: u64 = 10;

/// A probe fails when more than this fraction of its packets were
/// reported missing (numerator).
pub const PROBE_MAX_LOSS_NUM: u64 = 1;
/// See [`PROBE_MAX_LOSS_NUM`].
pub const PROBE_MAX_LOSS_DEN: u64 = 10;

/// The highest rate a probe tries, bits per second.
pub const PROBE_MAX_BPS: u64 = 20_000_000;

/// The most padding owed in one step, as time at the target rate: a
/// caller that was away for a moment does not make up for it in one
/// burst.
pub const PROBE_BURST: Duration = Duration::from_millis(20);

/// How often a caller should come back for padding while a probe runs.
pub const PROBE_TICK: Duration = Duration::from_millis(5);

/// The wait before the next probe after one that failed.
pub const PROBE_FIRST_RETRY: Duration = Duration::from_secs(5);

/// The wait before the next probe after the estimate was lowered with no
/// probe running: the time a queue takes to drain and the estimator's
/// detector to settle. Chosen.
pub const PROBE_AFTER_TROUBLE: Duration = Duration::from_secs(2);

/// The longest that wait grows to by doubling: 5 s, then 8 s every time.
/// Chosen so that a path that has widened is found, by two probes, well
/// inside fifteen seconds.
pub const PROBE_MAX_RETRY: Duration = Duration::from_secs(8);

/// The wait before a probe toward **less** than the rate the last failed
/// probe tried ([`DownlinkProbe::failed_bps`]): the path did not carry
/// that rate, which says nothing against a smaller set, and the smaller
/// set need not wait out the growing interval. The time the queue the
/// failed probe filled takes to drain. Chosen.
pub const PROBE_BELOW_FAILED_AFTER: Duration = Duration::from_secs(1);

/// "Less" is at most this fraction of the rate that failed (four
/// fifths): the rate a set needs is measured and moves by some percent
/// from second to second (a key frame is enough), and the same set a
/// second later is not a smaller one. Chosen.
pub const PROBE_BELOW_FAILED_NUM: u64 = 4;
/// See [`PROBE_BELOW_FAILED_NUM`].
pub const PROBE_BELOW_FAILED_DEN: u64 = 5;

/// The wait before the next probe after one that was carried. Chosen.
pub const PROBE_AFTER_SUCCESS: Duration = Duration::from_millis(500);

/// Why a probe failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFailure {
    /// The path estimator lowered its estimate while the probe ran:
    /// delay was growing, or loss had lasted.
    Trouble,
    /// Too many of the probe's packets were reported missing.
    Loss,
    /// The feedback acknowledged less than the target rate.
    Short,
    /// No feedback came for any packet of the probe.
    NoFeedback,
    /// The caller gave the probe up for a reason that is not the path's
    /// (a clump of media went out while it ran): nothing was learnt, and
    /// nothing is held against the rate.
    Disturbed,
}

impl ProbeFailure {
    /// Stable, low-cardinality label for logs and the counter.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trouble => "trouble",
            Self::Loss => "loss",
            Self::Short => "short",
            Self::NoFeedback => "no-feedback",
            Self::Disturbed => "disturbed",
        }
    }
}

/// How a probe ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeOutcome {
    /// The rate that was tried, bits per second.
    pub target_bps: u64,
    /// The rate the feedback acknowledged of the packets sent after the
    /// climb ([`PROBE_RAMP`]).
    pub carried_bps: u64,
    /// Bytes of padding the caller reported sending.
    pub padding_bytes: u64,
    /// Bytes the schedule called for and the caller did not send: held
    /// back by the bound on what may be unreported, or nothing to send.
    pub held_back_bytes: u64,
    /// `None`: the path carried the target. Otherwise why not.
    pub failure: Option<ProbeFailure>,
    /// When the next probe may start.
    pub next_allowed: Instant,
}

impl ProbeOutcome {
    /// Whether the path carried the target.
    #[must_use]
    pub const fn carried(&self) -> bool {
        self.failure.is_none()
    }
}

#[derive(Debug, Clone)]
struct Run {
    target_bps: u64,
    /// What was being sent when the probe started; the climb starts
    /// here. At least the target means no climb.
    base_bps: u64,
    /// The feedback's usual lag, as the caller measured it: how long a
    /// report takes to speak of the oldest packet in it, and of the
    /// newest.
    feedback_lag: Duration,
    feedback_floor: Duration,
    started: Instant,
    /// Bytes sent the subscriber since the start, media and padding, as
    /// the caller reported them.
    sent_bytes: u64,
    /// What the caller sends this subscriber and does not report
    /// (second copies of light packets), bits per second: part of the
    /// rate the path has to carry, not of the schedule.
    uncounted_bps: u64,
    /// When the newest packet a report has spoken of was sent.
    newest_reported: Instant,
    /// What the caller reported sending, as (when, bytes sent up to and
    /// including it), oldest first; entries at or before
    /// `newest_reported` are dropped.
    sent_marks: std::collections::VecDeque<(Instant, u64)>,
    /// `sent_bytes` as of the newest packet a report has spoken of.
    reported_upto_bytes: u64,
    padding_bytes: u64,
    /// Bytes acknowledged of the packets sent after the climb.
    plateau_acked_bytes: u64,
    /// Of the packets sent during the probe: bytes acknowledged, packets
    /// reported on, packets reported missing.
    acked_bytes: u64,
    reported: u64,
    lost: u64,
    /// Feedback has come for a packet sent after the padding ended.
    feedback_past_end: bool,
    /// The rate the reports of this probe acknowledge, smoothed; starts
    /// at `base_bps`. And the bytes acknowledged since the last report
    /// ended, and when that was.
    delivered_bps: u64,
    report_bytes: u64,
    last_report: Instant,
}

impl Run {
    fn end(&self) -> Instant {
        self.started + PROBE_DURATION
    }

    /// How long the climb lasts: nothing when there is nothing to climb.
    fn ramp(&self) -> Duration {
        if self.base_bps >= self.target_bps {
            Duration::ZERO
        } else {
            PROBE_RAMP
        }
    }

    /// Bytes the schedule has sent by `elapsed`: the climb from
    /// `base_bps` to the target, then the target.
    fn due_bytes(&self, elapsed: Duration) -> u64 {
        let ramp = self.ramp();
        if ramp.is_zero() {
            return bytes_at(self.target_bps, elapsed);
        }
        let climb = self.target_bps - self.base_bps;
        let in_ramp = elapsed.min(ramp);
        let after = elapsed.saturating_sub(ramp);
        // The area under a straight climb: base over the whole time,
        // plus the triangle, plus the full climb after it.
        let micros = |d: Duration| u128::from(u64::try_from(d.as_micros()).unwrap_or(u64::MAX));
        let ramp_us = micros(ramp).max(1);
        let triangle_bits =
            u128::from(climb) * micros(in_ramp) * micros(in_ramp) / (2 * ramp_us) / 1_000_000;
        let triangle = u64::try_from(triangle_bits / 8).unwrap_or(u64::MAX);
        bytes_at(self.base_bps, elapsed)
            .saturating_add(triangle)
            .saturating_add(bytes_at(climb, after))
    }

    /// Bytes the caller reported sending in the probe that the feedback
    /// has not reported on: everything sent after the newest packet a
    /// report has spoken of. Reckoned by time, so that packets the
    /// caller does not report sending (retransmissions, second copies)
    /// cannot make it drift.
    fn outstanding_bytes(&self) -> u64 {
        self.sent_bytes.saturating_sub(self.reported_upto_bytes)
    }

    /// The most that may be outstanding at `now`
    /// ([`PROBE_QUEUE_ALLOWANCE`]): what the path delivers in the time
    /// the feedback has had nothing to say (since its last report, and
    /// never more than its usual lag), and in the allowance on top.
    fn outstanding_cap(&self, now: Instant) -> u64 {
        let silent = (now.saturating_duration_since(self.last_report) + self.feedback_floor)
            .min(self.feedback_lag);
        bytes_at(
            self.delivered_bps.min(self.target_bps),
            silent + PROBE_QUEUE_ALLOWANCE,
        )
    }

    /// The rate acknowledged of what was sent after the climb.
    fn plateau_bps(&self) -> u64 {
        let plateau = PROBE_DURATION.saturating_sub(self.ramp());
        let micros = u128::from(u64::try_from(plateau.as_micros()).unwrap_or(u64::MAX)).max(1);
        u64::try_from(u128::from(self.plateau_acked_bytes) * 8 * 1_000_000 / micros)
            .unwrap_or(u64::MAX)
    }
}

/// The probe of one subscriber's downlink: at most one running, and the
/// wait before the next.
#[derive(Debug, Clone)]
pub struct DownlinkProbe {
    run: Option<Run>,
    /// The wait a failure costs next time.
    retry: Duration,
    next_allowed: Option<Instant>,
    /// The lowest rate a probe failed at since one last passed at that
    /// rate or more, and when a probe toward less may start.
    failed_bps: Option<u64>,
    below_allowed: Option<Instant>,
}

impl Default for DownlinkProbe {
    fn default() -> Self {
        Self {
            run: None,
            retry: PROBE_FIRST_RETRY,
            next_allowed: None,
            failed_bps: None,
            below_allowed: None,
        }
    }
}

impl DownlinkProbe {
    /// No probe has run.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a probe is running.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.run.is_some()
    }

    /// The target of the running probe.
    #[must_use]
    pub fn target_bps(&self) -> Option<u64> {
        self.run
            .as_ref()
            .map(|run| run.target_bps.saturating_add(run.uncounted_bps))
    }

    /// Whether a probe may start at `now`: none is running and the wait
    /// after the last one, or after the last trouble, is over.
    #[must_use]
    pub fn can_start(&self, now: Instant) -> bool {
        self.run.is_none() && self.next_allowed.map_or(true, |at| now >= at)
    }

    /// The lowest rate a probe failed at since one last passed at that
    /// rate or more: the path is known not to carry it for now.
    #[must_use]
    pub const fn failed_bps(&self) -> Option<u64> {
        self.failed_bps
    }

    /// Whether a probe toward `target_bps` may start at `now`. As
    /// [`Self::can_start`]; and a target below the rate the last failed
    /// probe tried may start [`PROBE_BELOW_FAILED_AFTER`] after that
    /// failure, without waiting out the growing interval.
    #[must_use]
    pub fn can_start_toward(&self, target_bps: u64, now: Instant) -> bool {
        if self.can_start(now) {
            return true;
        }
        self.run.is_none()
            && self
                .failed_bps
                .is_some_and(|failed| is_below(target_bps, failed))
            && self.below_allowed.map_or(true, |at| now >= at)
    }

    /// Start a probe toward `target_bps` (at most [`PROBE_MAX_BPS`])
    /// with no climb and the default feedback lag. Returns `false`, and
    /// starts nothing, when none may start ([`Self::can_start_toward`])
    /// or the target is zero.
    pub fn start(&mut self, target_bps: u64, now: Instant) -> bool {
        self.start_from(
            target_bps,
            target_bps,
            (PROBE_MIN_FEEDBACK_LAG, PROBE_DEFAULT_FEEDBACK_LAG),
            now,
        )
    }

    /// Start a probe toward `target_bps` that climbs from `base_bps`,
    /// what is being sent now, over [`PROBE_RAMP`]; `feedback_lag` is
    /// how long the subscriber's feedback usually takes to speak of a
    /// packet, as (the least, for the newest packet of a report; the
    /// most, for its oldest), each held to
    /// [`PROBE_MIN_FEEDBACK_LAG`]..=[`PROBE_MAX_FEEDBACK_LAG`]. Otherwise
    /// as [`Self::start`].
    pub fn start_from(
        &mut self,
        target_bps: u64,
        base_bps: u64,
        feedback_lag: (Duration, Duration),
        now: Instant,
    ) -> bool {
        let (floor, lag) = feedback_lag;
        let lag = lag.clamp(PROBE_MIN_FEEDBACK_LAG, PROBE_MAX_FEEDBACK_LAG);
        let floor = floor.clamp(PROBE_MIN_FEEDBACK_LAG, lag);
        if !self.can_start_toward(target_bps, now) || target_bps == 0 {
            return false;
        }
        self.run = Some(Run {
            target_bps: target_bps.min(PROBE_MAX_BPS),
            base_bps,
            feedback_lag: lag,
            feedback_floor: floor,
            delivered_bps: base_bps.min(target_bps),
            report_bytes: 0,
            last_report: now,
            started: now,
            sent_bytes: 0,
            uncounted_bps: 0,
            newest_reported: now,
            sent_marks: std::collections::VecDeque::new(),
            reported_upto_bytes: 0,
            padding_bytes: 0,
            plateau_acked_bytes: 0,
            acked_bytes: 0,
            reported: 0,
            lost: 0,
            feedback_past_end: false,
        });
        true
    }

    /// Tell the running probe what the caller sends the subscriber
    /// without reporting it through [`Self::on_sent`] (second copies of
    /// light packets), bits per second. The target and the base given at
    /// the start are then the rates **without** it, and the verdict asks
    /// for the feedback to have acknowledged it on top.
    pub fn set_uncounted(&mut self, bps: u64) {
        if let Some(run) = self.run.as_mut() {
            run.uncounted_bps = bps;
        }
    }

    /// The caller sent the subscriber `bytes` at `at`: media, or padding
    /// for this probe. Counted while the probe's padding time runs.
    pub fn on_sent(&mut self, bytes: usize, padding: bool, at: Instant) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        if at < run.started || at >= run.end() {
            return;
        }
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        run.sent_bytes = run.sent_bytes.saturating_add(bytes);
        if run.sent_marks.len() < MAX_SENT_MARKS {
            run.sent_marks.push_back((at, run.sent_bytes));
        }
        if padding {
            run.padding_bytes = run.padding_bytes.saturating_add(bytes);
        }
    }

    /// How many bytes of padding are owed at `now` for what was sent
    /// since the probe started to add up to its schedule (the climb,
    /// then the target rate); zero when no probe is running or its
    /// padding time is over. At most [`PROBE_BURST`] worth of the
    /// target, and nothing while more is outstanding than the path may
    /// be made to queue ([`PROBE_QUEUE_ALLOWANCE`]).
    #[must_use]
    pub fn padding_owed(&self, now: Instant) -> usize {
        let Some(run) = self.run.as_ref() else {
            return 0;
        };
        if now >= run.end() {
            return 0;
        }
        let elapsed = now.saturating_duration_since(run.started);
        let owed = run.due_bytes(elapsed).saturating_sub(run.sent_bytes);
        let burst = bytes_at(run.target_bps, PROBE_BURST);
        let room = run
            .outstanding_cap(now)
            .saturating_sub(run.outstanding_bytes());
        usize::try_from(owed.min(burst).min(room)).unwrap_or(usize::MAX)
    }

    /// When the caller should come back for padding: [`PROBE_TICK`] from
    /// `now` while a probe's padding time runs.
    #[must_use]
    pub fn next_tick(&self, now: Instant) -> Option<Instant> {
        self.run
            .as_ref()
            .filter(|run| now < run.end())
            .map(|_| now + PROBE_TICK)
    }

    /// The subscriber's feedback on one packet: when the SFU sent it, its
    /// size, and whether it arrived.
    pub fn on_feedback(&mut self, sent: Instant, size: usize, received: bool) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        if sent < run.started {
            return;
        }
        if sent > run.newest_reported {
            run.newest_reported = sent;
            while run.sent_marks.front().is_some_and(|(at, _)| *at <= sent) {
                if let Some((_, upto)) = run.sent_marks.pop_front() {
                    run.reported_upto_bytes = upto;
                }
            }
        }
        if sent >= run.end() {
            run.feedback_past_end = true;
            return;
        }
        run.reported = run.reported.saturating_add(1);
        if received {
            let size = u64::try_from(size).unwrap_or(u64::MAX);
            run.acked_bytes = run.acked_bytes.saturating_add(size);
            run.report_bytes = run.report_bytes.saturating_add(size);
            if sent >= run.started + run.ramp() {
                run.plateau_acked_bytes = run.plateau_acked_bytes.saturating_add(size);
            }
        } else {
            run.lost = run.lost.saturating_add(1);
        }
    }

    /// One feedback report has been handed over in full
    /// ([`Self::on_feedback`] for each of its packets) at `now`: the rate
    /// it acknowledged since the report before goes into the rate the
    /// path is taken to deliver. Reports closer together than 20 ms are
    /// taken as one.
    pub fn on_report(&mut self, now: Instant) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let since = now.saturating_duration_since(run.last_report);
        if since < Duration::from_millis(20) {
            return;
        }
        let micros = u128::from(u64::try_from(since.as_micros()).unwrap_or(u64::MAX)).max(1);
        let sample = u64::try_from(u128::from(run.report_bytes) * 8 * 1_000_000 / micros)
            .unwrap_or(u64::MAX);
        // Rising at once, falling by halves: what the last report
        // acknowledged the path did deliver, and a path that carries the
        // climb must not hold the probe back by the rate of a moment
        // ago.
        run.delivered_bps = sample.max(run.delivered_bps / 2 + sample / 2);
        run.report_bytes = 0;
        run.last_report = now;
    }

    /// The path estimator lowered its estimate at `now`. A running probe
    /// fails here and now; with none running the next one waits
    /// [`PROBE_AFTER_TROUBLE`] from `now` at least.
    pub fn on_trouble(&mut self, now: Instant) -> Option<ProbeOutcome> {
        self.on_trouble_while(now, false)
    }

    /// Give the running probe up at `now` without a verdict
    /// ([`ProbeFailure::Disturbed`]): the rate is not remembered as one
    /// that failed and the wait a failure costs does not grow; the next
    /// probe may start [`PROBE_AFTER_TROUBLE`] from `now`.
    pub fn abandon(&mut self, now: Instant) -> Option<ProbeOutcome> {
        let run = self.run.take()?;
        let at = now + PROBE_AFTER_TROUBLE;
        self.next_allowed = Some(self.next_allowed.map_or(at, |old| old.max(at)));
        self.below_allowed = self.below_allowed.map(|old| old.max(at));
        Some(ProbeOutcome {
            target_bps: run.target_bps.saturating_add(run.uncounted_bps),
            carried_bps: run.plateau_bps(),
            padding_bytes: run.padding_bytes,
            held_back_bytes: 0,
            failure: Some(ProbeFailure::Disturbed),
            next_allowed: at,
        })
    }

    /// As [`Self::on_trouble`]; `after_probe` says the trouble is the
    /// aftermath of a probe that has just ended (the queue it filled),
    /// which that probe's failure has already been charged for: no
    /// further wait is added.
    pub fn on_trouble_while(&mut self, now: Instant, after_probe: bool) -> Option<ProbeOutcome> {
        if let Some(run) = self.run.take() {
            return Some(self.finish(run, Some(ProbeFailure::Trouble), now));
        }
        if after_probe {
            return None;
        }
        let wait = now + PROBE_AFTER_TROUBLE;
        self.next_allowed = Some(self.next_allowed.map_or(wait, |at| at.max(wait)));
        self.below_allowed = Some(self.below_allowed.map_or(wait, |at| at.max(wait)));
        None
    }

    /// The outcome of the running probe, once it can be told: feedback
    /// has come for a packet sent after its padding ended, or
    /// [`PROBE_RESULT_WAIT`] has passed since then. `None` while it runs
    /// and when none does.
    pub fn poll(&mut self, now: Instant) -> Option<ProbeOutcome> {
        let running = self.run.as_ref()?;
        if now < running.end() {
            return None;
        }
        if !(running.feedback_past_end || now >= running.end() + PROBE_RESULT_WAIT) {
            return None;
        }
        let run = self.run.take()?;
        let carried = run.plateau_bps();
        let failure = if run.reported == 0 {
            Some(ProbeFailure::NoFeedback)
        } else if run.lost.saturating_mul(PROBE_MAX_LOSS_DEN)
            > run.reported.saturating_mul(PROBE_MAX_LOSS_NUM)
        {
            Some(ProbeFailure::Loss)
        } else if carried.saturating_mul(PROBE_PASS_DEN)
            < run
                .target_bps
                .saturating_add(run.uncounted_bps)
                .saturating_mul(PROBE_PASS_NUM)
            // Held back during the climb and yet the whole target
            // acknowledged on the plateau: the path showed the rate.
            || (carried < run.target_bps.saturating_add(run.uncounted_bps)
                && run
                    .sent_bytes
                    .saturating_add(bytes_at(run.target_bps, PROBE_SHORTFALL))
                    < run.due_bytes(PROBE_DURATION))
        {
            Some(ProbeFailure::Short)
        } else {
            None
        };
        Some(self.finish(run, failure, now))
    }

    fn finish(&mut self, run: Run, failure: Option<ProbeFailure>, now: Instant) -> ProbeOutcome {
        let tried = run.target_bps.saturating_add(run.uncounted_bps);
        let below = self
            .failed_bps
            .is_some_and(|failed| is_below(tried, failed));
        let next_allowed = if failure.is_some() {
            // The path does not carry this rate: the next try at it, or
            // at more, waits the growing interval; a try at less waits
            // only for the queue to drain.
            let wait = self.retry;
            self.retry = (self.retry * 2).min(PROBE_MAX_RETRY);
            self.failed_bps = Some(self.failed_bps.map_or(tried, |failed| failed.min(tried)));
            self.below_allowed = Some(now + PROBE_BELOW_FAILED_AFTER);
            let at = now + wait;
            self.next_allowed = Some(self.next_allowed.map_or(at, |old| old.max(at)));
            at
        } else if below {
            // A smaller set was carried. The rate that failed is still
            // the rate that failed, and its interval runs on.
            self.below_allowed = Some(now + PROBE_AFTER_SUCCESS);
            now + PROBE_AFTER_SUCCESS
        } else {
            self.retry = PROBE_FIRST_RETRY;
            self.failed_bps = None;
            self.below_allowed = None;
            let at = now + PROBE_AFTER_SUCCESS;
            self.next_allowed = Some(at);
            at
        };
        ProbeOutcome {
            target_bps: tried,
            carried_bps: run.plateau_bps(),
            padding_bytes: run.padding_bytes,
            held_back_bytes: run
                .due_bytes(
                    now.saturating_duration_since(run.started)
                        .min(PROBE_DURATION),
                )
                .saturating_sub(run.sent_bytes),
            failure,
            next_allowed,
        }
    }
}

/// Whether `target_bps` is clearly less than a rate that failed
/// ([`PROBE_BELOW_FAILED_NUM`]).
const fn is_below(target_bps: u64, failed_bps: u64) -> bool {
    target_bps.saturating_mul(PROBE_BELOW_FAILED_DEN)
        <= failed_bps.saturating_mul(PROBE_BELOW_FAILED_NUM)
}

/// Most sends remembered for one probe; beyond it a send still counts
/// as outstanding until the probe ends.
const MAX_SENT_MARKS: usize = 16_384;

/// Bytes sent in `elapsed` at `bps`.
fn bytes_at(bps: u64, elapsed: Duration) -> u64 {
    let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
    let bits = u128::from(bps) * u128::from(micros) / 1_000_000;
    u64::try_from(bits / 8).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: u64 = 8_000_000;
    const PACKET: usize = 1000;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Run a probe from `t0`: the caller sends media at `media_bps` and
    /// the padding owed every tick, and the path acknowledges every
    /// packet except one in `lose_every` (zero: none lost).
    fn run_probe(
        probe: &mut DownlinkProbe,
        t0: Instant,
        media_bps: u64,
        lose_every: u64,
    ) -> (u64, ProbeOutcome) {
        run_probe_toward(probe, t0, TARGET, media_bps, lose_every)
    }

    /// As [`run_probe`], toward `target`.
    fn run_probe_toward(
        probe: &mut DownlinkProbe,
        t0: Instant,
        target: u64,
        media_bps: u64,
        lose_every: u64,
    ) -> (u64, ProbeOutcome) {
        assert!(probe.start(target, t0));
        let mut padding = 0u64;
        let mut n = 0u64;
        let mut tick = 0u64;
        loop {
            let now = t0 + ms(5 * tick);
            if now >= t0 + PROBE_DURATION {
                break;
            }
            let media = usize::try_from(bytes_at(media_bps, ms(5))).expect("small");
            let mut send = |probe: &mut DownlinkProbe, bytes: usize, pad: bool| {
                probe.on_sent(bytes, pad, now);
                n += 1;
                let received = lose_every == 0 || n % lose_every != 0;
                probe.on_feedback(now, bytes, received);
            };
            if media > 0 {
                send(probe, media, false);
            }
            let mut owed = probe.padding_owed(now);
            while owed >= PACKET {
                send(probe, PACKET, true);
                padding += PACKET as u64;
                owed -= PACKET;
            }
            assert!(probe.poll(now).is_none(), "no verdict while it runs");
            tick += 1;
        }
        let end = t0 + PROBE_DURATION;
        // Feedback for a packet sent after the padding ended.
        probe.on_feedback(end + ms(10), PACKET, true);
        let outcome = probe.poll(end + ms(20)).expect("a verdict");
        (padding, outcome)
    }

    #[test]
    fn a_probe_pads_what_is_sent_up_to_its_target_and_is_carried() {
        let t0 = Instant::now();
        let mut probe = DownlinkProbe::new();
        assert!(probe.can_start(t0) && !probe.is_running());
        let (padding, outcome) = run_probe(&mut probe, t0, 5_000_000, 0);
        assert!(outcome.carried(), "{outcome:?}");
        assert_eq!(outcome.target_bps, TARGET);
        // Media 5 Mbit/s for a second, the rest padding: about 3 Mbit/s.
        assert!(
            (340_000..=380_000).contains(&padding),
            "padding {padding} bytes"
        );
        assert_eq!(outcome.padding_bytes, padding);
        assert!(outcome.carried_bps >= TARGET * 9 / 10 && outcome.carried_bps <= TARGET);
        assert!(!probe.is_running());
        // The next may start half a second later, not before.
        let done = t0 + PROBE_DURATION + ms(20);
        assert_eq!(outcome.next_allowed, done + PROBE_AFTER_SUCCESS);
        assert!(!probe.can_start(done + ms(499)));
        assert!(probe.can_start(done + PROBE_AFTER_SUCCESS));
    }

    #[test]
    fn media_at_the_target_needs_no_padding() {
        let t0 = Instant::now();
        let mut probe = DownlinkProbe::new();
        let (padding, outcome) = run_probe(&mut probe, t0, TARGET, 0);
        assert_eq!(padding, 0);
        assert!(outcome.carried());
    }

    #[test]
    fn a_probe_whose_packets_are_lost_fails_on_loss() {
        let t0 = Instant::now();
        let mut probe = DownlinkProbe::new();
        // One in five lost: 20%, above the tenth allowed.
        let (_, outcome) = run_probe(&mut probe, t0, 5_000_000, 5);
        assert_eq!(outcome.failure, Some(ProbeFailure::Loss));
        // One in twenty: within it, and nine tenths of the target is
        // still acknowledged.
        let t1 = outcome.next_allowed;
        let (_, outcome) = run_probe(&mut probe, t1, 5_000_000, 20);
        assert!(outcome.carried(), "{outcome:?}");
    }

    #[test]
    fn a_probe_the_path_did_not_carry_fails_as_short() {
        let t0 = Instant::now();
        let mut probe = DownlinkProbe::new();
        assert!(probe.start(TARGET, t0));
        // Everything is sent, and feedback comes for six megabits of it
        // with nothing reported missing (the rest is still queued).
        for tick in 0..200u64 {
            let now = t0 + ms(5 * tick);
            let owed = probe.padding_owed(now);
            probe.on_sent(owed, true, now);
            probe.on_feedback(now, 3750, true);
        }
        let end = t0 + PROBE_DURATION;
        assert!(probe.poll(end + ms(499)).is_none(), "still waiting");
        let outcome = probe.poll(end + PROBE_RESULT_WAIT).expect("judged");
        assert_eq!(outcome.failure, Some(ProbeFailure::Short));
        assert_eq!(outcome.carried_bps, 6_000_000);
    }

    #[test]
    fn a_probe_without_feedback_fails() {
        let t0 = Instant::now();
        let mut probe = DownlinkProbe::new();
        assert!(probe.start(TARGET, t0));
        let end = t0 + PROBE_DURATION;
        assert!(probe.poll(end).is_none());
        let outcome = probe.poll(end + PROBE_RESULT_WAIT).expect("judged");
        assert_eq!(outcome.failure, Some(ProbeFailure::NoFeedback));
    }

    #[test]
    fn trouble_ends_a_running_probe_at_once_and_holds_the_next_back() {
        let t0 = Instant::now();
        let mut probe = DownlinkProbe::new();
        assert!(probe.start(TARGET, t0));
        assert!(probe.padding_owed(t0 + ms(100)) > 0);
        let outcome = probe.on_trouble(t0 + ms(300)).expect("it was running");
        assert_eq!(outcome.failure, Some(ProbeFailure::Trouble));
        assert!(!probe.is_running());
        assert_eq!(probe.padding_owed(t0 + ms(301)), 0, "padding stops");
        assert!(!probe.can_start(t0 + ms(5_299)));
        assert!(probe.can_start(t0 + ms(300) + PROBE_FIRST_RETRY));

        // With none running, trouble holds the next one back from then.
        let mut idle = DownlinkProbe::new();
        assert!(idle.on_trouble(t0).is_none());
        assert!(!idle.can_start(t0 + PROBE_AFTER_TROUBLE.saturating_sub(ms(1))));
        assert!(idle.can_start(t0 + PROBE_AFTER_TROUBLE));
        // The aftermath of a probe that has just failed adds no wait.
        let mut after = DownlinkProbe::new();
        assert!(after.on_trouble_while(t0, true).is_none());
        assert!(after.can_start(t0));
        assert!(!idle.start(TARGET, t0 + ms(10)), "nothing starts early");
    }

    #[test]
    fn each_failure_doubles_the_wait_up_to_the_cap_and_a_success_resets_it() {
        let mut now = Instant::now();
        let mut probe = DownlinkProbe::new();
        let mut waits = Vec::new();
        for _ in 0..4 {
            assert!(probe.start(TARGET, now));
            let failed_at = now + ms(200);
            let outcome = probe.on_trouble(failed_at).expect("running");
            waits.push(outcome.next_allowed.duration_since(failed_at));
            now = outcome.next_allowed;
        }
        assert_eq!(
            waits,
            vec![
                PROBE_FIRST_RETRY,
                PROBE_MAX_RETRY,
                PROBE_MAX_RETRY,
                PROBE_MAX_RETRY
            ]
        );
        assert!(PROBE_FIRST_RETRY < PROBE_MAX_RETRY && PROBE_MAX_RETRY < PROBE_FIRST_RETRY * 2);
        // A probe that is carried puts the wait back to the first one.
        let (_, outcome) = run_probe(&mut probe, now, TARGET, 0);
        assert!(outcome.carried());
        now = outcome.next_allowed;
        assert!(probe.start(TARGET, now));
        let outcome = probe.on_trouble(now + ms(1)).expect("running");
        assert_eq!(
            outcome.next_allowed.duration_since(now + ms(1)),
            PROBE_FIRST_RETRY
        );
    }

    /// The growing wait is for the rate that failed and for more; a
    /// smaller set may be tried a second later, and its being carried
    /// neither shortens that wait nor forgets the rate that failed.
    #[test]
    fn a_smaller_rate_than_the_one_that_failed_need_not_wait_the_interval() {
        let t0 = Instant::now();
        let mut probe = DownlinkProbe::new();
        assert_eq!(probe.failed_bps(), None);
        assert!(probe.start(TARGET, t0));
        let failed_at = t0 + ms(200);
        let outcome = probe.on_trouble(failed_at).expect("running");
        assert_eq!(probe.failed_bps(), Some(TARGET));
        let frontier = outcome.next_allowed;
        assert_eq!(frontier, failed_at + PROBE_FIRST_RETRY);

        let smaller = TARGET / 2;
        let soon = failed_at + PROBE_BELOW_FAILED_AFTER;
        assert!(!probe.can_start_toward(
            smaller,
            failed_at + PROBE_BELOW_FAILED_AFTER.saturating_sub(ms(1))
        ));
        assert!(probe.can_start_toward(smaller, soon));
        assert!(!probe.can_start_toward(TARGET, soon), "the same rate waits");
        assert!(
            !probe.can_start_toward(TARGET - TARGET / 10, soon),
            "and so does one a tenth smaller: the same set, measured again"
        );
        assert!(
            !probe.can_start_toward(TARGET * 2, soon),
            "and so does more"
        );
        assert!(!probe.start(TARGET, soon));

        // The smaller one is carried.
        let (_, outcome) = run_probe_toward(&mut probe, soon, smaller, 0, 0);
        assert!(outcome.carried());
        assert_eq!(probe.failed_bps(), Some(TARGET), "still known not to fit");
        assert!(
            !probe.can_start(failed_at + PROBE_FIRST_RETRY.saturating_sub(ms(1))),
            "its interval runs on"
        );
        assert!(probe.can_start(frontier));

        // The rate that failed is carried at last: nothing is held back.
        let (_, outcome) = run_probe(&mut probe, frontier, 0, 0);
        assert!(outcome.carried());
        assert_eq!(probe.failed_bps(), None);
        assert!(probe.can_start(outcome.next_allowed));
    }

    /// A smaller rate that fails too becomes the rate known not to fit.
    #[test]
    fn the_lowest_rate_that_failed_is_the_one_remembered() {
        let t0 = Instant::now();
        let mut probe = DownlinkProbe::new();
        assert!(probe.start(TARGET, t0));
        let _ = probe.on_trouble(t0 + ms(100)).expect("running");
        let soon = t0 + ms(100) + PROBE_BELOW_FAILED_AFTER;
        assert!(probe.start(TARGET / 2, soon));
        let _ = probe.on_trouble(soon + ms(100)).expect("running");
        assert_eq!(probe.failed_bps(), Some(TARGET / 2));
        // Trouble on the path with no probe running holds every probe.
        let later = soon + ms(100) + PROBE_BELOW_FAILED_AFTER;
        assert!(probe.can_start_toward(TARGET / 4, later));
        assert!(probe.on_trouble(later).is_none());
        assert!(!probe.can_start_toward(TARGET / 4, later + ms(1)));
        assert!(probe.can_start_toward(TARGET / 4, later + PROBE_AFTER_TROUBLE));
    }

    /// A probe climbs to its target: half way through the climb it has
    /// asked for less than half of what a jump would have.
    #[test]
    fn a_probe_climbs_to_its_target() {
        let t0 = Instant::now();
        let mut probe = DownlinkProbe::new();
        let base = TARGET / 2;
        assert!(probe.start_from(TARGET, base, (ms(30), ms(100)), t0));
        let run = probe.run.clone().expect("running");
        assert_eq!(run.due_bytes(Duration::ZERO), 0);
        let half = PROBE_RAMP / 2;
        let climbed = run.due_bytes(half) - bytes_at(base, half);
        let jumped = bytes_at(TARGET - base, half);
        assert_eq!(
            climbed,
            jumped / 4,
            "a quarter of the jump's bytes by half way"
        );
        // After the climb the schedule runs at the target.
        let a = run.due_bytes(PROBE_RAMP + ms(100));
        let b = run.due_bytes(PROBE_RAMP + ms(200));
        assert_eq!(b - a, bytes_at(TARGET, ms(100)));
        // And the verdict is on what was sent after the climb.
        probe.on_sent(1000, true, t0 + ms(10));
        probe.on_feedback(t0 + ms(10), 1000, true);
        assert_eq!(probe.run.clone().expect("running").plateau_acked_bytes, 0);
        probe.on_feedback(t0 + PROBE_RAMP, 1000, true);
        assert_eq!(
            probe.run.clone().expect("running").plateau_acked_bytes,
            1000
        );
    }

    /// With no feedback at all a probe stops padding by itself: what it
    /// has sent and not heard of is bounded by the target rate over the
    /// feedback's lag and the queue it may build.
    #[test]
    fn padding_stops_when_feedback_does() {
        let t0 = Instant::now();
        let mut probe = DownlinkProbe::new();
        let lag = ms(100);
        let base = TARGET / 2;
        assert!(probe.start_from(TARGET, base, (ms(30), lag), t0));
        let mut padded = 0u64;
        for tick in 0..200u64 {
            let now = t0 + ms(5 * tick);
            let mut owed = probe.padding_owed(now);
            while owed >= PACKET {
                probe.on_sent(PACKET, true, now);
                padded += PACKET as u64;
                owed = probe.padding_owed(now);
            }
        }
        let cap = bytes_at(base, lag + PROBE_QUEUE_ALLOWANCE);
        assert!(padded <= cap + PACKET as u64, "{padded} > {cap}");
        assert!(padded > cap / 2, "and it did pad up to it: {padded}");
        let outcome = probe
            .poll(t0 + PROBE_DURATION + PROBE_RESULT_WAIT)
            .expect("judged");
        assert_eq!(outcome.failure, Some(ProbeFailure::NoFeedback));
    }

    /// A path that carries `capacity` behind a queue, with feedback
    /// reports every 100 ms: run a probe toward `TARGET` climbing from
    /// `base` and return the deepest queue (as milliseconds at the
    /// path's rate) and the outcome.
    fn probe_through_a_queue(capacity: u64, base: u64) -> (u64, ProbeOutcome) {
        probe_toward_through_a_queue(TARGET, capacity, base)
    }

    /// [`probe_through_a_queue`] toward `target`.
    fn probe_toward_through_a_queue(target: u64, capacity: u64, base: u64) -> (u64, ProbeOutcome) {
        let t0 = Instant::now();
        let mut probe = DownlinkProbe::new();
        assert!(probe.start_from(target, base, (ms(30), ms(100)), t0));
        let mut queue: std::collections::VecDeque<Instant> = std::collections::VecDeque::new();
        let mut left: Vec<Instant> = Vec::new();
        let mut credit = 0u64;
        let mut deepest = 0usize;
        for tick in 0..300u64 {
            let now = t0 + ms(5 * tick);
            credit += bytes_at(capacity, ms(5));
            while credit >= PACKET as u64 {
                let Some(sent) = queue.pop_front() else {
                    credit = 0;
                    break;
                };
                credit -= PACKET as u64;
                left.push(sent);
            }
            if tick % 20 == 19 {
                for sent in left.drain(..) {
                    probe.on_feedback(sent, PACKET, true);
                }
                probe.on_report(now);
            }
            // Media at `base`, then whatever padding is owed.
            let mut media = bytes_at(base, ms(5));
            while media >= PACKET as u64 {
                probe.on_sent(PACKET, false, now);
                queue.push_back(now);
                media -= PACKET as u64;
            }
            let mut owed = probe.padding_owed(now);
            while owed >= PACKET {
                probe.on_sent(PACKET, true, now);
                queue.push_back(now);
                owed = probe.padding_owed(now);
            }
            deepest = deepest.max(queue.len());
        }
        let depth_ms = (deepest * PACKET * 8) as u64 * 1000 / capacity;
        let outcome = probe
            .poll(t0 + PROBE_DURATION + PROBE_RESULT_WAIT)
            .expect("judged");
        (depth_ms, outcome)
    }

    /// On a path that carries less than the target the queue the probe
    /// builds stays near the allowance, with no help from the caller,
    /// and the probe fails as short; on a path that carries the target
    /// the same bound does not hold the probe back.
    #[test]
    fn a_probe_above_the_path_builds_a_bounded_queue() {
        let base = 4_000_000;
        for capacity in [base, 5_000_000, 6_000_000, 7_000_000] {
            let (depth_ms, outcome) = probe_through_a_queue(capacity, base);
            assert!(
                depth_ms <= 80,
                "a path of {capacity} bit/s queued {depth_ms} ms"
            );
            assert_eq!(outcome.failure, Some(ProbeFailure::Short), "{capacity}");
        }
        let (depth_ms, outcome) = probe_through_a_queue(TARGET * 2, base);
        assert!(outcome.carried(), "{outcome:?}");
        assert!(depth_ms <= 10, "{depth_ms}");
    }

    /// A probe is carried by a path that carries it, whatever the step:
    /// from the three light pictures to three middles is more than twice
    /// the rate, and a subscriber left on little must be able to get
    /// back.
    #[test]
    fn a_large_step_is_carried_by_a_path_that_carries_it() {
        for (target, base) in [
            (2_300_000, 1_000_000),
            (2_800_000, 1_000_000),
            (4_700_000, 1_500_000),
            (9_000_000, 5_100_000),
            (12_000_000, 1_000_000),
        ] {
            let (depth_ms, outcome) = probe_toward_through_a_queue(target, target * 2, base);
            assert!(outcome.carried(), "{base} to {target}: {outcome:?}");
            // Bursts of 20 ms of the target on a path of twice the
            // target: a queue of a few tens of milliseconds at most.
            assert!(depth_ms <= 40, "{base} to {target}: {depth_ms} ms");
        }
    }

    /// A probe given up is not a probe that failed: the same rate may be
    /// tried again after the short wait, and the wait a failure costs
    /// has not grown.
    #[test]
    fn a_probe_given_up_holds_nothing_against_its_rate() {
        let t0 = Instant::now();
        let mut probe = DownlinkProbe::new();
        assert!(probe.start(TARGET, t0));
        let outcome = probe.abandon(t0 + ms(300)).expect("was running");
        assert_eq!(outcome.failure, Some(ProbeFailure::Disturbed));
        assert!(!outcome.carried());
        assert_eq!(probe.failed_bps(), None);
        let again = t0 + ms(300) + PROBE_AFTER_TROUBLE;
        assert!(!probe.can_start_toward(TARGET, t0 + ms(299) + PROBE_AFTER_TROUBLE));
        assert!(probe.can_start_toward(TARGET, again));
        // And a real failure after it still costs the first interval.
        assert!(probe.start(TARGET, again));
        let failed = probe.on_trouble(again + ms(300)).expect("was running");
        assert_eq!(failed.next_allowed, again + ms(300) + PROBE_FIRST_RETRY);
        assert!(probe.abandon(again + ms(400)).is_none(), "none running");
    }

    #[test]
    fn padding_is_bounded() {
        let t0 = Instant::now();
        let mut probe = DownlinkProbe::new();
        // A target above the cap is the cap.
        assert!(probe.start(u64::MAX, t0));
        assert_eq!(probe.target_bps(), Some(PROBE_MAX_BPS));
        // A caller that was away for a tenth of a second is owed one burst.
        let burst = usize::try_from(bytes_at(PROBE_MAX_BPS, PROBE_BURST)).expect("fits");
        assert_eq!(probe.padding_owed(t0 + ms(100)), burst);
        assert_eq!(burst, 50_000);
        // Nothing is owed once the padding time is over, and what is
        // reported sent after it is not counted.
        assert_eq!(probe.padding_owed(t0 + PROBE_DURATION), 0);
        probe.on_sent(1_000, true, t0 + PROBE_DURATION);
        assert!(probe.next_tick(t0 + ms(10)).is_some());
        assert!(probe.next_tick(t0 + PROBE_DURATION).is_none());
        let outcome = probe
            .poll(t0 + PROBE_DURATION + PROBE_RESULT_WAIT)
            .expect("judged");
        assert_eq!(outcome.padding_bytes, 0);
        // Only one at a time, and never toward nothing.
        let mut probe = DownlinkProbe::new();
        assert!(!probe.start(0, t0));
        assert!(probe.start(TARGET, t0));
        assert!(!probe.start(TARGET, t0 + ms(1)));
        // Without a probe nothing is owed or counted.
        let mut idle = DownlinkProbe::new();
        assert_eq!(idle.padding_owed(t0), 0);
        idle.on_sent(1, true, t0);
        idle.on_feedback(t0, 1, true);
        assert!(idle.poll(t0).is_none());
        assert!(idle.next_tick(t0).is_none());
    }

    #[test]
    fn feedback_on_packets_sent_before_the_probe_is_not_its_own() {
        let before = Instant::now();
        let t0 = before + ms(1);
        let mut probe = DownlinkProbe::new();
        assert!(probe.start(TARGET, t0));
        probe.on_feedback(before, 1_000_000, true);
        probe.on_feedback(before, 1_000_000, false);
        let outcome = probe
            .poll(t0 + PROBE_DURATION + PROBE_RESULT_WAIT)
            .expect("judged");
        assert_eq!(outcome.failure, Some(ProbeFailure::NoFeedback));
        assert_eq!(outcome.carried_bps, 0);
    }
}
