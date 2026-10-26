// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! How much a publisher's camera senders may send, decided by the SFU and
//! told to the publisher over Lakko's own signaling
//! ([ADR-0068](../../../docs/adr/0068-server-side-bitrate-control.md),
//! amendment of 2026-10-11).
//!
//! ## Why
//!
//! The SFU already estimates what each publisher's path carries
//! ([`crate::uplink_estimate`]) and says so with REMB. Measured on live
//! calls (2026-10-11, packet capture at the SFU): with the estimate held at
//! 1000 kbit/s one browser's main sender followed it and another's did not
//! (a flat 1537 kbit/s), while that same browser kept its light sender,
//! which carries a client-set ceiling, under the ceiling. A publisher that
//! sends more than its path carries stands a queue in front of its own
//! audio, and that queue is the conversation's delay. So the estimate also
//! goes out as a per-sender ceiling in a `publish-budget` frame, a form
//! every client can obey with one call.
//!
//! ## The rule
//!
//! The server decides, the client executes: the whole allocation is here,
//! so three clients cannot drift.
//!
//! 1. **Headroom.** From the estimate take what the publisher sends beside
//!    its two heavy senders: audio (the rate arriving on its audio tracks
//!    when the caller has measured one, kept within
//!    [`AUDIO_RESERVE_MIN_BPS`] and [`AUDIO_RESERVE_MAX_BPS`]; otherwise
//!    [`AUDIO_RESERVE_BPS`], a constant) and the light sender
//!    ([`LIGHT_RESERVE_BPS`], its ceiling, a constant, when the publisher
//!    declared one). What is left is divided by `1 +`
//!    [`PROTECTION_PERCENT`]`/100`: a sender's ceiling bounds its encoder,
//!    and retransmissions and FEC ride on top of it. The share is a
//!    constant; nothing new is measured for it.
//! 2. **Screen share** ([ADR-0083](../../../docs/adr/0083-screen-share.md)).
//!    While the publisher sends a screen, its sender is given its share of
//!    what is left **before** any camera sender: everything above the
//!    floors of the camera senders that are on, kept within
//!    [`SCREEN_FLOOR_BPS`] and [`SCREEN_PLAN_BPS`]. The cameras share what
//!    the screen leaves. A publisher that sends no screen is told no screen
//!    ceiling (`screen_kbps` absent) and the rest of the rule is unchanged.
//! 3. **Middle.** The middle sender gets its plan rate
//!    ([`MIDDLE_PLAN_BPS`]). When the main sender is needed too and the
//!    budget does not hold both, the middle sender gives way until the main
//!    one has its floor.
//! 4. **Main.** The main sender gets the rest: all of it when the middle
//!    sender is not needed (or the publisher has none), what the middle
//!    sender leaves otherwise.
//! 5. **Bounds.** Never below a floor that still carries a recognisable
//!    picture ([`MAIN_FLOOR_BPS`], [`MIDDLE_FLOOR_BPS`]), never above the
//!    plan maximum ([`MAIN_PLAN_BPS`], [`MIDDLE_PLAN_BPS`]). On a path that
//!    carries less than the floors add up to, the floors win and the
//!    budget is above the estimate: the alternative is no picture.
//! 6. **A sender nobody watches** ([`crate::video_needed`]) takes nothing
//!    from the budget and is given its ceiling anyway, the one it would
//!    have if it were switched on now. It is off; the figure is what it
//!    starts under.
//!
//! ## When the publisher is told
//!
//! - The **first** budget as soon as there is an estimate to make one from
//!   (the caller decides when that is).
//! - A **lower** one at once: a ceiling below [`DROP_PERCENT`] of what was
//!   last told.
//! - A change of **which senders are needed** at once, when it changes a
//!   ceiling: the allocation changed by rule, not by noise. A screen share
//!   starting or stopping is such a change.
//! - A **higher** one (or a fall too small to be "lower") no sooner than
//!   [`RAISE_INTERVAL`] after the last frame, and only when a ceiling
//!   differs from what was told by at least [`RAISE_SHARE_PERCENT`] of it,
//!   or reaches its plan maximum. So the frame does not flap with the
//!   estimate's 8% a second climb.
//!
//! ## What it is not
//!
//! Pure bookkeeping: no I/O, no clock of its own (the caller supplies every
//! time), no packet, no key. Reading the estimate, sending the frame and
//! the REMB that goes on beside it are the caller's ([`crate::media`]).
//! Nothing here limits audio or the light sender.
//!
//! Every constant below was chosen, not measured.

