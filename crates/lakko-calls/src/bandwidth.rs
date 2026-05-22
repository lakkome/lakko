// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Bandwidth adaptation + simulcast/SVC layer selection
//! (Phase 3c7).
//!
//! State machine that consumes [`crate::quality::QualitySample`]
//! inputs and emits [`LayerSwitch`] recommendations — which
//! simulcast / SVC layer the platform should subscribe to
//! per remote participant. Pure-data layer; the actual
//! SFU subscription RPC lives in the platform's WebRTC pod.
//!
//! ## Three-tier simulcast ladder
//!
//! Lakko publishes every video stream at three resolutions
//! by default (matching standard WebRTC simulcast cadence):
//!
//! | Layer | Resolution | Target bitrate |
//! |---|---|---|
//! | [`SimulcastLayer::High`]   | 1080p @ 30fps | ~2.5 Mbps |
//! | [`SimulcastLayer::Medium`] | 540p @ 30fps  | ~750 kbps |
//! | [`SimulcastLayer::Low`]    | 270p @ 15fps  | ~150 kbps |
//!
//! A receiver subscribes to one layer per remote
//! participant — typically `High` for the pinned/active
//! speaker, `Medium` for visible gallery tiles, `Low` for
//! off-screen / minimised tiles.
//!
//! ## Why a hysteresis state machine
//!
//! Adapting on every single [`QualitySample`] would
//! ping-pong the layer choice at the boundaries (a sample
//! at 4.05 MOS picks Medium, the next at 4.02 picks Low,
//! the next at 4.06 picks Medium again, etc.). The SFU
//! subscription RPC has a non-trivial round-trip cost plus
//! a frame-decode discontinuity each switch — a tens-of-
//! seconds period of stable choice matters more than picking
//! the theoretically-optimal layer every 100 ms.
//!
//! [`BandwidthController`] applies a **two-sample
//! confirmation rule**: a downgrade requires two consecutive
//! samples below the lower threshold; an upgrade requires
//! two consecutive samples above the upper threshold. Single-
//! sample blips don't move the layer.
//!
//! ## Pinning policy
//!
//! Independent of network state, the platform can pin a
//! layer for a specific remote (the "active speaker" /
//! pinned-tile UX). [`BandwidthController::set_pin`] freezes
//! that participant's layer at the pinned value; quality
//! samples still accumulate but the layer stays.
//!
//! ## What this module does NOT do
//!
//! - Talk to the SFU. The output [`LayerSwitch`]
//!   recommendation is consumed by the platform's WebRTC
//!   pod which makes the actual subscription RPC.
//! - Encode-side adaptation. The publisher adjusts its
//!   Opus bitrate via [`crate::quality::recommend_bitrate`];
//!   this module adapts the RECEIVE side's subscription
//!   only.
//! - SVC-specific layer math. SVC (AV1/VP9 supports) uses
//!   the same tier names — the SFU runs the equivalent
//!   selective forwarding for SVC's temporal-spatial layers.

use crate::quality::{QualityLevel, QualityScore};

/// Three simulcast layers Lakko publishes. Discriminant is
/// the canonical numeric encoding (low=0, mid=1, hi=2) the
/// SFU subscription RPC uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SimulcastLayer {
    /// 270p @ 15fps, ~150 kbps. For off-screen / minimised
    /// tiles.
    Low = 0,
    /// 540p @ 30fps, ~750 kbps. For gallery tiles.
    Medium = 1,
    /// 1080p @ 30fps, ~2.5 Mbps. For pinned / active speaker.
    High = 2,
}

impl SimulcastLayer {
    /// Short name for diagnostics / logging.
    #[must_use]
    pub const fn short_name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    /// Target bitrate in bits per second the publisher
    /// allocates to this layer. Used by the SFU to size
    /// receive buffers.
    #[must_use]
    pub const fn target_bps(self) -> u32 {
        match self {
            Self::Low => 150_000,
            Self::Medium => 750_000,
            Self::High => 2_500_000,
        }
    }

    /// One step down toward [`Self::Low`]. Returns `None`
    /// when already at the floor — the caller treats this
    /// as "stay put".
    #[must_use]
    pub const fn step_down(self) -> Option<Self> {
        match self {
            Self::High => Some(Self::Medium),
            Self::Medium => Some(Self::Low),
            Self::Low => None,
        }
    }

