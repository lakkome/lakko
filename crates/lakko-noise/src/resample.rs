// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Moving a capture stream that does not run at 48 kHz to RNNoise's rate and
//! back ([ADR-0088](../../../docs/adr/0088-client-noise-suppression-rnnoise.md),
//! amendment of 2026-11-10).
//!
//! A capture hook can be handed audio at a rate other than 48 kHz: the
//! Pixel 7a's libwebrtc capture stage opened at 16 kHz before its microphone
//! settled at 48 kHz, and the iOS Simulator captures at 16 kHz. Only rates
//! that divide 48 kHz by a whole factor are taken (24, 16, 12 and 8 kHz):
//! the stream is raised by that factor (zeros between samples, then a
//! low-pass filter), cleaned at 48 kHz, filtered again and lowered back by
//! keeping one sample in each factor. Every other rate is refused, and the
//! host passes its audio on unprocessed, as before.
//!
//! The filter is a windowed-sinc low-pass (Blackman window) with its corner
//! at 0.45 of the lower rate, [`TAPS_PER_FACTOR`] taps per unit of the factor
//! plus one. The tap count and the corner are chosen, not measured; what
//! RNNoise does to a voice that reaches it band-limited by the capture rate
//! is not measured either.

/// The rates, in hertz, the suppressor takes, with the factor each is
/// raised by to reach 48 kHz. 48 kHz itself is factor 1 and needs no filter.
pub const SUPPORTED_RATES: [(u32, usize); 5] = [
    (48_000, 1),
    (24_000, 2),
    (16_000, 3),
    (12_000, 4),
    (8_000, 6),
];

/// The largest factor in [`SUPPORTED_RATES`].
pub const MAX_FACTOR: usize = 6;

/// Taps per unit of the factor; a factor-`k` filter has `TAPS_PER_FACTOR * k
/// + 1` taps.
pub(crate) const TAPS_PER_FACTOR: usize = 16;

/// The corner of the low-pass filter, as a fraction of the lower rate.
const CORNER: f64 = 0.45;

/// The factor that raises `rate` to 48 kHz, or `None` for a rate the
/// suppressor does not take.
#[must_use]
pub fn factor_of(rate: u32) -> Option<usize> {
    SUPPORTED_RATES
        .iter()
        .find(|(r, _)| *r == rate)
        .map(|(_, k)| *k)
}

