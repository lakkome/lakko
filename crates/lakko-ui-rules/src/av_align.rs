// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Keeping a remote person's sound and picture together in a group call.
//!
//! ADR-0070, note of 2026-10-12. A subscriber gets, per remote person, one
//! audio track and up to three video tracks: main (stream id `<peer>`),
//! middle (`<peer>~mid`) and light (`<peer>~light`). The media engine's own
//! audio/video synchronisation covers ONE video stream per sync group, the
//! first; main shares the audio's stream id and the other two do not, so
//! only main is synchronised by the engine. Measured on a live call on
//! 2026-10-11 in Chrome: the audio jitter buffer sat at 320 to 600 ms, main
//! followed it, and light sat at the server's 290 ms floor with nothing
//! tying it to the audio, so the picture ran 100 to 300 ms ahead of the
//! sound.
//!
//! This module is the rule that ties them, written once because three
//! clients must agree on it. One [`AvAlign`] per remote person. Once per
//! reading interval ([`AV_READING_MS`]) the client hands it what its own
//! receivers report and gets back the target playout delay to set on each
//! receiver. The engine executes; Lakko's code decides (ADR-0070's rule).
//!
//! Why it is in this crate: it decides something a person sees and hears,
//! it has no platform in it, and nothing in it is security-relevant. Every
//! input is this device's own measurement of its own receivers; there is no
//! key, no secret and no peer's assertion.
//!
//! # The rule
//!
//! **The sound is the reference.** A person notices a conversation delay
//! before a lip-sync error, and the audio's delay is set by its own network
//! path, so the picture is moved to the sound and not the other way round.
//!
//! - A video track the engine synchronises (main) is observed and never
//!   written: [`AvVideoReason::EngineSynced`].
//! - A video track the engine does not synchronise **follows the sound**:
//!   its target is the audio's measured jitter-buffer delay. A target is a
//!   hint to the engine (a minimum; the engine uses the larger of it, the
//!   network's need and the server's floor), so it is corrected by what is
//!   measured: once the track has settled, half of the remaining skew is
//!   added to a trim, bounded by [`AV_TRIM_MAX_MS`], until the skew is
//!   inside [`AV_DEAD_BAND_MS`].
//! - A target moves by at most [`AV_RAISE_PER_S_MS`] upwards and
//!   [`AV_LOWER_PER_S_MS`] downwards per second, and elapsed time counts for
//!   at most [`AV_MAX_ELAPSED_MS`] per reading. Quicker to raise than to
//!   lower: raising makes an early picture wait, which nobody sees happen;
//!   lowering makes the engine play fast to catch up, which is seen.
//! - A shown, unsynchronised track that stays **later than the sound**
//!   although its target is as low as the trim allows is held there by
//!   something else: its own network path, or the server's playout-delay
//!   floor. Then **the sound is asked to wait for it**, by at most
//!   [`AV_SOUND_WAIT_MAX_MS`] above what the audio needs for itself.
//!   Beyond that bound the picture is left late
//!   ([`AvVideoReason::PictureLate`]) rather than the sound delayed
//!   further: a bad picture path must not be able to drag the conversation
//!   delay without limit, and a late picture costs less than a late answer.
//!   While the sound waits for a track, that track's target is the audio's
//!   OWN need and not the audio's measured delay; otherwise each would hold
//!   the other up for the rest of the call.
//! - A track that is not flowing is left alone and, when it flows again,
//!   starts at the audio's delay, not at its stale value. A reading that is
//!   missing changes nothing. A gap between readings longer than
//!   [`AV_STALE_MS`] starts every track again.
//! - An engine that does not follow a target is noticed
//!   ([`AV_IGNORED_READINGS`]) and the target stops being corrected, so
//!   nothing winds up and nothing oscillates:
//!   [`AvVideoReason::HintIgnored`], [`AvAudioReason::HintIgnored`].
//!
//! Skew is the video track's measured delay minus the audio's. That is an
//! approximation of lip sync: it assumes the two travelled the same path
//! (they do: one peer connection to one SFU) and that decoding and
//! rendering cost about the same.
//!
//! Everything is integer milliseconds, saturating, with no clock of its
//! own; no input can panic; the state is a fixed handful of numbers.
//!
//! **Every constant below was chosen, not measured.** Nothing here has been
//! seen on a call.

/// How often a client is expected to read its receivers and call
/// [`AvAlign::read`], milliseconds. Chosen: the engine's own statistics are
/// averages, and the jitter buffer moves over seconds, so a faster reading
/// would only read noise. Any interval from about half of this to
/// [`AV_MAX_ELAPSED_MS`] works; the counts of readings below then take
/// proportionally shorter or longer.
pub const AV_READING_MS: u64 = 1000;

/// The skew, either way, inside which a track is left as it is,
/// milliseconds. Chosen to be the engine's own (`kMinDeltaMs` in
/// libwebrtc's `stream_synchronization.cc`): asking for finer than the
/// engine itself holds its synchronised track would only move targets.
pub const AV_DEAD_BAND_MS: u32 = 30;

/// A target is changed only when what is wanted differs from it by more
/// than this, milliseconds. Chosen: half the dead band, so the noise of one
/// reading of the audio's delay does not move a video target every second.
pub const AV_MOVE_MS: u32 = 15;

/// The most a target is raised per second, milliseconds. Chosen: three of
/// the engine's own 80 ms steps (`kMaxChangeMs`), so the measured case of a
/// picture 530 ms ahead of the sound is closed in a few seconds.
pub const AV_RAISE_PER_S_MS: u32 = 240;

/// The most a target is lowered per second, milliseconds. Chosen: one of
/// the engine's 80 ms steps; a receiver shortens its delay by playing fast,
/// and faster than this would be seen.
pub const AV_LOWER_PER_S_MS: u32 = 80;

/// The longest time between two readings that is counted in full,
/// milliseconds: a later reading moves a target by no more than this much
/// time allows. Chosen: the web client's two-second statistics interval.
pub const AV_MAX_ELAPSED_MS: u64 = 2000;

/// A gap between two readings longer than this starts every track again,
/// as if it had just begun to flow, milliseconds. Chosen: a page or an app
/// that was in the background knows nothing about its receivers.
pub const AV_STALE_MS: u64 = 5000;

/// The most the measured skew may add to or take from a video target on
/// top of the audio's delay, and add to an audio target, milliseconds.
/// Chosen: three dead bands, enough for an engine that plays a little above
/// or below what it was asked, and small enough that a target can never
/// wander far from what it follows.
pub const AV_TRIM_MAX_MS: u32 = 90;

/// A target must have stood unchanged for this many readings, and the
/// track's measured delay must have stopped moving ([`AV_MOVING_MS`]),
/// before the skew is taken as the engine's answer and not as its lag.
/// Chosen.
pub const AV_SETTLED_READINGS: u32 = 2;

/// A measured delay that changed by more than this since the last reading
/// is still moving, milliseconds. Chosen: half the dead band.
pub const AV_MOVING_MS: u32 = 15;

/// For this many settled readings a shown track must stay later than the
/// sound, with its target as low as the trim allows (or with a reported
/// network need above the sound's delay), before the sound is asked to
/// wait for it. Chosen: delaying the conversation is the costly step, so it
/// is not taken on one reading.
pub const AV_HELD_READINGS: u32 = 2;

/// For this many readings a track the sound waits for must be found no
/// longer above its target before the sound stops waiting. Chosen.
pub const AV_RELEASE_READINGS: u32 = 2;

/// For this many settled readings a target must sit at the trim's limit
/// with the skew still outside the dead band before the engine is taken to
/// ignore the target. Chosen.
pub const AV_IGNORED_READINGS: u32 = 3;

/// The most the sound is asked to wait for a picture, above what the audio
/// needs for itself, milliseconds. Chosen: the server's playout-delay floor
/// after a burst (`BURST_DELAY_MS` in the SFU, 300 ms), so a quiet audio
/// path can always wait for a light track held at that floor, and never
/// for more than a third of a second.
pub const AV_SOUND_WAIT_MAX_MS: u32 = 300;

/// The largest target ever returned, milliseconds. Chosen: twice the
/// largest audio delay measured on the call that prompted this (990 ms).
pub const AV_TARGET_MAX_MS: u32 = 2000;

/// After this many readings in a row without a measured audio delay the
/// video targets are given back (set to 0), since there is no sound to
/// keep the picture with. Chosen.
pub const AV_NO_SOUND_READINGS: u32 = 5;

/// A measured delay above this is read as this, milliseconds. Chosen: far
/// above anything a receiver reports, and it keeps every sum small.
pub const AV_READING_MAX_MS: u32 = 10_000;