    /// One step up toward [`Self::High`]. Returns `None`
    /// when already at the ceiling.
    #[must_use]
    pub const fn step_up(self) -> Option<Self> {
        match self {
            Self::Low => Some(Self::Medium),
            Self::Medium => Some(Self::High),
            Self::High => None,
        }
    }
}

/// Output of [`BandwidthController::observe`] — what the
/// platform should do about the subscription for this
/// participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerSwitch {
    /// No change — current layer is correct for the
    /// observed quality.
    Hold,
    /// Step the subscription down to a lower layer.
    DownTo(SimulcastLayer),
    /// Step the subscription up to a higher layer.
    UpTo(SimulcastLayer),
}

/// Per-participant adaptation state. Tracks the current
/// layer + two-sample confirmation counters for upgrade /
/// downgrade decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandwidthController {
    /// Layer the SFU currently forwards for this
    /// participant. Initialised to [`SimulcastLayer::High`]
    /// for pinned tiles, [`SimulcastLayer::Medium`] for
    /// gallery tiles — the caller picks based on UI state.
    current: SimulcastLayer,
    /// `Some(layer)` while the user has pinned this
    /// participant. Pinned controllers ignore network
    /// signals and stay at the pinned layer.
    pinned: Option<SimulcastLayer>,
    /// Number of consecutive samples below the downgrade
    /// threshold. Resets on any sample at or above the
    /// hold band.
    bad_streak: u8,
    /// Number of consecutive samples above the upgrade
    /// threshold. Resets on any sample at or below the
    /// hold band.
    good_streak: u8,
}

/// Streak length the controller requires before acting on
/// the observation. Two samples = ~2 seconds at the
/// default 1Hz quality-sampling cadence — long enough to
/// ignore single-frame blips, short enough to react to
/// real congestion.
pub const CONFIRMATION_STREAK: u8 = 2;

impl BandwidthController {
    /// Initial state. The caller picks the starting layer
    /// based on UI hint (pinned → [`SimulcastLayer::High`],
    /// gallery → [`SimulcastLayer::Medium`]).
    #[must_use]
    pub const fn new(initial: SimulcastLayer) -> Self {
        Self {
            current: initial,
            pinned: None,
            bad_streak: 0,
            good_streak: 0,
        }
    }

    /// Current layer the SFU forwards.
    #[must_use]
    pub const fn current_layer(&self) -> SimulcastLayer {
        self.current
    }

    /// Whether the user has pinned this participant.
    #[must_use]
    pub const fn is_pinned(&self) -> bool {
        self.pinned.is_some()
    }

    /// Pin the layer for this participant. Subsequent
    /// [`Self::observe`] calls return [`LayerSwitch::Hold`]
    /// while the pin is active.
    pub fn set_pin(&mut self, layer: SimulcastLayer) {
        self.pinned = Some(layer);
        self.current = layer;
        self.bad_streak = 0;
        self.good_streak = 0;
    }

    /// Clear the pin. Subsequent observations resume normal
    /// adaptation from the current layer.
    pub fn clear_pin(&mut self) {
        self.pinned = None;
    }

