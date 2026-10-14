// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Who is speaking in a group call.
//!
//! `docs/ux/group-call-grid.md` sections 5.2 and 7.3. Worked out on the
//! device from audio it has already received, decrypted and decoded, so the
//! call server never learns it. Two outputs with two different timings,
//! because they answer two different questions:
//!
//! - **the ring** on a tile: on after [`RING_ON_MS`] of sound, off after
//!   [`RING_OFF_MS`] of silence. It may come on for a cough. "Sound" is a
//!   level well above the person's own noise floor ([`NoiseGate`]), not above
//!   one fixed number.
//! - **the floor**: the one person the large tile follows. A person takes it
//!   after [`FLOOR_ONSET_MS`] of continuous speech and keeps it while they
//!   keep speaking, so a cough never moves a tile and two people talking over
//!   each other do not make the stage flip between them.
//!
//! This module is that rule, written once. It existed as three hand-written
//! copies (TypeScript, Swift, Kotlin) which drifted: the fall of the noise
//! floor was per reading on one client and per unit of time on the others,
//! and the three read their levels at different intervals. What stays on each
//! client is reading a level, the loop that reads it, and its diagnostics.
//!
//! One [`SpeakingTracker`] per call. The caller feeds it one level reading
//! per person ([`SpeakingTracker::sample`]), asks it to settle the floor
//! ([`SpeakingTracker::decide_floor`]) and reads the answers back. Pure and
//! clock-free: every time is the caller's, one clock, whole milliseconds, any
//! origin. Total: no input panics.
//!
//! # Levels and floating point
//!
//! A level is what the clients already have: the root mean square of decoded
//! samples in `-1..1`, so a number in `0..1`, as an `f64`. It is converted
//! once, on the way in, to a whole number of [`LEVEL_UNITS_PER_ONE`]ths by
//! one multiplication and one truncation, both of which IEEE 754 defines
//! exactly, and every decision after that is integer arithmetic. The noise
//! floor's rise (`1.1 ^ seconds`) and fall (`0.5 ^ (milliseconds / 100)`)
//! are fixed-point tables, not `pow`: a platform's `pow` is not required to
//! be correctly rounded, and wasm, iOS and Android each bring their own, so a
//! floor computed with it could differ in its last bit and a level sitting
//! exactly on a threshold would light a ring on one client and not on
//! another. The threshold handed back for a diagnostic line
//! ([`SpeakingTracker::threshold`]) is one correctly rounded division, so it
//! is the same `f64` everywhere too.
//!
//! A level that is not a number (NaN, an infinity) is silence for the ring
//! and teaches the noise floor nothing. A level above 1 is off the scale and
//! is read as 1.
//!
//! # Time
//!
//! A reading earlier than the one before it counts as no time passed: nothing
//! rises, falls, turns on or turns off because a clock stepped back.
//!
//! Every number here is chosen, not measured.

use std::collections::{BTreeMap, BTreeSet};

/// The threshold a person starts with, before anything is known about the
/// noise around their microphone. On the scale of every level: `0..1`.
pub const SPEAKING_LEVEL: f64 = 0.02;

/// Sound counts as speech when it is this many times the person's noise
/// floor (about 9.5 dB above it).
///
/// Why the threshold follows the person instead of being one number: on a
/// real call (2026-10-11) a phone's microphone delivered speech at 0.01 to
/// 0.05 on this scale, while a laptop in a room idled at 0.01 to 0.02. No
/// single number separates the two; each person's own floor does.
pub const SPEECH_OVER_NOISE: u32 = 3;

/// The threshold never goes below this, however quiet the floor (about
/// -46 dB full scale).
pub const SPEAKING_LEVEL_MIN: f64 = 0.005;

/// Nor above this, however loud the room: a floor learned wrong must not shut
/// a person out for good.
pub const SPEAKING_LEVEL_MAX: f64 = 0.05;

/// The floor climbs towards steady sound by this factor a second (doubling
/// in about seven seconds), so a fan switched on stops counting as speech
/// and a sentence does not.
pub const NOISE_RISE_PER_SECOND: f64 = 1.1;

/// The floor may fall to half of itself in this many milliseconds, so one
/// torn reading does not empty it. A rule per unit of time, not per reading:
/// over `t` milliseconds the floor may fall to `0.5 ^ (t / 100)` of itself,
/// however often a client reads.
pub const NOISE_FALL_HALF_MS: u64 = 100;

/// The longest time between two readings that counts, for the rise and for
/// the fall, milliseconds: a long gap is not a long rise.
pub const NOISE_ELAPSED_MAX_MS: u64 = 1000;

/// A reading below this says nothing about the room: it is a muted sender or
/// a stream that is not flowing (digital silence), not a quiet room.
pub const NOISE_EVIDENCE_MIN: f64 = 1e-4;

/// Sound for this long turns the ring on, milliseconds.
pub const RING_ON_MS: u64 = 200;

/// Silence for this long turns it off, milliseconds.
pub const RING_OFF_MS: u64 = 800;

/// Continuous speech for this long takes the floor, milliseconds.
pub const FLOOR_ONSET_MS: u64 = 1500;

/// How many people one tracker follows. A reading for one more is dropped:
/// that person has no ring, cannot take the floor and reads the starting
/// threshold, until someone is forgotten. Far above any call's size; it is
/// what bounds the memory.
pub const MAX_TRACKED_PEOPLE: usize = 1024;

/// The longest person id a tracker follows, bytes. A longer one is dropped
/// like a person beyond [`MAX_TRACKED_PEOPLE`].
pub const MAX_PERSON_ID_BYTES: usize = 256;

/// How many level units make full scale (a level of 1).
///
/// `3 * 10^6 * 2^28`: divisible by everything the constants above divide it
/// by, so each of them is a whole number of units, and below `2^53`, so it is
/// exact as an `f64`.
pub const LEVEL_UNITS_PER_ONE: u64 = 3_000_000 << 28;

/// [`LEVEL_UNITS_PER_ONE`] as the `f64` a level is multiplied by.
const UNITS_F: f64 = 805_306_368_000_000.0;

/// [`SPEAKING_LEVEL`] in units.
const START_UNITS: u64 = LEVEL_UNITS_PER_ONE / 50;
/// The floor a person starts with: [`START_UNITS`] over [`SPEECH_OVER_NOISE`].
const START_FLOOR_UNITS: u64 = START_UNITS / SPEECH_OVER_NOISE as u64;
/// [`SPEAKING_LEVEL_MIN`] in units.
const MIN_UNITS: u64 = LEVEL_UNITS_PER_ONE / 200;
/// [`SPEAKING_LEVEL_MAX`] in units.
const MAX_UNITS: u64 = LEVEL_UNITS_PER_ONE / 20;
/// [`NOISE_EVIDENCE_MIN`] in units.
const EVIDENCE_UNITS: u64 = LEVEL_UNITS_PER_ONE / 10_000;

/// Fraction bits of the factors in the tables below.
const FACTOR_BITS: u32 = 60;
/// A factor of exactly 1.
const FACTOR_ONE: u64 = 1 << FACTOR_BITS;

/// `2 ^ (-i / 10)` for `i` in `0..10`, rounded to nearest.
const FALL_TENTHS: [u64; 10] = [
    1_152_921_504_606_846_976,
    1_075_713_800_450_443_527,
    1_003_676_465_271_705_617,
    936_463_254_927_548_089,
    873_751_112_209_346_102,
    815_238_614_083_298_888,
    760_644_522_914_461_219,
    709_706_434_710_967_409,
    662_179_517_891_295_819,
    617_835_336_512_516_627,
];

/// `2 ^ (-i / 100)` for `i` in `0..10`, rounded to nearest.
const FALL_HUNDREDTHS: [u64; 10] = [
    1_152_921_504_606_846_976,
    1_144_957_694_053_035_747,
    1_137_048_893_556_963_550,
    1_129_194_723_136_405_545,
    1_121_394_805_433_865_842,
    1_113_648_765_698_447_176,
    1_105_956_231_767_845_816,
    1_098_316_834_050_470_841,
    1_090_730_205_507_686_931,
    1_083_195_981_636_179_815,
];

/// `1.1 ^ (i / 10)` for `i` in `0..=10`, rounded to nearest.
const RISE_TENTHS: [u64; 11] = [
    1_152_921_504_606_846_976,
    1_163_962_552_830_967_582,
    1_175_109_336_567_350_081,
    1_186_362_868_400_880_105,
    1_197_724_170_613_546_263,
    1_209_194_275_277_305_247,
    1_220_774_224_347_836_278,
    1_232_465_069_759_193_385,
    1_244_267_873_519_364_143,
    1_256_183_707_806_743_530,
    1_268_213_655_067_531_674,
];