use std::time::{Duration, Instant};

/// The most the main sender is ever allowed, bits per second: the nominal
/// rate of a Lakko client's 720p sender
/// ([`crate::video_needed::NOMINAL_MAIN_BPS`]).
pub const MAIN_PLAN_BPS: u64 = crate::video_needed::NOMINAL_MAIN_BPS;

/// The plan rate of the middle sender, bits per second, and the most it is
/// ever allowed: the ceiling a Lakko client sets on its half-size sender
/// ([`crate::video_needed::NOMINAL_MIDDLE_BPS`]).
pub const MIDDLE_PLAN_BPS: u64 = crate::video_needed::NOMINAL_MIDDLE_BPS;

/// The least the main sender is allowed. Chosen: about what a camera
/// encoder needs to keep a face recognisable once it has scaled the
/// picture down, and above the light sender's ceiling, so the main picture
/// is never the worse of the two.
pub const MAIN_FLOOR_BPS: u64 = 200_000;

/// The least the middle sender is allowed. Chosen: a half-size picture at
/// a fifth of its plan rate.
pub const MIDDLE_FLOOR_BPS: u64 = 100_000;

/// What audio is taken to cost when the caller has no measured rate.
/// Chosen: one Opus stream with redundancy (RED) and its packet headers.
pub const AUDIO_RESERVE_BPS: u64 = 64_000;

/// The least a measured audio rate is taken for: a silent microphone
/// (DTX) must not shrink the headroom speech then needs.
pub const AUDIO_RESERVE_MIN_BPS: u64 = 32_000;

/// The most a measured audio rate is taken for, so that one odd reading
/// cannot take the picture away.
pub const AUDIO_RESERVE_MAX_BPS: u64 = 128_000;

/// What the light sender is taken to cost: the ceiling a Lakko client
/// sets on it (150 kbit/s). A constant, not the rate arriving on it: the
/// sender may use its whole ceiling the moment the picture moves.
pub const LIGHT_RESERVE_BPS: u64 = 150_000;

/// Retransmissions and FEC, as a share of the video the two ceilings
/// allow, in percent. They ride on top of an encoder's ceiling. Chosen.
pub const PROTECTION_PERCENT: u64 = 15;

/// A ceiling below this share of what was last told, in percent, is
/// "lower" and is told at once. The same share the REMB uses for "fell"
/// (`REMB_DROP_PERCENT` in [`crate::media`]).
pub const DROP_PERCENT: u64 = 97;

/// A budget that is not lower is told no sooner than this after the last
/// frame. Chosen: a few times the estimate's own climb step, short against
/// the seconds a path takes to recover.
pub const RAISE_INTERVAL: Duration = Duration::from_secs(2);

/// And only when a ceiling differs from what was told by at least this
/// share of it, in percent (or reaches its plan maximum). Chosen.
pub const RAISE_SHARE_PERCENT: u64 = 10;

/// The most the screen-share sender is ever allowed, bits per second
/// (ADR-0083). Chosen, not measured: a screen of text and windows, encoded
/// with resolution kept over frame rate, at 1080p and a few frames a
/// second.
pub const SCREEN_PLAN_BPS: u64 = 1_500_000;

/// The least the screen-share sender is allowed (ADR-0083). Chosen, not
/// measured: about what keeps text on a shared screen readable at a lower
/// frame rate; above the main camera's floor, since a share is what the
/// whole call is looking at.
pub const SCREEN_FLOOR_BPS: u64 = 300_000;

/// What one budget is made from.
// Five independent facts about one publisher, each a yes or a no.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inputs {
    /// The SFU's estimate of what the publisher's path carries, bits per
    /// second.
    pub estimate_bps: u64,
    /// The rate arriving on the publisher's audio tracks, when the caller
    /// has measured one; `None` (or zero) takes [`AUDIO_RESERVE_BPS`].
    pub audio_bps: Option<u64>,
    /// The publisher declared a light sender.
    pub has_light: bool,
    /// The publisher declared a middle sender.
    pub has_middle: bool,
    /// Somebody is being sent the main sender (what the publisher was
    /// last told in `video-needed`; `true` when it was told nothing).
    pub main_needed: bool,
    /// And the middle sender. Read only with [`Self::has_middle`].
    pub middle_needed: bool,
    /// The publisher sends a screen share (a declared screen video line
    /// that it sends on, ADR-0083).
    pub has_screen: bool,
}

