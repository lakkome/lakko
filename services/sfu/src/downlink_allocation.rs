// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! What one subscriber is sent of each publisher's video when its
//! downlink is short
//! ([ADR-0070](../../../docs/adr/0070-call-media-through-a-loss-burst.md)
//! step 6).
//!
//! Pure: no I/O, no clock of its own (the caller supplies the time), no
//! `str0m` type. One [`DownlinkAllocation`] per subscriber. The media
//! loop feeds it the subscriber's **budget** (the SFU's own estimate of
//! what the path from the SFU to the subscriber carries, or nothing while
//! that is not known) and, per publisher, what the subscriber asked for
//! and what each of the publisher's tracks costs; it answers with the
//! quality **given** per publisher, which is never above the one asked
//! for.
//!
//! ## The rule
//!
//! 1. Audio and every light track come first. Their measured rates are
//!    taken off the budget before anything else and they are never given
//!    up: the allocation has no answer below [`VideoQuality::Light`], and
//!    the light track is sent at every quality.
//! 2. Then the publishers are walked **in the order given**, which is the
//!    subscriber's order of importance. Each gets the heavy track it asked
//!    for if what is left of the budget covers that track's measured rate
//!    times [`HEADROOM_NUM`]/[`HEADROOM_DEN`], else the next smaller one
//!    that fits (main, then middle, then the light track alone). What a
//!    publisher is given is taken off the budget before the next one is
//!    looked at.
//! 3. A track the publisher does not have is skipped. A track that is
//!    there and has no measured rate yet costs nothing.
//! 4. An unknown budget gives everyone what they asked for.
//!
//! ## Down at once, up only on proof
//!
//! A **downgrade takes effect at once**: a path that is short is not
//! helped by waiting. The headroom is asked for when a track is given,
//! not to keep it: while what is being given fits the budget **as it is
//! on the wire**, the rule above shares out what that set needs and no
//! more, so only a change of importance moves a track; when it no longer
//! fits, the rule runs on the budget and nobody keeps more than it gives
//! them.
//!
//! Not built: when a more important publisher's larger track costs more
//! than the tracks the less important ones would give up for it, the
//! swap is not tried by itself; that publisher waits until the path has
//! carried the larger set.
//!
//! An **upgrade needs the path to have carried the rate of the set it
//! leads to**. The budget alone is not enough: an estimate may stand
//! above what was ever sent on the path (it starts high, and it rises by
//! rule), and a set given on its word would be tried on the subscriber's
//! picture. So besides the budget the caller says what the path has been
//! seen to **carry**: the rate of a probe that just passed
//! ([`crate::downlink_probe`]), or what is arriving at the subscriber
//! now on a quiet path. A publisher is moved up only when the tracks of
//! the whole set after the move, as they are on the wire
//! ([`wire_bps`], no headroom), come to no more than that. The caller
//! finds the set to try next with [`next_step`] and tries it at its rate
//! *with* headroom ([`need_bps`]), so a set that is given has been
//! carried with its headroom.
//!
//! That goes for every move up: a publisher that has just joined starts
//! at its light track, and a subscriber that asks for more of a publisher
//! gets it when the path has carried it. The one exception is the start:
//! while the budget is unknown everyone is given what they asked for,
//! and what is being sent when the budget becomes known is kept.
//!
//! The constants are **chosen, not measured**.

use std::collections::HashMap;
use std::hash::Hash;

use crate::protocol::VideoQuality;

/// Numerator of the headroom a heavy track's measured rate is multiplied
/// by before it is compared with what is left of the budget: 1.2. Chosen,
/// not measured: a video sender's rate swings around its mean from frame
/// to frame, and the measured rate is of payload bytes, a few percent
/// under what the track costs on the wire.
pub const HEADROOM_NUM: u64 = 6;

/// Denominator of the headroom; see [`HEADROOM_NUM`].
pub const HEADROOM_DEN: u64 = 5;

/// What one publisher costs one subscriber, and what the subscriber asked
/// for. The rates are bits per second as measured where the publisher's
/// packets arrive; `None` means the subscriber has no such track of this
/// publisher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publisher<K> {
    /// Who.
    pub publisher: K,
    /// The quality the subscriber asked for.
    pub wanted: VideoQuality,
    /// The main track.
    pub main_bps: Option<u64>,
    /// The middle track.
    pub middle_bps: Option<u64>,
    /// The light track.
    pub light_bps: Option<u64>,
    /// The publisher's audio; zero when it has none.
    pub audio_bps: u64,
}

/// What one publisher is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Given<K> {
    /// Who.
    pub publisher: K,
    /// The quality given: the one asked for, or a smaller one.
    pub given: VideoQuality,
    /// What it was given by the call before this one; `None` for a
    /// publisher seen for the first time.
    pub previous: Option<VideoQuality>,
}

