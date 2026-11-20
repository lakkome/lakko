// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The stored choice, the processor's health and the frame gate
//! (ADR-0089 section 3, UX `call-backgrounds.md` section 3).
//!
//! The rule every client applies per camera frame: **while a background is
//! selected, a frame it was not applied to is never sent.** A background that
//! is still starting sends nothing; one that failed sends nothing and the
//! client turns the camera off. Someone who blurred the room did so to hide
//! it, and a silent fall-back to the raw camera would show it.

/// The ids of the six built-in backgrounds, in the order the strip shows
/// them: two light, two mid, two dark (UX section 5). Their spoken names are
/// catalog strings on each client.
pub const BUILTIN_IDS: [&str; 6] = ["dawn", "morning", "meadow", "harbor", "dusk", "night"];

/// The longest id a stored own photo may have.
const MAX_PHOTO_ID_LEN: usize = 64;

/// What the person chose. Stored per device as the text of
/// [`Choice::to_stored`]; never synced, never sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// The camera as it is.
    None,
    /// The room blurred.
    Blur,
    /// The room blurred so nothing in it can be told apart.
    StrongBlur,
    /// One of [`BUILTIN_IDS`].
    Builtin(&'static str),
    /// One of the person's own photos, by its id in this device's store.
    Photo(String),
}

impl Choice {
    /// Reads a stored choice. An empty value or `none` is [`Choice::None`].
    /// **Anything else that cannot be read is [`Choice::Blur`]:** the person
    /// selected something other than none, and the safe reading of an
    /// unreadable background is one that still hides the room.
    #[must_use]
    pub fn from_stored(stored: &str) -> Self {
        match stored {
            "" | "none" => Self::None,
            "blur" => Self::Blur,
            "strong-blur" => Self::StrongBlur,
            other => {
                if let Some(id) = other.strip_prefix("builtin:") {
                    if let Some(found) = BUILTIN_IDS.iter().find(|b| **b == id) {
                        return Self::Builtin(found);
                    }
                } else if let Some(id) = other.strip_prefix("photo:") {
                    if is_photo_id(id) {
                        return Self::Photo(id.to_owned());
                    }
                }
                Self::Blur
            },
        }
    }

    /// The text to store.
    #[must_use]
    pub fn to_stored(&self) -> String {
        match self {
            Self::None => "none".to_owned(),
            Self::Blur => "blur".to_owned(),
            Self::StrongBlur => "strong-blur".to_owned(),
            Self::Builtin(id) => format!("builtin:{id}"),
            Self::Photo(id) => format!("photo:{id}"),
        }
    }

    /// Whether this choice replaces what is behind the person (anything but
    /// [`Choice::None`]).
    #[must_use]
    pub const fn is_background(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// An own photo's id: 1 to 64 characters of `[A-Za-z0-9_-]`, so it is safe as
/// a file name and a storage key on every client.
#[must_use]
pub fn is_photo_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_PHOTO_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The state of the background processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorState {
    /// The model is loading or has not produced its first mask.
    Starting,
    /// Masks are arriving in time.
    Working,
    /// The model could not load, the processing failed, or it stayed too slow.
    /// Final until [`HealthWatch::restart`].
    Failed,
}

/// What a client does with one camera frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAction {
    /// No background is selected: send the camera frame as it is.
    SendCamera,
    /// Send the frame with the background applied.
    SendProcessed,
    /// Send nothing for this frame.
    Drop,
    /// Send nothing, turn the camera off and show the failure line.
    DropAndStopCamera,
}

/// The frame gate (ADR-0089 section 3).
#[must_use]
pub const fn frame_action(background_selected: bool, state: ProcessorState) -> FrameAction {
    if !background_selected {
        return FrameAction::SendCamera;
    }
    match state {
        ProcessorState::Starting => FrameAction::Drop,
        ProcessorState::Working => FrameAction::SendProcessed,
        ProcessorState::Failed => FrameAction::DropAndStopCamera,
    }
}

/// How long the model may take to produce its first mask, from
/// [`HealthWatch::new`] or [`HealthWatch::restart`]. Chosen, not measured:
/// generous, since the first use loads the model.
pub const START_TIMEOUT_MS: u64 = 15_000;

/// How long camera frames may keep arriving with no new mask before the
/// processing counts as stopped. Chosen, not measured.
pub const STALL_MS: u64 = 2_000;

/// The average time per mask, over the last [`SLOW_WINDOW`] masks, above
/// which this device counts as too slow to keep up (fewer than four masks a
/// second; the edge would trail the person). Chosen, not measured.
pub const SLOW_MS: u32 = 250;

/// How many masks the slowness average covers.
pub const SLOW_WINDOW: usize = 30;

/// Decides the [`ProcessorState`] from what the pipeline reports: each camera
/// frame, each mask and how long it took, each error. Time is the caller's
/// monotonic clock in milliseconds; it holds no clock of its own.
#[derive(Debug, Clone)]
pub struct HealthWatch {
    state: ProcessorState,
    since_ms: u64,
    last_mask_ms: Option<u64>,
    took: [u32; SLOW_WINDOW],
    took_len: usize,
    took_next: usize,
}

impl HealthWatch {
    /// A watch in [`ProcessorState::Starting`] from `now_ms`.
    #[must_use]
    pub const fn new(now_ms: u64) -> Self {
        Self {
            state: ProcessorState::Starting,
            since_ms: now_ms,
            last_mask_ms: None,
            took: [0; SLOW_WINDOW],
            took_len: 0,
            took_next: 0,
        }
    }