/// The content of one `publish-budget` frame: the ceilings, kilobits per
/// second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// The main sender's ceiling.
    pub main_kbps: u32,
    /// The middle sender's ceiling.
    pub middle_kbps: u32,
    /// The screen-share sender's ceiling (ADR-0083); `None` while the
    /// publisher sends no screen.
    pub screen_kbps: Option<u32>,
}

/// Why a budget is sent. A fixed, small set: it is a log field and a
/// metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The publisher was told nothing yet.
    First,
    /// A ceiling fell below [`DROP_PERCENT`] of what was told.
    Lower,
    /// Which senders are needed changed, and a ceiling with it.
    Senders,
    /// A ceiling differs enough, [`RAISE_INTERVAL`] after the last frame.
    Higher,
}

impl Why {
    /// The word in the log line and the metric label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Lower => "lower",
            Self::Senders => "senders",
            Self::Higher => "higher",
        }
    }
}

/// Which senders take from the budget: what a change of is told at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shape {
    main: bool,
    middle: bool,
    screen: bool,
}

impl Shape {
    const fn of(inputs: Inputs) -> Self {
        Self {
            main: inputs.main_needed,
            middle: inputs.has_middle && inputs.middle_needed,
            screen: inputs.has_screen,
        }
    }
}

/// Bits per second as whole kilobits per second, rounded down.
fn kbps(bps: u64) -> u32 {
    u32::try_from(bps / 1000).unwrap_or(u32::MAX)
}

/// What is left of the estimate for the encoders of the heavy senders (the
/// two camera senders and, while there is one, the screen), bits per
/// second (step 1 of the rule).
#[must_use]
pub fn video_budget_bps(inputs: Inputs) -> u64 {
    let audio = inputs
        .audio_bps
        .filter(|bps| *bps > 0)
        .map_or(AUDIO_RESERVE_BPS, |bps| {
            bps.clamp(AUDIO_RESERVE_MIN_BPS, AUDIO_RESERVE_MAX_BPS)
        });
    let light = if inputs.has_light {
        LIGHT_RESERVE_BPS
    } else {
        0
    };
    inputs
        .estimate_bps
        .saturating_sub(audio)
        .saturating_sub(light)
        .saturating_mul(100)
        / (100 + PROTECTION_PERCENT)
}

/// The screen sender's share of `video`, bits per second (step 2 of the
/// rule): what the camera senders that are on leave above their floors,
/// within [`SCREEN_FLOOR_BPS`] and [`SCREEN_PLAN_BPS`].
fn screen_bps(video: u64, shape: Shape) -> u64 {
    let mut camera_floors = 0;
    if shape.main {
        camera_floors += MAIN_FLOOR_BPS;
    }
    if shape.middle {
        camera_floors += MIDDLE_FLOOR_BPS;
    }
    video
        .saturating_sub(camera_floors)
        .clamp(SCREEN_FLOOR_BPS, SCREEN_PLAN_BPS)
}

/// The ceilings for `inputs` (the rule of the module, steps 1 to 6).
/// Total: every input gives a budget within the floors and the plan
/// maxima.
#[must_use]
pub fn allocate(inputs: Inputs) -> Budget {
    let shape = Shape::of(inputs);
    let all_video = video_budget_bps(inputs);
    // The screen is given its share first; the cameras share the rest.
    let screen = shape.screen.then(|| screen_bps(all_video, shape));
    let video = all_video.saturating_sub(screen.unwrap_or(0));
    // The middle sender gets its plan rate; it gives way only to the main
    // sender's floor, and only while the main sender is needed.
    let middle_room = if shape.main {
        video.saturating_sub(MAIN_FLOOR_BPS)
    } else {
        video
    };
    let middle = middle_room.clamp(MIDDLE_FLOOR_BPS, MIDDLE_PLAN_BPS);
    // The main sender gets the rest; a middle sender nobody watches (or
    // that does not exist) takes nothing.
    let main_room = if shape.middle {
        video.saturating_sub(middle)
    } else {
        video
    };
    let main = main_room.clamp(MAIN_FLOOR_BPS, MAIN_PLAN_BPS);
    Budget {
        main_kbps: kbps(main),
        middle_kbps: kbps(middle),
        screen_kbps: screen.map(kbps),
    }
}