impl<K> Given<K> {
    /// Whether a publisher that was already being given something is now
    /// given something else.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.previous.is_some_and(|previous| previous != self.given)
    }
}

/// Qualities in ascending order of what they cost.
const fn rank(quality: VideoQuality) -> u8 {
    match quality {
        VideoQuality::Light => 0,
        VideoQuality::Middle => 1,
        VideoQuality::Main => 2,
    }
}

/// The smaller of two qualities.
#[must_use]
pub const fn smaller(a: VideoQuality, b: VideoQuality) -> VideoQuality {
    if rank(a) <= rank(b) {
        a
    } else {
        b
    }
}

/// A heavy track's measured rate with the headroom on it.
#[must_use]
pub const fn with_headroom(bps: u64) -> u64 {
    (bps / HEADROOM_DEN)
        .saturating_mul(HEADROOM_NUM)
        .saturating_add((bps % HEADROOM_DEN) * HEADROOM_NUM / HEADROOM_DEN)
}

/// The rule without its hysteresis: what each publisher would be given by
/// this budget alone, in the order of `publishers`. See the module
/// documentation.
#[must_use]
pub fn targets<K>(budget_bps: Option<u64>, publishers: &[Publisher<K>]) -> Vec<VideoQuality> {
    let Some(budget) = budget_bps else {
        return publishers.iter().map(|p| p.wanted).collect();
    };
    let first: u64 = publishers
        .iter()
        .map(|p| p.audio_bps.saturating_add(p.light_bps.unwrap_or(0)))
        .fold(0, u64::saturating_add);
    let mut left = budget.saturating_sub(first);
    publishers
        .iter()
        .map(|p| {
            let ladder = [
                (VideoQuality::Main, p.main_bps),
                (VideoQuality::Middle, p.middle_bps),
            ];
            for (quality, rate) in ladder {
                if rank(quality) > rank(p.wanted) {
                    continue;
                }
                let Some(rate) = rate else {
                    continue;
                };
                let cost = with_headroom(rate);
                if cost <= left {
                    left -= cost;
                    return quality;
                }
            }
            VideoQuality::Light
        })
        .collect()
}

/// The rate the subscriber would be sent if every publisher were given
/// what it asked for, heavy tracks with their headroom: what the budget
/// has to reach for nothing to be given up. The estimator is told this,
/// so that it probes toward it while less is being sent.
#[must_use]
pub fn demand_bps<K>(publishers: &[Publisher<K>]) -> u64 {
    publishers
        .iter()
        .map(|p| {
            let heavy = match p.wanted {
                VideoQuality::Main => p.main_bps.or(p.middle_bps),
                VideoQuality::Middle => p.middle_bps,
                VideoQuality::Light => None,
            };
            p.audio_bps
                .saturating_add(p.light_bps.unwrap_or(0))
                .saturating_add(with_headroom(heavy.unwrap_or(0)))
        })
        .fold(0, u64::saturating_add)
}

/// The bits per second the tracks of a set put on the wire: every
/// publisher's audio and light track, and the heavy track `given` names
/// for it, **without** headroom. `given` is in the order of `publishers`;
/// a publisher beyond its end counts as on its light track.
#[must_use]
pub fn wire_bps<K>(publishers: &[Publisher<K>], given: &[VideoQuality]) -> u64 {
    set_bps(publishers, given, false)
}

/// What a set needs of the budget: as [`wire_bps`], with the headroom on
/// every heavy track. The rate a set is tried at before it is given.
#[must_use]
pub fn need_bps<K>(publishers: &[Publisher<K>], given: &[VideoQuality]) -> u64 {
    set_bps(publishers, given, true)
}

fn set_bps<K>(publishers: &[Publisher<K>], given: &[VideoQuality], headroom: bool) -> u64 {
    publishers
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let heavy = match given.get(i).copied().unwrap_or(VideoQuality::Light) {
                VideoQuality::Main => p.main_bps,
                VideoQuality::Middle => p.middle_bps,
                VideoQuality::Light => None,
            }
            .unwrap_or(0);
            let heavy = if headroom {
                with_headroom(heavy)
            } else {
                heavy
            };
            p.audio_bps
                .saturating_add(p.light_bps.unwrap_or(0))
                .saturating_add(heavy)
        })
        .fold(0, u64::saturating_add)
}