    /// Starts over, including after a failure: the person chose a background
    /// again, or turned the camera back on.
    pub fn restart(&mut self, now_ms: u64) {
        *self = Self::new(now_ms);
    }

    /// The current state.
    #[must_use]
    pub const fn state(&self) -> ProcessorState {
        self.state
    }

    /// A camera frame arrived. Returns the state to judge it by: a start that
    /// took too long, or masks that stopped while frames keep coming, fail
    /// here.
    pub fn on_frame(&mut self, now_ms: u64) -> ProcessorState {
        match self.state {
            ProcessorState::Starting => {
                if now_ms.saturating_sub(self.since_ms) > START_TIMEOUT_MS {
                    self.state = ProcessorState::Failed;
                }
            },
            ProcessorState::Working => {
                let last = self.last_mask_ms.unwrap_or(self.since_ms);
                if now_ms.saturating_sub(last) > STALL_MS {
                    self.state = ProcessorState::Failed;
                }
            },
            ProcessorState::Failed => {},
        }
        self.state
    }

    /// A mask was produced, taking `took_ms`. Moves a starting watch to
    /// working; fails one whose recent masks are too slow on average.
    pub fn on_mask(&mut self, now_ms: u64, took_ms: u32) -> ProcessorState {
        if self.state == ProcessorState::Failed {
            return self.state;
        }
        self.last_mask_ms = Some(now_ms);
        self.took[self.took_next] = took_ms;
        self.took_next = (self.took_next + 1) % SLOW_WINDOW;
        self.took_len = (self.took_len + 1).min(SLOW_WINDOW);
        self.state = ProcessorState::Working;
        if self.took_len == SLOW_WINDOW {
            let total: u64 = self.took.iter().map(|&t| u64::from(t)).sum();
            if total > u64::from(SLOW_MS) * SLOW_WINDOW as u64 {
                self.state = ProcessorState::Failed;
            }
        }
        self.state
    }

