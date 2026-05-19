// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Call quality metrics — Phase 3a11.
//!
//! Two responsibilities:
//!
//! 1. **MOS estimation** from a network/RTP stats sample
//!    (round-trip time, jitter, packet-loss percentage,
//!    codec hint). Implements a simplified E-model
//!    (ITU-T G.107) with Opus / non-Opus impairment
//!    coefficients.
//! 2. **Adaptive-bitrate recommendation** — given the
//!    current encoder bitrate + a fresh sample, recommend
//!    a new bitrate. Linear stepwise control: decrement
//!    aggressively on loss, increment gently when the
//!    channel is clean.
//!
//! The crate stays **wall-clock-free and stats-agnostic** —
//! the platform layer is responsible for sampling
//! `RTCStatsReport` (web) /  `RTCStatistics` (iOS) /
//! `RTCStatsCollector` (Android) on its own schedule and
//! feeding [`QualitySample`] instances in. The math here
//! has no I/O.
//!
//! ## Why a simplified E-model
//!
//! ITU-T G.107 in full requires expectation-factor `A`,
//! advantage-factor calibration, and a dozen tuning
//! parameters. For a call-quality UX indicator we only
//! need monotonicity, sane bounds, and codec-awareness.
//! The simplified model used here:
//!
//! ```text
//!   R = R0 - Id - Ie_eff
//!
//!     R0      = 93.2                       (max R-factor, AAC-LC baseline)
//!     Id      = delay impairment           (round-trip + jitter)
//!     Ie_eff  = effective equipment        (codec-dependent + loss)
//!
//!   MOS = 1.0                              when R <  6.5
//!       = 1 + 0.035 R + R(R-60)(100-R) * 7e-6  when 6.5 ≤ R ≤ 100
//!       = 4.5                              when R > 100 (clamped)
//! ```
//!
//! Opus is treated as transparent at zero loss
//! (`Ie = 0`) — at 32 kbps+ it's perceptually
//! indistinguishable from the source — and its FEC +
//! LBRR mechanism encodes as a large `Bpl = 25` so the
//! per-loss-percent degradation curve is much flatter than
//! the no-FEC baseline (`G.711`-like: `Ie = 0`,
//! `Bpl = 4.3` per ITU-T G.113 Appendix I).
//!
//! ## Adaptive bitrate
//!
//! Bitrate control is a classic step-controller:
//!
//! - loss > 5 % → decrement by 20 % (clamped to codec floor)
//! - loss < 1 % AND MOS > 4.0 → increment by 10 % (capped at codec ceiling)
//! - otherwise → hold
//!
//! Hysteresis is one sample. A production stack typically
//! adds a 3-of-5 majority vote across samples; the platform
//! layer can wrap this primitive with whatever smoothing
//! suits its sample cadence.

use core::cmp::Ordering;

// =============================================================================
// Types
// =============================================================================

/// Audio codec hint for the loss-impairment curve. Defaults
/// to [`AudioCodec::Other`] when the caller doesn't know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// Opus (RFC 6716) — FEC-capable, narrow PLC
    /// degradation. The default for Lakko's voice / video
    /// call audio path (Phase 3a6).
    Opus,
    /// G.711, ISAC, AAC-LC, or any other non-Opus codec.
    /// Treated as no-FEC for impairment math.
    Other,
}

/// Discretised quality bucket. Cheap UX indicator the
/// caller flips into "Excellent / Good / Fair / Poor / Bad"
/// glyphs without re-computing the MOS threshold table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityLevel {
    /// MOS ≥ 4.3 — toll-quality, no observable degradation.
    Excellent,
    /// 4.0 ≤ MOS < 4.3 — satisfaction, occasional minor
    /// artefacts.
    Good,
    /// 3.6 ≤ MOS < 4.0 — perceptible artefacts; most users
    /// continue without complaint.
    Fair,
    /// 3.1 ≤ MOS < 3.6 — many users would complain.
    Poor,
    /// MOS < 3.1 — nearly all users dissatisfied; call may
    /// be near-unusable.
    Bad,
}