    /// Fold a quality observation into the controller.
    /// Returns the recommended action — [`LayerSwitch::Hold`]
    /// in the common case.
    pub fn observe(&mut self, score: QualityScore) -> LayerSwitch {
        // Pinned participants ignore network signals — the
        // user explicitly chose this layer.
        if self.pinned.is_some() {
            return LayerSwitch::Hold;
        }

        match score.level {
            QualityLevel::Bad | QualityLevel::Poor => {
                // Bad sample — accumulate the downgrade
                // streak, reset the upgrade streak.
                self.bad_streak = self.bad_streak.saturating_add(1);
                self.good_streak = 0;
                if self.bad_streak >= CONFIRMATION_STREAK {
                    if let Some(next) = self.current.step_down() {
                        self.current = next;
                        self.bad_streak = 0;
                        return LayerSwitch::DownTo(next);
                    }
                    // Already at floor; reset the streak so
                    // a future bounce-back can act
                    // immediately.
                    self.bad_streak = 0;
                }
                LayerSwitch::Hold
            },
            QualityLevel::Excellent => {
                // Good sample — accumulate upgrade streak.
                self.good_streak = self.good_streak.saturating_add(1);
                self.bad_streak = 0;
                if self.good_streak >= CONFIRMATION_STREAK {
                    if let Some(next) = self.current.step_up() {
                        self.current = next;
                        self.good_streak = 0;
                        return LayerSwitch::UpTo(next);
                    }
                    self.good_streak = 0;
                }
                LayerSwitch::Hold
            },
            QualityLevel::Good | QualityLevel::Fair => {
                // Middle band — current layer is fine; reset
                // both streaks so a future drift accumulates
                // cleanly from zero.
                self.bad_streak = 0;
                self.good_streak = 0;
                LayerSwitch::Hold
            },
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn score_level(level: QualityLevel) -> QualityScore {
        // Pick a representative MOS for each level. The
        // controller dispatches on `level`, not `mos`, so the
        // exact MOS value doesn't matter — but pick a value
        // inside the band so a future refactor that switches
        // to MOS dispatch surfaces here.
        let (mos, r_factor) = match level {
            QualityLevel::Excellent => (4.5, 95.0),
            QualityLevel::Good => (4.15, 85.0),
            QualityLevel::Fair => (3.8, 75.0),
            QualityLevel::Poor => (3.3, 65.0),
            QualityLevel::Bad => (2.8, 50.0),
        };
        QualityScore {
            r_factor,
            mos,
            level,
        }
    }

    // -----------------------------------------------------------
    // SimulcastLayer ladder
    // -----------------------------------------------------------

    #[test]
    fn layer_ordinals_match_ord_ranking() {
        assert!(SimulcastLayer::Low < SimulcastLayer::Medium);
        assert!(SimulcastLayer::Medium < SimulcastLayer::High);
    }

    #[test]
    fn step_down_from_high_lands_on_medium() {
        assert_eq!(
            SimulcastLayer::High.step_down(),
            Some(SimulcastLayer::Medium)
        );
        assert_eq!(
            SimulcastLayer::Medium.step_down(),
            Some(SimulcastLayer::Low)
        );
        assert_eq!(SimulcastLayer::Low.step_down(), None, "floor — None");
    }

    #[test]
    fn step_up_from_low_lands_on_medium() {
        assert_eq!(SimulcastLayer::Low.step_up(), Some(SimulcastLayer::Medium));
        assert_eq!(SimulcastLayer::Medium.step_up(), Some(SimulcastLayer::High));
        assert_eq!(SimulcastLayer::High.step_up(), None, "ceiling — None");
    }

    #[test]
    fn target_bps_documented() {
        // Pin the bitrate ladder. The SFU sizes receive
        // buffers off these values; drift here causes
        // under-provisioned buffers on borderline links.
        assert_eq!(SimulcastLayer::Low.target_bps(), 150_000);
        assert_eq!(SimulcastLayer::Medium.target_bps(), 750_000);
        assert_eq!(SimulcastLayer::High.target_bps(), 2_500_000);
    }

    // -----------------------------------------------------------
    // BandwidthController — hysteresis state machine
    // -----------------------------------------------------------

    #[test]
    fn initial_observation_is_hold() {
        let mut ctrl = BandwidthController::new(SimulcastLayer::Medium);
        let action = ctrl.observe(score_level(QualityLevel::Good));
        assert_eq!(action, LayerSwitch::Hold);
        assert_eq!(ctrl.current_layer(), SimulcastLayer::Medium);
    }

    #[test]
    fn single_bad_sample_does_not_downgrade() {
        // Hysteresis pin: one bad blip is not enough to
        // act. Must be CONFIRMATION_STREAK consecutive.
        let mut ctrl = BandwidthController::new(SimulcastLayer::High);
        let action = ctrl.observe(score_level(QualityLevel::Bad));
        assert_eq!(action, LayerSwitch::Hold);
        assert_eq!(ctrl.current_layer(), SimulcastLayer::High);
    }

    #[test]
    fn two_consecutive_bad_samples_trigger_downgrade() {
        let mut ctrl = BandwidthController::new(SimulcastLayer::High);
        let _ = ctrl.observe(score_level(QualityLevel::Bad));
        let action = ctrl.observe(score_level(QualityLevel::Bad));
        assert_eq!(action, LayerSwitch::DownTo(SimulcastLayer::Medium));
        assert_eq!(ctrl.current_layer(), SimulcastLayer::Medium);
    }

    #[test]
    fn good_sample_between_bad_resets_streak() {
        // 1 bad → 1 good (middle band, resets) → 1 bad →
        // still only 1 bad in the streak, no action.
        let mut ctrl = BandwidthController::new(SimulcastLayer::High);
        let _ = ctrl.observe(score_level(QualityLevel::Bad));
        let _ = ctrl.observe(score_level(QualityLevel::Good));
        let action = ctrl.observe(score_level(QualityLevel::Bad));
        assert_eq!(action, LayerSwitch::Hold);
        assert_eq!(ctrl.current_layer(), SimulcastLayer::High);
    }

    #[test]
    fn downgrade_chain_stops_at_low() {
        // Floor — even continuous bad samples don't go
        // below Low.
        let mut ctrl = BandwidthController::new(SimulcastLayer::Low);
        let _ = ctrl.observe(score_level(QualityLevel::Bad));
        let action = ctrl.observe(score_level(QualityLevel::Bad));
        assert_eq!(action, LayerSwitch::Hold);
        assert_eq!(ctrl.current_layer(), SimulcastLayer::Low);
    }

    #[test]
    fn two_consecutive_excellent_samples_trigger_upgrade() {
        let mut ctrl = BandwidthController::new(SimulcastLayer::Low);
        let _ = ctrl.observe(score_level(QualityLevel::Excellent));
        let action = ctrl.observe(score_level(QualityLevel::Excellent));
        assert_eq!(action, LayerSwitch::UpTo(SimulcastLayer::Medium));
        assert_eq!(ctrl.current_layer(), SimulcastLayer::Medium);
    }

    #[test]
    fn upgrade_chain_stops_at_high() {
        let mut ctrl = BandwidthController::new(SimulcastLayer::High);
        let _ = ctrl.observe(score_level(QualityLevel::Excellent));
        let action = ctrl.observe(score_level(QualityLevel::Excellent));
        assert_eq!(action, LayerSwitch::Hold);
        assert_eq!(ctrl.current_layer(), SimulcastLayer::High);
    }

    // -----------------------------------------------------------
    // Pinning
    // -----------------------------------------------------------

    #[test]
    fn pinned_participant_ignores_bad_samples() {
        let mut ctrl = BandwidthController::new(SimulcastLayer::Medium);
        ctrl.set_pin(SimulcastLayer::High);
        let _ = ctrl.observe(score_level(QualityLevel::Bad));
        let action = ctrl.observe(score_level(QualityLevel::Bad));
        assert_eq!(action, LayerSwitch::Hold);
        assert_eq!(ctrl.current_layer(), SimulcastLayer::High);
        assert!(ctrl.is_pinned());
    }

    #[test]
    fn clear_pin_resumes_adaptation() {
        let mut ctrl = BandwidthController::new(SimulcastLayer::Medium);
        ctrl.set_pin(SimulcastLayer::High);
        ctrl.clear_pin();
        assert!(!ctrl.is_pinned());
        let _ = ctrl.observe(score_level(QualityLevel::Bad));
        let action = ctrl.observe(score_level(QualityLevel::Bad));
        assert_eq!(action, LayerSwitch::DownTo(SimulcastLayer::Medium));
    }

    #[test]
    fn confirmation_streak_constant_is_two() {
        // Pin the documented hysteresis constant. Bumping
        // it (e.g. to 3) reduces ping-pong further but
        // slows reaction; bumping down (to 1) makes the
        // controller jittery.
        assert_eq!(CONFIRMATION_STREAK, 2);
    }

    // -----------------------------------------------------------
    // Mid-band behaviour
    // -----------------------------------------------------------

    #[test]
    fn good_and_fair_observations_hold_without_streak() {
        let mut ctrl = BandwidthController::new(SimulcastLayer::Medium);
        // Long string of mid-band samples → no action,
        // streaks stay at 0.
        for _ in 0..10 {
            let action = ctrl.observe(score_level(QualityLevel::Good));
            assert_eq!(action, LayerSwitch::Hold);
        }
        for _ in 0..10 {
            let action = ctrl.observe(score_level(QualityLevel::Fair));
            assert_eq!(action, LayerSwitch::Hold);
        }
        assert_eq!(ctrl.current_layer(), SimulcastLayer::Medium);
    }
}