/// What the audio receiver of one remote person reports for one interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AvAudioReading {
    /// Packets arrived in the interval.
    pub flowing: bool,
    /// Average time a sample spent in the jitter buffer over the interval,
    /// milliseconds; `None` when nothing left the buffer or the platform
    /// reports none.
    pub delay_ms: Option<u32>,
    /// The delay the network alone would need, milliseconds, where the
    /// platform reports it.
    pub network_ms: Option<u32>,
}

/// What one video receiver of one remote person reports for one interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AvVideoReading {
    /// Packets arrived in the interval. A track the person does not have,
    /// or one the SFU has paused, is not flowing.
    pub flowing: bool,
    /// A tile is drawing this track now. Only a shown track can make the
    /// sound wait.
    pub shown: bool,
    /// The media engine synchronises this track with the audio itself (the
    /// main track, which shares the audio's stream id).
    pub engine_synced: bool,
    /// Average time a frame spent in the jitter buffer over the interval,
    /// milliseconds; `None` when no frame left it or the platform reports
    /// none.
    pub delay_ms: Option<u32>,
    /// The delay the network alone would need, milliseconds, where the
    /// platform reports it.
    pub network_ms: Option<u32>,
}

impl AvVideoReading {
    /// A track that is not there.
    pub const ABSENT: Self = Self {
        flowing: false,
        shown: false,
        engine_synced: false,
        delay_ms: None,
        network_ms: None,
    };
}

/// One interval's readings for one remote person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AvReadings {
    /// The audio receiver.
    pub audio: AvAudioReading,
    /// The main video track (stream id `<peer>`).
    pub main: AvVideoReading,
    /// The middle video track (`<peer>~mid`).
    pub middle: AvVideoReading,
    /// The light video track (`<peer>~light`).
    pub light: AvVideoReading,
}

/// Why the audio receiver's target is what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvAudioReason {
    /// No audio packets arrived; nothing is changed.
    NotFlowing,
    /// The audio flows but reported no delay; nothing is changed.
    NoReading,
    /// This device cannot set an audio receiver's delay.
    NoSetter,
    /// Nothing is asked of the audio.
    Free,
    /// The sound waits for a picture that needs more delay than it has.
    WaitingForPicture,
    /// The sound waits as long as it may ([`AV_SOUND_WAIT_MAX_MS`]); the
    /// picture needs more and is left late.
    WaitBounded,
    /// No picture needs the sound to wait any more; the target is coming
    /// down.
    Releasing,
    /// The engine does not follow the audio target; it is no longer
    /// corrected.
    HintIgnored,
}

/// Why a video receiver's target is what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvVideoReason {
    /// No packets arrived; the track is left alone.
    NotFlowing,
    /// The track flows but reported no delay; nothing is changed.
    NoReading,
    /// There is no measured audio delay to follow; nothing is changed, or,
    /// after [`AV_NO_SOUND_READINGS`], the target is given back.
    NoSound,
    /// This device cannot set a video receiver's delay.
    NoSetter,
    /// The engine synchronises this track itself; it is only observed.
    EngineSynced,
    /// The track began to flow (or the readings began again): it starts at
    /// the audio's delay.
    Started,
    /// The skew is inside the dead band.
    Aligned,
    /// The target is being raised: the picture waits for the sound.
    Raising,
    /// The target is being lowered towards the sound.
    Lowering,
    /// The target stands and the receiver has not answered it yet.
    Settling,
    /// The engine does not follow the target; the picture stays early.
    HintIgnored,
    /// The track needs more delay than the sound has; the sound waits.
    SoundWaits,
    /// The track is later than the sound and stays so: the sound may not
    /// wait (longer), cannot be delayed on this device, or the track is not
    /// shown.
    PictureLate,
}

impl AvAudioReason {
    /// The word a diagnostic line uses for this reason, the same on every
    /// client.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NotFlowing => "not-flowing",
            Self::NoReading => "no-reading",
            Self::NoSetter => "no-setter",
            Self::Free => "free",
            Self::WaitingForPicture => "waiting-for-picture",
            Self::WaitBounded => "wait-bounded",
            Self::Releasing => "releasing",
            Self::HintIgnored => "hint-ignored",
        }
    }
}

impl AvVideoReason {
    /// The word a diagnostic line uses for this reason, the same on every
    /// client.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NotFlowing => "not-flowing",
            Self::NoReading => "no-reading",
            Self::NoSound => "no-sound",
            Self::NoSetter => "no-setter",
            Self::EngineSynced => "engine-synced",
            Self::Started => "started",
            Self::Aligned => "aligned",
            Self::Raising => "raising",
            Self::Lowering => "lowering",
            Self::Settling => "settling",
            Self::HintIgnored => "hint-ignored",
            Self::SoundWaits => "sound-waits",
            Self::PictureLate => "picture-late",
        }
    }
}

/// What to do with the audio receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvAudioPlan {
    /// The target delay to set on the receiver now, milliseconds; `None`
    /// leaves it alone. `0` gives the target back (on the web:
    /// `jitterBufferTarget = null`).
    pub set_ms: Option<u32>,
    /// What the receiver was last told, milliseconds; `0` when nothing.
    pub asked_ms: u32,
    /// Why.
    pub reason: AvAudioReason,
}

/// What to do with one video receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvVideoPlan {
    /// The target delay to set on the receiver now, milliseconds; `None`
    /// leaves it alone. `0` gives the target back.
    pub set_ms: Option<u32>,
    /// What the receiver was last told, milliseconds; `0` when nothing.
    pub asked_ms: u32,
    /// The track's measured delay minus the audio's, milliseconds: positive
    /// when the picture is later than the sound. `None` when either was not
    /// measured in this interval.
    pub skew_ms: Option<i32>,
    /// Why.
    pub reason: AvVideoReason,
}

/// One interval's answer for one remote person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvPlan {
    /// The audio receiver.
    pub audio: AvAudioPlan,
    /// The main video track.
    pub main: AvVideoPlan,
    /// The middle video track.
    pub middle: AvVideoPlan,
    /// The light video track.
    pub light: AvVideoPlan,
}

/// What the rule can achieve on a device, from which receivers' delays the
/// device can set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvCapability {
    /// Both can be set: an early picture waits for the sound, and the sound
    /// waits (within its bound) for a picture that needs more.
    Full,
    /// Only video can be set: an early picture waits for the sound, but the
    /// audio cannot be delayed here, so a picture that needs more delay
    /// than the sound has stays late.
    PictureWaitsOnly,
    /// Only audio can be set: the sound waits for a late picture, but a
    /// picture that runs ahead of the sound cannot be held back here.
    SoundWaitsOnly,
    /// Neither can be set: the skew is measured and nothing is done about
    /// it.
    ObserveOnly,
}

impl AvCapability {
    /// The word a diagnostic line uses for this, the same on every client.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::PictureWaitsOnly => "picture-waits-only",
            Self::SoundWaitsOnly => "sound-waits-only",
            Self::ObserveOnly => "observe-only",
        }
    }
}

/// What the rule can and cannot achieve with the setters a device has.
#[must_use]
pub const fn av_align_capability(audio_settable: bool, video_settable: bool) -> AvCapability {
    match (audio_settable, video_settable) {
        (true, true) => AvCapability::Full,
        (false, true) => AvCapability::PictureWaitsOnly,
        (true, false) => AvCapability::SoundWaitsOnly,
        (false, false) => AvCapability::ObserveOnly,
    }
}

const BAND: i64 = AV_DEAD_BAND_MS as i64;
const MOVE: i64 = AV_MOVE_MS as i64;
const TRIM_MAX: i64 = AV_TRIM_MAX_MS as i64;
const MOVING: i64 = AV_MOVING_MS as i64;
const TARGET_MAX: i64 = AV_TARGET_MAX_MS as i64;
const WAIT_MAX: i64 = AV_SOUND_WAIT_MAX_MS as i64;

/// A reading as the arithmetic uses it: bounded, signed.
fn bounded(ms: u32) -> i64 {
    i64::from(ms.min(AV_READING_MAX_MS))
}

/// A target as it is returned. Targets are kept inside `0..=TARGET_MAX`.
fn as_target(ms: i64) -> u32 {
    u32::try_from(ms.clamp(0, TARGET_MAX)).unwrap_or(AV_TARGET_MAX_MS)
}

/// A skew as it is returned. Both readings are bounded, so it fits.
fn as_skew(ms: i64) -> i32 {
    i32::try_from(ms).unwrap_or(if ms < 0 { i32::MIN } else { i32::MAX })
}