/// `1.1 ^ (i / 100)` for `i` in `0..10`, rounded to nearest.
const RISE_HUNDREDTHS: [u64; 10] = [
    1_152_921_504_606_846_976,
    1_154_020_879_990_995_174,
    1_155_121_303_691_295_375,
    1_156_222_776_707_375_886,
    1_157_325_300_039_818_217,
    1_158_428_874_690_157_988,
    1_159_533_501_660_885_839,
    1_160_639_181_955_448_340,
    1_161_745_916_578_248_908,
    1_162_853_706_534_648_709,
];

/// `1.1 ^ (i / 1000)` for `i` in `0..10`, rounded to nearest.
const RISE_THOUSANDTHS: [u64; 10] = [
    1_152_921_504_606_846_976,
    1_153_031_394_999_504_682,
    1_153_141_295_866_334_610,
    1_153_251_207_208_335_103,
    1_153_361_129_026_504_600,
    1_153_471_061_321_841_634,
    1_153_581_004_095_344_833,
    1_153_690_957_348_012_920,
    1_153_800_921_080_844_714,
    1_153_910_895_294_839_129,
];

/// One entry of a factor table; a factor of 1 for an index outside it, which
/// no caller produces.
fn factor(table: &[u64], index: u64) -> u64 {
    usize::try_from(index)
        .ok()
        .and_then(|i| table.get(i))
        .copied()
        .unwrap_or(FACTOR_ONE)
}

/// `value` times a table factor, rounded to nearest.
fn scaled(value: u64, by: u64) -> u64 {
    let product = u128::from(value) * u128::from(by) + (1 << (FACTOR_BITS - 1));
    u64::try_from(product >> FACTOR_BITS).unwrap_or(u64::MAX)
}

/// `floor * 0.5 ^ (ms / 100)`, for `ms` up to [`NOISE_ELAPSED_MAX_MS`].
fn fallen(floor: u64, ms: u64) -> u64 {
    let ms = ms.min(NOISE_ELAPSED_MAX_MS);
    let part = ms % NOISE_FALL_HALF_MS;
    let within = scaled(
        scaled(floor, factor(&FALL_TENTHS, part / 10)),
        factor(&FALL_HUNDREDTHS, part % 10),
    );
    match ms / NOISE_FALL_HALF_MS {
        0 => within,
        // At most 10: `ms` is capped above.
        halvings => (within + (1 << (halvings - 1))) >> halvings,
    }
}

/// `floor * 1.1 ^ (ms / 1000)`, for `ms` up to [`NOISE_ELAPSED_MAX_MS`].
fn risen(floor: u64, ms: u64) -> u64 {
    let ms = ms.min(NOISE_ELAPSED_MAX_MS);
    let tenths = scaled(floor, factor(&RISE_TENTHS, ms / 100));
    let hundredths = scaled(tenths, factor(&RISE_HUNDREDTHS, (ms / 10) % 10));
    scaled(hundredths, factor(&RISE_THOUSANDTHS, ms % 10))
}

/// A level as whole units, or `None` when it is not a number. Negative reads
/// as 0 and anything above full scale as full scale.
fn level_units(level: f64) -> Option<u64> {
    if !level.is_finite() {
        return None;
    }
    if level <= 0.0 {
        return Some(0);
    }
    // One IEEE multiplication, then a truncation of a value in
    // `0..=UNITS_F`: the same integer on every platform.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let units = (level.min(1.0) * UNITS_F) as u64;
    Some(units.min(LEVEL_UNITS_PER_ONE))
}

/// Units as a level in `0..1`: one correctly rounded division.
fn level_of(units: u64) -> f64 {
    // Units never exceed `LEVEL_UNITS_PER_ONE`, which is below 2^53.
    #[allow(clippy::cast_precision_loss)]
    let level = units as f64 / UNITS_F;
    level
}

/// One person's noise floor and the threshold that follows from it.
///
/// The floor falls quickly to the quietest recent reading and rises slowly
/// towards whatever is steadily there. Pure: the caller supplies the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoiseGate {
    /// The floor, in level units. Never 0.
    floor: u64,
    /// When the last reading was folded in, or `None` before the first.
    last_at_ms: Option<u64>,
}

impl Default for NoiseGate {
    fn default() -> Self {
        Self::new()
    }
}

impl NoiseGate {
    /// A gate that knows nothing yet: its threshold is [`SPEAKING_LEVEL`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            floor: START_FLOOR_UNITS,
            last_at_ms: None,
        }
    }

    /// The threshold in level units.
    const fn threshold_units(&self) -> u64 {
        let raw = self.floor.saturating_mul(SPEECH_OVER_NOISE as u64);
        if raw < MIN_UNITS {
            MIN_UNITS
        } else if raw > MAX_UNITS {
            MAX_UNITS
        } else {
            raw
        }
    }

    /// A level at or above this counts as sound, now. Between
    /// [`SPEAKING_LEVEL_MIN`] and [`SPEAKING_LEVEL_MAX`].
    #[must_use]
    pub fn threshold(&self) -> f64 {
        level_of(self.threshold_units())
    }

    /// Whether `level` counts as sound against the threshold as it stands.
    /// A level that is not a number never does.
    #[must_use]
    pub fn is_sound(&self, level: f64) -> bool {
        level_units(level).is_some_and(|units| units >= self.threshold_units())
    }

    /// Fold one reading in. Digital silence (below [`NOISE_EVIDENCE_MIN`])
    /// and a level that is not a number change the floor by nothing; they
    /// still count as a reading for the time between two of them.
    ///
    /// A reading below the floor pulls it down, to no less than
    /// `0.5 ^ (elapsed / 100 ms)` of itself; the first reading, with no time
    /// before it, may fall as far as 100 ms allows. A reading at or above the
    /// floor lets it climb by `1.1 ^ (elapsed / 1 s)`, never past the reading
    /// itself. Elapsed time is capped at [`NOISE_ELAPSED_MAX_MS`], and a
    /// clock that stepped back counts as no time passed.
    pub fn update(&mut self, level: f64, now_ms: u64) {
        let elapsed = self
            .last_at_ms
            .map(|last| now_ms.saturating_sub(last).min(NOISE_ELAPSED_MAX_MS));
        self.last_at_ms = Some(now_ms);
        let Some(units) = level_units(level) else {
            return;
        };
        if units < EVIDENCE_UNITS {
            return;
        }
        self.floor = if units < self.floor {
            units.max(fallen(self.floor, elapsed.unwrap_or(NOISE_FALL_HALF_MS)))
        } else {
            units.min(risen(self.floor, elapsed.unwrap_or(0)))
        };
    }
}

/// What is known about one person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Voice {
    /// Since when the level has been at or above the threshold.
    sound_since: Option<u64>,
    /// Since when it has been below.
    silence_since: Option<u64>,
    /// The ring.
    speaking: bool,
    /// Since when the ring has been on without a break.
    speaking_since: Option<u64>,
    /// The last reading with sound while the ring was on.
    last_sound_at: Option<u64>,
    /// When the person last held the floor.
    held_floor_at: Option<u64>,
    /// What counts as sound for this person.
    gate: NoiseGate,
}

impl Voice {
    /// Enough to take the floor: the speech itself has lasted the onset. The
    /// time the ring stays on after the last sound does not count, or a
    /// 0.8-second sound would grow into a 1.5-second one by standing still.
    fn has_earned_the_floor(&self) -> bool {
        self.speaking
            && matches!(
                (self.speaking_since, self.last_sound_at),
                (Some(since), Some(last))
                    if last.checked_sub(since).is_some_and(|spoken| spoken >= FLOOR_ONSET_MS)
            )
    }

    /// Everything but what was learned about the room and the memory of
    /// having held the floor.
    const fn hush(&mut self) {
        self.sound_since = None;
        self.silence_since = None;
        self.speaking = false;
        self.speaking_since = None;
        self.last_sound_at = None;
    }
}

/// Whether `since` lies at least `for_ms` before `now_ms`. A `since` after
/// `now_ms` (a clock that stepped back) is not.
fn lasted(since: u64, now_ms: u64, for_ms: u64) -> bool {
    now_ms
        .checked_sub(since)
        .is_some_and(|elapsed| elapsed >= for_ms)
}

/// Follows the people of one call: whose ring is on and who holds the floor.
///
/// People are the opaque ids the caller already uses. The tracker holds at
/// most [`MAX_TRACKED_PEOPLE`] of them, each id at most
/// [`MAX_PERSON_ID_BYTES`] long.
///
/// The floor changes only in [`Self::decide_floor`] (and is given up at once
/// by [`Self::silence`], [`Self::forget`], [`Self::keep_only`] and
/// [`Self::set_local`] when they concern its holder), so read it after
/// deciding.
#[derive(Debug, Clone, Default)]
pub struct SpeakingTracker {
    /// Everyone sampled and not forgotten.
    voices: BTreeMap<String, Voice>,
    /// Who holds the floor.
    floor: Option<String>,
    /// This device's own person, who never holds it.
    local: Option<String>,
}