/// The next larger track of `p` above `from` that it has and that is not
/// above what was asked for.
fn rung_above<K>(p: &Publisher<K>, from: VideoQuality) -> Option<VideoQuality> {
    [VideoQuality::Middle, VideoQuality::Main]
        .into_iter()
        .find(|quality| {
            rank(*quality) > rank(from)
                && rank(*quality) <= rank(p.wanted)
                && match quality {
                    VideoQuality::Main => p.main_bps.is_some(),
                    VideoQuality::Middle => p.middle_bps.is_some(),
                    VideoQuality::Light => false,
                }
        })
}

/// The next larger set after `given`: the most important publisher that
/// is given less than it asked for, moved up by one track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NextStep {
    /// Index of that publisher in the list.
    pub index: usize,
    /// The quality it would be given.
    pub quality: VideoQuality,
    /// What the set after the move needs, headroom included
    /// ([`need_bps`]): the rate to try on the path.
    pub need_bps: u64,
}

/// The set to try next, or `None` when everyone is given what they asked
/// for (or has no larger track). `given` is in the order of `publishers`.
#[must_use]
pub fn next_step<K>(publishers: &[Publisher<K>], given: &[VideoQuality]) -> Option<NextStep> {
    next_steps(publishers, given).into_iter().next()
}

/// Every set one move larger than `given`: each publisher that is given
/// less than it asked for, moved up by one track, the most important
/// first. The first is [`next_step`]; when the path is known not to carry
/// what that one needs, a later, cheaper one can still be tried.
#[must_use]
pub fn next_steps<K>(publishers: &[Publisher<K>], given: &[VideoQuality]) -> Vec<NextStep> {
    publishers
        .iter()
        .enumerate()
        .filter_map(|(index, p)| {
            let from = given.get(index).copied().unwrap_or(VideoQuality::Light);
            let quality = rung_above(p, from)?;
            let mut set: Vec<VideoQuality> = (0..publishers.len())
                .map(|i| given.get(i).copied().unwrap_or(VideoQuality::Light))
                .collect();
            set[index] = quality;
            Some(NextStep {
                index,
                quality,
                need_bps: need_bps(publishers, &set),
            })
        })
        .collect()
}

/// The allocation of one subscriber: the rule of the module documentation
/// and the memory of what each publisher was last given.
#[derive(Debug, Clone)]
pub struct DownlinkAllocation<K> {
    given: HashMap<K, VideoQuality>,
}

impl<K> Default for DownlinkAllocation<K> {
    fn default() -> Self {
        Self {
            given: HashMap::new(),
        }
    }
}

impl<K: Clone + Eq + Hash> DownlinkAllocation<K> {
    /// An allocation that has seen no publisher.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What each publisher is given, in the order of `publishers` (the
    /// subscriber's order of importance, most important first).
    ///
    /// `budget_bps` is the estimate of what the path carries, `None`
    /// while nothing is known of it. `carried_bps` is what the path has
    /// been seen to carry, `None` when nothing vouches for any rate.
    ///
    /// A publisher that is no longer listed is forgotten; listed again
    /// later it is seen for the first time. A publisher listed twice is
    /// taken at its first entry and answered at each.
    pub fn allocate(
        &mut self,
        budget_bps: Option<u64>,
        carried_bps: Option<u64>,
        publishers: &[Publisher<K>],
    ) -> Vec<Given<K>> {
        let previous: Vec<Option<VideoQuality>> = publishers
            .iter()
            .map(|p| self.given.get(&p.publisher).copied())
            .collect();
        let first_of: Vec<usize> = publishers
            .iter()
            .map(|p| {
                publishers
                    .iter()
                    .position(|q| q.publisher == p.publisher)
                    .unwrap_or(0)
            })
            .collect();
        let mut set: Vec<VideoQuality> = if let Some(budget) = budget_bps {
            // What is being given now; a publisher seen for the first
            // time starts at its light track, and nobody is given more
            // than was asked for.
            let current: Vec<VideoQuality> = publishers
                .iter()
                .zip(&previous)
                .map(|(p, previous)| smaller(p.wanted, previous.unwrap_or(VideoQuality::Light)))
                .collect();
            // The headroom is asked for when a track is given, not to
            // keep it: what is being given stays while what it puts on
            // the wire fits the estimate. (A set given at exactly what
            // it needs would otherwise be taken away again by the next
            // measurement of its tracks a percent higher. A cut of the
            // estimate lands below what was arriving, so a path that is
            // short still takes a track away at once.)
            //
            // And the estimate may stand above anything the path was
            // seen to carry, while the rule fills from the most important
            // publisher down: run on such an estimate it would take a
            // track from a less important publisher for a larger one of
            // a more important publisher that the path then turns out
            // not to carry. So while what is being given fits, the rule
            // shares out what that set needs and no more: the same
            // budget, by importance (a change of importance moves a
            // track at once; nothing else moves). When it no longer
            // fits, the rule runs on the estimate. Either way a
            // publisher is moved down at once.
            let shared = if wire_bps(publishers, &current) <= budget {
                need_bps(publishers, &current)
            } else {
                budget
            };
            let mut set: Vec<VideoQuality> = targets(Some(shared), publishers)
                .iter()
                .zip(&current)
                .map(|(target, current)| smaller(*target, *current))
                .collect();
            // Up only as far as the path has carried and the budget
            // covers, the most important publisher first, each to the
            // largest track that fits.
            if let Some(carried) = carried_bps {
                for i in 0..publishers.len() {
                    if first_of[i] != i {
                        continue;
                    }
                    let mut trial = set.clone();
                    let mut best = None;
                    let mut from = set[i];
                    while let Some(quality) = rung_above(&publishers[i], from) {
                        trial[i] = quality;
                        if wire_bps(publishers, &trial) <= carried
                            && need_bps(publishers, &trial) <= budget
                        {
                            best = Some(quality);
                        }
                        from = quality;
                    }
                    if let Some(quality) = best {
                        set[i] = quality;
                    }
                }
            }
            set
        } else {
            targets(None, publishers)
        };
        // A publisher listed twice is answered as at its first entry.
        for i in 0..set.len() {
            set[i] = set[first_of[i]];
        }
        let out: Vec<Given<K>> = publishers
            .iter()
            .zip(&set)
            .zip(&previous)
            .enumerate()
            .map(|(i, ((p, given), previous))| Given {
                publisher: p.publisher.clone(),
                given: *given,
                previous: if first_of[i] == i {
                    *previous
                } else {
                    Some(*given)
                },
            })
            .collect();
        self.given = publishers
            .iter()
            .zip(&set)
            .map(|(p, given)| (p.publisher.clone(), *given))
            .collect();
        out
    }