    /// The model could not load or a run failed.
    pub fn on_error(&mut self) -> ProcessorState {
        self.state = ProcessorState::Failed;
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn the_gate_table() {
        use FrameAction as A;
        use ProcessorState as S;
        for s in [S::Starting, S::Working, S::Failed] {
            assert_eq!(frame_action(false, s), A::SendCamera);
        }
        assert_eq!(frame_action(true, S::Starting), A::Drop);
        assert_eq!(frame_action(true, S::Working), A::SendProcessed);
        assert_eq!(frame_action(true, S::Failed), A::DropAndStopCamera);
    }

    #[test]
    fn stored_choices_round_trip() {
        let all = [
            Choice::None,
            Choice::Blur,
            Choice::StrongBlur,
            Choice::Builtin("dusk"),
            Choice::Photo("a1_B-2".to_owned()),
        ];
        for c in all {
            assert_eq!(Choice::from_stored(&c.to_stored()), c);
        }
        for id in BUILTIN_IDS {
            let c = Choice::from_stored(&format!("builtin:{id}"));
            assert_eq!(c, Choice::Builtin(id));
        }
    }

    #[test]
    fn an_unreadable_choice_still_hides_the_room() {
        assert_eq!(Choice::from_stored(""), Choice::None);
        assert_eq!(Choice::from_stored("none"), Choice::None);
        for bad in [
            "builtin:",
            "builtin:sunset",
            "photo:",
            "photo:../x",
            "Blur",
            "x",
        ] {
            assert_eq!(Choice::from_stored(bad), Choice::Blur, "{bad}");
        }
        let long = format!("photo:{}", "a".repeat(65));
        assert_eq!(Choice::from_stored(&long), Choice::Blur);
    }

    #[test]
    fn starts_works_and_stalls() {
        let mut w = HealthWatch::new(1_000);
        assert_eq!(w.on_frame(1_100), ProcessorState::Starting);
        assert_eq!(w.on_mask(1_200, 40), ProcessorState::Working);
        assert_eq!(w.on_frame(1_300), ProcessorState::Working);
        assert_eq!(w.on_frame(1_200 + STALL_MS), ProcessorState::Working);
        assert_eq!(w.on_frame(1_201 + STALL_MS), ProcessorState::Failed);
        // Final: a late mask does not bring it back.
        assert_eq!(w.on_mask(9_000, 10), ProcessorState::Failed);
        w.restart(10_000);
        assert_eq!(w.state(), ProcessorState::Starting);
    }

    #[test]
    fn a_start_that_never_ends_fails() {
        let mut w = HealthWatch::new(0);
        assert_eq!(w.on_frame(START_TIMEOUT_MS), ProcessorState::Starting);
        assert_eq!(w.on_frame(START_TIMEOUT_MS + 1), ProcessorState::Failed);
    }

    #[test]
    fn a_slow_device_fails_after_a_full_window() {
        let mut w = HealthWatch::new(0);
        for i in 0..SLOW_WINDOW - 1 {
            assert_eq!(w.on_mask(i as u64, SLOW_MS + 50), ProcessorState::Working);
        }
        assert_eq!(w.on_mask(100, SLOW_MS + 50), ProcessorState::Failed);
        let mut ok = HealthWatch::new(0);
        for i in 0..SLOW_WINDOW * 3 {
            assert_eq!(ok.on_mask(i as u64, SLOW_MS), ProcessorState::Working);
        }
    }

    #[test]
    fn an_error_fails() {
        let mut w = HealthWatch::new(0);
        assert_eq!(w.on_error(), ProcessorState::Failed);
        assert_eq!(
            frame_action(true, w.on_frame(1)),
            FrameAction::DropAndStopCamera
        );
    }

    #[derive(Debug, Clone)]
    enum Event {
        Frame(u64),
        Mask(u64, u32),
        Error,
        Restart(u64),
    }

    fn event() -> impl Strategy<Value = Event> {
        prop_oneof![
            (0u64..100_000).prop_map(Event::Frame),
            (0u64..100_000, 0u32..1_000).prop_map(|(t, d)| Event::Mask(t, d)),
            Just(Event::Error),
            (0u64..100_000).prop_map(Event::Restart),
        ]
    }

    proptest! {
        /// Whatever happens, a frame judged with a background selected is
        /// either processed or dropped; never the raw camera.
        #[test]
        fn a_selected_background_never_sends_the_camera(
            events in proptest::collection::vec(event(), 0..200)
        ) {
            let mut w = HealthWatch::new(0);
            for e in events {
                let s = match e {
                    Event::Frame(t) => w.on_frame(t),
                    Event::Mask(t, d) => w.on_mask(t, d),
                    Event::Error => w.on_error(),
                    Event::Restart(t) => { w.restart(t); w.state() }
                };
                prop_assert_ne!(frame_action(true, s), FrameAction::SendCamera);
                if s == ProcessorState::Failed {
                    prop_assert_eq!(w.on_mask(0, 0), ProcessorState::Failed);
                }
            }
        }
    }
}
