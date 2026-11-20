// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The bundled segmentation model and its runner.
//!
//! The model is MediaPipe's selfie segmenter, landscape variant (Apache-2.0,
//! Google's model card of 2021-05-06), as the ONNX conversion published by
//! `onnx-community` on Hugging Face. ADR-0089 open question 1: a conversion of
//! Google's own file by a pinned script replaces it before release. Input:
//! RGB in `[0, 1]`, `[1, 3, 144, 256]` (NCHW). Output: one channel, the
//! probability that a pixel is the person.
//!
//! `tract` loads it with the value-info and output-shape annotations ignored
//! (the conversion's annotations are partly symbolic) and a fixed input fact,
//! as measured in the ADR's spike.

use std::io::Cursor;

use thiserror::Error;
#[allow(clippy::wildcard_imports)]
use tract_onnx::prelude::*;

use crate::mask::MaskSmoother;
use crate::{INPUT_HEIGHT, INPUT_PIXELS, INPUT_WIDTH};

const MODEL: &[u8] = include_bytes!("../model/selfie_segmenter_landscape.onnx");

/// SHA-256 of the bundled model file, lower-case hex. A test pins the file to
/// it, so a changed model is a reviewed change.
pub const MODEL_SHA256: &str = "7a0adcfdb1715d3b0ff0f61486d8a181c33f4a208343a5abfccbc6540e872d24";

/// How the pixels handed to [`Segmenter::segment`] are laid out. Each is
/// 8 bits per channel, rows top to bottom, no padding between rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelLayout {
    /// Red, green, blue, alpha (a browser canvas, an OpenGL ES read-back).
    Rgba,
    /// Blue, green, red, alpha (an iOS `CVPixelBuffer` in `32BGRA`).
    Bgra,
    /// Red, green, blue.
    Rgb,
}

impl PixelLayout {
    /// Bytes per pixel.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgba | Self::Bgra => 4,
            Self::Rgb => 3,
        }
    }

    /// Offsets of red, green and blue within one pixel.
    const fn rgb_offsets(self) -> [usize; 3] {
        match self {
            Self::Rgba | Self::Rgb => [0, 1, 2],
            Self::Bgra => [2, 1, 0],
        }
    }
}

/// Why the segmenter could not be made or could not run. The text is for logs
/// on the device; it never names the person or the picture.
#[derive(Debug, Error)]
pub enum SegmentError {
    /// The model could not be loaded or optimised.
    #[error("the segmentation model could not be loaded: {0}")]
    Load(String),
    /// The pixel buffer is not [`INPUT_WIDTH`] x [`INPUT_HEIGHT`] in the
    /// stated layout.
    #[error("the picture is {got} bytes, {want} expected")]
    WrongSize {
        /// Bytes given.
        got: usize,
        /// Bytes expected.
        want: usize,
    },
    /// The model ran and failed, or returned something other than one value
    /// per pixel.
    #[error("the segmentation model failed: {0}")]
    Run(String),
}

/// Runs the model and smooths its masks. One per camera; not shared between
/// threads at the same time (it is `Send`, so it may move to a worker).
pub struct Segmenter {
    state: TypedSimpleState,
    input: Vec<f32>,
    smoother: MaskSmoother,
}

impl std::fmt::Debug for Segmenter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Segmenter").finish_non_exhaustive()
    }
}

impl Segmenter {
    /// Loads and optimises the bundled model. Takes tens of milliseconds on a
    /// desktop (measured: 36-39 ms native, 151 ms in wasm, ADR-0089); call it
    /// off any real-time thread.
    ///
    /// # Errors
    ///
    /// [`SegmentError::Load`] if the model cannot be loaded.
    pub fn new() -> Result<Self, SegmentError> {
        let load = |e: TractError| SegmentError::Load(e.to_string());
        let model = tract_onnx::onnx()
            .with_ignore_value_info(true)
            .with_ignore_output_shapes(true)
            .model_for_read(&mut Cursor::new(MODEL))
            .map_err(load)?
            .with_input_fact(0, f32::fact([1, 3, INPUT_HEIGHT, INPUT_WIDTH]).into())
            .map_err(load)?
            .into_optimized()
            .map_err(load)?
            .into_runnable()
            .map_err(load)?;
        let state = model.spawn().map_err(load)?;
        Ok(Self {
            state,
            input: vec![0.0; 3 * INPUT_PIXELS],
            smoother: MaskSmoother::new(INPUT_PIXELS),
        })
    }