/// Single sample of the network channel as seen by the
/// receiver. The platform layer constructs one of these
/// per stats poll (typical cadence: 1–2 Hz) and feeds it
/// into [`estimate_quality`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualitySample {
    /// Round-trip time in milliseconds. Sources:
    /// `RTCIceCandidatePairStats.currentRoundTripTime`
    /// (web), `RTCIceCandidatePairStats.currentRoundTripTime`
    /// (iOS), `RTCIceCandidatePairStats.currentRoundTripTime`
    /// (Android) — all the same canonical field.
    pub rtt_ms: u32,
    /// Inter-arrival jitter in milliseconds. Source:
    /// `RTCInboundRtpStreamStats.jitter` (seconds → ms).
    pub jitter_ms: u32,
    /// Packet-loss percentage as a fraction in `[0.0, 100.0]`.
    /// Source: `100 * packetsLost / (packetsReceived + packetsLost)`.
    pub loss_pct: f32,
    /// Codec hint for the loss-impairment math.
    pub codec: AudioCodec,
}

/// Output of [`estimate_quality`]. Carries both the raw
/// R-factor / MOS pair (for callers that want to do their
/// own UX mapping) and the discretised [`QualityLevel`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityScore {
    /// E-model R-factor in `[0.0, 100.0]`. Higher is
    /// better. Caller-visible only for power users; UX
    /// surfaces should consume `level` / `mos` instead.
    pub r_factor: f32,
    /// Mean Opinion Score in `[1.0, 4.5]` (4.5 is the
    /// theoretical max under the simplified mapping;
    /// real-world toll-quality calls land ≈ 4.4).
    pub mos: f32,
    /// Discretised bucket.
    pub level: QualityLevel,
}

// =============================================================================
// Constants
// =============================================================================

/// Maximum possible R-factor on this codec (clean channel,
/// no delay, no loss). AAC-LC reference baseline; Opus
/// matches it after Lakko's loss-curve compensation.
const R_MAX: f32 = 93.2;

/// One-way delay above which the per-millisecond delay
/// impairment slope kicks in (ITU-T G.107 §A.2.1).
const DELAY_KNEE_MS: f32 = 177.3;

// Opus impairment coefficients. We treat high-bitrate
// Opus as **transparent at zero loss** (`Ie = 0`) — at
// 32 kbps+ Opus is perceptually indistinguishable from
// the source. The FEC + LBRR mechanism encodes as a
// large `Bpl` so the per-loss-percent degradation curve
// is flatter than the no-FEC baseline.
const OPUS_IE: f32 = 0.0;
const OPUS_BPL: f32 = 25.0;

// Non-Opus baseline (G.711 µ-law per ITU-T G.113
// Appendix I). No FEC, packet loss hurts hard.
const OTHER_IE: f32 = 0.0;
const OTHER_BPL: f32 = 4.3;

/// Opus encoder bitrate range (RFC 6716 §2.1.1). 8 kbps
/// floor matches narrowband mode; 510 kbps ceiling matches
/// fullband stereo.
pub const OPUS_MIN_BITRATE_BPS: u32 = 8_000;
/// Upper limit for adaptive-bitrate recommendations.
pub const OPUS_MAX_BITRATE_BPS: u32 = 510_000;

// Loss thresholds for the bitrate step-controller.
const LOSS_DECREASE_THRESHOLD_PCT: f32 = 5.0;
const LOSS_INCREASE_THRESHOLD_PCT: f32 = 1.0;
const MOS_INCREASE_THRESHOLD: f32 = 4.0;

// Step sizes — kept conservative so the step-controller
// doesn't oscillate. A production stack typically wraps
// this with EWMA / vote across N samples.
const BITRATE_DECREASE_FACTOR: f32 = 0.8;
const BITRATE_INCREASE_FACTOR: f32 = 1.1;

// =============================================================================
// Public API
// =============================================================================

impl QualitySample {
    /// Convenience — a zero-impairment baseline sample.
    /// Used by tests + as the "before any RTP arrives"
    /// initial value.
    #[must_use]
    pub const fn ideal(codec: AudioCodec) -> Self {
        Self {
            rtt_ms: 0,
            jitter_ms: 0,
            loss_pct: 0.0,
            codec,
        }
    }
}