/// Whether `next` has a ceiling below [`DROP_PERCENT`] of the same ceiling
/// in `last`. A screen ceiling that appears or goes is not a fall: it is a
/// change of senders.
fn any_lower(next: Budget, last: Budget) -> bool {
    let screen_lower = match (next.screen_kbps, last.screen_kbps) {
        (Some(next), Some(last)) => below(next, last, DROP_PERCENT),
        _ => false,
    };
    below(next.main_kbps, last.main_kbps, DROP_PERCENT)
        || below(next.middle_kbps, last.middle_kbps, DROP_PERCENT)
        || screen_lower
}

/// Whether `next` differs from `last` enough to be told as a rise: by
/// [`RAISE_SHARE_PERCENT`] of a ceiling, or by reaching a plan maximum.
fn any_higher(next: Budget, last: Budget) -> bool {
    let main_plan = kbps(MAIN_PLAN_BPS);
    let middle_plan = kbps(MIDDLE_PLAN_BPS);
    let screen_plan = kbps(SCREEN_PLAN_BPS);
    let screen_higher = match (next.screen_kbps, last.screen_kbps) {
        (Some(next), Some(last)) => {
            differs_enough(next, last) || (next == screen_plan && last != screen_plan)
        },
        _ => false,
    };
    differs_enough(next.main_kbps, last.main_kbps)
        || differs_enough(next.middle_kbps, last.middle_kbps)
        || (next.main_kbps == main_plan && last.main_kbps != main_plan)
        || (next.middle_kbps == middle_plan && last.middle_kbps != middle_plan)
        || screen_higher
}

/// `value` is below `percent` of `told`.
fn below(value: u32, told: u32, percent: u64) -> bool {
    u64::from(value).saturating_mul(100) < u64::from(told).saturating_mul(percent)
}

/// `value` differs from `told` by at least [`RAISE_SHARE_PERCENT`] of
/// `told`.
fn differs_enough(value: u32, told: u32) -> bool {
    u64::from(value.abs_diff(told)).saturating_mul(100)
        >= u64::from(told).saturating_mul(RAISE_SHARE_PERCENT)
}

/// What was last told, when, and under which need.
#[derive(Debug, Clone, Copy)]
struct Told {
    budget: Budget,
    at: Instant,
    shape: Shape,
}

/// What one publisher has been told of its budget.
#[derive(Debug, Clone, Copy, Default)]
pub struct PublishBudget {
    told: Option<Told>,
}