    /// Segments one picture of [`INPUT_WIDTH`] x [`INPUT_HEIGHT`] pixels (the
    /// camera frame scaled to that size, not mirrored) and returns the
    /// smoothed alpha mask, one byte per pixel, 255 where the person is.
    ///
    /// # Errors
    ///
    /// [`SegmentError::WrongSize`] for a buffer of the wrong length (the
    /// smoothing state is untouched); [`SegmentError::Run`] if the model
    /// fails.
    pub fn segment(&mut self, pixels: &[u8], layout: PixelLayout) -> Result<&[u8], SegmentError> {
        let bpp = layout.bytes_per_pixel();
        let want = INPUT_PIXELS * bpp;
        if pixels.len() != want {
            return Err(SegmentError::WrongSize {
                got: pixels.len(),
                want,
            });
        }
        let [r, g, b] = layout.rgb_offsets();
        let (red, rest) = self.input.split_at_mut(INPUT_PIXELS);
        let (green, blue) = rest.split_at_mut(INPUT_PIXELS);
        for (i, px) in pixels.chunks_exact(bpp).enumerate() {
            red[i] = f32::from(px[r]) / 255.0;
            green[i] = f32::from(px[g]) / 255.0;
            blue[i] = f32::from(px[b]) / 255.0;
        }
        let run = |e: TractError| SegmentError::Run(e.to_string());
        let tensor =
            Tensor::from_shape(&[1, 3, INPUT_HEIGHT, INPUT_WIDTH], &self.input).map_err(run)?;
        let outputs = self.state.run(tvec!(tensor.into())).map_err(run)?;
        let first = outputs
            .first()
            .ok_or_else(|| SegmentError::Run("no output".to_owned()))?;
        let view = first.to_plain_array_view::<f32>().map_err(run)?;
        if view.len() != INPUT_PIXELS {
            return Err(SegmentError::Run(format!(
                "{} output values, {INPUT_PIXELS} expected",
                view.len()
            )));
        }
        if let Some(probabilities) = view.as_slice() {
            return Ok(self.smoother.update(probabilities));
        }
        let probabilities: Vec<f32> = view.iter().copied().collect();
        Ok(self.smoother.update(&probabilities))
    }

    /// The last mask [`Segmenter::segment`] returned (all background before
    /// the first). A pipeline that runs the model below the camera's frame
    /// rate composites the frames in between with this one.
    #[must_use]
    pub fn mask(&self) -> &[u8] {
        self.smoother.mask()
    }

    /// Forgets the smoothing history (the camera was switched or turned back
    /// on).
    pub fn reset(&mut self) {
        self.smoother.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    #[test]
    fn the_bundled_model_is_the_pinned_one() {
        let digest = Sha256::digest(MODEL);
        let mut hex = String::new();
        for b in digest {
            let _ = write!(hex, "{b:02x}");
        }
        assert_eq!(hex, MODEL_SHA256);
    }

    fn picture(layout: PixelLayout, paint: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let bpp = layout.bytes_per_pixel();
        let offsets = layout.rgb_offsets();
        let mut out = vec![255u8; INPUT_PIXELS * bpp];
        for row in 0..INPUT_HEIGHT {
            for col in 0..INPUT_WIDTH {
                let at = (row * INPUT_WIDTH + col) * bpp;
                for (channel, value) in paint(col, row).into_iter().enumerate() {
                    out[at + offsets[channel]] = value;
                }
            }
        }
        out
    }

    #[test]
    fn loads_runs_and_agrees_across_layouts() {
        let mut s = Segmenter::new().expect("model loads");
        let f = |x: usize, y: usize| {
            #[allow(clippy::cast_possible_truncation)]
            let v = |k: usize| ((y * 7 + x * 3 + k * 50) % 256) as u8;
            [v(0), v(1), v(2)]
        };
        let rgba = s
            .segment(&picture(PixelLayout::Rgba, f), PixelLayout::Rgba)
            .expect("runs")
            .to_vec();
        assert_eq!(rgba.len(), INPUT_PIXELS);
        s.reset();
        let bgra = s
            .segment(&picture(PixelLayout::Bgra, f), PixelLayout::Bgra)
            .expect("runs")
            .to_vec();
        s.reset();
        let rgb = s
            .segment(&picture(PixelLayout::Rgb, f), PixelLayout::Rgb)
            .expect("runs")
            .to_vec();
        assert_eq!(rgba, bgra);
        assert_eq!(rgba, rgb);
    }

    #[test]
    fn a_wrong_size_is_refused_and_keeps_the_mask() {
        let mut s = Segmenter::new().expect("model loads");
        let before = s.mask().to_vec();
        assert!(matches!(
            s.segment(&[0u8; 12], PixelLayout::Rgba),
            Err(SegmentError::WrongSize { got: 12, .. })
        ));
        assert_eq!(s.mask(), before.as_slice());
    }

    #[test]
    fn a_flat_wall_is_background() {
        // A uniform grey picture has no person in it; the model must not call
        // most of it person.
        let mut s = Segmenter::new().expect("model loads");
        let mask = s
            .segment(
                &picture(PixelLayout::Rgb, |_, _| [128, 128, 128]),
                PixelLayout::Rgb,
            )
            .expect("runs");
        let person = mask.iter().filter(|&&a| a > 127).count();
        assert!(
            person * 10 < INPUT_PIXELS,
            "{person} of {INPUT_PIXELS} pixels read as person"
        );
    }
}