/// Estimate call quality from a single network sample.
///
/// Monotonicity guarantees:
///
/// - Increasing `rtt_ms` (others fixed) → never raises MOS.
/// - Increasing `jitter_ms` (others fixed) → never raises MOS.
/// - Increasing `loss_pct` (others fixed) → never raises MOS.
///
/// Bounds: `1.0 ≤ mos ≤ 4.5`, `0.0 ≤ r_factor ≤ 100.0`.
#[must_use]
pub fn estimate_quality(sample: QualitySample) -> QualityScore {
    // Effective one-way delay = half RTT + jitter (jitter
    // models additional buffering needed on the receive
    // side).
    let one_way_delay_ms =
        (f32::from(u16::try_from(sample.rtt_ms.min(u32::from(u16::MAX))).unwrap_or(u16::MAX))
            / 2.0)
            + f32::from(
                u16::try_from(sample.jitter_ms.min(u32::from(u16::MAX))).unwrap_or(u16::MAX),
            );

    let id = delay_impairment(one_way_delay_ms);
    let (ie, bpl) = match sample.codec {
        AudioCodec::Opus => (OPUS_IE, OPUS_BPL),
        AudioCodec::Other => (OTHER_IE, OTHER_BPL),
    };
    let ie_eff = effective_equipment_impairment(ie, bpl, sample.loss_pct);

    let r = (R_MAX - id - ie_eff).clamp(0.0, 100.0);
    let mos = r_to_mos(r);
    let level = mos_to_level(mos);

    QualityScore {
        r_factor: r,
        mos,
        level,
    }
}

/// Recommend a new encoder bitrate given the current one
/// and a fresh quality sample. Returns the current bitrate
/// unchanged when the sample falls in the "hold" band.
///
/// `current_bps` is clamped to [`OPUS_MIN_BITRATE_BPS`] /
/// [`OPUS_MAX_BITRATE_BPS`] before any step is applied; the
/// returned value is similarly clamped.
///
/// Caller is responsible for smoothing — feed one
/// recommendation per stats poll, optionally consume only
/// the median / majority of N samples.
#[must_use]
pub fn recommend_bitrate(current_bps: u32, sample: QualitySample) -> u32 {
    let clamped = current_bps.clamp(OPUS_MIN_BITRATE_BPS, OPUS_MAX_BITRATE_BPS);

    if sample.loss_pct > LOSS_DECREASE_THRESHOLD_PCT {
        let scaled = f64::from(clamped) * f64::from(BITRATE_DECREASE_FACTOR);
        return scale_to_bitrate(scaled);
    }

    let score = estimate_quality(sample);
    if sample.loss_pct < LOSS_INCREASE_THRESHOLD_PCT && score.mos > MOS_INCREASE_THRESHOLD {
        let scaled = f64::from(clamped) * f64::from(BITRATE_INCREASE_FACTOR);
        return scale_to_bitrate(scaled);
    }

    clamped
}

// =============================================================================
// Internal math
// =============================================================================

fn delay_impairment(one_way_delay_ms: f32) -> f32 {
    let id_base = 0.024 * one_way_delay_ms;
    let above_knee = one_way_delay_ms - DELAY_KNEE_MS;
    let id_excess = if above_knee > 0.0 {
        0.11 * above_knee
    } else {
        0.0
    };
    id_base + id_excess
}

fn effective_equipment_impairment(ie: f32, bpl: f32, loss_pct: f32) -> f32 {
    // ITU-T G.107 §A.2.1.2:
    //   Ie_eff = Ie + (95 - Ie) * Ppl / (Ppl + Bpl)
    let ppl = loss_pct.max(0.0);
    ie + (95.0 - ie) * ppl / (ppl + bpl)
}

fn r_to_mos(r: f32) -> f32 {
    if r < 6.5 {
        1.0
    } else if r > 100.0 {
        4.5
    } else {
        let mos = 1.0 + 0.035 * r + r * (r - 60.0) * (100.0 - r) * 7e-6;
        mos.clamp(1.0, 4.5)
    }
}