/// The low-pass taps for factor `k` (`k >= 2`), normalised to a sum of 1.
// The counts cast to `f64` are at most `TAPS_PER_FACTOR * MAX_FACTOR + 1`,
// exact in any float; the taps are within [-1, 1], so `f32` holds them.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn lowpass_taps(k: usize) -> Vec<f32> {
    let n = TAPS_PER_FACTOR * k + 1;
    let mid = (n - 1) as f64 / 2.0;
    // Corner in cycles per sample at the HIGH rate.
    let fc = CORNER / k as f64;
    let mut taps: Vec<f64> = (0..n)
        .map(|i| {
            let x = i as f64 - mid;
            let sinc = if x == 0.0 {
                2.0 * fc
            } else {
                (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
            };
            let phase = 2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64;
            let blackman = 0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos();
            sinc * blackman
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    for t in &mut taps {
        *t /= sum;
    }
    taps.into_iter().map(|t| t as f32).collect()
}

/// One FIR filter with its own history. The history is stored twice in a
/// row so the window of the last `taps.len()` samples is always one
/// contiguous slice.
struct Fir {
    taps: Vec<f32>,
    history: Vec<f32>,
    pos: usize,
}

impl Fir {
    fn new(taps: Vec<f32>) -> Self {
        let len = taps.len();
        Self {
            taps,
            history: vec![0.0; 2 * len],
            pos: 0,
        }
    }

    fn clear(&mut self) {
        self.history.iter_mut().for_each(|h| *h = 0.0);
        self.pos = 0;
    }

    /// Feeds one sample and returns the filter's output for it.
    fn push(&mut self, sample: f32) -> f32 {
        let len = self.taps.len();
        self.history[self.pos] = sample;
        self.history[self.pos + len] = sample;
        self.pos = (self.pos + 1) % len;
        // `history[pos..pos + len]` is now oldest-to-newest; the taps are
        // symmetric, so their order does not matter.
        self.history[self.pos..self.pos + len]
            .iter()
            .zip(&self.taps)
            .map(|(h, t)| h * t)
            .sum()
    }
}

/// The raising and lowering filters of one factor.
pub(crate) struct RateAdapter {
    factor: usize,
    up: Fir,
    down: Fir,
}

impl RateAdapter {
    /// The filters for factor `k` (`2..=MAX_FACTOR`). Allocates.
    pub(crate) fn new(k: usize) -> Self {
        Self {
            factor: k,
            up: Fir::new(lowpass_taps(k)),
            down: Fir::new(lowpass_taps(k)),
        }
    }

    pub(crate) fn factor(&self) -> usize {
        self.factor
    }

    /// Forgets the history, for a stream that starts again.
    pub(crate) fn clear(&mut self) {
        self.up.clear();
        self.down.clear();
    }

    /// Raises `input` by the factor into `out` (cleared first; `factor`
    /// output samples per input sample).
    pub(crate) fn raise(&mut self, input: &[f32], out: &mut Vec<f32>) {
        out.clear();
        // At most `MAX_FACTOR`, exact as `f32`.
        #[allow(clippy::cast_precision_loss)]
        let gain = self.factor as f32;
        for &x in input {
            out.push(self.up.push(x * gain));
            for _ in 1..self.factor {
                out.push(self.up.push(0.0));
            }
        }
    }

    /// Lowers `high` by the factor into `output`, which must hold exactly
    /// `high.len() / factor` samples.
    pub(crate) fn lower(&mut self, high: &[f32], output: &mut [f32]) {
        let mut slots = output.iter_mut();
        for (i, &x) in high.iter().enumerate() {
            let y = self.down.push(x);
            if i % self.factor == 0 {
                if let Some(slot) = slots.next() {
                    *slot = y;
                }
            }
        }
    }
}

#[cfg(test)]
// Test signals of at most a few seconds: lengths and rates are exact as `f32`.
#[allow(clippy::cast_precision_loss)]
mod tests {
    use super::*;

    #[test]
    fn only_whole_divisors_of_48k_are_taken() {
        assert_eq!(factor_of(48_000), Some(1));
        assert_eq!(factor_of(24_000), Some(2));
        assert_eq!(factor_of(16_000), Some(3));
        assert_eq!(factor_of(12_000), Some(4));
        assert_eq!(factor_of(8_000), Some(6));
        assert_eq!(factor_of(44_100), None);
        assert_eq!(factor_of(32_000), None);
        assert_eq!(factor_of(0), None);
    }

    #[test]
    fn taps_are_symmetric_and_sum_to_one() {
        for k in [2, 3, 4, 6] {
            let taps = lowpass_taps(k);
            assert_eq!(taps.len(), TAPS_PER_FACTOR * k + 1);
            let sum: f32 = taps.iter().sum();
            assert!((sum - 1.0).abs() < 1e-4, "factor {k}: sum {sum}");
            for i in 0..taps.len() {
                assert!((taps[i] - taps[taps.len() - 1 - i]).abs() < 1e-6);
            }
        }
    }

    /// A tone well inside the low rate's band survives a raise and a lower
    /// with its level (a filter passband check), delayed by the two
    /// filters' group delay.
    #[test]
    fn a_tone_in_band_survives_raise_and_lower() {
        for (rate, k) in [(16_000_u32, 3_usize), (8_000, 6), (24_000, 2), (12_000, 4)] {
            let mut adapter = RateAdapter::new(k);
            let freq = 440.0_f32;
            let len = rate as usize; // one second
            let input: Vec<f32> = (0..len)
                .map(|i| {
                    1000.0 * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin()
                })
                .collect();
            let mut high = Vec::with_capacity(len * k);
            adapter.raise(&input, &mut high);
            assert_eq!(high.len(), len * k);
            let mut output = vec![0.0; len];
            adapter.lower(&high, &mut output);
            let skip = len / 4;
            let rms_in =
                (input[skip..].iter().map(|x| x * x).sum::<f32>() / (len - skip) as f32).sqrt();
            let rms_out =
                (output[skip..].iter().map(|x| x * x).sum::<f32>() / (len - skip) as f32).sqrt();
            let ratio = rms_out / rms_in;
            assert!(
                (0.97..1.03).contains(&ratio),
                "rate {rate}: level ratio {ratio}"
            );
        }
    }

    /// A tone above the low rate's band is removed by the lowering filter
    /// before it can fold back.
    #[test]
    fn a_tone_above_the_band_is_stopped_before_lowering() {
        let k = 3;
        let mut adapter = RateAdapter::new(k);
        let len = 48_000;
        // 12 kHz at 48 kHz: above the 8 kHz band edge of 16 kHz audio.
        let high: Vec<f32> = (0..len)
            .map(|i| 1000.0 * (2.0 * std::f32::consts::PI * 12_000.0 * i as f32 / 48_000.0).sin())
            .collect();
        let mut output = vec![0.0; len / k];
        adapter.lower(&high, &mut output);
        let skip = output.len() / 4;
        let rms = (output[skip..].iter().map(|x| x * x).sum::<f32>()
            / (output.len() - skip) as f32)
            .sqrt();
        assert!(rms < 10.0, "rms {rms} of a 1000-amplitude tone");
    }
}