    /// What `publisher` was given by the last call, if it was listed.
    #[must_use]
    pub fn given(&self, publisher: &K) -> Option<VideoQuality> {
        self.given.get(publisher).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: u64 = 2_000_000;
    const MIDDLE: u64 = 500_000;
    const LIGHT: u64 = 150_000;
    const AUDIO: u64 = 32_000;

    fn publisher(name: &'static str, wanted: VideoQuality) -> Publisher<&'static str> {
        Publisher {
            publisher: name,
            wanted,
            main_bps: Some(MAIN),
            middle_bps: Some(MIDDLE),
            light_bps: Some(LIGHT),
            audio_bps: AUDIO,
        }
    }

    fn three(wanted: VideoQuality) -> Vec<Publisher<&'static str>> {
        vec![
            publisher("a", wanted),
            publisher("b", wanted),
            publisher("c", wanted),
        ]
    }

    /// Audio and light of `n` publishers.
    const fn first(n: u64) -> u64 {
        n * (AUDIO + LIGHT)
    }

    fn qualities(given: &[Given<&str>]) -> Vec<VideoQuality> {
        given.iter().map(|g| g.given).collect()
    }

    use VideoQuality::{Light, Main, Middle};

    #[test]
    fn an_unknown_budget_gives_everyone_what_they_asked_for() {
        let mut publishers = three(Main);
        publishers[1].wanted = Middle;
        publishers[2].wanted = Light;
        assert_eq!(targets(None, &publishers), vec![Main, Middle, Light]);
        let mut allocation = DownlinkAllocation::new();
        let given = allocation.allocate(None, None, &publishers);
        assert_eq!(qualities(&given), vec![Main, Middle, Light]);
        assert!(given.iter().all(|g| g.previous.is_none() && !g.changed()));
    }

    #[test]
    fn a_budget_that_covers_everything_gives_everything() {
        let publishers = three(Main);
        let enough = first(3) + 3 * with_headroom(MAIN);
        assert_eq!(demand_bps(&publishers), enough);
        assert_eq!(need_bps(&publishers, &[Main, Main, Main]), enough);
        assert_eq!(targets(Some(enough), &publishers), vec![Main; 3]);
        // One bit short and the least important publisher steps down.
        assert_eq!(
            targets(Some(enough - 1), &publishers),
            vec![Main, Main, Middle]
        );
    }

    #[test]
    fn the_headroom_is_part_of_what_a_heavy_track_costs() {
        assert_eq!(with_headroom(MAIN), 2_400_000);
        let publishers = vec![publisher("a", Main)];
        // The measured rate alone is not enough.
        assert_eq!(targets(Some(first(1) + MAIN), &publishers), vec![Middle]);
        assert_eq!(
            targets(Some(first(1) + with_headroom(MAIN)), &publishers),
            vec![Main]
        );
        // And the same for the middle track.
        assert_eq!(targets(Some(first(1) + MIDDLE), &publishers), vec![Light]);
        assert_eq!(
            targets(Some(first(1) + with_headroom(MIDDLE)), &publishers),
            vec![Middle]
        );
        // On the wire a set costs its tracks; what it needs has the
        // headroom on the heavy ones.
        assert_eq!(wire_bps(&publishers, &[Main]), first(1) + MAIN);
        assert_eq!(need_bps(&publishers, &[Main]), first(1) + 2_400_000);
        assert_eq!(wire_bps(&publishers, &[Light]), first(1));
        assert_eq!(need_bps(&publishers, &[]), first(1));
    }

    #[test]
    fn the_larger_pictures_are_given_up_first_and_the_least_important_first() {
        let publishers = three(Main);
        let one_main_two_middles = first(3) + with_headroom(MAIN) + 2 * with_headroom(MIDDLE);
        assert_eq!(
            targets(Some(one_main_two_middles), &publishers),
            vec![Main, Middle, Middle]
        );
        // A little less: the last publisher is on its light track alone.
        assert_eq!(
            targets(Some(one_main_two_middles - 1), &publishers),
            vec![Main, Middle, Light]
        );
        // Not enough for a main track at all: three middles.
        assert_eq!(
            targets(Some(first(3) + 3 * with_headroom(MIDDLE)), &publishers),
            vec![Middle, Middle, Middle]
        );
        // The order of the list is the order of importance.
        let mut reversed = publishers;
        reversed.reverse();
        let given = targets(Some(one_main_two_middles), &reversed);
        assert_eq!(given, vec![Main, Middle, Middle]);
        assert_eq!(reversed[0].publisher, "c");
    }

    #[test]
    fn audio_and_light_come_first_and_are_never_given_up() {
        let publishers = three(Main);
        // A budget below even audio and light: everyone is on the light
        // track, which is as low as the allocation goes.
        for budget in [0, 1, first(3) - 1, first(3)] {
            assert_eq!(targets(Some(budget), &publishers), vec![Light; 3]);
        }
        // Audio and light are taken off before any heavy track: a budget
        // that would cover one middle track without them covers none.
        assert_eq!(
            targets(Some(first(3) + with_headroom(MIDDLE) - 1), &publishers),
            vec![Light; 3]
        );
        assert_eq!(
            targets(Some(first(3) + with_headroom(MIDDLE)), &publishers),
            vec![Middle, Light, Light]
        );
    }

    #[test]
    fn nobody_is_given_more_than_they_asked_for() {
        let mut publishers = three(Light);
        publishers[1].wanted = Middle;
        assert_eq!(
            targets(Some(100_000_000), &publishers),
            vec![Light, Middle, Light]
        );
        // What the first one does not take is there for the next.
        let budget = first(3) + with_headroom(MIDDLE);
        assert_eq!(
            targets(Some(budget), &publishers),
            vec![Light, Middle, Light]
        );
        assert_eq!(demand_bps(&publishers), budget);
        // And proof of any rate moves nobody above what was asked for.
        let mut allocation = DownlinkAllocation::new();
        let plenty = Some(100_000_000);
        let _ = allocation.allocate(plenty, None, &publishers);
        let given = allocation.allocate(plenty, plenty, &publishers);
        assert_eq!(qualities(&given), vec![Light, Middle, Light]);
    }

    #[test]
    fn a_publisher_without_a_middle_or_a_light_track() {
        // No middle track: main or the light track alone.
        let mut no_middle = publisher("a", Main);
        no_middle.middle_bps = None;
        let publishers = vec![no_middle.clone()];
        let base = AUDIO + LIGHT;
        assert_eq!(
            targets(Some(base + with_headroom(MAIN)), &publishers),
            vec![Main]
        );
        assert_eq!(
            targets(Some(base + with_headroom(MAIN) - 1), &publishers),
            vec![Light]
        );
        // Its next step from the light track is the main one.
        assert_eq!(
            next_step(&publishers, &[Light]),
            Some(NextStep {
                index: 0,
                quality: Main,
                need_bps: base + with_headroom(MAIN),
            })
        );
        // No light track: nothing is taken off for it.
        let mut no_light = publisher("a", Main);
        no_light.light_bps = None;
        assert_eq!(
            targets(
                Some(AUDIO + with_headroom(MAIN)),
                std::slice::from_ref(&no_light)
            ),
            vec![Main]
        );
        assert_eq!(demand_bps(&[no_light]), AUDIO + with_headroom(MAIN));
        // No main track (asked for all the same): the middle one.
        let mut no_main = publisher("a", Main);
        no_main.main_bps = None;
        assert_eq!(
            targets(
                Some(base + with_headroom(MIDDLE)),
                std::slice::from_ref(&no_main)
            ),
            vec![Middle]
        );
        assert_eq!(
            next_step(std::slice::from_ref(&no_main), &[Middle]),
            None,
            "there is nothing above its middle track"
        );
        assert_eq!(demand_bps(&[no_main]), base + with_headroom(MIDDLE));
        // Audio only.
        let audio_only = Publisher {
            publisher: "a",
            wanted: Main,
            main_bps: None,
            middle_bps: None,
            light_bps: None,
            audio_bps: AUDIO,
        };
        assert_eq!(
            targets(Some(0), std::slice::from_ref(&audio_only)),
            vec![Light]
        );
        assert_eq!(next_step(std::slice::from_ref(&audio_only), &[Light]), None);
        assert_eq!(demand_bps(&[audio_only]), AUDIO);
        // A track that is there and not yet measured costs nothing.
        let mut unmeasured = publisher("a", Main);
        unmeasured.main_bps = Some(0);
        assert_eq!(targets(Some(base), &[unmeasured]), vec![Main]);
    }

    #[test]
    fn a_downgrade_takes_effect_at_once() {
        let publishers = three(Main);
        let mut allocation = DownlinkAllocation::new();
        // Nothing known: everything; then the budget becomes known and
        // covers it: what is being sent is kept, with no proof asked.
        let given = allocation.allocate(None, None, &publishers);
        assert!(given.iter().all(|g| g.given == Main));
        let enough = demand_bps(&publishers);
        let given = allocation.allocate(Some(enough), None, &publishers);
        assert!(given.iter().all(|g| g.given == Main && !g.changed()));
        let short = first(3) + with_headroom(MAIN) + 2 * with_headroom(MIDDLE);
        let given = allocation.allocate(Some(short), None, &publishers);
        assert_eq!(qualities(&given), vec![Main, Middle, Middle]);
        assert_eq!(
            given.iter().map(Given::changed).collect::<Vec<_>>(),
            vec![false, true, true]
        );
        assert_eq!(given[1].previous, Some(Main));
        assert_eq!(allocation.given(&"b"), Some(Middle));
        assert_eq!(allocation.given(&"z"), None);
    }

    #[test]
    fn an_upgrade_needs_the_path_to_have_carried_the_set() {
        let publishers = three(Main);
        let mut allocation = DownlinkAllocation::new();
        let plenty = Some(100_000_000);
        let short = first(3) + 3 * with_headroom(MIDDLE);
        let _ = allocation.allocate(None, None, &publishers);
        assert_eq!(
            qualities(&allocation.allocate(Some(short), None, &publishers)),
            vec![Middle; 3],
            "down to what the budget covers"
        );
        // The budget is back, however high: nothing moves on its word.
        assert_eq!(
            qualities(&allocation.allocate(plenty, None, &publishers)),
            vec![Middle; 3]
        );
        // What is arriving now (three middles on the wire) vouches for
        // nothing larger.
        let now = wire_bps(&publishers, &[Middle; 3]);
        assert_eq!(
            qualities(&allocation.allocate(plenty, Some(now), &publishers)),
            vec![Middle; 3]
        );
        // The next set is the most important publisher on main; tried at
        // what it needs, it is given when the path carried that.
        let step = next_step(&publishers, &[Middle; 3]).expect("a step");
        assert_eq!((step.index, step.quality), (0, Main));
        assert_eq!(
            step.need_bps,
            need_bps(&publishers, &[Main, Middle, Middle])
        );
        let one_bit_short = wire_bps(&publishers, &[Main, Middle, Middle]) - 1;
        assert_eq!(
            qualities(&allocation.allocate(plenty, Some(one_bit_short), &publishers)),
            vec![Middle; 3]
        );
        let given = allocation.allocate(plenty, Some(step.need_bps), &publishers);
        assert_eq!(qualities(&given), vec![Main, Middle, Middle]);
        assert!(given[0].changed() && !given[1].changed());
        // What was carried stops vouching (the caller's matter): nobody
        // moves down for it.
        assert_eq!(
            qualities(&allocation.allocate(plenty, None, &publishers)),
            vec![Main, Middle, Middle]
        );
        // And the budget still bounds an upgrade the path has carried.
        let step = next_step(&publishers, &[Main, Middle, Middle]).expect("a step");
        assert_eq!((step.index, step.quality), (1, Main));
        let covers_what_is_given = need_bps(&publishers, &[Main, Middle, Middle]);
        assert_eq!(
            qualities(&allocation.allocate(
                Some(covers_what_is_given),
                Some(step.need_bps),
                &publishers
            )),
            vec![Main, Middle, Middle]
        );
        assert_eq!(
            qualities(&allocation.allocate(Some(step.need_bps), Some(step.need_bps), &publishers)),
            vec![Main, Main, Middle]
        );
    }

    #[test]
    fn proof_of_a_high_rate_moves_everyone_it_covers_in_order_of_importance() {
        let publishers = three(Main);
        let mut allocation = DownlinkAllocation::new();
        let plenty = Some(100_000_000);
        let _ = allocation.allocate(Some(0), None, &publishers);
        assert_eq!(allocation.given(&"a"), Some(Light));
        // Carried: enough for two mains and the third's light track.
        let carried = wire_bps(&publishers, &[Main, Main, Light]) + MIDDLE - 1;
        let given = allocation.allocate(plenty, Some(carried), &publishers);
        assert_eq!(qualities(&given), vec![Main, Main, Light]);
        // One middle track more is carried: the third gets it.
        let given = allocation.allocate(plenty, Some(carried + 1), &publishers);
        assert_eq!(qualities(&given), vec![Main, Main, Middle]);
    }

    #[test]
    fn a_new_publisher_and_a_larger_want_also_wait_for_proof() {
        let mut allocation = DownlinkAllocation::new();
        let plenty = Some(100_000_000);
        let mut publishers = vec![publisher("a", Light)];
        assert_eq!(
            allocation.allocate(plenty, None, &publishers)[0].given,
            Light
        );
        // The subscriber now shows the main picture: not on the budget's
        // word.
        publishers[0].wanted = Main;
        assert_eq!(
            allocation.allocate(plenty, None, &publishers)[0].given,
            Light
        );
        let step = next_step(&publishers, &[Light]).expect("a step");
        assert_eq!(step.quality, Middle, "one track at a time");
        let given = allocation.allocate(plenty, Some(step.need_bps), &publishers);
        assert_eq!(given[0].given, Middle);
        assert_eq!(given[0].previous, Some(Light));
        // A smaller want is answered at once.
        publishers[0].wanted = Light;
        assert_eq!(
            allocation.allocate(plenty, None, &publishers)[0].given,
            Light
        );
        // A publisher that joins starts at its light track, and is new
        // again when it comes back.
        publishers.push(publisher("b", Main));
        let given = allocation.allocate(plenty, None, &publishers);
        assert_eq!((given[1].given, given[1].previous), (Light, None));
        publishers.remove(1);
        let _ = allocation.allocate(plenty, None, &publishers);
        assert_eq!(allocation.given(&"b"), None);
        publishers.push(publisher("b", Main));
        let given = allocation.allocate(plenty, None, &publishers);
        assert_eq!(given[1].previous, None);
        // Unless nothing is known of the path: then it gets what was
        // asked for.
        let mut fresh = DownlinkAllocation::new();
        assert_eq!(fresh.allocate(None, None, &publishers)[1].given, Main);
    }

    #[test]
    fn the_next_step_is_the_most_important_publisher_given_less_than_asked() {
        let mut publishers = three(Main);
        assert_eq!(next_step(&publishers, &[Main, Main, Main]), None);
        let step = next_step(&publishers, &[Main, Light, Middle]).expect("a step");
        assert_eq!((step.index, step.quality), (1, Middle));
        assert_eq!(
            step.need_bps,
            need_bps(&publishers, &[Main, Middle, Middle])
        );
        // One that asks for no more than it has is passed over.
        publishers[1].wanted = Light;
        let step = next_step(&publishers, &[Main, Light, Middle]).expect("a step");
        assert_eq!((step.index, step.quality), (2, Main));
        // A list shorter than the publishers counts the rest as light.
        let step = next_step(&publishers, &[Main]).expect("a step");
        assert_eq!((step.index, step.quality), (2, Middle));
    }

    /// An estimate standing above anything the path was seen to carry
    /// takes nothing from anyone: the rule fills from the most important
    /// publisher down, and run on such an estimate it would give the
    /// second a main track and the third nothing.
    #[test]
    fn an_estimate_above_what_is_given_takes_nothing_away() {
        use VideoQuality::{Light, Main, Middle};
        let publishers = three(Main);
        let mut allocation = DownlinkAllocation::new();
        // Settled on one main and two middles.
        let settled = first(3) + with_headroom(MAIN) + 2 * with_headroom(MIDDLE);
        let _ = allocation.allocate(None, None, &publishers);
        let given = allocation.allocate(Some(settled), None, &publishers);
        assert_eq!(qualities(&given), vec![Main, Middle, Middle]);
        // The estimate rises by rule to where two mains would fit and a
        // third track would not.
        let inflated = first(3) + 2 * with_headroom(MAIN) + with_headroom(MIDDLE) / 2;
        assert_eq!(
            targets(Some(inflated), &publishers),
            vec![Main, Main, Light],
            "what the rule alone would say"
        );
        let given = allocation.allocate(Some(inflated), None, &publishers);
        assert_eq!(
            qualities(&given),
            vec![Main, Middle, Middle],
            "nothing moves"
        );
        // A change of importance moves a track at once: the same budget,
        // shared out in the new order.
        let reordered = vec![
            publisher("c", Main),
            publisher("a", Main),
            publisher("b", Main),
        ];
        let given = allocation.allocate(Some(inflated), None, &reordered);
        assert_eq!(
            qualities(&given),
            vec![Middle, Middle, Middle],
            "a gives its main track up at once; c gets one when the path has carried it"
        );
        let carried = wire_bps(&reordered, &[Main, Middle, Middle]);
        let given = allocation.allocate(Some(inflated), Some(carried), &reordered);
        assert_eq!(qualities(&given), vec![Main, Middle, Middle]);
    }

    /// The headroom is asked for to give a track, not to keep it: a set
    /// stays while what it puts on the wire fits the budget, and goes
    /// the moment that does not.
    #[test]
    fn a_track_is_kept_while_what_is_on_the_wire_fits() {
        use VideoQuality::{Main, Middle};
        let publishers = vec![publisher("a", Main)];
        let mut allocation = DownlinkAllocation::new();
        let _ = allocation.allocate(None, None, &publishers);
        let wire = wire_bps(&publishers, &[Main]);
        let need = need_bps(&publishers, &[Main]);
        assert!(wire < need);
        // Between the two: kept.
        let given = allocation.allocate(Some(wire), None, &publishers);
        assert_eq!(qualities(&given), vec![Main]);
        let given = allocation.allocate(Some(need - 1), None, &publishers);
        assert_eq!(qualities(&given), vec![Main]);
        // One bit under what is on the wire: gone at once, for the next
        // smaller track.
        let given = allocation.allocate(Some(wire - 1), None, &publishers);
        assert_eq!(qualities(&given), vec![Middle]);
        // And giving it back needs the headroom as well as the proof.
        let given = allocation.allocate(Some(need - 1), Some(need), &publishers);
        assert_eq!(qualities(&given), vec![Middle]);
        let given = allocation.allocate(Some(need), Some(wire), &publishers);
        assert_eq!(qualities(&given), vec![Main]);
    }

    /// Every set one move larger, most important first: the caller takes
    /// the first the path is not known to refuse.
    #[test]
    fn the_next_steps_list_every_single_move_in_order_of_importance() {
        use VideoQuality::{Light, Main, Middle};
        let publishers = three(Main);
        let steps = next_steps(&publishers, &[Main, Middle, Light]);
        assert_eq!(
            steps
                .iter()
                .map(|s| (s.index, s.quality))
                .collect::<Vec<_>>(),
            vec![(1, Main), (2, Middle)]
        );
        assert_eq!(
            steps[0].need_bps,
            need_bps(&publishers, &[Main, Main, Light])
        );
        assert_eq!(
            steps[1].need_bps,
            need_bps(&publishers, &[Main, Middle, Middle])
        );
        assert!(
            steps[1].need_bps < steps[0].need_bps,
            "the later one is cheaper"
        );
        assert_eq!(
            next_step(&publishers, &[Main, Middle, Light]),
            Some(steps[0])
        );
        assert!(next_steps(&publishers, &[Main, Main, Main]).is_empty());
    }

    #[test]
    fn a_publisher_listed_twice_is_taken_once() {
        let publishers = vec![publisher("a", Main), publisher("a", Light)];
        let mut allocation = DownlinkAllocation::new();
        let given = allocation.allocate(None, None, &publishers);
        assert_eq!(given[0].given, Main);
        assert_eq!(given[1].given, Main);
    }

    #[test]
    fn smaller_orders_light_below_middle_below_main() {
        assert_eq!(smaller(Main, Middle), Middle);
        assert_eq!(smaller(Light, Main), Light);
        assert_eq!(smaller(Middle, Light), Light);
        assert_eq!(smaller(Main, Main), Main);
    }

    #[test]
    fn nothing_overflows_at_the_extremes() {
        let mut huge = publisher("a", Main);
        huge.main_bps = Some(u64::MAX);
        huge.light_bps = Some(u64::MAX);
        huge.audio_bps = u64::MAX;
        assert_eq!(demand_bps(std::slice::from_ref(&huge)), u64::MAX);
        assert_eq!(wire_bps(std::slice::from_ref(&huge), &[Main]), u64::MAX);
        // Audio and light alone take the whole budget.
        assert_eq!(targets(Some(u64::MAX), &[huge]), vec![Light]);
        assert_eq!(with_headroom(u64::MAX), u64::MAX);
    }
}