impl PublishBudget {
    /// A publisher that has been told nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { told: None }
    }

    /// Take the current inputs and return the budget to send and why, if
    /// one is due ("When the publisher is told" in the module
    /// documentation). The returned budget is recorded as told at `now`;
    /// a caller that could not deliver it calls [`Self::forget_told`].
    pub fn update(&mut self, inputs: Inputs, now: Instant) -> Option<(Budget, Why)> {
        let next = allocate(inputs);
        let shape = Shape::of(inputs);
        let why = match self.told {
            None => Why::First,
            Some(told) if told.budget == next => {
                // Nothing to say; the need it was said under is kept
                // current so a later change of ceilings is not blamed on
                // a change of need long past.
                self.told = Some(Told { shape, ..told });
                return None;
            },
            Some(told) => {
                let last = told.budget;
                if any_lower(next, last) {
                    Why::Lower
                } else if told.shape != shape {
                    Why::Senders
                } else if now.saturating_duration_since(told.at) >= RAISE_INTERVAL
                    && any_higher(next, last)
                {
                    Why::Higher
                } else {
                    return None;
                }
            },
        };
        self.told = Some(Told {
            budget: next,
            at: now,
            shape,
        });
        Some((next, why))
    }

    /// What the publisher was last told; `None` before the first frame.
    #[must_use]
    pub fn told(&self) -> Option<Budget> {
        self.told.map(|told| told.budget)
    }

    /// The last frame did not reach the publisher: the next
    /// [`Self::update`] returns a budget again, as the first.
    pub fn forget_told(&mut self) {
        self.told = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn inputs(estimate_kbps: u64) -> Inputs {
        Inputs {
            estimate_bps: estimate_kbps * 1000,
            audio_bps: None,
            has_light: true,
            has_middle: true,
            main_needed: true,
            middle_needed: true,
            has_screen: false,
        }
    }

    const fn budget(main_kbps: u32, middle_kbps: u32) -> Budget {
        Budget {
            main_kbps,
            middle_kbps,
            screen_kbps: None,
        }
    }

    const fn sharing(estimate_kbps: u64) -> Inputs {
        Inputs {
            has_screen: true,
            ..inputs(estimate_kbps)
        }
    }

    const fn with_screen(main_kbps: u32, middle_kbps: u32, screen_kbps: u32) -> Budget {
        Budget {
            main_kbps,
            middle_kbps,
            screen_kbps: Some(screen_kbps),
        }
    }

    /// The video budget of `inputs(estimate)`, kbit/s: (estimate - 64 -
    /// 150) / 1.15.
    fn video_kbps(estimate_kbps: u64) -> u64 {
        video_budget_bps(inputs(estimate_kbps)) / 1000
    }

    #[test]
    fn the_headroom_is_audio_light_and_a_share_for_protection() {
        // 1000 - 64 - 150 = 786; / 1.15 = 683.
        assert_eq!(video_kbps(1000), 683);
        // No light sender declared: nothing reserved for one.
        let no_light = Inputs {
            has_light: false,
            ..inputs(1000)
        };
        assert_eq!(video_budget_bps(no_light) / 1000, 813);
        // A measured audio rate replaces the constant, within its bounds.
        let audio = |bps| Inputs {
            audio_bps: Some(bps),
            ..inputs(1000)
        };
        assert_eq!(video_budget_bps(audio(100_000)) / 1000, 652);
        assert_eq!(
            video_budget_bps(audio(5_000)),
            video_budget_bps(audio(AUDIO_RESERVE_MIN_BPS))
        );
        assert_eq!(
            video_budget_bps(audio(9_000_000)),
            video_budget_bps(audio(AUDIO_RESERVE_MAX_BPS))
        );
        // Zero is "not measured", not "free".
        assert_eq!(video_budget_bps(audio(0)), video_budget_bps(inputs(1000)));
        // An estimate below the headroom leaves nothing, without a panic.
        assert_eq!(video_budget_bps(inputs(100)), 0);
        assert_eq!(video_budget_bps(inputs(0)), 0);
    }

    #[test]
    fn the_middle_sender_gets_its_plan_rate_and_the_main_sender_the_rest() {
        // 3000 -> video 2422: middle 500, main 1922.
        assert_eq!(allocate(inputs(3000)), budget(1922, 500));
        // The measured case: an estimate of 1000 leaves the main sender
        // 683 with the middle one off, 200 (its floor) with it on, the
        // middle one giving way to that floor.
        assert_eq!(allocate(inputs(1000)), budget(200, 483));
        let middle_off = Inputs {
            middle_needed: false,
            ..inputs(1000)
        };
        assert_eq!(allocate(middle_off), budget(683, 483));
        // No middle sender at all is the same for the main one.
        let no_middle = Inputs {
            has_middle: false,
            ..inputs(1000)
        };
        assert_eq!(allocate(no_middle).main_kbps, 683);
    }

    #[test]
    fn a_sender_nobody_watches_takes_nothing_and_gets_its_ceiling_anyway() {
        // Main off: the middle sender does not give way to a floor nobody
        // uses, and the main ceiling is what it would start under.
        let main_off = Inputs {
            main_needed: false,
            ..inputs(1000)
        };
        assert_eq!(allocate(main_off), budget(200, 500));
        let main_off_rich = Inputs {
            main_needed: false,
            ..inputs(3000)
        };
        assert_eq!(allocate(main_off_rich), budget(1922, 500));
        // Both off: each as if it alone were switched on beside the other.
        let both_off = Inputs {
            main_needed: false,
            middle_needed: false,
            ..inputs(3000)
        };
        assert_eq!(allocate(both_off), budget(2422, 500));
    }

    #[test]
    fn every_budget_is_within_the_floors_and_the_plan_maxima() {
        let main_floor = kbps(MAIN_FLOOR_BPS);
        let middle_floor = kbps(MIDDLE_FLOOR_BPS);
        let main_plan = kbps(MAIN_PLAN_BPS);
        let middle_plan = kbps(MIDDLE_PLAN_BPS);
        // Nothing at all, and far more than any plan.
        assert_eq!(allocate(inputs(0)), budget(main_floor, middle_floor));
        assert_eq!(allocate(inputs(50_000)), budget(main_plan, middle_plan));
        let extreme = Inputs {
            estimate_bps: u64::MAX,
            audio_bps: Some(u64::MAX),
            ..inputs(0)
        };
        assert_eq!(allocate(extreme), budget(main_plan, middle_plan));
        for estimate in (0..6000).step_by(7) {
            for (main_needed, middle_needed, has_middle, has_light) in [
                (true, true, true, true),
                (true, false, true, true),
                (false, true, true, true),
                (false, false, true, true),
                (true, true, false, false),
                (true, false, false, true),
            ] {
                let got = allocate(Inputs {
                    has_light,
                    has_middle,
                    main_needed,
                    middle_needed,
                    ..inputs(estimate)
                });
                assert!((main_floor..=main_plan).contains(&got.main_kbps));
                assert!((middle_floor..=middle_plan).contains(&got.middle_kbps));
                // The frame's own bounds hold too.
                assert_eq!(
                    lakko_sfu_proto::publish_budget_problem(got.main_kbps, got.middle_kbps),
                    None
                );
            }
        }
    }

    #[test]
    fn the_senders_that_are_on_never_exceed_the_video_budget_above_the_floors() {
        for estimate in (0..6000).step_by(11) {
            let got = allocate(inputs(estimate));
            let sum = u64::from(got.main_kbps) + u64::from(got.middle_kbps);
            let floors = (MAIN_FLOOR_BPS + MIDDLE_FLOOR_BPS) / 1000;
            assert!(
                sum <= video_kbps(estimate).max(floors),
                "estimate {estimate}: {got:?} against {}",
                video_kbps(estimate)
            );
        }
    }

    #[test]
    fn a_richer_path_never_gets_a_lower_ceiling() {
        let mut last = allocate(inputs(0));
        for estimate in 1..6000 {
            let got = allocate(inputs(estimate));
            assert!(got.main_kbps >= last.main_kbps && got.middle_kbps >= last.middle_kbps);
            last = got;
        }
    }

    #[test]
    fn the_first_budget_is_sent_at_once_and_the_same_one_never_again() {
        let t0 = Instant::now();
        let mut state = PublishBudget::new();
        assert_eq!(state.told(), None);
        assert_eq!(
            state.update(inputs(3000), t0),
            Some((budget(1922, 500), Why::First))
        );
        assert_eq!(state.told(), Some(budget(1922, 500)));
        assert_eq!(state.update(inputs(3000), t0), None);
        assert_eq!(state.update(inputs(3000), t0 + RAISE_INTERVAL * 10), None);
    }

    #[test]
    fn a_lower_budget_is_sent_at_once() {
        let t0 = Instant::now();
        let mut state = PublishBudget::new();
        let _ = state.update(inputs(3000), t0);
        // The estimator's cut to 0.85, a millisecond later.
        let cut = t0 + Duration::from_millis(1);
        assert_eq!(
            state.update(inputs(2550), cut),
            Some((budget(1531, 500), Why::Lower))
        );
        // And another right after it.
        assert_eq!(
            state.update(inputs(2000), cut),
            Some((budget(1053, 500), Why::Lower))
        );
        // A fall too small to be "lower" waits like a rise, and is then
        // too small to be told at all.
        assert_eq!(state.update(inputs(1990), cut), None);
        assert_eq!(state.update(inputs(1990), cut + RAISE_INTERVAL), None);
    }

    #[test]
    fn a_higher_budget_waits_for_the_interval_and_for_the_share() {
        let t0 = Instant::now();
        let mut state = PublishBudget::new();
        let _ = state.update(inputs(2000), t0);
        assert_eq!(state.told(), Some(budget(1053, 500)));
        // Far higher, but too soon.
        let soon = t0 + RAISE_INTERVAL.saturating_sub(Duration::from_millis(1));
        assert_eq!(state.update(inputs(3000), soon), None);
        // In time, but by less than the share (1053 -> 1140 is 8%).
        assert_eq!(state.update(inputs(2100), t0 + RAISE_INTERVAL), None);
        // In time and by the share.
        assert_eq!(
            state.update(inputs(2200), t0 + RAISE_INTERVAL),
            Some((budget(1226, 500), Why::Higher))
        );
        // The interval starts over at every frame sent.
        assert_eq!(
            state.update(
                inputs(3000),
                t0 + RAISE_INTERVAL + RAISE_INTERVAL.saturating_sub(Duration::from_millis(1))
            ),
            None
        );
        assert_eq!(
            state.update(inputs(3000), t0 + RAISE_INTERVAL * 2),
            Some((budget(1922, 500), Why::Higher))
        );
    }

    #[test]
    fn a_climbing_estimate_is_told_in_steps_not_at_every_pass() {
        // 8% a second from 1000 to the plan, looked at every 20 ms.
        let t0 = Instant::now();
        let mut state = PublishBudget::new();
        let no_middle = |estimate_bps: u64| Inputs {
            estimate_bps,
            has_middle: false,
            ..inputs(0)
        };
        let mut estimate_bps = 1_000_000_u64;
        let mut frames = 0_u32;
        let mut last_at = None;
        for step in 0..1500_u32 {
            let now = t0 + Duration::from_millis(u64::from(step) * 20);
            if state.update(no_middle(estimate_bps), now).is_some() {
                if let Some(at) = last_at {
                    assert!(now.saturating_duration_since(at) >= RAISE_INTERVAL);
                }
                last_at = Some(now);
                frames += 1;
            }
            estimate_bps = (estimate_bps * 10_016 / 10_000).min(5_000_000);
        }
        // Thirty seconds: at most one frame per interval, and the plan
        // maximum is reached and told.
        assert!(frames <= 15, "{frames} frames");
        assert_eq!(state.told().map(|b| b.main_kbps), Some(kbps(MAIN_PLAN_BPS)));
    }

    #[test]
    fn reaching_the_plan_maximum_is_told_even_by_less_than_the_share() {
        let t0 = Instant::now();
        let mut state = PublishBudget::new();
        let no_middle = |estimate_kbps: u64| Inputs {
            has_middle: false,
            ..inputs(estimate_kbps)
        };
        // 2900 -> video 2335; the plan maximum is 2500, 7% above.
        let _ = state.update(no_middle(2900), t0);
        assert_eq!(state.told(), Some(budget(2335, 500)));
        assert_eq!(
            state.update(no_middle(5000), t0 + RAISE_INTERVAL),
            Some((budget(2500, 500), Why::Higher))
        );
    }

    #[test]
    fn a_change_of_which_senders_are_needed_is_told_at_once() {
        let t0 = Instant::now();
        let mut state = PublishBudget::new();
        let _ = state.update(inputs(2000), t0);
        // The middle sender is switched off: the main one gets its share,
        // a rise, told in the same pass.
        let middle_off = Inputs {
            middle_needed: false,
            ..inputs(2000)
        };
        let at = t0 + Duration::from_millis(20);
        assert_eq!(
            state.update(middle_off, at),
            Some((budget(1553, 500), Why::Senders))
        );
        // Switched on again: the main one gives it back, a fall.
        assert_eq!(
            state.update(inputs(2000), at),
            Some((budget(1053, 500), Why::Lower))
        );
        // A change of need that changes no ceiling says nothing, and is
        // not remembered as a reason for a later rise.
        let main_off = Inputs {
            main_needed: false,
            ..inputs(2000)
        };
        assert_eq!(state.update(main_off, at), None);
        let richer = Inputs {
            main_needed: false,
            ..inputs(3000)
        };
        assert_eq!(state.update(richer, at + Duration::from_millis(20)), None);
    }

    #[test]
    fn an_undelivered_frame_is_returned_again() {
        let t0 = Instant::now();
        let mut state = PublishBudget::new();
        let first = state.update(inputs(2000), t0);
        assert!(first.is_some());
        assert_eq!(state.update(inputs(2000), t0), None);
        state.forget_told();
        assert_eq!(state.update(inputs(2000), t0), first);
    }

    #[test]
    fn a_publisher_without_a_screen_is_told_no_screen_ceiling() {
        for estimate in (0..6000).step_by(13) {
            assert_eq!(allocate(inputs(estimate)).screen_kbps, None);
        }
    }

    #[test]
    fn the_screen_is_given_its_share_before_the_cameras() {
        // 3000 -> video 2422. The screen takes what the camera floors
        // (200 + 100) leave, up to its plan of 1500; the cameras share the
        // 922 left: middle 500, main 422.
        assert_eq!(allocate(sharing(3000)), with_screen(422, 500, 1500));
        // 1000 -> video 683: the screen 383, the cameras at their floors.
        assert_eq!(allocate(sharing(1000)), with_screen(200, 100, 383));
        // Nothing at all: every sender at its floor.
        assert_eq!(
            allocate(sharing(0)),
            with_screen(
                kbps(MAIN_FLOOR_BPS),
                kbps(MIDDLE_FLOOR_BPS),
                kbps(SCREEN_FLOOR_BPS)
            )
        );
        // Plenty: the screen at its plan, the cameras as without a share
        // but for what the screen took.
        let rich = allocate(sharing(50_000));
        assert_eq!(rich.screen_kbps, Some(kbps(SCREEN_PLAN_BPS)));
        assert_eq!(rich.main_kbps, kbps(MAIN_PLAN_BPS));
        // Camera senders nobody watches leave the screen their floors too.
        let cameras_off = Inputs {
            main_needed: false,
            middle_needed: false,
            ..sharing(1000)
        };
        assert_eq!(allocate(cameras_off).screen_kbps, Some(683));
    }

    #[test]
    fn every_budget_with_a_screen_is_within_its_bounds() {
        let screen_floor = kbps(SCREEN_FLOOR_BPS);
        let screen_plan = kbps(SCREEN_PLAN_BPS);
        let extreme = Inputs {
            estimate_bps: u64::MAX,
            audio_bps: Some(u64::MAX),
            ..sharing(0)
        };
        assert_eq!(allocate(extreme).screen_kbps, Some(screen_plan));
        let mut last = allocate(sharing(0));
        for estimate in 0..6000 {
            let got = allocate(sharing(estimate));
            let screen = got.screen_kbps.unwrap_or(0);
            assert!((screen_floor..=screen_plan).contains(&screen));
            assert!((kbps(MAIN_FLOOR_BPS)..=kbps(MAIN_PLAN_BPS)).contains(&got.main_kbps));
            assert_eq!(
                lakko_sfu_proto::publish_budget_problem(got.main_kbps, got.middle_kbps),
                None
            );
            // A richer path never gets a lower screen ceiling.
            assert!(screen >= last.screen_kbps.unwrap_or(0));
            last = got;
        }
    }

    #[test]
    fn a_share_starting_or_stopping_is_told_at_once() {
        let t0 = Instant::now();
        let mut state = PublishBudget::new();
        let _ = state.update(inputs(3000), t0);
        let at = t0 + Duration::from_millis(20);
        // The share starts: the main camera gives the screen its room, a
        // fall, told in the same pass.
        assert_eq!(
            state.update(sharing(3000), at),
            Some((with_screen(422, 500, 1500), Why::Lower))
        );
        assert_eq!(state.update(sharing(3000), at), None);
        // The share stops: the screen ceiling goes and the main camera
        // gets its room back, a change of senders told at once.
        assert_eq!(
            state.update(inputs(3000), at),
            Some((budget(1922, 500), Why::Senders))
        );
    }

    #[test]
    fn the_constants_keep_their_order() {
        const { assert!(MAIN_FLOOR_BPS < MAIN_PLAN_BPS) };
        const { assert!(MIDDLE_FLOOR_BPS < MIDDLE_PLAN_BPS) };
        const { assert!(MIDDLE_PLAN_BPS < MAIN_PLAN_BPS) };
        // The main picture is never the worse of main and light.
        const { assert!(MAIN_FLOOR_BPS > LIGHT_RESERVE_BPS) };
        const { assert!(AUDIO_RESERVE_MIN_BPS <= AUDIO_RESERVE_BPS) };
        const { assert!(AUDIO_RESERVE_BPS <= AUDIO_RESERVE_MAX_BPS) };
        const { assert!(DROP_PERCENT < 100) };
        const { assert!(RAISE_SHARE_PERCENT > 100 - DROP_PERCENT) };
        const { assert!(SCREEN_FLOOR_BPS < SCREEN_PLAN_BPS) };
        const { assert!(SCREEN_FLOOR_BPS > MAIN_FLOOR_BPS) };
        const { assert!(SCREEN_FLOOR_BPS % 1000 == 0 && SCREEN_PLAN_BPS % 1000 == 0) };
        // Whole kilobits, so a ceiling at a bound is exactly the bound.
        const { assert!(MAIN_FLOOR_BPS % 1000 == 0 && MIDDLE_FLOOR_BPS % 1000 == 0) };
        const { assert!(MAIN_PLAN_BPS % 1000 == 0 && MIDDLE_PLAN_BPS % 1000 == 0) };
        // Within the frame's bound.
        assert!(kbps(MAIN_PLAN_BPS) <= lakko_sfu_proto::MAX_PUBLISH_BUDGET_KBPS);
        assert!(kbps(SCREEN_PLAN_BPS) <= lakko_sfu_proto::MAX_PUBLISH_BUDGET_KBPS);
        assert!(RAISE_INTERVAL >= Duration::from_secs(1));
    }
}
