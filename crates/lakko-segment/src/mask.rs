// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! From the model's per-pixel person probability to the alpha the
//! compositor uses.
//!
//! Two steps, both chosen, not measured:
//!
//! 1. an exponential average per pixel across frames ([`SMOOTHING`] is the
//!    weight of the new frame), so the edge around the person does not
//!    flicker from one mask to the next;
//! 2. a linear ramp from [`ALPHA_LOW`] (fully background) to [`ALPHA_HIGH`]
//!    (fully person), so the uncertain band of the model becomes a soft edge
//!    rather than a hard cut, and pixels the model is fairly sure about are
//!    fully one or the other.

/// Weight of the newest probability in the per-pixel average.
pub const SMOOTHING: f32 = 0.6;

/// Probability at and below which a pixel is fully background (alpha 0).
pub const ALPHA_LOW: f32 = 0.3;

/// Probability at and above which a pixel is fully person (alpha 255).
pub const ALPHA_HIGH: f32 = 0.7;

/// Smooths successive probability maps of one fixed size and turns them into
/// `u8` alpha masks.
#[derive(Debug, Clone)]
pub struct MaskSmoother {
    average: Vec<f32>,
    mask: Vec<u8>,
    primed: bool,
}

impl MaskSmoother {
    /// A smoother for maps of `pixels` values.
    #[must_use]
    pub fn new(pixels: usize) -> Self {
        Self {
            average: vec![0.0; pixels],
            mask: vec![0; pixels],
            primed: false,
        }
    }

    /// Forgets the previous frames, so the next map is taken as it is (after a
    /// camera switch, or when the camera comes back on).
    pub fn reset(&mut self) {
        self.primed = false;
    }

    /// Folds `probabilities` (one per pixel, any value: a non-finite one reads
    /// as 0, others are clamped to `[0, 1]`) into the average and returns the
    /// alpha mask. A map of the wrong length leaves the state alone and
    /// returns the previous mask.
    pub fn update(&mut self, probabilities: &[f32]) -> &[u8] {
        if probabilities.len() != self.average.len() {
            return &self.mask;
        }
        for ((avg, out), &p) in self
            .average
            .iter_mut()
            .zip(self.mask.iter_mut())
            .zip(probabilities)
        {
            let p = if p.is_finite() {
                p.clamp(0.0, 1.0)
            } else {
                0.0
            };
            *avg = if self.primed {
                SMOOTHING.mul_add(p - *avg, *avg)
            } else {
                p
            };
            *out = alpha_of(*avg);
        }
        self.primed = true;
        &self.mask
    }

    /// The last mask returned by [`MaskSmoother::update`] (all background
    /// before the first one).
    #[must_use]
    pub fn mask(&self) -> &[u8] {
        &self.mask
    }
}

/// The alpha, `0..=255`, of a smoothed probability.
#[must_use]
pub fn alpha_of(probability: f32) -> u8 {
    let p = if probability.is_finite() {
        probability
    } else {
        0.0
    };
    let t = ((p - ALPHA_LOW) / (ALPHA_HIGH - ALPHA_LOW)).clamp(0.0, 1.0);
    // `t` is in [0, 1], so the product is in [0, 255].
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let a = (t * 255.0).round() as u8;
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn ramp_ends_and_middle() {
        assert_eq!(alpha_of(0.0), 0);
        assert_eq!(alpha_of(ALPHA_LOW), 0);
        assert_eq!(alpha_of(ALPHA_HIGH), 255);
        assert_eq!(alpha_of(1.0), 255);
        assert_eq!(alpha_of(0.5), 128);
        assert_eq!(alpha_of(f32::NAN), 0);
        assert_eq!(alpha_of(f32::INFINITY), 0);
    }

    #[test]
    fn first_map_is_taken_as_it_is_then_averaged() {
        let mut s = MaskSmoother::new(2);
        assert_eq!(s.update(&[1.0, 0.0]), &[255, 0]);
        // 0.6 * 0 + 0.4 * 1 = 0.4 -> (0.4 - 0.3) / 0.4 = 0.25 -> 64
        assert_eq!(s.update(&[0.0, 0.0]), &[64, 0]);
        s.reset();
        assert_eq!(s.update(&[0.0, 1.0]), &[0, 255]);
    }

    #[test]
    fn wrong_length_keeps_the_previous_mask() {
        let mut s = MaskSmoother::new(2);
        s.update(&[1.0, 1.0]);
        assert_eq!(s.update(&[0.0]), &[255, 255]);
    }

    proptest! {
        #[test]
        fn any_input_gives_a_mask_of_the_right_length(
            maps in proptest::collection::vec(proptest::collection::vec(any::<f32>(), 4), 1..6)
        ) {
            let mut s = MaskSmoother::new(4);
            for m in &maps {
                prop_assert_eq!(s.update(m).len(), 4);
            }
        }

        #[test]
        fn a_constant_input_converges_to_its_alpha(p in 0.0f32..=1.0) {
            let mut s = MaskSmoother::new(1);
            s.update(&[0.0]);
            for _ in 0..60 { s.update(&[p]); }
            let want = i32::from(alpha_of(p));
            let got = i32::from(s.mask()[0]);
            prop_assert!((want - got).abs() <= 1, "want {} got {}", want, got);
        }
    }
}