/// How far a target may move in one reading.
#[derive(Debug, Clone, Copy)]
struct Step {
    up: i64,
    down: i64,
}

impl Step {
    fn for_elapsed(elapsed_ms: u64) -> Self {
        let ms = i64::try_from(elapsed_ms.min(AV_MAX_ELAPSED_MS)).unwrap_or(0);
        Self {
            up: i64::from(AV_RAISE_PER_S_MS) * ms / 1000,
            down: i64::from(AV_LOWER_PER_S_MS) * ms / 1000,
        }
    }

    /// `from` moved towards `want` by no more than the step allows.
    fn towards(self, from: i64, want: i64) -> i64 {
        from + (want - from).clamp(-self.down, self.up)
    }
}

/// What is remembered about one receiver's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    /// The target the rule holds, milliseconds.
    target: i64,
    /// What the receiver was last told; `None` when never.
    told: Option<u32>,
    /// The correction the measured skew has added.
    trim: i64,
    /// Readings since the target last moved.
    steady: u32,
    /// The measured delay at the last reading that had one.
    prev_delay: Option<i64>,
    /// Settled readings at the trim's limit with the skew outside the band.
    pinned: u32,
    /// The engine does not follow the target.
    ignored: bool,
}

impl Held {
    const NEW: Self = Self {
        target: 0,
        told: None,
        trim: 0,
        steady: 0,
        prev_delay: None,
        pinned: 0,
        ignored: false,
    };

    /// Forget everything but what the receiver was told.
    const fn forget(&mut self) {
        let told = self.told;
        *self = Self::NEW;
        self.told = told;
    }

    /// The target has stood long enough and the measured delay has stopped
    /// moving: the skew now is the engine's answer.
    fn settled(&self, delay: i64) -> bool {
        self.steady >= AV_SETTLED_READINGS
            && self
                .prev_delay
                .is_some_and(|prev| (delay - prev).abs() <= MOVING)
    }

    /// Move the target towards `want` when it is far enough away. Returns
    /// the direction moved: 1 up, -1 down, 0 not at all.
    fn slew(&mut self, want: i64, step: Step) -> i8 {
        let want = want.clamp(0, TARGET_MAX);
        if (want - self.target).abs() <= MOVE {
            self.steady = self.steady.saturating_add(1);
            return 0;
        }
        self.steady = 0;
        let up = want > self.target;
        self.target = step.towards(self.target, want);
        if up {
            1
        } else {
            -1
        }
    }

    /// What to tell the receiver so that it holds `desired`: `Some` only
    /// when that differs from what it was last told and it can be told.
    fn tell(&mut self, desired: i64, settable: bool) -> Option<u32> {
        if !settable {
            return None;
        }
        let desired = as_target(desired);
        if self.told.unwrap_or(0) == desired {
            return None;
        }
        self.told = Some(desired);
        Some(desired)
    }

    fn asked(&self) -> u32 {
        self.told.unwrap_or(0)
    }
}

/// One video track's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VideoState {
    held: Held,
    /// The track flows and has a target of its own.
    active: bool,
    /// Settled readings on which the track was found to need more than the
    /// sound has.
    needs_more: u32,
    /// The sound waits for this track.
    sound_waits: bool,
    /// Readings on which a track the sound waits for was no longer above
    /// its target.
    calm: u32,
    /// The track is later than the sound and nothing more is done.
    late: bool,
    /// The delay this track needs, as found at the last reading, while the
    /// sound waits for it.
    need: i64,
}

impl VideoState {
    const NEW: Self = Self {
        held: Held::NEW,
        active: false,
        needs_more: 0,
        sound_waits: false,
        calm: 0,
        late: false,
        need: 0,
    };

    const fn forget(&mut self) {
        let told = self.held.told;
        *self = Self::NEW;
        self.held.told = told;
    }
}

/// The rule's state for one remote person: three video tracks and the
/// audio. See the module documentation for the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvAlign {
    audio_settable: bool,
    video_settable: bool,
    /// When the last reading was taken; `None` before the first.
    last_ms: Option<u64>,
    audio: Held,
    /// What the audio needs for itself, milliseconds.
    own: i64,
    /// Readings in a row without a measured audio delay.
    no_sound: u32,
    video: [VideoState; 3],
}

impl AvAlign {
    /// A remote person's alignment on a device that can (or cannot) set the
    /// target delay of an audio receiver and of a video receiver.
    #[must_use]
    pub const fn new(audio_settable: bool, video_settable: bool) -> Self {
        Self {
            audio_settable,
            video_settable,
            last_ms: None,
            audio: Held::NEW,
            own: 0,
            no_sound: 0,
            video: [VideoState::NEW; 3],
        }
    }

    /// What the rule can achieve on this device.
    #[must_use]
    pub const fn capability(&self) -> AvCapability {
        av_align_capability(self.audio_settable, self.video_settable)
    }

    /// The receivers were replaced (a new peer connection, a renegotiation
    /// that made new receivers): nothing has been told to them. The next
    /// reading starts every track again and tells every receiver its
    /// target.
    pub const fn receivers_replaced(&mut self) {
        *self = Self::new(self.audio_settable, self.video_settable);
    }

    /// One interval's readings, taken at `now_ms` (one clock, whole
    /// milliseconds, any origin; a time earlier than the last one is read
    /// as the last one). Returns what to set on each receiver.
    pub fn read(&mut self, now_ms: u64, readings: &AvReadings) -> AvPlan {
        let step = self.advance(now_ms);
        let videos = [readings.main, readings.middle, readings.light];
        let audio_delay = if readings.audio.flowing {
            readings.audio.delay_ms.map(bounded)
        } else {
            None
        };
        let Some(sound) = audio_delay else {
            return self.without_sound(readings, &videos);
        };
        self.no_sound = 0;
        // What the audio needs for itself: what the platform says the
        // network needs, else what it measures while nothing is asked of
        // it, else what it measured before something was.
        if let Some(network) = readings.audio.network_ms {
            self.own = bounded(network);
        } else if self.audio.target == 0 {
            self.own = sound;
        }
        let own = self.own;
        let asking = self.audio.target > 0;

        let mut plans = [Self::video_plan(None, 0, None, AvVideoReason::NotFlowing); 3];
        let mut need: Option<i64> = None;
        for ((state, reading), plan) in self.video.iter_mut().zip(videos).zip(plans.iter_mut()) {
            *plan = Self::read_video(
                state,
                reading,
                sound,
                own,
                step,
                self.audio_settable,
                self.video_settable,
            );
            // What the sound is asked to wait for.
            if state.sound_waits {
                need = Some(need.map_or(state.need, |n| n.max(state.need)));
            } else if reading.flowing && reading.shown && reading.engine_synced {
                // A synchronised track's measured delay follows the sound,
                // so only what its network needs counts.
                if let Some(network) = reading.network_ms.map(bounded) {
                    if network > own + BAND || (asking && network > own) {
                        need = Some(need.map_or(network, |n| n.max(network)));
                    }
                }
            }
        }

        let (audio, bounded_wait) = self.read_audio(sound, own, need, step);
        if bounded_wait || audio.reason == AvAudioReason::HintIgnored {
            for (state, plan) in self.video.iter().zip(plans.iter_mut()) {
                if state.sound_waits && plan.skew_ms.is_some_and(|skew| i64::from(skew) > BAND) {
                    plan.reason = AvVideoReason::PictureLate;
                }
            }
        }
        self.audio.prev_delay = Some(sound);
        let [main, middle, light] = plans;
        AvPlan {
            audio,
            main,
            middle,
            light,
        }
    }

    /// Move the clock and say how far a target may move. A stale gap
    /// starts every track again.
    fn advance(&mut self, now_ms: u64) -> Step {
        let Some(last) = self.last_ms else {
            self.last_ms = Some(now_ms);
            return Step::for_elapsed(0);
        };
        let now = now_ms.max(last);
        let elapsed = now - last;
        self.last_ms = Some(now);
        if elapsed > AV_STALE_MS {
            for state in &mut self.video {
                state.forget();
            }
            self.audio.forget();
            self.no_sound = 0;
            return Step::for_elapsed(0);
        }
        Step::for_elapsed(elapsed)
    }

    const fn video_plan(
        set_ms: Option<u32>,
        asked_ms: u32,
        skew_ms: Option<i32>,
        reason: AvVideoReason,
    ) -> AvVideoPlan {
        AvVideoPlan {
            set_ms,
            asked_ms,
            skew_ms,
            reason,
        }
    }