impl SpeakingTracker {
    /// A tracker of a call nobody has been heard in.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            voices: BTreeMap::new(),
            floor: None,
            local: None,
        }
    }

    /// Name this device's own person, or `None` when there is none.
    ///
    /// The own person is sampled like anyone and has a ring: it is the
    /// quickest check that the microphone works. They never hold the floor,
    /// wherever they stand in the order given to [`Self::decide_floor`]: the
    /// floor exists for the large tile, which never shows the own tile while
    /// anyone else is present, and a floor held by the own microphone (the
    /// person talking, or the room heard through it) kept everyone else from
    /// taking it, so the large tile never moved to the person speaking.
    ///
    /// If that person holds the floor now they give it up at once, and are
    /// no longer remembered as having held it.
    pub fn set_local(&mut self, person: Option<&str>) {
        self.local = person
            .filter(|id| id.len() <= MAX_PERSON_ID_BYTES)
            .map(str::to_owned);
        let Some(local) = self.local.as_deref() else {
            return;
        };
        if self.floor.as_deref() == Some(local) {
            self.floor = None;
        }
        if let Some(voice) = self.voices.get_mut(local) {
            voice.held_floor_at = None;
        }
    }

    /// This device's own person, as last named.
    #[must_use]
    pub fn local(&self) -> Option<&str> {
        self.local.as_deref()
    }

    /// One level reading for one person at `now_ms`.
    ///
    /// `level` is on the scale of [`SPEAKING_LEVEL`]. `None` is "could not be
    /// read": it counts as silence for the ring and teaches the noise floor
    /// nothing. A level that is not a number is silence too.
    ///
    /// The reading is judged against the person's threshold as it stood
    /// before this reading, and then folded into their noise floor.
    pub fn sample(&mut self, person: &str, level: Option<f64>, now_ms: u64) {
        if !self.voices.contains_key(person) {
            if self.voices.len() >= MAX_TRACKED_PEOPLE || person.len() > MAX_PERSON_ID_BYTES {
                return;
            }
            self.voices.insert(person.to_owned(), Voice::default());
        }
        let Some(voice) = self.voices.get_mut(person) else {
            return;
        };
        let sound = level.is_some_and(|l| voice.gate.is_sound(l));
        if let Some(level) = level {
            voice.gate.update(level, now_ms);
        }
        if sound {
            voice.silence_since = None;
            let since = *voice.sound_since.get_or_insert(now_ms);
            if !voice.speaking && lasted(since, now_ms, RING_ON_MS) {
                voice.speaking = true;
                voice.speaking_since = Some(since);
            }
            if voice.speaking {
                voice.last_sound_at = Some(now_ms);
            }
        } else {
            voice.sound_since = None;
            let since = *voice.silence_since.get_or_insert(now_ms);
            if voice.speaking && lasted(since, now_ms, RING_OFF_MS) {
                voice.speaking = false;
                voice.speaking_since = None;
                voice.last_sound_at = None;
            }
        }
    }

    /// The person is muted: their ring goes off at once and they give up the
    /// floor. A muted person is never speaking, whatever a level reading
    /// says, so call this instead of [`Self::sample`] for as long as they are
    /// muted.
    ///
    /// What was learned about their room is kept, and so is the memory of
    /// their having held the floor. After unmuting, sound has to last
    /// [`RING_ON_MS`] all over again.
    pub fn silence(&mut self, person: &str) {
        if let Some(voice) = self.voices.get_mut(person) {
            voice.hush();
        }
        if self.floor.as_deref() == Some(person) {
            self.floor = None;
        }
    }

    /// The person left the call: everything about them is dropped, the floor
    /// if they held it and the memory of their having held it included.
    pub fn forget(&mut self, person: &str) {
        self.voices.remove(person);
        if self.floor.as_deref() == Some(person) {
            self.floor = None;
        }
    }

    /// Forget everyone who is not one of `people`: the roster of the call as
    /// it is now.
    pub fn keep_only<S: AsRef<str>>(&mut self, people: &[S]) {
        let present: BTreeSet<&str> = people.iter().map(AsRef::as_ref).collect();
        self.voices.retain(|id, _| present.contains(id.as_str()));
        if self
            .floor
            .as_deref()
            .is_some_and(|holder| !present.contains(holder))
        {
            self.floor = None;
        }
    }

    /// Settle who holds the floor after the samples so far, and return them.
    ///
    /// `order` is the people of the call in roster order. Whoever holds the
    /// floor keeps it while their ring is on and they are still in `order`;
    /// otherwise it goes to the first person in `order` whose speech has
    /// lasted [`FLOOR_ONSET_MS`], which is what breaks the tie when two reach
    /// the onset in the same reading. When the holder stops and nobody has
    /// earned it, the floor is empty and [`Self::last_held_floor_ms`] still
    /// names them, which is what keeps the large tile on them.
    ///
    /// This device's own person ([`Self::set_local`]) is skipped wherever
    /// they stand in `order`. `now_ms` is recorded as the moment the holder
    /// last held the floor.
    pub fn decide_floor<S: AsRef<str>>(&mut self, order: &[S], now_ms: u64) -> Option<&str> {
        let local = self.local.as_deref();
        let may_hold = |id: &str| Some(id) != local;
        let kept = self.floor.as_deref().is_some_and(|holder| {
            may_hold(holder)
                && order.iter().any(|id| id.as_ref() == holder)
                && self.voices.get(holder).is_some_and(|v| v.speaking)
        });
        if !kept {
            self.floor = order
                .iter()
                .map(AsRef::as_ref)
                .find(|id| {
                    may_hold(id)
                        && self
                            .voices
                            .get(*id)
                            .is_some_and(Voice::has_earned_the_floor)
                })
                .map(str::to_owned);
        }
        if let Some(voice) = self
            .floor
            .as_deref()
            .and_then(|holder| self.voices.get_mut(holder))
        {
            voice.held_floor_at = Some(now_ms);
        }
        self.floor.as_deref()
    }

    /// Who holds the floor, as last decided.
    #[must_use]
    pub fn floor(&self) -> Option<&str> {
        self.floor.as_deref()
    }

    /// Whether the person's ring is on.
    #[must_use]
    pub fn is_speaking(&self, person: &str) -> bool {
        self.voices.get(person).is_some_and(|v| v.speaking)
    }

    /// Everyone whose ring is on, the own person included, sorted by id.
    #[must_use]
    pub fn speaking(&self) -> Vec<&str> {
        self.voices
            .iter()
            .filter(|(_, v)| v.speaking)
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// When the person last held the floor: the `now_ms` of the last
    /// [`Self::decide_floor`] that found them holding it. `None` for someone
    /// who never has, and for someone forgotten.
    #[must_use]
    pub fn last_held_floor_ms(&self, person: &str) -> Option<u64> {
        self.voices.get(person).and_then(|v| v.held_floor_at)
    }

    /// Everyone who has held the floor, with when they last did, sorted by
    /// id.
    #[must_use]
    pub fn floor_history(&self) -> Vec<(&str, u64)> {
        self.voices
            .iter()
            .filter_map(|(id, v)| v.held_floor_at.map(|at| (id.as_str(), at)))
            .collect()
    }

    /// What counts as sound for the person now, on the scale of the levels
    /// (for a diagnostic line). [`SPEAKING_LEVEL`] for someone not tracked.
    #[must_use]
    pub fn threshold(&self, person: &str) -> f64 {
        self.voices
            .get(person)
            .map_or(SPEAKING_LEVEL, |v| v.gate.threshold())
    }

    /// How many people are tracked.
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.voices.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const LOUD: f64 = SPEAKING_LEVEL * 5.0;
    const ORDER: [&str; 2] = ["a", "b"];
    /// The interval the web client reads at, and the one iOS and Android do.
    const INTERVALS: [u64; 2] = [100, 150];
    const OVER: f64 = 3.0;

    fn close(got: f64, want: f64) {
        assert!((got - want).abs() < 1e-9, "{got} is not {want}");
    }

    /// Feed `id` one level every 100 ms over `[from, to]`.
    fn feed(t: &mut SpeakingTracker, id: &str, level: f64, from: u64, to: u64) {
        let mut now = from;
        while now <= to {
            t.sample(id, Some(level), now);
            now += 100;
        }
    }

    /// Feed a gate one level every `every` ms for `ms`, starting at `from`.
    fn hold(gate: &mut NoiseGate, level: f64, from: u64, ms: u64, every: u64) -> u64 {
        let mut now = from;
        while now < from + ms {
            gate.update(level, now);
            now += every;
        }
        now
    }

    /// Speech as a call delivers it: syllables around `level`, never steady.
    fn talk(t: &mut SpeakingTracker, id: &str, level: f64, from: u64, ms: u64, every: u64) -> u64 {
        let mut now = from;
        let mut i = 0;
        while now < from + ms {
            let factor = if i % 3 == 0 { 0.6 } else { 1.0 };
            t.sample(id, Some(level * factor), now);
            now += every;
            i += 1;
        }
        now
    }

    fn floor_of(t: &mut SpeakingTracker, order: &[&str], now: u64) -> Option<String> {
        t.decide_floor(order, now).map(str::to_owned)
    }

    // ---- The numbers ---------------------------------------------------------

    #[test]
    fn the_numbers_are_the_ones_the_three_clients_had() {
        close(SPEAKING_LEVEL, 0.02);
        assert_eq!(SPEECH_OVER_NOISE, 3);
        close(SPEAKING_LEVEL_MIN, 0.005);
        close(SPEAKING_LEVEL_MAX, 0.05);
        close(NOISE_RISE_PER_SECOND, 1.1);
        assert_eq!(NOISE_FALL_HALF_MS, 100);
        assert_eq!(NOISE_ELAPSED_MAX_MS, 1000);
        close(NOISE_EVIDENCE_MIN, 1e-4);
        assert_eq!((RING_ON_MS, RING_OFF_MS, FLOOR_ONSET_MS), (200, 800, 1500));
    }

    #[test]
    fn every_constant_is_a_whole_number_of_units() {
        #[allow(clippy::cast_precision_loss)]
        let units = LEVEL_UNITS_PER_ONE as f64;
        assert!(units.to_bits() == UNITS_F.to_bits());
        const { assert!(LEVEL_UNITS_PER_ONE < 1 << 53) };
        assert_eq!(START_UNITS * 50, LEVEL_UNITS_PER_ONE);
        assert_eq!(START_FLOOR_UNITS * 3, START_UNITS);
        assert_eq!(MIN_UNITS * 200, LEVEL_UNITS_PER_ONE);
        assert_eq!(MAX_UNITS * 20, LEVEL_UNITS_PER_ONE);
        assert_eq!(EVIDENCE_UNITS * 10_000, LEVEL_UNITS_PER_ONE);
        // The public floats and the units are the same numbers, exactly.
        assert_eq!(level_units(SPEAKING_LEVEL), Some(START_UNITS));
        assert_eq!(level_units(SPEAKING_LEVEL_MIN), Some(MIN_UNITS));
        assert_eq!(level_units(SPEAKING_LEVEL_MAX), Some(MAX_UNITS));
        assert_eq!(level_units(NOISE_EVIDENCE_MIN), Some(EVIDENCE_UNITS));
        assert!(level_of(START_UNITS).to_bits() == SPEAKING_LEVEL.to_bits());
        assert!(level_of(MIN_UNITS).to_bits() == SPEAKING_LEVEL_MIN.to_bits());
        assert!(level_of(MAX_UNITS).to_bits() == SPEAKING_LEVEL_MAX.to_bits());
    }

    /// The tables stand in for `pow`; this is the one place `pow` is used,
    /// to catch a mistyped entry.
    #[test]
    fn the_tables_are_the_powers_they_stand_for() {
        #[allow(clippy::cast_precision_loss)]
        let as_f = |q: u64| q as f64 / FACTOR_ONE as f64;
        let check = |table: &[u64], base: f64, per: f64, sign: f64| {
            for (i, q) in table.iter().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let want = base.powf(sign * i as f64 / per);
                assert!((as_f(*q) - want).abs() < 1e-14, "{i}: {} {want}", as_f(*q));
            }
        };
        check(&FALL_TENTHS, 2.0, 10.0, -1.0);
        check(&FALL_HUNDREDTHS, 2.0, 100.0, -1.0);
        check(&RISE_TENTHS, 1.1, 10.0, 1.0);
        check(&RISE_HUNDREDTHS, 1.1, 100.0, 1.0);
        check(&RISE_THOUSANDTHS, 1.1, 1000.0, 1.0);
        // Over the whole range, against `pow`.
        let floor = LEVEL_UNITS_PER_ONE / 7;
        for ms in 0..=NOISE_ELAPSED_MAX_MS {
            #[allow(clippy::cast_precision_loss)]
            let t = ms as f64;
            close(
                level_of(fallen(floor, ms)),
                level_of(floor) * 0.5_f64.powf(t / 100.0),
            );
            close(
                level_of(risen(floor, ms)),
                level_of(floor) * 1.1_f64.powf(t / 1000.0),
            );
        }
        // No time: nothing moves, exactly. A whole step: exactly half.
        assert_eq!(fallen(floor, 0), floor);
        assert_eq!(risen(floor, 0), floor);
        assert_eq!(fallen(1 << 40, 100), 1 << 39);
        assert_eq!(fallen(1 << 40, 1000), 1 << 30);
        // Past the cap is the cap.
        assert_eq!(fallen(floor, u64::MAX), fallen(floor, 1000));
        assert_eq!(risen(floor, u64::MAX), risen(floor, 1000));
    }

    // ---- The ring ------------------------------------------------------------

    #[test]
    fn the_ring_comes_on_after_a_short_sound_and_goes_off_after_a_pause() {
        let mut t = SpeakingTracker::new();
        t.sample("a", Some(LOUD), 0);
        assert!(!t.is_speaking("a"));
        t.sample("a", Some(LOUD), RING_ON_MS);
        assert!(t.is_speaking("a"));
        t.sample("a", Some(0.0), RING_ON_MS + 100);
        assert!(t.is_speaking("a"));
        t.sample("a", Some(0.0), RING_ON_MS + 100 + RING_OFF_MS - 1);
        assert!(t.is_speaking("a"));
        t.sample("a", Some(0.0), RING_ON_MS + 100 + RING_OFF_MS);
        assert!(!t.is_speaking("a"));
    }

    #[test]
    fn the_ring_is_on_for_everyone_who_is_speaking_at_once() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "b", LOUD, 0, 500);
        feed(&mut t, "a", LOUD, 0, 500);
        assert_eq!(t.speaking(), vec!["a", "b"]);
    }

    #[test]
    fn a_level_that_is_not_a_number_is_silence() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "a", f64::NAN, 0, 600);
        feed(&mut t, "b", f64::INFINITY, 0, 600);
        feed(&mut t, "c", f64::NEG_INFINITY, 0, 600);
        feed(&mut t, "d", -1.0, 0, 600);
        assert!(t.speaking().is_empty());
        // Half the starting level is not sound either.
        feed(&mut t, "a", SPEAKING_LEVEL / 2.0, 700, 1300);
        assert!(t.speaking().is_empty());
    }

    #[test]
    fn a_level_above_full_scale_is_read_as_full_scale() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "a", 1e300, 0, 600);
        feed(&mut t, "b", 1.0, 0, 600);
        assert_eq!(t.speaking(), vec!["a", "b"]);
        assert!(t.threshold("a").to_bits() == t.threshold("b").to_bits());
    }

    /// The first reading at or past 200 ms of sound turns the ring on: the
    /// one at 200 ms when read every 100 ms, the one at 300 ms every 150 ms.
    /// The floor's onset is a whole number of both intervals.
    #[test]
    fn the_floor_is_taken_at_the_same_moment_whether_read_every_100_ms_or_every_150() {
        let first = |every: u64, floor: bool| -> u64 {
            let mut t = SpeakingTracker::new();
            let mut now = 0;
            loop {
                t.sample("a", Some(LOUD), now);
                let held = t.decide_floor(&ORDER, now) == Some("a");
                if (floor && held) || (!floor && t.is_speaking("a")) {
                    return now;
                }
                assert!(now <= 10_000);
                now += every;
            }
        };
        assert_eq!(FLOOR_ONSET_MS % 100, 0);
        assert_eq!(FLOOR_ONSET_MS % 150, 0);
        assert_eq!(first(100, true), FLOOR_ONSET_MS);
        assert_eq!(first(150, true), FLOOR_ONSET_MS);
        assert_eq!(first(100, false), 200);
        assert_eq!(first(150, false), 300);
    }

    #[test]
    fn a_clock_that_steps_back_turns_nothing_on_or_off() {
        let mut t = SpeakingTracker::new();
        t.sample("a", Some(LOUD), 5000);
        t.sample("a", Some(LOUD), 1000);
        assert!(!t.is_speaking("a"));
        t.sample("a", Some(LOUD), 5200);
        assert!(t.is_speaking("a"));
        t.sample("a", Some(0.0), 9000);
        t.sample("a", Some(0.0), 100);
        assert!(t.is_speaking("a"));
        assert_eq!(t.decide_floor(&ORDER, 0), None);
    }

    // ---- The floor -----------------------------------------------------------

    #[test]
    fn the_floor_is_not_taken_by_a_cough() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "a", LOUD, 0, 600);
        assert_eq!(t.decide_floor(&ORDER, 600), None);
        assert!(t.floor_history().is_empty());
    }

    #[test]
    fn the_floor_goes_to_someone_who_keeps_speaking_and_is_remembered_after_they_stop() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "a", LOUD, 0, FLOOR_ONSET_MS);
        assert_eq!(t.decide_floor(&ORDER, FLOOR_ONSET_MS), Some("a"));
        assert_eq!(t.floor(), Some("a"));
        let end = FLOOR_ONSET_MS + 100 + RING_OFF_MS;
        feed(&mut t, "a", 0.0, FLOOR_ONSET_MS + 100, end);
        assert_eq!(t.decide_floor(&ORDER, end), None);
        assert_eq!(t.last_held_floor_ms("a"), Some(FLOOR_ONSET_MS));
        assert_eq!(t.floor_history(), vec![("a", FLOOR_ONSET_MS)]);
    }

    #[test]
    fn the_floor_stays_with_whoever_holds_it_while_a_second_person_talks_over_them() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "b", LOUD, 0, FLOOR_ONSET_MS);
        assert_eq!(t.decide_floor(&ORDER, FLOOR_ONSET_MS), Some("b"));
        // `a` is earlier in the roster and now qualifies too; `b` keeps it.
        let end = 2 * FLOOR_ONSET_MS + 200;
        feed(&mut t, "a", LOUD, FLOOR_ONSET_MS, end);
        feed(&mut t, "b", LOUD, FLOOR_ONSET_MS, end);
        assert_eq!(t.decide_floor(&ORDER, end), Some("b"));
    }

    #[test]
    fn the_roster_order_breaks_a_tie() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "b", LOUD, 0, FLOOR_ONSET_MS);
        feed(&mut t, "a", LOUD, 0, FLOOR_ONSET_MS);
        assert_eq!(t.decide_floor(&ORDER, FLOOR_ONSET_MS), Some("a"));
        // The order is the caller's, not the ids'.
        let mut t = SpeakingTracker::new();
        feed(&mut t, "b", LOUD, 0, FLOOR_ONSET_MS);
        feed(&mut t, "a", LOUD, 0, FLOOR_ONSET_MS);
        assert_eq!(t.decide_floor(&["b", "a"], FLOOR_ONSET_MS), Some("b"));
    }

    /// The onset is measured on the speech itself.
    #[test]
    fn the_rings_tail_does_not_count_towards_the_onset() {
        for sound_ms in [800, 900] {
            let mut t = SpeakingTracker::new();
            feed(&mut t, "a", LOUD, 0, sound_ms);
            // Silence, sampled while the ring is still on.
            feed(&mut t, "a", 0.0, sound_ms + 100, 1600);
            assert!(t.is_speaking("a"), "the ring is still on");
            assert_eq!(t.decide_floor(&ORDER, 1600), None);
        }
    }

    #[test]
    fn the_holder_keeps_the_floor_through_a_pause_shorter_than_the_rings() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "a", LOUD, 0, FLOOR_ONSET_MS);
        assert_eq!(t.decide_floor(&ORDER, FLOOR_ONSET_MS), Some("a"));
        feed(&mut t, "a", 0.0, FLOOR_ONSET_MS + 100, FLOOR_ONSET_MS + 500);
        assert_eq!(t.decide_floor(&ORDER, FLOOR_ONSET_MS + 500), Some("a"));
    }

    #[test]
    fn a_holder_who_is_no_longer_in_the_order_gives_the_floor_up() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "a", LOUD, 0, FLOOR_ONSET_MS);
        assert_eq!(t.decide_floor(&ORDER, FLOOR_ONSET_MS), Some("a"));
        feed(&mut t, "a", LOUD, FLOOR_ONSET_MS, FLOOR_ONSET_MS + 300);
        assert_eq!(t.decide_floor(&["b"], FLOOR_ONSET_MS + 300), None);
        // Back in the order and still speaking: they have earned it.
        assert_eq!(t.decide_floor(&ORDER, FLOOR_ONSET_MS + 400), Some("a"));
    }

    #[test]
    fn someone_who_left_the_call_is_forgotten() {
        for by_roster in [true, false] {
            let mut t = SpeakingTracker::new();
            feed(&mut t, "a", LOUD, 0, FLOOR_ONSET_MS);
            feed(&mut t, "b", 0.002, 0, 3000);
            assert_eq!(t.decide_floor(&ORDER, FLOOR_ONSET_MS), Some("a"));
            let learned = t.threshold("b");
            if by_roster {
                t.keep_only(&["b"]);
            } else {
                t.forget("a");
            }
            assert_eq!(t.floor(), None, "given up at once");
            assert_eq!(t.decide_floor(&["b"], FLOOR_ONSET_MS + 100), None);
            assert_eq!(t.last_held_floor_ms("a"), None);
            assert!(t.floor_history().is_empty());
            assert!(t.speaking().is_empty());
            assert_eq!(t.tracked(), 1);
            close(t.threshold("a"), SPEAKING_LEVEL);
            close(t.threshold("b"), learned);
            // Forgetting someone never heard does nothing.
            t.forget("nobody");
        }
    }

    #[test]
    fn several_people_hand_the_floor_on_only_once_the_holder_has_stopped() {
        let order = ["a", "b", "c"];
        let mut t = SpeakingTracker::new();
        let mut now = 0;
        let mut run = |t: &mut SpeakingTracker, loud: &[&str], for_ms: u64| -> Option<String> {
            let until = now + for_ms;
            let mut active = None;
            while now <= until {
                for id in order {
                    let level = if loud.contains(&id) { LOUD } else { 0.0 };
                    t.sample(id, Some(level), now);
                }
                active = floor_of(t, &order, now);
                now += 100;
            }
            active
        };
        assert_eq!(run(&mut t, &["b"], 3000).as_deref(), Some("b"));
        // c joins in over b: the stage does not flip.
        assert_eq!(run(&mut t, &["b", "c"], 3000).as_deref(), Some("b"));
        // b stops; c has been speaking long enough and takes it.
        assert_eq!(run(&mut t, &["c"], RING_OFF_MS + 200).as_deref(), Some("c"));
        // A short pause inside c's speech does not drop the floor.
        assert_eq!(run(&mut t, &[], RING_OFF_MS - 300).as_deref(), Some("c"));
        assert_eq!(run(&mut t, &["c"], 500).as_deref(), Some("c"));
        // Everyone quiet: nobody holds it, and c is the most recent.
        assert_eq!(run(&mut t, &[], RING_OFF_MS + 200), None);
        assert!(t.last_held_floor_ms("c") > t.last_held_floor_ms("b"));
        assert!(t.last_held_floor_ms("b").is_some());
        assert_eq!(t.last_held_floor_ms("a"), None);
    }

    // ---- This device's own voice ---------------------------------------------

    /// Seen on a live five-person call: the large tile never moved to the
    /// person speaking. The roster starts with this device's own person, and
    /// its microphone took the floor and kept everyone else from it.
    #[test]
    fn the_own_voice_draws_its_ring_and_never_holds_the_floor_against_someone_else() {
        let order = ["me", "b", "c"];
        let mut t = SpeakingTracker::new();
        t.set_local(Some("me"));
        assert_eq!(t.local(), Some("me"));
        let mut now = 0;
        while now <= 10_000 {
            t.sample("me", Some(LOUD), now);
            t.sample("b", Some(0.0), now);
            t.sample("c", Some(LOUD), now);
            assert_ne!(t.decide_floor(&order, now), Some("me"));
            now += 100;
        }
        assert!(t.is_speaking("me"));
        assert_eq!(t.floor(), Some("c"));
        assert_eq!(t.floor_history(), vec![("c", 10_000)]);
    }

    #[test]
    fn the_floor_is_empty_when_only_this_device_speaks() {
        let mut t = SpeakingTracker::new();
        t.set_local(Some("me"));
        feed(&mut t, "me", LOUD, 0, 5000);
        assert_eq!(t.decide_floor(&["me", "b"], 5000), None);
        assert_eq!(t.speaking(), vec!["me"]);
        assert_eq!(t.last_held_floor_ms("me"), None);
    }

    /// Leaving a person out of the order keeps them from the floor as well:
    /// what the three clients did before the tracker knew the own person.
    #[test]
    fn someone_who_is_not_in_the_order_never_takes_the_floor() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "local", LOUD, 0, 2 * FLOOR_ONSET_MS);
        assert_eq!(t.decide_floor(&ORDER, 2 * FLOOR_ONSET_MS), None);
        assert_eq!(t.speaking(), vec!["local"], "the ring is for everyone");
        assert_eq!(t.last_held_floor_ms("local"), None);
    }

    #[test]
    fn someone_else_takes_the_floor_while_the_own_microphone_is_loud() {
        let order = ["me", "a", "b"];
        let mut t = SpeakingTracker::new();
        t.set_local(Some("me"));
        feed(&mut t, "me", LOUD, 0, 2 * FLOOR_ONSET_MS);
        assert_eq!(t.decide_floor(&order, 2 * FLOOR_ONSET_MS), None);
        feed(&mut t, "me", LOUD, 2 * FLOOR_ONSET_MS, 4 * FLOOR_ONSET_MS);
        feed(&mut t, "b", LOUD, 2 * FLOOR_ONSET_MS, 4 * FLOOR_ONSET_MS);
        assert_eq!(t.decide_floor(&order, 4 * FLOOR_ONSET_MS), Some("b"));
    }

    #[test]
    fn naming_the_own_person_takes_the_floor_from_them_at_once() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "a", LOUD, 0, FLOOR_ONSET_MS);
        assert_eq!(t.decide_floor(&ORDER, FLOOR_ONSET_MS), Some("a"));
        t.set_local(Some("a"));
        assert_eq!(t.floor(), None);
        assert_eq!(t.last_held_floor_ms("a"), None);
        assert!(t.is_speaking("a"));
        feed(&mut t, "a", LOUD, FLOOR_ONSET_MS, 2 * FLOOR_ONSET_MS);
        assert_eq!(t.decide_floor(&ORDER, 2 * FLOOR_ONSET_MS), None);
        // No own person again: they may hold it like anyone.
        t.set_local(None);
        assert_eq!(t.decide_floor(&ORDER, 2 * FLOOR_ONSET_MS), Some("a"));
    }

    // ---- A muted person ------------------------------------------------------

    #[test]
    fn a_muted_person_loses_the_ring_at_once_and_gives_up_the_floor() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "a", LOUD, 0, FLOOR_ONSET_MS);
        assert_eq!(t.decide_floor(&ORDER, FLOOR_ONSET_MS), Some("a"));
        t.silence("a");
        assert!(!t.is_speaking("a"));
        assert_eq!(t.floor(), None, "before the next decision too");
        assert_eq!(t.decide_floor(&ORDER, FLOOR_ONSET_MS + 10), None);
        // They held it, which is still remembered for the large tile.
        assert_eq!(t.last_held_floor_ms("a"), Some(FLOOR_ONSET_MS));
        // Silencing someone never sampled does nothing.
        t.silence("nobody");
        assert_eq!(t.tracked(), 1);
    }

    #[test]
    fn a_muted_person_has_to_earn_the_ring_again_after_unmuting() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "a", LOUD, 0, 1000);
        t.silence("a");
        t.sample("a", Some(LOUD), 1100);
        assert!(!t.is_speaking("a"));
        t.sample("a", Some(LOUD), 1100 + RING_ON_MS);
        assert!(t.is_speaking("a"));
        // And the floor: 1.5 s from the new start, not from the old one.
        assert_eq!(t.decide_floor(&ORDER, 1100 + RING_ON_MS), None);
        feed(&mut t, "a", LOUD, 1400, 1100 + FLOOR_ONSET_MS - 100);
        assert_eq!(t.decide_floor(&ORDER, 1100 + FLOOR_ONSET_MS - 100), None);
        t.sample("a", Some(LOUD), 1100 + FLOOR_ONSET_MS);
        assert_eq!(t.decide_floor(&ORDER, 1100 + FLOOR_ONSET_MS), Some("a"));
    }

    #[test]
    fn what_was_learned_about_the_room_is_kept_across_a_mute() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "a", 0.002, 0, 2900);
        let learned = t.threshold("a");
        close(learned, 0.006);
        t.silence("a");
        assert!(t.threshold("a").to_bits() == learned.to_bits());
    }

    // ---- What counts as sound ------------------------------------------------

    #[test]
    fn the_threshold_starts_at_the_fixed_level_before_anything_is_known() {
        assert!(NoiseGate::new().threshold().to_bits() == SPEAKING_LEVEL.to_bits());
        assert!(NoiseGate::default() == NoiseGate::new());
        let t = SpeakingTracker::new();
        assert!(t.threshold("nobody").to_bits() == SPEAKING_LEVEL.to_bits());
    }

    #[test]
    fn it_follows_a_quiet_room_down_to_a_bound() {
        for every in INTERVALS {
            let mut gate = NoiseGate::new();
            hold(&mut gate, 0.002, 0, 1000, every);
            close(gate.threshold(), 0.002 * OVER);
            hold(&mut gate, 0.0002, 1050, 2000, every);
            assert!(gate.threshold().to_bits() == SPEAKING_LEVEL_MIN.to_bits());
        }
    }

    #[test]
    fn one_torn_reading_does_not_empty_the_floor() {
        let mut gate = NoiseGate::new();
        hold(&mut gate, 0.006, 0, 1000, 100);
        let before = gate.threshold();
        // 100 ms after the last reading: half.
        gate.update(0.0002, 1000);
        close(gate.threshold(), before / 2.0);
    }

    #[test]
    fn the_first_reading_may_fall_as_far_as_100_ms_allows() {
        let mut gate = NoiseGate::new();
        gate.update(0.0002, 0);
        close(gate.threshold(), SPEAKING_LEVEL / 2.0);
        // And it does not rise: there is no time before it.
        let mut loud = NoiseGate::new();
        loud.update(0.9, 777);
        assert!(
            loud == {
                let mut same = NoiseGate::new();
                same.update(SPEAKING_LEVEL / OVER, 777);
                same
            }
        );
        assert!(loud.threshold().to_bits() == SPEAKING_LEVEL.to_bits());
    }

    #[test]
    fn it_learns_nothing_from_digital_silence_or_from_a_reading_that_is_not_a_number() {
        let mut gate = NoiseGate::new();
        hold(&mut gate, 0.01, 0, 20_000, 100);
        let before = gate.threshold();
        hold(&mut gate, 0.0, 20_000, 5000, 100);
        hold(&mut gate, NOISE_EVIDENCE_MIN / 2.0, 25_000, 5000, 100);
        gate.update(f64::NAN, 30_000);
        gate.update(f64::INFINITY, 30_100);
        gate.update(-0.5, 30_200);
        assert!(gate.threshold().to_bits() == before.to_bits());
        // The bound itself is evidence.
        gate.update(NOISE_EVIDENCE_MIN, 30_300);
        assert!(gate.threshold() < before);
    }

    #[test]
    fn it_climbs_slowly_towards_steady_sound_and_never_past_the_bound_or_the_sound_itself() {
        for every in INTERVALS {
            let mut gate = NoiseGate::new();
            // Three seconds of speech barely move it.
            hold(&mut gate, 0.2, 0, 3000, every);
            assert!(gate.threshold() < SPEAKING_LEVEL * 1.4);
            assert!(gate.threshold() > SPEAKING_LEVEL);
            // A minute of the same steady sound stops at the bound.
            hold(&mut gate, 0.2, 3000, 60_000, every);
            assert!(gate.threshold().to_bits() == SPEAKING_LEVEL_MAX.to_bits());
            // A steady hum settles the floor at the hum, not above it.
            let mut hum = NoiseGate::new();
            hold(&mut hum, 0.01, 0, 60_000, every);
            close(hum.threshold(), 0.01 * OVER);
        }
    }

    #[test]
    fn the_rise_over_a_stretch_of_time_does_not_depend_on_how_often_it_is_read() {
        let mut web = NoiseGate::new();
        let mut native = NoiseGate::new();
        hold(&mut web, 0.2, 0, 3001, 100);
        hold(&mut native, 0.2, 0, 3001, 150);
        // Both were last read at 3 000 ms.
        close(web.threshold(), native.threshold());
        close(web.threshold(), SPEAKING_LEVEL * 1.1_f64.powi(3));
    }

    #[test]
    fn a_long_gap_between_readings_is_not_a_long_rise() {
        let mut gate = NoiseGate::new();
        gate.update(0.2, 0);
        gate.update(0.2, 3_600_000);
        assert!(gate.threshold() < SPEAKING_LEVEL * 1.2);
        close(gate.threshold(), SPEAKING_LEVEL * 1.1);
        // Nor a long fall: one second's worth, 2^-10.
        let mut quiet = NoiseGate::new();
        hold(&mut quiet, 0.3, 0, 60_000, 100);
        quiet.update(0.0001, 3_600_000);
        assert!(
            quiet == {
                let mut same = NoiseGate::new();
                hold(&mut same, 0.3, 0, 60_000, 100);
                same.update(0.0001, 60_900);
                same.last_at_ms = Some(3_600_000);
                same
            }
        );
    }

    /// A clock that steps back counts as no time passed, never as a rise or
    /// a fall.
    #[test]
    fn a_clock_that_goes_back_changes_nothing() {
        let mut gate = NoiseGate::new();
        gate.update(0.2, 5000);
        let before = gate.threshold();
        gate.update(0.2, 1000);
        assert!(gate.threshold().to_bits() == before.to_bits());
        gate.update(0.0002, 500);
        assert!(gate.threshold().to_bits() == before.to_bits());
    }

    /// The fall is per unit of time, so 150 ms readings and 100 ms readings
    /// let the floor fall alike.
    #[test]
    fn the_fall_is_per_unit_of_time() {
        let mut gate = NoiseGate::new();
        // Long enough for the floor to reach the level itself.
        hold(&mut gate, 0.016, 0, 20_000, 100);
        let learned = gate.threshold();
        close(learned, 0.048);
        // 100 ms after the last reading: a half.
        gate.update(0.001, 20_000);
        close(gate.threshold(), learned / 2.0);
        // 150 ms later: one and a half times that.
        gate.update(0.001, 20_150);
        close(gate.threshold(), learned / 2.0 * 0.5_f64.powf(1.5));
    }

    /// Two readings at one instant: no time passed, nothing falls. (The web
    /// client's copy halved the floor at every reading, whatever the time.)
    #[test]
    fn no_time_no_fall() {
        let mut gate = NoiseGate::new();
        hold(&mut gate, 0.016, 0, 20_000, 100);
        gate.update(0.001, 20_000);
        let after = gate;
        gate.update(0.001, 20_000);
        assert!(gate == after);
    }

    #[test]
    fn the_floor_falls_by_the_same_factor_per_unit_of_time_at_150_ms_as_at_100() {
        let fallen_after = |every: u64, for_ms: u64| -> f64 {
            let mut gate = NoiseGate::new();
            // Learn a floor of 0.016 (threshold 0.048, inside the bounds).
            hold(&mut gate, 0.016, 0, 30_000, every);
            close(gate.threshold(), 0.048);
            // The first reading of the falling stretch comes `every` after
            // the last one of the learning stretch.
            let mut now = 30_000;
            while now - 30_000 < for_ms {
                gate.update(NOISE_EVIDENCE_MIN, now);
                now += every;
            }
            gate.threshold()
        };
        // One reading each: 0.5 at 100 ms, 0.5^1.5 (about 0.354) at 150 ms.
        close(fallen_after(100, 100), 0.048 * 0.5);
        close(fallen_after(150, 150), 0.048 * 0.5_f64.powf(1.5));
        // After 300 ms (three readings there, two here) both have fallen by
        // 0.5^3, and the result is still above the lower bound.
        close(fallen_after(100, 300), 0.048 * 0.125);
        close(fallen_after(100, 300), fallen_after(150, 300));
        assert!(fallen_after(150, 300) > SPEAKING_LEVEL_MIN);
    }

    #[test]
    fn a_reading_is_judged_against_the_threshold_as_it_stood_before_it() {
        // A level exactly at the starting threshold is sound. A second later
        // the same level is judged against the same threshold, and only then
        // lets the floor climb past it: judged after, it would be silence.
        let mut t = SpeakingTracker::new();
        t.sample("a", Some(SPEAKING_LEVEL), 0);
        t.sample("a", Some(SPEAKING_LEVEL), 1000);
        assert!(t.is_speaking("a"));
        close(t.threshold("a"), SPEAKING_LEVEL * 1.1);
        // Now it is under the threshold, and 800 ms of that turn the ring off.
        t.sample("a", Some(SPEAKING_LEVEL), 1100);
        t.sample("a", Some(SPEAKING_LEVEL), 1900);
        assert!(!t.is_speaking("a"));

        let mut gate = NoiseGate::new();
        assert!(gate.is_sound(SPEAKING_LEVEL));
        assert!(!gate.is_sound(SPEAKING_LEVEL * 0.999));
        assert!(!gate.is_sound(f64::NAN));
        gate.update(0.0002, 0);
        assert!(gate.is_sound(SPEAKING_LEVEL / 2.0));
    }

    // ---- One scale for a loud room and a quiet microphone --------------------

    /// The 2026-10-11 call: a phone whose speech peaked at 0.03 to 0.05.
    #[test]
    fn the_ring_is_drawn_for_speech_at_0_03_over_a_room_at_0_002() {
        for every in INTERVALS {
            let mut t = SpeakingTracker::new();
            let mut now = 0;
            while now < 3000 {
                t.sample("a", Some(0.002), now);
                now += every;
            }
            assert!(!t.is_speaking("a"));
            // 0.6 * 0.03 = 0.018 is under the old fixed 0.02 every third
            // reading.
            now = talk(&mut t, "a", 0.03, now, FLOOR_ONSET_MS + 500, every);
            assert!(t.is_speaking("a"));
            assert_eq!(t.decide_floor(&ORDER, now), Some("a"));
        }
    }

    #[test]
    fn it_is_not_drawn_for_a_room_that_idles_at_0_015_and_is_for_speech_over_that_room() {
        for every in INTERVALS {
            let mut t = SpeakingTracker::new();
            let mut now = 0;
            while now < 10_000 {
                t.sample("a", Some(0.015), now);
                assert!(!t.is_speaking("a"));
                now += every;
            }
            talk(&mut t, "a", 0.15, now, 1000, every);
            assert!(t.is_speaking("a"));
        }
    }

    #[test]
    fn a_steady_noise_that_came_on_stops_counting_within_seconds() {
        for every in INTERVALS {
            let mut t = SpeakingTracker::new();
            let mut now = 0;
            while now < 1000 {
                t.sample("a", Some(0.03), now);
                now += every;
            }
            assert!(t.is_speaking("a"));
            while now < 7000 {
                t.sample("a", Some(0.03), now);
                now += every;
            }
            assert!(!t.is_speaking("a"));
        }
    }

    #[test]
    fn a_level_that_could_not_be_read_is_silence_and_teaches_the_floor_nothing() {
        let mut t = SpeakingTracker::new();
        feed(&mut t, "a", LOUD, 0, 500);
        assert!(t.is_speaking("a"));
        let before = t.threshold("a");
        let mut now = 600;
        while now <= 600 + RING_OFF_MS {
            t.sample("a", None, now);
            now += 100;
        }
        assert!(!t.is_speaking("a"));
        assert!(t.threshold("a").to_bits() == before.to_bits());
        // Someone only ever unreadable is tracked, silent, at the start.
        t.sample("b", None, 0);
        assert_eq!(t.tracked(), 2);
        assert!(t.threshold("b").to_bits() == SPEAKING_LEVEL.to_bits());
    }

    /// A missing reading is not a reading for the gate's clock either: the
    /// time it covers counts towards the next real one, up to the cap.
    #[test]
    fn a_missing_reading_is_not_a_quiet_room() {
        let mut t = SpeakingTracker::new();
        let mut now = 0;
        while now <= 3000 {
            t.sample("a", None, now);
            now += 150;
        }
        assert!(t.threshold("a").to_bits() == SPEAKING_LEVEL.to_bits());
        // A quiet room, read: the threshold follows it down.
        while now <= 6000 {
            t.sample("a", Some(0.002), now);
            now += 150;
        }
        close(t.threshold("a"), 0.002 * OVER);
    }

    // ---- Bounds --------------------------------------------------------------

    #[test]
    fn people_beyond_the_cap_are_not_tracked_until_someone_is_forgotten() {
        let mut t = SpeakingTracker::new();
        let ids: Vec<String> = (0..=MAX_TRACKED_PEOPLE).map(|i| format!("p{i}")).collect();
        for now in [0, 200, 400, 1700] {
            for id in &ids {
                t.sample(id, Some(LOUD), now);
            }
        }
        assert_eq!(t.tracked(), MAX_TRACKED_PEOPLE);
        let (last, rest) = ids.split_last().unwrap();
        assert!(!t.is_speaking(last));
        assert!(rest.iter().all(|id| t.is_speaking(id)));
        assert_eq!(t.speaking().len(), MAX_TRACKED_PEOPLE);
        // The person beyond the cap cannot take the floor.
        assert_eq!(t.decide_floor(&[last.as_str()], 1700), None);
        t.forget("p0");
        t.sample(last, Some(LOUD), 1800);
        assert_eq!(t.tracked(), MAX_TRACKED_PEOPLE);
    }

    #[test]
    fn an_id_that_is_too_long_is_not_tracked() {
        let mut t = SpeakingTracker::new();
        let long = "x".repeat(MAX_PERSON_ID_BYTES + 1);
        let longest = "x".repeat(MAX_PERSON_ID_BYTES);
        feed(&mut t, &long, LOUD, 0, 2000);
        feed(&mut t, &longest, LOUD, 0, 2000);
        assert_eq!(t.speaking(), vec![longest.as_str()]);
        t.set_local(Some(&long));
        assert_eq!(t.local(), None);
        // The empty id is an id like any other.
        feed(&mut t, "", LOUD, 0, 2000);
        assert!(t.is_speaking(""));
    }

    // ---- Properties ----------------------------------------------------------

    const PEOPLE: [&str; 5] = ["me", "a", "b", "c", "d"];

    #[derive(Debug, Clone)]
    enum Op {
        Sample(usize, Option<f64>, u64),
        Silence(usize),
        Forget(usize),
        KeepOnly(Vec<usize>),
        SetLocal(Option<usize>),
        Decide(Vec<usize>, u64),
    }

    fn a_level() -> impl Strategy<Value = Option<f64>> {
        prop_oneof![
            3 => (0.0..0.3_f64).prop_map(Some),
            2 => Just(Some(0.1)),
            2 => Just(Some(0.0)),
            1 => Just(None),
            1 => any::<f64>().prop_map(Some),
            1 => prop_oneof![
                Just(f64::NAN),
                Just(f64::INFINITY),
                Just(f64::NEG_INFINITY),
                Just(-0.0),
                Just(f64::MIN_POSITIVE),
                Just(f64::MAX),
            ]
            .prop_map(Some),
        ]
    }

    fn a_time() -> impl Strategy<Value = u64> {
        prop_oneof![
            6 => 0..20_000_u64,
            1 => any::<u64>(),
            1 => Just(u64::MAX),
        ]
    }

    fn an_op() -> impl Strategy<Value = Op> {
        let person = 0..PEOPLE.len();
        let some = proptest::collection::vec(0..PEOPLE.len(), 0..7);
        prop_oneof![
            12 => (person.clone(), a_level(), a_time()).prop_map(|(p, l, t)| Op::Sample(p, l, t)),
            1 => person.clone().prop_map(Op::Silence),
            1 => person.clone().prop_map(Op::Forget),
            1 => some.clone().prop_map(Op::KeepOnly),
            1 => proptest::option::of(person).prop_map(Op::SetLocal),
            4 => (some, a_time()).prop_map(|(o, t)| Op::Decide(o, t)),
        ]
    }

    /// A call as a monitor drives it: everyone read every 100 ms on a clock
    /// that only goes forward, the floor decided after each round.
    fn a_call() -> impl Strategy<Value = Vec<[Option<f64>; 5]>> {
        let level = prop_oneof![
            4 => Just(Some(0.1)),
            3 => Just(Some(0.0)),
            1 => Just(None),
            1 => (0.0..0.2_f64).prop_map(Some),
        ];
        proptest::collection::vec(proptest::array::uniform5(level), 1..200)
    }

    proptest! {
        /// Whatever is done to a tracker, in any order and at any times: it
        /// does not panic, the own person never holds the floor, at most one
        /// person does and they are someone a decision could have chosen,
        /// every threshold stays within its bounds, and nothing is kept
        /// about anyone forgotten.
        #[test]
        fn any_sequence_is_total_and_the_own_microphone_never_holds_the_floor(
            ops in proptest::collection::vec(an_op(), 0..400)
        ) {
            let mut t = SpeakingTracker::new();
            let mut ever_held: BTreeSet<&str> = BTreeSet::new();
            for op in ops {
                match op {
                    Op::Sample(p, level, now) => t.sample(PEOPLE[p], level, now),
                    Op::Silence(p) => {
                        t.silence(PEOPLE[p]);
                        prop_assert!(!t.is_speaking(PEOPLE[p]));
                        prop_assert_ne!(t.floor(), Some(PEOPLE[p]));
                    }
                    Op::Forget(p) => {
                        t.forget(PEOPLE[p]);
                        ever_held.remove(PEOPLE[p]);
                        prop_assert!(!t.is_speaking(PEOPLE[p]));
                        prop_assert_ne!(t.floor(), Some(PEOPLE[p]));
                        prop_assert_eq!(t.last_held_floor_ms(PEOPLE[p]), None);
                    }
                    Op::KeepOnly(keep) => {
                        let keep: Vec<&str> = keep.into_iter().map(|p| PEOPLE[p]).collect();
                        t.keep_only(&keep);
                        ever_held.retain(|id| keep.contains(id));
                        for id in PEOPLE {
                            if !keep.contains(&id) {
                                prop_assert!(!t.is_speaking(id));
                                prop_assert_eq!(t.last_held_floor_ms(id), None);
                            }
                        }
                    }
                    Op::SetLocal(p) => {
                        t.set_local(p.map(|p| PEOPLE[p]));
                        if let Some(p) = p {
                            ever_held.remove(PEOPLE[p]);
                        }
                    }
                    Op::Decide(order, now) => {
                        let order: Vec<&str> = order.into_iter().map(|p| PEOPLE[p]).collect();
                        let holder = t.decide_floor(&order, now).map(str::to_owned);
                        prop_assert_eq!(holder.as_deref(), t.floor());
                        if let Some(holder) = holder.as_deref() {
                            prop_assert!(order.contains(&holder));
                            prop_assert!(t.is_speaking(holder));
                            prop_assert_eq!(t.last_held_floor_ms(holder), Some(now));
                            ever_held.insert(PEOPLE.iter().find(|id| **id == holder).unwrap());
                        }
                    }
                }
                // The own microphone never holds the floor.
                if let Some(local) = t.local() {
                    prop_assert_ne!(t.floor(), Some(local));
                    prop_assert_eq!(t.last_held_floor_ms(local), None);
                }
                // At most one holder, and a tracked one.
                if let Some(holder) = t.floor() {
                    prop_assert!(PEOPLE.contains(&holder));
                    prop_assert!(t.threshold(holder).is_finite());
                }
                // Only people who held the floor are remembered as having.
                let history = t.floor_history();
                for (id, at) in &history {
                    prop_assert!(ever_held.contains(id));
                    prop_assert_eq!(t.last_held_floor_ms(id), Some(*at));
                }
                prop_assert!(t.tracked() <= PEOPLE.len());
                prop_assert!(t.speaking().len() <= t.tracked());
                for id in PEOPLE {
                    let threshold = t.threshold(id);
                    prop_assert!((SPEAKING_LEVEL_MIN..=SPEAKING_LEVEL_MAX).contains(&threshold));
                }
            }
        }

        /// On a call driven as a monitor drives it, the floor has one holder
        /// at a time, never the own person, changes hands only when the
        /// holder's ring has gone off, and is only ever given to someone
        /// whose ring has been on for the whole onset.
        #[test]
        fn the_floor_has_one_holder_who_keeps_it_while_they_speak(rounds in a_call()) {
            let mut t = SpeakingTracker::new();
            t.set_local(Some("me"));
            let mut previous: Option<String> = None;
            let mut ring_on_since: BTreeMap<&str, u64> = BTreeMap::new();
            let mut now = 0;
            for levels in rounds {
                for (id, level) in PEOPLE.iter().zip(levels) {
                    t.sample(id, level, now);
                    if t.is_speaking(id) {
                        ring_on_since.entry(id).or_insert(now);
                    } else {
                        ring_on_since.remove(id);
                    }
                }
                let holder = t.decide_floor(&PEOPLE, now).map(str::to_owned);
                prop_assert_ne!(holder.as_deref(), Some("me"));
                let holders = PEOPLE
                    .iter()
                    .filter(|id| t.last_held_floor_ms(id) == Some(now))
                    .count();
                prop_assert_eq!(holders, usize::from(holder.is_some()));
                if let Some(was) = previous.as_deref() {
                    if t.is_speaking(was) {
                        prop_assert_eq!(holder.as_deref(), Some(was));
                    }
                }
                if let Some(is) = holder.as_deref() {
                    if previous.as_deref() != Some(is) {
                        // The ring came on 200 ms into the sound, so it has
                        // been on for the onset less that.
                        let since = ring_on_since.get(is).copied();
                        prop_assert!(
                            since.is_some_and(|s| now - s >= FLOOR_ONSET_MS - RING_ON_MS)
                        );
                    }
                }
                previous = holder;
                now += 100;
            }
        }

        /// A gate never panics, its floor never reaches zero and its
        /// threshold stays within the bounds, whatever it is fed.
        #[test]
        fn a_gate_is_total_and_bounded(
            readings in proptest::collection::vec((any::<f64>(), a_time()), 0..300)
        ) {
            let mut gate = NoiseGate::new();
            for (level, now) in readings {
                let before = gate;
                gate.update(level, now);
                prop_assert!(gate.floor >= EVIDENCE_UNITS.min(START_FLOOR_UNITS));
                prop_assert!(gate.floor <= LEVEL_UNITS_PER_ONE);
                let threshold = gate.threshold();
                prop_assert!((SPEAKING_LEVEL_MIN..=SPEAKING_LEVEL_MAX).contains(&threshold));
                if !level.is_finite() || level < NOISE_EVIDENCE_MIN {
                    prop_assert_eq!(gate.floor, before.floor);
                }
                let _ = gate.is_sound(level);
            }
        }

        /// The fall over a stretch of time does not depend on how it is cut
        /// into readings, to well within anything a threshold could notice.
        #[test]
        fn the_fall_does_not_depend_on_how_the_time_is_cut(
            cuts in proptest::collection::vec(1..400_u64, 1..12)
        ) {
            let total: u64 = cuts.iter().sum();
            prop_assume!(total <= NOISE_ELAPSED_MAX_MS);
            let start = LEVEL_UNITS_PER_ONE / 3;
            let pieces = cuts.iter().fold(start, |floor, ms| fallen(floor, *ms));
            let whole = fallen(start, total);
            prop_assert!(pieces.abs_diff(whole) <= 64, "{pieces} {whole}");
            let pieces = cuts.iter().fold(start, |floor, ms| risen(floor, *ms));
            let whole = risen(start, total);
            prop_assert!(pieces.abs_diff(whole) <= 64, "{pieces} {whole}");
        }
    }
}