fn mos_to_level(mos: f32) -> QualityLevel {
    match mos.partial_cmp(&4.3).unwrap_or(Ordering::Less) {
        Ordering::Greater | Ordering::Equal => return QualityLevel::Excellent,
        Ordering::Less => {},
    }
    if mos >= 4.0 {
        QualityLevel::Good
    } else if mos >= 3.6 {
        QualityLevel::Fair
    } else if mos >= 3.1 {
        QualityLevel::Poor
    } else {
        QualityLevel::Bad
    }
}

fn scale_to_bitrate(scaled: f64) -> u32 {
    let rounded = scaled.round();
    if rounded < f64::from(OPUS_MIN_BITRATE_BPS) {
        return OPUS_MIN_BITRATE_BPS;
    }
    if rounded > f64::from(OPUS_MAX_BITRATE_BPS) {
        return OPUS_MAX_BITRATE_BPS;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let v = rounded as u32;
    v
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ideal_opus_sample_is_excellent() {
        let s = QualitySample::ideal(AudioCodec::Opus);
        let q = estimate_quality(s);
        assert!(q.r_factor > 90.0, "r={}", q.r_factor);
        assert!(q.mos > 4.3, "mos={}", q.mos);
        assert_eq!(q.level, QualityLevel::Excellent);
    }

    #[test]
    fn ideal_other_codec_also_excellent() {
        // No-loss, no-delay sample should be Excellent
        // regardless of codec — the codec matters only on
        // the loss curve.
        let q = estimate_quality(QualitySample::ideal(AudioCodec::Other));
        assert_eq!(q.level, QualityLevel::Excellent);
    }

    #[test]
    fn mos_within_bounds_across_loss_sweep() {
        for loss in 0u16..=100 {
            let s = QualitySample {
                rtt_ms: 50,
                jitter_ms: 5,
                loss_pct: f32::from(loss),
                codec: AudioCodec::Opus,
            };
            let q = estimate_quality(s);
            assert!(
                q.mos >= 1.0 && q.mos <= 4.5,
                "mos={} at loss={}",
                q.mos,
                loss
            );
            assert!(
                q.r_factor >= 0.0 && q.r_factor <= 100.0,
                "r={} at loss={}",
                q.r_factor,
                loss
            );
        }
    }

    #[test]
    fn mos_monotonic_in_loss() {
        let mut prev = f32::INFINITY;
        // Step through 0, 1, 2, … 50 % loss.
        for loss_pct_x10 in 0u16..=500 {
            let loss_pct = f32::from(loss_pct_x10) / 10.0;
            let s = QualitySample {
                rtt_ms: 50,
                jitter_ms: 5,
                loss_pct,
                codec: AudioCodec::Opus,
            };
            let q = estimate_quality(s);
            assert!(
                q.mos <= prev + 1e-4,
                "MOS not monotonic: was {prev}, became {} at loss={loss_pct}",
                q.mos
            );
            prev = q.mos;
        }
    }

    #[test]
    fn mos_monotonic_in_rtt() {
        let mut prev = f32::INFINITY;
        for rtt in (0..=2000).step_by(20) {
            let s = QualitySample {
                rtt_ms: rtt,
                jitter_ms: 0,
                loss_pct: 0.0,
                codec: AudioCodec::Opus,
            };
            let q = estimate_quality(s);
            assert!(
                q.mos <= prev + 1e-4,
                "MOS not monotonic in rtt: was {prev}, became {} at rtt={rtt}",
                q.mos
            );
            prev = q.mos;
        }
    }

    #[test]
    fn mos_monotonic_in_jitter() {
        let mut prev = f32::INFINITY;
        for jitter in (0..=1000).step_by(10) {
            let s = QualitySample {
                rtt_ms: 50,
                jitter_ms: jitter,
                loss_pct: 0.0,
                codec: AudioCodec::Opus,
            };
            let q = estimate_quality(s);
            assert!(
                q.mos <= prev + 1e-4,
                "MOS not monotonic in jitter: was {prev}, became {} at jitter={jitter}",
                q.mos
            );
            prev = q.mos;
        }
    }

    #[test]
    fn opus_more_resilient_than_other_codec_at_same_loss() {
        // Same network conditions; Opus's FEC must score
        // higher than a no-FEC codec at every meaningful
        // loss percentage.
        for loss_pct_x10 in 1u16..=200 {
            let loss_pct = f32::from(loss_pct_x10) / 10.0;
            let opus = estimate_quality(QualitySample {
                rtt_ms: 50,
                jitter_ms: 5,
                loss_pct,
                codec: AudioCodec::Opus,
            });
            let other = estimate_quality(QualitySample {
                rtt_ms: 50,
                jitter_ms: 5,
                loss_pct,
                codec: AudioCodec::Other,
            });
            assert!(
                opus.mos >= other.mos,
                "Opus MOS {} < Other {} at loss={loss_pct}",
                opus.mos,
                other.mos
            );
        }
    }

    #[test]
    fn severe_loss_is_bad() {
        let q = estimate_quality(QualitySample {
            rtt_ms: 50,
            jitter_ms: 5,
            loss_pct: 30.0,
            codec: AudioCodec::Other,
        });
        assert_eq!(q.level, QualityLevel::Bad, "got mos={}", q.mos);
    }

    #[test]
    fn high_delay_degrades_quality() {
        // 1 second RTT — way past the 177 ms knee.
        let q = estimate_quality(QualitySample {
            rtt_ms: 1_000,
            jitter_ms: 0,
            loss_pct: 0.0,
            codec: AudioCodec::Opus,
        });
        // Should be at most Fair under the simplified
        // model; exact bucket depends on tuning but the
        // contract is "noticeably worse than no-delay".
        assert!(matches!(
            q.level,
            QualityLevel::Fair | QualityLevel::Poor | QualityLevel::Bad
        ));
    }

    #[test]
    fn recommend_bitrate_decreases_on_high_loss() {
        let s = QualitySample {
            rtt_ms: 50,
            jitter_ms: 5,
            loss_pct: 10.0,
            codec: AudioCodec::Opus,
        };
        let new_bps = recommend_bitrate(64_000, s);
        assert!(new_bps < 64_000, "expected decrease, got {new_bps}");
        // 64_000 * 0.8 = 51_200, allow ±rounding.
        let diff = new_bps.abs_diff(51_200);
        assert!(diff < 10, "expected near 51_200, got {new_bps}");
    }

    #[test]
    fn recommend_bitrate_increases_on_clean_channel() {
        let s = QualitySample::ideal(AudioCodec::Opus);
        let new_bps = recommend_bitrate(64_000, s);
        assert!(new_bps > 64_000, "expected increase, got {new_bps}");
    }

    #[test]
    fn recommend_bitrate_holds_when_mediocre() {
        // 2 % loss — above the decrease threshold's
        // sibling but below it, AND MOS likely below 4.0
        // → hold.
        let s = QualitySample {
            rtt_ms: 80,
            jitter_ms: 10,
            loss_pct: 2.0,
            codec: AudioCodec::Other,
        };
        let new_bps = recommend_bitrate(64_000, s);
        assert_eq!(new_bps, 64_000);
    }

    #[test]
    fn recommend_bitrate_clamps_to_floor() {
        let s = QualitySample {
            rtt_ms: 50,
            jitter_ms: 5,
            loss_pct: 50.0,
            codec: AudioCodec::Opus,
        };
        // Starting at the floor; decrement clamps.
        let new_bps = recommend_bitrate(OPUS_MIN_BITRATE_BPS, s);
        assert_eq!(new_bps, OPUS_MIN_BITRATE_BPS);
    }

    #[test]
    fn recommend_bitrate_clamps_to_ceiling() {
        let s = QualitySample::ideal(AudioCodec::Opus);
        // Starting at the ceiling; increment clamps.
        let new_bps = recommend_bitrate(OPUS_MAX_BITRATE_BPS, s);
        assert_eq!(new_bps, OPUS_MAX_BITRATE_BPS);
    }

    #[test]
    fn recommend_bitrate_normalises_out_of_range_input() {
        // Caller hands in 1_000 bps (below floor); we
        // normalise to floor before applying any step.
        let new_bps = recommend_bitrate(1_000, QualitySample::ideal(AudioCodec::Opus));
        // 8_000 * 1.1 = 8_800, but the clean-channel
        // increment fires, so we should land between
        // 8_000 and 9_000.
        assert!((OPUS_MIN_BITRATE_BPS..=10_000).contains(&new_bps));
    }
}