    /// A reading with no measured audio delay: nothing to follow. Nothing
    /// is changed until that has lasted [`AV_NO_SOUND_READINGS`]; then every
    /// target is given back.
    fn without_sound(&mut self, readings: &AvReadings, videos: &[AvVideoReading; 3]) -> AvPlan {
        self.no_sound = self.no_sound.saturating_add(1);
        let gone = self.no_sound >= AV_NO_SOUND_READINGS;
        let audio_settable = self.audio_settable;
        let video_settable = self.video_settable;
        let mut plans = [Self::video_plan(None, 0, None, AvVideoReason::NoSound); 3];
        for ((state, reading), plan) in self.video.iter_mut().zip(videos).zip(plans.iter_mut()) {
            let mut set_ms = None;
            if gone {
                state.forget();
                if reading.flowing {
                    set_ms = state.held.tell(0, video_settable);
                }
            }
            let reason = if !reading.flowing {
                state.forget();
                AvVideoReason::NotFlowing
            } else if reading.engine_synced {
                AvVideoReason::EngineSynced
            } else {
                AvVideoReason::NoSound
            };
            *plan = Self::video_plan(set_ms, state.held.asked(), None, reason);
        }
        let mut set_ms = None;
        if gone {
            self.audio.forget();
            set_ms = self.audio.tell(0, audio_settable);
        }
        let reason = if !readings.audio.flowing {
            AvAudioReason::NotFlowing
        } else if !audio_settable {
            AvAudioReason::NoSetter
        } else {
            AvAudioReason::NoReading
        };
        let [main, middle, light] = plans;
        AvPlan {
            audio: AvAudioPlan {
                set_ms,
                asked_ms: self.audio.asked(),
                reason,
            },
            main,
            middle,
            light,
        }
    }

    /// One video track against a measured audio delay of `sound`.
    fn read_video(
        state: &mut VideoState,
        reading: AvVideoReading,
        sound: i64,
        own: i64,
        step: Step,
        audio_settable: bool,
        video_settable: bool,
    ) -> AvVideoPlan {
        if !reading.flowing {
            state.forget();
            return Self::video_plan(None, state.held.asked(), None, AvVideoReason::NotFlowing);
        }
        let delay = reading.delay_ms.map(bounded);
        let skew = delay.map(|d| as_skew(d - sound));
        if reading.engine_synced {
            // Never written; a target left from before it was synchronised
            // is given back.
            state.forget();
            let set_ms = state.held.tell(0, video_settable);
            return Self::video_plan(
                set_ms,
                state.held.asked(),
                skew,
                AvVideoReason::EngineSynced,
            );
        }
        let Some(delay) = delay else {
            return Self::video_plan(None, state.held.asked(), None, AvVideoReason::NoReading);
        };
        let reason = if state.active {
            Self::follow(state, reading, delay, sound, own, step, audio_settable)
        } else {
            // Starts at the sound's delay, not at a stale value.
            state.active = true;
            state.held.target = sound.clamp(0, TARGET_MAX);
            AvVideoReason::Started
        };
        state.held.prev_delay = Some(delay);
        let set_ms = state.held.tell(state.held.target, video_settable);
        let reason = if video_settable || reason == AvVideoReason::SoundWaits {
            reason
        } else if skew.is_some_and(|s| i64::from(s).abs() <= BAND) {
            AvVideoReason::Aligned
        } else if reason == AvVideoReason::PictureLate {
            reason
        } else {
            AvVideoReason::NoSetter
        };
        Self::video_plan(set_ms, state.held.asked(), skew, reason)
    }

    /// An active, unsynchronised track with a measured delay.
    fn follow(
        state: &mut VideoState,
        reading: AvVideoReading,
        delay: i64,
        sound: i64,
        own: i64,
        step: Step,
        audio_settable: bool,
    ) -> AvVideoReason {
        // How much longer the picture must wait: positive when it is early.
        let early = sound - delay;
        if state.sound_waits {
            // The sound stops waiting once the track is no longer above its
            // target, once the sound is later than the track anyway, or
            // once nobody is looking at it.
            if delay > state.held.target + BAND && early <= BAND {
                state.calm = 0;
            } else {
                state.calm = state.calm.saturating_add(1);
            }
            if !reading.shown || state.calm >= AV_RELEASE_READINGS {
                state.sound_waits = false;
                state.needs_more = 0;
                state.held.trim = 0;
                state.held.steady = 0;
            }
        }
        if state.sound_waits {
            // Its target is the audio's own need, never the audio's
            // measured delay, which now waits for this track.
            state.need = delay.max(reading.network_ms.map_or(0, bounded));
            state.held.slew(own, step);
            return AvVideoReason::SoundWaits;
        }

        // The skew is the engine's answer only when the target stands where
        // it is wanted; a sound that has just moved is followed first.
        let settled = state.held.settled(delay)
            && (sound + state.held.trim - state.held.target).abs() <= MOVE;
        if early.abs() <= BAND {
            state.held.pinned = 0;
            state.held.ignored = false;
            state.needs_more = 0;
            state.late = false;
        } else if settled && !state.held.ignored {
            let trim = (state.held.trim + early / 2).clamp(-TRIM_MAX, TRIM_MAX);
            if trim == state.held.trim && early > 0 {
                // As high as the trim goes and the picture is still early.
                state.held.pinned = state.held.pinned.saturating_add(1);
                if state.held.pinned >= AV_IGNORED_READINGS {
                    state.held.ignored = true;
                    state.held.trim = 0;
                }
            } else {
                state.held.trim = trim;
                state.held.pinned = 0;
            }
        }
        if early < -BAND {
            // Later than the sound. Held there by something else when the
            // target is as low as the trim allows, or when the network is
            // reported to need more than the sound has.
            let at_floor = settled && state.held.trim == -TRIM_MAX;
            let network_needs = reading
                .network_ms
                .is_some_and(|network| bounded(network) > sound + BAND);
            if at_floor || network_needs {
                state.needs_more = state.needs_more.saturating_add(1);
            }
            if state.needs_more >= AV_HELD_READINGS {
                if reading.shown && audio_settable {
                    state.sound_waits = true;
                    state.calm = 0;
                    state.late = false;
                    state.held.trim = 0;
                    state.need = delay.max(reading.network_ms.map_or(0, bounded));
                    state.held.slew(own, step);
                    return AvVideoReason::SoundWaits;
                }
                state.late = true;
            }
        } else {
            state.needs_more = 0;
        }

        let moved = state.held.slew(sound + state.held.trim, step);
        if early.abs() <= BAND {
            AvVideoReason::Aligned
        } else if state.late {
            AvVideoReason::PictureLate
        } else if state.held.ignored {
            AvVideoReason::HintIgnored
        } else {
            match moved {
                1 => AvVideoReason::Raising,
                -1 => AvVideoReason::Lowering,
                _ => AvVideoReason::Settling,
            }
        }
    }

    /// The audio receiver, given the largest delay a shown picture needs
    /// of it (`need`). Returns the plan and whether the wait was cut at its
    /// bound.
    fn read_audio(
        &mut self,
        sound: i64,
        own: i64,
        need: Option<i64>,
        step: Step,
    ) -> (AvAudioPlan, bool) {
        let settable = self.audio_settable;
        let plan = |held: &mut Held, reason| {
            let set_ms = held.tell(held.target, settable);
            AvAudioPlan {
                set_ms,
                asked_ms: held.asked(),
                reason,
            }
        };
        if !settable {
            self.audio.target = 0;
            return (plan(&mut self.audio, AvAudioReason::NoSetter), false);
        }
        let Some(need) = need else {
            // Nothing to wait for: come down, and let go once the target
            // no longer holds the audio above its own need.
            self.audio.trim = 0;
            self.audio.pinned = 0;
            self.audio.ignored = false;
            if self.audio.target == 0 {
                return (plan(&mut self.audio, AvAudioReason::Free), false);
            }
            let lowered = self.audio.target - step.down;
            self.audio.steady = 0;
            if lowered <= own + BAND {
                self.audio.target = 0;
                return (plan(&mut self.audio, AvAudioReason::Free), false);
            }
            self.audio.target = lowered;
            return (plan(&mut self.audio, AvAudioReason::Releasing), false);
        };
        let cap = (own + WAIT_MAX).min(TARGET_MAX);
        let goal = need.min(cap);
        let bounded_wait = need > cap + BAND;
        let short = goal - sound;
        let settled =
            self.audio.settled(sound) && (goal + self.audio.trim - self.audio.target).abs() <= MOVE;
        if short <= BAND {
            self.audio.pinned = 0;
            self.audio.ignored = false;
        } else if settled && !self.audio.ignored {
            let trim = (self.audio.trim + short / 2).min(TRIM_MAX);
            if trim == self.audio.trim {
                self.audio.pinned = self.audio.pinned.saturating_add(1);
                if self.audio.pinned >= AV_IGNORED_READINGS {
                    self.audio.ignored = true;
                    self.audio.trim = 0;
                }
            } else {
                self.audio.trim = trim;
                self.audio.pinned = 0;
            }
        }
        let want = (goal + self.audio.trim).clamp(0, TARGET_MAX);
        // A target below what the audio already has holds nothing: start
        // from there.
        if self.audio.target < sound.min(want) {
            self.audio.target = sound.min(want);
        }
        self.audio.slew(want, step);
        let reason = if self.audio.ignored {
            AvAudioReason::HintIgnored
        } else if bounded_wait {
            AvAudioReason::WaitBounded
        } else {
            AvAudioReason::WaitingForPicture
        };
        (plan(&mut self.audio, reason), bounded_wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const fn audio(delay: u32) -> AvAudioReading {
        AvAudioReading {
            flowing: true,
            delay_ms: Some(delay),
            network_ms: None,
        }
    }

    /// A shown track the engine does not synchronise.
    const fn video(delay: u32) -> AvVideoReading {
        AvVideoReading {
            flowing: true,
            shown: true,
            engine_synced: false,
            delay_ms: Some(delay),
            network_ms: None,
        }
    }

    /// Sound and a light track only.
    const fn sound_and_light(sound: u32, light: u32) -> AvReadings {
        AvReadings {
            audio: audio(sound),
            main: AvVideoReading::ABSENT,
            middle: AvVideoReading::ABSENT,
            light: video(light),
        }
    }

    #[test]
    fn the_constants_are_the_chosen_values() {
        assert_eq!(AV_READING_MS, 1000);
        assert_eq!(AV_DEAD_BAND_MS, 30);
        assert_eq!(AV_MOVE_MS, 15);
        assert_eq!(AV_RAISE_PER_S_MS, 240);
        assert_eq!(AV_LOWER_PER_S_MS, 80);
        assert_eq!(AV_MAX_ELAPSED_MS, 2000);
        assert_eq!(AV_STALE_MS, 5000);
        assert_eq!(AV_TRIM_MAX_MS, 90);
        assert_eq!(AV_SETTLED_READINGS, 2);
        assert_eq!(AV_MOVING_MS, 15);
        assert_eq!(AV_HELD_READINGS, 2);
        assert_eq!(AV_RELEASE_READINGS, 2);
        assert_eq!(AV_IGNORED_READINGS, 3);
        assert_eq!(AV_SOUND_WAIT_MAX_MS, 300);
        assert_eq!(AV_TARGET_MAX_MS, 2000);
        assert_eq!(AV_NO_SOUND_READINGS, 5);
        assert_eq!(AV_READING_MAX_MS, 10_000);
    }

    #[test]
    fn says_what_each_set_of_setters_can_achieve() {
        assert_eq!(av_align_capability(true, true), AvCapability::Full);
        assert_eq!(
            av_align_capability(false, true),
            AvCapability::PictureWaitsOnly
        );
        assert_eq!(
            av_align_capability(true, false),
            AvCapability::SoundWaitsOnly
        );
        assert_eq!(av_align_capability(false, false), AvCapability::ObserveOnly);
        assert_eq!(
            AvAlign::new(false, true).capability(),
            AvCapability::PictureWaitsOnly
        );
    }

    #[test]
    fn the_diagnostic_words_are_distinct() {
        let audio = [
            AvAudioReason::NotFlowing,
            AvAudioReason::NoReading,
            AvAudioReason::NoSetter,
            AvAudioReason::Free,
            AvAudioReason::WaitingForPicture,
            AvAudioReason::WaitBounded,
            AvAudioReason::Releasing,
            AvAudioReason::HintIgnored,
        ]
        .map(AvAudioReason::name);
        let video = [
            AvVideoReason::NotFlowing,
            AvVideoReason::NoReading,
            AvVideoReason::NoSound,
            AvVideoReason::NoSetter,
            AvVideoReason::EngineSynced,
            AvVideoReason::Started,
            AvVideoReason::Aligned,
            AvVideoReason::Raising,
            AvVideoReason::Lowering,
            AvVideoReason::Settling,
            AvVideoReason::HintIgnored,
            AvVideoReason::SoundWaits,
            AvVideoReason::PictureLate,
        ]
        .map(AvVideoReason::name);
        let distinct = |names: &[&str]| {
            names
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == names.len()
        };
        assert!(distinct(&audio));
        assert!(distinct(&video));
        assert_eq!(AvCapability::PictureWaitsOnly.name(), "picture-waits-only");
    }

    // ----- one rule at a time ----------------------------------------------

    #[test]
    fn a_track_starts_at_the_sounds_delay() {
        let mut a = AvAlign::new(true, true);
        let plan = a.read(0, &sound_and_light(450, 290));
        assert_eq!(plan.light.set_ms, Some(450));
        assert_eq!(plan.light.asked_ms, 450);
        assert_eq!(plan.light.skew_ms, Some(-160));
        assert_eq!(plan.light.reason, AvVideoReason::Started);
        assert_eq!(plan.audio.set_ms, None);
        assert_eq!(plan.audio.reason, AvAudioReason::Free);
        assert_eq!(plan.main.reason, AvVideoReason::NotFlowing);
        assert_eq!(plan.main.set_ms, None);
    }

    #[test]
    fn an_engine_synced_track_is_observed_and_never_written() {
        let mut a = AvAlign::new(true, true);
        let mut r = sound_and_light(450, 450);
        r.main = AvVideoReading {
            engine_synced: true,
            ..video(520)
        };
        for t in 0..20u64 {
            let plan = a.read(t * 1000, &r);
            assert_eq!(plan.main.set_ms, None);
            assert_eq!(plan.main.asked_ms, 0);
            assert_eq!(plan.main.skew_ms, Some(70));
            assert_eq!(plan.main.reason, AvVideoReason::EngineSynced);
            // Its measured delay never makes the sound wait.
            assert_eq!(plan.audio.asked_ms, 0);
        }
    }

    #[test]
    fn a_shown_synced_tracks_network_need_makes_the_sound_wait() {
        let mut a = AvAlign::new(true, true);
        let mut r = sound_and_light(150, 150);
        r.main = AvVideoReading {
            engine_synced: true,
            network_ms: Some(260),
            ..video(260)
        };
        let plan = a.read(0, &r);
        assert_eq!(plan.audio.reason, AvAudioReason::WaitingForPicture);
        // Starts from what the audio already has, then rises within the
        // step.
        assert_eq!(plan.audio.set_ms, Some(150));
        let plan = a.read(1000, &r);
        assert_eq!(plan.audio.set_ms, Some(260));
        // Not while it is hidden.
        let mut b = AvAlign::new(true, true);
        r.main.shown = false;
        assert_eq!(b.read(0, &r).audio.reason, AvAudioReason::Free);
    }

    #[test]
    fn a_target_rises_and_falls_within_its_step() {
        let mut a = AvAlign::new(true, true);
        a.read(0, &sound_and_light(300, 300));
        // The sound jumps to 990: 240 ms a second upwards.
        let plan = a.read(1000, &sound_and_light(990, 300));
        assert_eq!(plan.light.set_ms, Some(540));
        assert_eq!(plan.light.reason, AvVideoReason::Raising);
        // Two seconds count in full, more does not.
        let plan = a.read(3000, &sound_and_light(990, 540));
        assert_eq!(plan.light.set_ms, Some(990));
        // The sound falls back: 80 ms a second downwards.
        let plan = a.read(4000, &sound_and_light(300, 990));
        assert_eq!(plan.light.set_ms, Some(910));
        assert_eq!(plan.light.reason, AvVideoReason::Lowering);
        let plan = a.read(4500, &sound_and_light(300, 990));
        assert_eq!(plan.light.set_ms, Some(870));
    }

    #[test]
    fn nothing_moves_inside_the_dead_band_or_the_move_threshold() {
        let mut a = AvAlign::new(true, true);
        a.read(0, &sound_and_light(400, 400));
        for (t, sound, light) in [(1, 410, 395), (2, 392, 405), (3, 415, 400), (4, 385, 412)] {
            let plan = a.read(t * 1000, &sound_and_light(sound, light));
            assert_eq!(plan.light.set_ms, None, "t={t}");
            assert_eq!(plan.light.asked_ms, 400);
            assert_eq!(plan.light.reason, AvVideoReason::Aligned);
        }
    }

    #[test]
    fn a_settled_skew_is_trimmed_by_half() {
        // An engine that plays 50 ms above what it is asked.
        let mut a = AvAlign::new(true, true);
        a.read(0, &sound_and_light(400, 450));
        assert_eq!(a.read(1000, &sound_and_light(400, 450)).light.set_ms, None);
        assert_eq!(a.read(2000, &sound_and_light(400, 450)).light.set_ms, None);
        // Settled: half of -50 comes off.
        let plan = a.read(3000, &sound_and_light(400, 450));
        assert_eq!(plan.light.set_ms, Some(375));
        assert_eq!(plan.light.reason, AvVideoReason::Lowering);
        // The engine answers; inside the band nothing more happens.
        let plan = a.read(4000, &sound_and_light(400, 425));
        assert_eq!(plan.light.set_ms, None);
        assert_eq!(plan.light.reason, AvVideoReason::Aligned);
    }

    #[test]
    fn a_missing_reading_changes_nothing() {
        let mut a = AvAlign::new(true, true);
        a.read(0, &sound_and_light(400, 300));
        let mut r = sound_and_light(600, 300);
        r.light.delay_ms = None;
        let plan = a.read(1000, &r);
        assert_eq!(plan.light.set_ms, None);
        assert_eq!(plan.light.asked_ms, 400);
        assert_eq!(plan.light.skew_ms, None);
        assert_eq!(plan.light.reason, AvVideoReason::NoReading);
        // A missing audio reading: nothing changes either.
        let mut r = sound_and_light(0, 400);
        r.audio.delay_ms = None;
        let plan = a.read(2000, &r);
        assert_eq!(plan.light.set_ms, None);
        assert_eq!(plan.light.reason, AvVideoReason::NoSound);
        assert_eq!(plan.audio.set_ms, None);
        assert_eq!(plan.audio.reason, AvAudioReason::NoReading);
        // And the next full reading carries on from where it was.
        let plan = a.read(3000, &sound_and_light(400, 400));
        assert_eq!(plan.light.set_ms, None);
        assert_eq!(plan.light.asked_ms, 400);
    }

    #[test]
    fn a_paused_track_is_left_alone_and_resumes_from_the_sound() {
        let mut a = AvAlign::new(true, true);
        a.read(0, &sound_and_light(320, 300));
        let mut paused = sound_and_light(600, 0);
        paused.light = AvVideoReading::ABSENT;
        for t in 1..=10u64 {
            let plan = a.read(t * 1000, &paused);
            assert_eq!(plan.light.set_ms, None);
            assert_eq!(plan.light.asked_ms, 320);
            assert_eq!(plan.light.reason, AvVideoReason::NotFlowing);
        }
        // Back: at the sound's delay now, in one step, not from 320.
        let plan = a.read(11_000, &sound_and_light(600, 290));
        assert_eq!(plan.light.set_ms, Some(600));
        assert_eq!(plan.light.reason, AvVideoReason::Started);
    }

    #[test]
    fn a_stale_gap_starts_every_track_again() {
        let mut a = AvAlign::new(true, true);
        a.read(0, &sound_and_light(320, 300));
        let plan = a.read(60_000, &sound_and_light(700, 300));
        assert_eq!(plan.light.set_ms, Some(700));
        assert_eq!(plan.light.reason, AvVideoReason::Started);
    }

    #[test]
    fn time_never_runs_backwards() {
        let mut a = AvAlign::new(true, true);
        a.read(5000, &sound_and_light(300, 300));
        // An earlier time is no time at all: nothing may move.
        let plan = a.read(1000, &sound_and_light(900, 300));
        assert_eq!(plan.light.set_ms, None);
        assert_eq!(plan.light.asked_ms, 300);
    }

    #[test]
    fn the_targets_are_given_back_when_the_sound_is_gone() {
        let mut a = AvAlign::new(true, true);
        a.read(0, &sound_and_light(500, 300));
        let mut silent = sound_and_light(0, 500);
        silent.audio = AvAudioReading::default();
        for t in 1..AV_NO_SOUND_READINGS {
            let plan = a.read(u64::from(t) * 1000, &silent);
            assert_eq!(plan.light.set_ms, None);
            assert_eq!(plan.audio.reason, AvAudioReason::NotFlowing);
        }
        let plan = a.read(u64::from(AV_NO_SOUND_READINGS) * 1000, &silent);
        assert_eq!(plan.light.set_ms, Some(0));
        assert_eq!(plan.light.asked_ms, 0);
        assert_eq!(plan.light.reason, AvVideoReason::NoSound);
    }

    #[test]
    fn receivers_replaced_tells_everything_again() {
        let mut a = AvAlign::new(true, true);
        assert_eq!(
            a.read(0, &sound_and_light(400, 400)).light.set_ms,
            Some(400)
        );
        assert_eq!(a.read(1000, &sound_and_light(400, 400)).light.set_ms, None);
        a.receivers_replaced();
        assert_eq!(
            a.read(2000, &sound_and_light(400, 400)).light.set_ms,
            Some(400)
        );
    }

    #[test]
    fn nothing_is_set_without_a_setter() {
        let mut a = AvAlign::new(false, false);
        for t in 0..30u64 {
            let plan = a.read(t * 1000, &sound_and_light(450, 290));
            assert_eq!(plan.light.set_ms, None);
            assert_eq!(plan.audio.set_ms, None);
            assert_eq!(plan.light.asked_ms, 0);
            assert_eq!(plan.audio.reason, AvAudioReason::NoSetter);
            assert_eq!(plan.light.skew_ms, Some(-160));
            if t > 0 {
                assert_eq!(plan.light.reason, AvVideoReason::NoSetter);
            }
        }
    }

    #[test]
    fn a_hidden_late_track_never_makes_the_sound_wait() {
        let mut a = AvAlign::new(true, true);
        let mut r = sound_and_light(120, 290);
        r.light.shown = false;
        let mut last = a.read(0, &r);
        for t in 1..40u64 {
            last = a.read(t * 1000, &r);
            assert_eq!(last.audio.asked_ms, 0);
        }
        assert_eq!(last.light.reason, AvVideoReason::PictureLate);
    }

    // ----- a model receiver ------------------------------------------------

    /// Noise of one reading: a fixed pseudo-random sequence in `-6..=6` ms.
    struct Noise(u64);

    impl Noise {
        fn next(&mut self) -> i64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            i64::try_from((self.0 >> 33) % 13).unwrap() - 6
        }
    }

    /// A receiver whose delay follows the largest of its network need, the
    /// server's floor and (when it honours the hint) the target it was
    /// given, closing `lag_quarters / 4` of the distance per reading.
    #[derive(Clone, Copy)]
    struct Receiver {
        need: i64,
        floor: i64,
        honours: bool,
        target: i64,
        delay: i64,
        lag_quarters: i64,
    }

    impl Receiver {
        const fn new(need: i64, floor: i64) -> Self {
            Self {
                need,
                floor,
                honours: true,
                target: 0,
                delay: if need > floor { need } else { floor },
                lag_quarters: 2,
            }
        }

        fn step(&mut self) {
            let hint = if self.honours { self.target } else { 0 };
            let goal = self.need.max(self.floor).max(hint);
            let gap = goal - self.delay;
            let moved = gap * self.lag_quarters / 4;
            self.delay += if moved == 0 { gap.signum() } else { moved };
        }

        fn reading(&self, noise: &mut Noise) -> u32 {
            u32::try_from((self.delay + noise.next()).max(0)).unwrap()
        }
    }

    /// One remote person: sound and a shown light track.
    struct Call {
        align: AvAlign,
        sound: Receiver,
        light: Receiver,
        light_flowing: bool,
        light_reads: bool,
        noise: Noise,
        now: u64,
        /// The true skew after each reading (light delay minus sound delay).
        skews: Vec<i64>,
        /// Whether each reading set anything.
        sets: Vec<bool>,
        last: Option<AvPlan>,
    }

    impl Call {
        fn new(sound: Receiver, light: Receiver) -> Self {
            Self {
                align: AvAlign::new(true, true),
                sound,
                light,
                light_flowing: true,
                light_reads: true,
                noise: Noise(7),
                now: 0,
                skews: Vec::new(),
                sets: Vec::new(),
                last: None,
            }
        }

        /// One reading interval: the receivers move, are read, and are told.
        fn tick(&mut self) -> AvPlan {
            self.now += AV_READING_MS;
            self.sound.step();
            if self.light_flowing {
                self.light.step();
            }
            let readings = AvReadings {
                audio: audio(self.sound.reading(&mut self.noise)),
                main: AvVideoReading::ABSENT,
                middle: AvVideoReading::ABSENT,
                light: if self.light_flowing {
                    AvVideoReading {
                        delay_ms: self
                            .light_reads
                            .then(|| self.light.reading(&mut self.noise)),
                        ..video(0)
                    }
                } else {
                    AvVideoReading::ABSENT
                },
            };
            let plan = self.align.read(self.now, &readings);
            if let Some(ms) = plan.audio.set_ms {
                self.sound.target = i64::from(ms);
            }
            if let Some(ms) = plan.light.set_ms {
                self.light.target = i64::from(ms);
            }
            self.skews.push(self.light.delay - self.sound.delay);
            self.sets
                .push(plan.audio.set_ms.is_some() || plan.light.set_ms.is_some());
            self.last = Some(plan);
            plan
        }

        fn run(&mut self, readings: usize) {
            for _ in 0..readings {
                self.tick();
            }
        }

        /// The number of readings after which the true skew stayed inside
        /// the dead band for the rest of the run.
        fn converged_after(&self) -> usize {
            self.skews
                .iter()
                .rposition(|s| s.abs() > BAND)
                .map_or(0, |i| i + 1)
        }

        /// The largest true skew over the last `n` readings.
        fn residual(&self, n: usize) -> i64 {
            self.skews[self.skews.len() - n..]
                .iter()
                .map(|s| s.abs())
                .max()
                .unwrap()
        }

        /// How many of the last `n` readings set anything.
        fn sets_in_last(&self, n: usize) -> usize {
            self.sets[self.sets.len() - n..]
                .iter()
                .filter(|s| **s)
                .count()
        }
    }

    // ----- the measured cases, on the model ---------------------------------

    /// A light track whose network needs 100 ms, held at the server's 290 ms
    /// floor.
    const fn light_at_the_floor() -> Receiver {
        Receiver::new(100, 290)
    }

    #[test]
    fn an_early_light_track_is_brought_to_a_steady_sound() {
        // The sound at 450 ms, light at the 290 ms floor: 160 ms early.
        let mut c = Call::new(Receiver::new(450, 0), light_at_the_floor());
        c.run(60);
        assert_eq!(c.skews[0], -160);
        assert!(c.converged_after() <= 3, "{}", c.converged_after());
        assert!(c.residual(30) <= 10, "{}", c.residual(30));
        assert_eq!(c.sets_in_last(30), 0);
        let plan = c.last.unwrap();
        assert_eq!(plan.light.reason, AvVideoReason::Aligned);
        assert_eq!(plan.audio.asked_ms, 0);
    }

    #[test]
    fn a_light_track_follows_a_sound_that_wanders_between_320_and_600() {
        // The audio's need goes 320 -> 600 -> 320 at 10 ms a second, as the
        // measured call's did; light is at the 290 ms floor throughout.
        let mut c = Call::new(Receiver::new(320, 0), light_at_the_floor());
        let mut worst = 0;
        let mut sound_asked = 0;
        for i in 0..168i64 {
            let phase = i % 56;
            c.sound.need = if phase < 28 {
                320 + phase * 10
            } else {
                600 - (phase - 28) * 10
            };
            let plan = c.tick();
            sound_asked = sound_asked.max(plan.audio.asked_ms);
            if i >= 5 {
                worst = worst.max(c.skews[c.skews.len() - 1].abs());
            }
        }
        // A moving sound is followed one reading and one receiver lag
        // behind: the skew stays under 40 ms, against 100 to 300 ms before.
        assert!(worst <= 40, "{worst}");
        // The picture never made the sound wait.
        assert_eq!(sound_asked, 0);
    }

    #[test]
    fn a_picture_530_ms_ahead_of_the_sound_is_closed_in_seconds() {
        // The measured moment: the audio buffer at 990 ms, video at 460.
        let mut light = light_at_the_floor();
        light.delay = 460;
        light.target = 460;
        let mut c = Call::new(Receiver::new(990, 0), light);
        c.run(60);
        assert_eq!(c.skews[0], -530);
        assert!(c.converged_after() <= 5, "{}", c.converged_after());
        assert!(c.residual(30) <= 10, "{}", c.residual(30));
        assert_eq!(c.sets_in_last(30), 0);
        // The same with receivers that close only a quarter of the distance
        // per reading: slower, and no overshoot.
        let mut light = light_at_the_floor();
        light.delay = 460;
        light.target = 460;
        light.lag_quarters = 1;
        let mut sound = Receiver::new(990, 0);
        sound.lag_quarters = 1;
        let mut c = Call::new(sound, light);
        c.run(60);
        assert!(c.converged_after() <= 10, "{}", c.converged_after());
        assert!(c.skews.iter().all(|s| *s <= BAND), "{:?}", c.skews);
        assert_eq!(c.sets_in_last(30), 0);
    }

    #[test]
    fn the_sound_waits_for_a_picture_that_needs_more() {
        // A quiet audio path (120 ms) and a light track held at 290.
        let mut c = Call::new(Receiver::new(120, 0), light_at_the_floor());
        c.run(60);
        assert_eq!(c.skews[0], 170);
        assert!(c.converged_after() <= 10, "{}", c.converged_after());
        assert!(c.residual(30) <= 10, "{}", c.residual(30));
        assert_eq!(c.sets_in_last(30), 0);
        let plan = c.last.unwrap();
        assert_eq!(plan.audio.reason, AvAudioReason::WaitingForPicture);
        assert_eq!(plan.light.reason, AvVideoReason::SoundWaits);
        // The track's own target is the audio's own need, so neither holds
        // the other up.
        assert!(plan.light.asked_ms <= 120 + AV_DEAD_BAND_MS);

        // The floor goes away: the sound stops waiting and comes back down.
        c.light.floor = 0;
        c.run(60);
        let plan = c.last.unwrap();
        assert_eq!(plan.audio.asked_ms, 0);
        assert_eq!(plan.audio.reason, AvAudioReason::Free);
        assert_eq!(c.sound.delay, 120);
        assert!(c.residual(30) <= 10, "{}", c.residual(30));
        assert_eq!(c.sets_in_last(30), 0);
        // Given back within ten readings of the floor going.
        assert!(c.converged_after() <= 60 + 10, "{}", c.converged_after());
    }

    #[test]
    fn the_sound_waits_no_longer_than_its_bound() {
        // The picture needs 700 ms, the audio 120: the sound goes to 420 and
        // the picture stays 280 ms late.
        let mut c = Call::new(Receiver::new(120, 0), Receiver::new(700, 0));
        let mut sound_asked = 0;
        for _ in 0..60 {
            sound_asked = sound_asked.max(c.tick().audio.asked_ms);
        }
        assert!(
            sound_asked <= 120 + AV_SOUND_WAIT_MAX_MS + AV_DEAD_BAND_MS,
            "{sound_asked}"
        );
        assert!((c.sound.delay - 420).abs() <= 10, "{}", c.sound.delay);
        let plan = c.last.unwrap();
        assert_eq!(plan.audio.reason, AvAudioReason::WaitBounded);
        assert_eq!(plan.light.reason, AvVideoReason::PictureLate);
        assert_eq!(c.sets_in_last(30), 0);
    }

    #[test]
    fn a_track_that_pauses_and_resumes_starts_from_the_sound() {
        let mut c = Call::new(Receiver::new(320, 0), light_at_the_floor());
        c.run(10);
        // Paused for ten readings while the sound goes to 600.
        c.light_flowing = false;
        c.sound.need = 600;
        for _ in 0..10 {
            let plan = c.tick();
            assert_eq!(plan.light.set_ms, None);
            assert_eq!(plan.light.reason, AvVideoReason::NotFlowing);
        }
        c.light_flowing = true;
        let plan = c.tick();
        assert_eq!(plan.light.reason, AvVideoReason::Started);
        // At the sound's delay now, within one reading's noise, in one step.
        let set = i64::from(plan.light.set_ms.unwrap());
        assert!((set - 600).abs() <= 10, "{set}");
        c.run(40);
        assert!(c.residual(30) <= 10, "{}", c.residual(30));
        assert_eq!(c.sets_in_last(30), 0);
    }

    #[test]
    fn missing_readings_move_nothing() {
        let mut c = Call::new(Receiver::new(450, 0), light_at_the_floor());
        c.run(10);
        let asked = c.last.unwrap().light.asked_ms;
        // Five readings without a video delay while the sound moves.
        c.light_reads = false;
        c.sound.need = 600;
        for _ in 0..5 {
            let plan = c.tick();
            assert_eq!(plan.light.set_ms, None);
            assert_eq!(plan.light.asked_ms, asked);
            assert_eq!(plan.light.reason, AvVideoReason::NoReading);
        }
        c.light_reads = true;
        c.run(40);
        assert!(c.residual(30) <= 10, "{}", c.residual(30));
        assert_eq!(c.sets_in_last(30), 0);
    }

    #[test]
    fn an_engine_that_ignores_the_video_hint_is_left_alone() {
        let mut light = light_at_the_floor();
        light.honours = false;
        let mut c = Call::new(Receiver::new(450, 0), light);
        c.run(60);
        // The picture stays early; the rule says so and stops moving.
        assert_eq!(c.residual(30), 160);
        assert_eq!(c.sets_in_last(45), 0);
        let plan = c.last.unwrap();
        assert_eq!(plan.light.reason, AvVideoReason::HintIgnored);
        // The target stands at the sound's delay, not wound up above it.
        assert!((i64::from(plan.light.asked_ms) - 450).abs() <= 20);
        assert_eq!(plan.audio.asked_ms, 0);
    }

    #[test]
    fn an_engine_that_ignores_the_audio_hint_is_left_alone() {
        let mut sound = Receiver::new(120, 0);
        sound.honours = false;
        let mut c = Call::new(sound, light_at_the_floor());
        c.run(60);
        assert_eq!(c.residual(30), 170);
        assert_eq!(c.sets_in_last(30), 0);
        let plan = c.last.unwrap();
        assert_eq!(plan.audio.reason, AvAudioReason::HintIgnored);
        assert_eq!(plan.light.reason, AvVideoReason::PictureLate);
        // Not wound up: at what the picture needs.
        assert!((i64::from(plan.audio.asked_ms) - 290).abs() <= 20);
    }

    #[test]
    fn a_sound_that_drops_is_followed_down_slowly_and_is_never_held_up() {
        // The audio's need falls from 600 to 320 at once. The model's audio
        // buffer comes down faster than a video target is lowered, so the
        // picture is late for some seconds; the sound is never asked to
        // wait for it.
        let mut c = Call::new(Receiver::new(600, 0), light_at_the_floor());
        c.run(20);
        c.sound.need = 320;
        let mut sound_asked = 0;
        for _ in 0..60 {
            sound_asked = sound_asked.max(c.tick().audio.asked_ms);
        }
        assert_eq!(sound_asked, 0);
        let worst = c.skews[20..].iter().copied().max().unwrap();
        assert!(worst <= 170, "{worst}");
        // Inside the band again within seven readings.
        assert!(c.skews[27..].iter().all(|s| s.abs() <= BAND));
        assert_eq!(c.sets_in_last(30), 0);
    }

    #[test]
    fn a_device_that_cannot_delay_audio_leaves_the_picture_late() {
        let mut c = Call::new(Receiver::new(120, 0), light_at_the_floor());
        c.align = AvAlign::new(false, true);
        c.run(40);
        let plan = c.last.unwrap();
        assert_eq!(plan.audio.set_ms, None);
        assert_eq!(plan.audio.asked_ms, 0);
        assert_eq!(plan.audio.reason, AvAudioReason::NoSetter);
        assert_eq!(plan.light.reason, AvVideoReason::PictureLate);
        assert_eq!(c.residual(30), 170);
        assert_eq!(c.sets_in_last(30), 0);
    }

    #[test]
    fn a_device_that_can_only_delay_audio_still_waits_for_a_late_picture() {
        let mut c = Call::new(Receiver::new(120, 0), light_at_the_floor());
        c.align = AvAlign::new(true, false);
        c.run(60);
        let plan = c.last.unwrap();
        assert_eq!(plan.light.set_ms, None);
        assert_eq!(plan.light.asked_ms, 0);
        assert_eq!(plan.light.reason, AvVideoReason::SoundWaits);
        assert!(c.residual(30) <= 10, "{}", c.residual(30));
        assert_eq!(c.sets_in_last(30), 0);
    }

    #[test]
    fn the_same_readings_give_the_same_plans() {
        let run = || {
            let mut c = Call::new(Receiver::new(120, 0), light_at_the_floor());
            (0..40).map(|_| c.tick()).collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    // ----- arbitrary readings ------------------------------------------------

    fn delay() -> impl Strategy<Value = Option<u32>> {
        prop_oneof![
            1 => Just(None),
            6 => (0u32..1500).prop_map(Some),
            1 => any::<u32>().prop_map(Some),
        ]
    }

    fn a_video() -> impl Strategy<Value = AvVideoReading> {
        (prop::bool::weighted(0.85), any::<bool>(), delay(), delay()).prop_map(
            |(flowing, shown, delay_ms, network_ms)| AvVideoReading {
                flowing,
                shown,
                engine_synced: false,
                delay_ms,
                network_ms,
            },
        )
    }

    fn a_reading() -> impl Strategy<Value = (u64, AvReadings)> {
        (
            prop_oneof![8 => 0u64..3000, 1 => 0u64..20_000, 1 => any::<u64>()],
            prop::bool::weighted(0.9),
            delay(),
            delay(),
            a_video(),
            a_video(),
            a_video(),
            any::<bool>(),
        )
            .prop_map(
                |(dt, flowing, delay_ms, network_ms, mut main, middle, light, synced)| {
                    main.engine_synced = synced;
                    let audio = AvAudioReading {
                        flowing,
                        delay_ms,
                        network_ms,
                    };
                    (
                        dt,
                        AvReadings {
                            audio,
                            main,
                            middle,
                            light,
                        },
                    )
                },
            )
    }

    proptest! {
        #[test]
        fn arbitrary_readings_never_panic_and_stay_in_bounds(
            audio_settable in any::<bool>(),
            video_settable in any::<bool>(),
            steps in proptest::collection::vec(a_reading(), 0..200),
        ) {
            let mut a = AvAlign::new(audio_settable, video_settable);
            let mut now = 0u64;
            let mut asked = [0u32; 3];
            for (dt, readings) in &steps {
                now = now.wrapping_add(*dt);
                let plan = a.read(now, readings);
                // Bounded, and only where there is a setter.
                prop_assert!(plan.audio.asked_ms <= AV_TARGET_MAX_MS);
                prop_assert!(audio_settable || plan.audio.set_ms.is_none());
                prop_assert!(plan.audio.set_ms.is_none_or(|ms| ms == plan.audio.asked_ms));
                let videos = [
                    (plan.main, readings.main),
                    (plan.middle, readings.middle),
                    (plan.light, readings.light),
                ];
                for (i, (video, reading)) in videos.into_iter().enumerate() {
                    prop_assert!(video.asked_ms <= AV_TARGET_MAX_MS);
                    prop_assert!(video_settable || video.set_ms.is_none());
                    prop_assert!(video.set_ms.is_none_or(|ms| ms == video.asked_ms));
                    // What was asked changes only by being set.
                    prop_assert!(video.set_ms.is_some() || video.asked_ms == asked[i]);
                    // A track that is not flowing is left alone.
                    prop_assert!(reading.flowing || video.set_ms.is_none());
                    // A synchronised track is never given a delay.
                    prop_assert!(
                        !(reading.flowing && reading.engine_synced)
                            || video.set_ms.is_none_or(|ms| ms == 0)
                    );
                    // A missing reading sets nothing on that track unless
                    // the sound is gone and the target is given back.
                    if reading.flowing && reading.delay_ms.is_none() && !reading.engine_synced {
                        prop_assert!(video.set_ms.is_none_or(|ms| ms == 0));
                    }
                    // A following track moves within its step.
                    let stepping = matches!(
                        video.reason,
                        AvVideoReason::Raising
                            | AvVideoReason::Lowering
                            | AvVideoReason::Settling
                            | AvVideoReason::Aligned
                            | AvVideoReason::HintIgnored
                            | AvVideoReason::PictureLate
                            | AvVideoReason::SoundWaits
                    );
                    if stepping && video_settable {
                        let up = i64::from(AV_RAISE_PER_S_MS) * 2;
                        let down = i64::from(AV_LOWER_PER_S_MS) * 2;
                        let change = i64::from(video.asked_ms) - i64::from(asked[i]);
                        prop_assert!(change <= up && change >= -down, "{change}");
                    }
                    // A skew is reported exactly when both were measured.
                    let both = readings.audio.flowing
                        && readings.audio.delay_ms.is_some()
                        && reading.flowing
                        && reading.delay_ms.is_some();
                    prop_assert_eq!(video.skew_ms.is_some(), both);
                    asked[i] = video.asked_ms;
                }
            }
        }
    }
}
